//! Read-only FMDL inspection on a serial CPU worker. The proven foxcore container owns file layout;
//! this viewer adapter reads attributes/materials/bind bones without changing the codec or source assets.
use super::gpu::{LineVertex, ModelVertex};
use super::texture::{self, Image};
use foxcore::fmdl::Fmdl;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const MAX_EDGES: usize = 400_000;
pub const MAX_NORMALS: usize = 4_000;
const MAX_FILE: u64 = 128 * 1024 * 1024;
const MAX_VERTICES: usize = 2_000_000;
const MAX_INDICES: usize = 6_000_000;
const MAX_TEXTURE_BYTES: usize = 96 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct ModelInfo {
    pub path: String,
    pub bytes: u64,
    pub version: f32,
    pub meshes: usize,
    pub vertices: usize,
    pub triangles: usize,
    pub bbox: ([f32; 3], [f32; 3]),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mesh {
    pub id: usize,
    pub material: usize,
    pub vertices: Range<usize>,
    pub indices: Range<u32>,
    pub has_uv: bool,
    pub source_normals: bool,
    pub alpha_mode: u8,
}

#[derive(Clone, Debug)]
pub struct Material {
    pub id: usize,
    pub base_hash: Option<u64>,
    pub base_label: String,
    pub linear: bool,
    pub path: Option<PathBuf>,
    pub image: Option<Arc<Vec<Image>>>,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bone {
    pub id: usize,
    pub name: String,
    pub parent: Option<usize>,
    pub local: [f32; 3],
    pub world: [f32; 3],
}

#[derive(Clone, Debug)]
pub struct Model {
    pub info: ModelInfo,
    pub verts: Vec<ModelVertex>,
    pub indices: Vec<u32>,
    pub meshes: Vec<Mesh>,
    pub materials: Vec<Material>,
    pub bones: Vec<Bone>,
    pub edges: Vec<LineVertex>,
    pub normals: Vec<LineVertex>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadOptions {
    /// Ordered unpacked/project texture folders. Only exact asset-path hashes bind; no filename guessing.
    pub texture_roots: Vec<PathBuf>,
    /// Optional user-supplied name dictionaries; unresolved bones retain their source hash.
    pub name_files: Vec<PathBuf>,
}

pub fn load(path: &Path) -> Result<Model, String> {
    load_with(path, &LoadOptions::default())
}

pub fn load_with(path: &Path, options: &LoadOptions) -> Result<Model, String> {
    let n = std::fs::metadata(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .len();
    if n > MAX_FILE {
        return Err("model exceeds the 128 MiB inspection limit".into());
    }
    let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut model = decode(path, &b)?;
    model.resolve_textures(path, options);
    let mut names = HashMap::new();
    for file in &options.name_files {
        if std::fs::metadata(file).is_ok_and(|metadata| metadata.len() < 8 * 1024 * 1024)
            && let Ok(text) = std::fs::read_to_string(file)
        {
            for name in text.lines().map(str::trim).filter(|s| !s.is_empty()) {
                names
                    .entry(foxcore::hash::strcode64(name.as_bytes()))
                    .or_insert_with(|| name.to_string());
            }
        }
    }
    for bone in &mut model.bones {
        if let Ok(hash) = u64::from_str_radix(&bone.name, 16)
            && let Some(name) = names.get(&hash)
        {
            bone.name.clone_from(name);
        }
    }
    Ok(model)
}

fn u16at(b: &[u8], at: usize) -> Result<u16, String> {
    Ok(u16::from_le_bytes(
        b.get(at..at + 2).ok_or("truncated FMDL table")?.try_into().unwrap(),
    ))
}
fn u32at(b: &[u8], at: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        b.get(at..at + 4).ok_or("truncated FMDL table")?.try_into().unwrap(),
    ))
}
fn u64at(b: &[u8], at: usize) -> Result<u64, String> {
    Ok(u64::from_le_bytes(
        b.get(at..at + 8)
            .ok_or("truncated FMDL name/path table")?
            .try_into()
            .unwrap(),
    ))
}
fn f32at(b: &[u8], at: usize) -> Result<f32, String> {
    Ok(f32::from_bits(u32at(b, at)?))
}

/// Exact binary16 conversion, including subnormals; nonfinite data is rejected by the channel reader.
pub fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = (h >> 10) & 31;
    let m = h & 1023;
    sign * match e {
        0 => (m as f32) * 2.0f32.powi(-24),
        31 => {
            if m == 0 {
                f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => (1.0 + m as f32 / 1024.0) * 2.0f32.powi(e as i32 - 15),
    }
}

fn attributes(
    container: &Fmdl,
    layout_index: usize,
    vertex_count: usize,
    usage: u8,
) -> Result<Option<Vec<[f32; 3]>>, String> {
    let layouts = container.entries(9);
    let headers = container.entries(10);
    let elements = container.entries(11);
    let files = container.entries(14);
    let layout_header = layouts.get(layout_index).ok_or("missing vertex layout")?;
    let mut element_index = u16at(layout_header, 6)? as usize;
    let first_buffer = u16at(layout_header, 4)? as usize;
    let (base, size) = container
        .s1_infos
        .iter()
        .find(|channel_value| channel_value.0 == 2)
        .map(|channel_value| (channel_value.1 as usize, channel_value.2 as usize))
        .ok_or("missing vertex/index section")?;
    let section = container
        .section1
        .get(base..base + size)
        .ok_or("vertex section values of range")?;
    for buffer_index in 0..layout_header[0] as usize {
        let buffer_header = headers
            .get(first_buffer + buffer_index)
            .ok_or("missing vertex buffer header")?;
        let stride = buffer_header[2] as usize;
        for element_offset in 0..buffer_header[1] as usize {
            let element = elements
                .get(element_index + element_offset)
                .ok_or("missing vertex element")?;
            if element[0] != usage {
                continue;
            }
            let components = if usage == 8 { 2 } else { 3 };
            let (component_width, stored_components) = match (usage, element[1]) {
                (0, 1) | (2, 1) | (8, 1) => (4, components),
                (2, 6) => (2, 4),
                (8, 7) => (2, 2),
                _ => return Err(format!("unsupported usage {usage} format {}", element[1])),
            };
            let attribute_offset = u16at(element, 2)? as usize;
            if stride == 0 || attribute_offset + component_width * stored_components > stride {
                return Err("attribute exceeds vertex stride".into());
            }
            let file = files
                .get(buffer_header[0] as usize)
                .ok_or("missing vertex file buffer")?;
            let start = u32at(file, 8)? as usize;
            let len = u32at(file, 4)? as usize;
            let buffer = section
                .get(start..start + len)
                .ok_or("vertex file buffer values of range")?;
            let buffer_start = u32at(buffer_header, 4)? as usize;
            if buffer_start
                .checked_add(stride * vertex_count)
                .is_none_or(|end| end > buffer.len())
            {
                return Err("truncated vertex buffer".into());
            }
            let mut values = Vec::with_capacity(vertex_count);
            for vertex_index in 0..vertex_count {
                let at = buffer_start + stride * vertex_index + attribute_offset;
                let mut value = [0.0; 3];
                for (component_index, channel_value) in value.iter_mut().take(components).enumerate() {
                    *channel_value = if component_width == 2 {
                        half_to_f32(u16at(buffer, at + component_width * component_index)?)
                    } else {
                        f32at(buffer, at + component_width * component_index)?
                    };
                }
                if value.iter().any(|channel_value| !channel_value.is_finite()) {
                    return Err(format!("nonfinite usage {usage} at vertex {vertex_index}"));
                }
                values.push(value);
            }
            return Ok(Some(values));
        }
        element_index += buffer_header[1] as usize;
    }
    Ok(None)
}

pub fn decode(path: &Path, b: &[u8]) -> Result<Model, String> {
    if b.len() as u64 > MAX_FILE {
        return Err("model exceeds the inspection limit".into());
    }
    // Bound the generic container's allocations before passing untrusted preview input to it.
    if u32at(b, 0x20)? > 64 || u32at(b, 0x24)? > 64 {
        return Err("too many FMDL blocks".into());
    }
    let version = f32at(b, 4)?;
    if (version - 2.04).abs() > 0.0001 || !version.is_finite() {
        return Err(format!("unsupported FMDL version {version} (expected 2.04)"));
    }
    let f = foxcore::fmdl::read(b)?;
    let (base, size) = f
        .s1_infos
        .iter()
        .find(|x| x.0 == 2)
        .map(|x| (x.1 as usize, x.2 as usize))
        .ok_or("missing geometry section")?;
    let section = f
        .section1
        .get(base..base + size)
        .ok_or("geometry section out of range")?;
    let files = f.entries(14);
    let index_file = files
        .iter()
        .find(|e| u32at(e, 0) == Ok(1))
        .ok_or("missing 16-bit triangle buffer")?;
    let start = u32at(index_file, 8)? as usize;
    let index_buf = section
        .get(start..start + u32at(index_file, 4)? as usize)
        .ok_or("index file buffer out of range")?;
    let mut verts = Vec::new();
    let mut indices = Vec::new();
    let mut meshes = Vec::new();
    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    let mut notes = Vec::new();
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for (id, md) in f.entries(3).iter().enumerate() {
        let nv = u16at(md, 10)? as usize;
        let layout = u16at(md, 8)? as usize;
        let count = u32at(md, 20)? as usize;
        if nv == 0 || count == 0 {
            notes.push(format!("mesh {id}: empty high-LOD geometry"));
            continue;
        }
        if verts.len() + nv > MAX_VERTICES || indices.len() + count > MAX_INDICES {
            return Err("model exceeds vertex/index inspection budget".into());
        }
        let pos = attributes(&f, layout, nv, 0)
            .map_err(|e| format!("mesh {id}: {e}"))?
            .ok_or_else(|| format!("mesh {id}: no supported positions"))?;
        let mut channel = |usage| match attributes(&f, layout, nv, usage) {
            Ok(Some(a)) => Some(a),
            Ok(None) => {
                notes.push(format!(
                    "mesh {id}: {} absent",
                    if usage == 8 { "UV0" } else { "source normals" }
                ));
                None
            }
            Err(e) => {
                notes.push(format!("mesh {id}: {e}"));
                None
            }
        };
        let normals = channel(2);
        let uv = channel(8);
        let mut smooth = vec![[0.0; 3]; nv];
        let first = u32at(md, 16)? as usize * 2;
        if !count.is_multiple_of(3) {
            return Err(format!("mesh {id}: index count is not a triangle list"));
        }
        let raw = index_buf
            .get(first..first + count * 2)
            .ok_or_else(|| format!("mesh {id}: truncated indices"))?;
        let vb = verts.len();
        let ib = indices.len() as u32;
        for tri in raw.chunks_exact(6) {
            let t = [
                u16at(tri, 0)? as usize,
                u16at(tri, 2)? as usize,
                u16at(tri, 4)? as usize,
            ];
            if t.iter().any(|&v| v >= nv) {
                return Err(format!("mesh {id}: triangle index outside vertex buffer"));
            }
            let (a, b, c) = (pos[t[0]], pos[t[1]], pos[t[2]]);
            let u = sub(b, a);
            let v = sub(c, a);
            let n = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            for &k in &t {
                for j in 0..3 {
                    smooth[k][j] += n[j];
                }
                indices.push((vb + k) as u32);
            }
            for (x, y) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                if edges.len() < MAX_EDGES * 2 && seen.insert((vb + x.min(y), vb + x.max(y))) {
                    for k in [x, y] {
                        edges.push(LineVertex {
                            pos: pos[k],
                            color: [0.1, 0.65, 1.0, 0.55],
                        });
                    }
                }
            }
        }
        let source_normals = normals
            .as_ref()
            .is_some_and(|n| n.iter().all(|v| v.iter().map(|x| x * x).sum::<f32>() > 1e-12));
        if normals.is_some() && !source_normals {
            notes.push(format!("mesh {id}: zero source normals; using geometry normals"));
        }
        for (k, &pos) in pos.iter().enumerate() {
            for j in 0..3 {
                lo[j] = lo[j].min(pos[j]);
                hi[j] = hi[j].max(pos[j]);
            }
            let nrm = unit(if source_normals {
                normals.as_ref().unwrap()[k]
            } else {
                smooth[k]
            });
            let uv0 = uv.as_ref().map_or([0.0; 2], |u| [u[k][0], u[k][1]]);
            verts.push(ModelVertex { pos, nrm, uv: uv0 });
        }
        meshes.push(Mesh {
            id,
            material: u16at(md, 4)? as usize,
            vertices: vb..verts.len(),
            indices: ib..indices.len() as u32,
            has_uv: uv.is_some(),
            source_normals,
            alpha_mode: md[0],
        });
    }
    if meshes.is_empty() {
        return Err("no high-LOD triangle mesh".into());
    }
    let names = f.entries(22);
    let mut bones = Vec::new();
    for (id, bone) in f.entries(0).iter().enumerate() {
        let hash = names
            .get(u16at(bone, 0)? as usize)
            .map(|b| u64at(b, 0))
            .transpose()?
            .unwrap_or(0)
            & 0xffff_ffff_ffff;
        let parent = i16::from_le_bytes(bone[2..4].try_into().unwrap());
        let read3 =
            |at| -> Result<[f32; 3], String> { Ok([f32at(bone, at)?, f32at(bone, at + 4)?, f32at(bone, at + 8)?]) };
        let (local, world) = (read3(16)?, read3(32)?);
        if local.iter().chain(&world).any(|v| !v.is_finite()) {
            return Err(format!("bone {id}: nonfinite bind position"));
        }
        bones.push(Bone {
            id,
            name: format!("{hash:012x}"),
            parent: (parent >= 0).then_some(parent as usize),
            local,
            world,
        });
    }
    for bone in &bones {
        let mut seen = HashSet::new();
        let mut at = Some(bone.id);
        while let Some(i) = at {
            if !seen.insert(i) {
                return Err(format!("bone {}: cyclic hierarchy", bone.id));
            }
            at = bones.get(i).ok_or("bone parent outside table")?.parent;
        }
    }
    let materials = read_materials(&f, &mut notes)?;
    for mesh in &meshes {
        if mesh.material >= materials.len() {
            notes.push(format!("mesh {}: missing material {}", mesh.id, mesh.material));
        }
        if matches!(mesh.alpha_mode, 0x10 | 0x11) {
            notes.push(format!(
                "mesh {}: Fox translucent/emissive shader is not reproduced (opaque inspection)",
                mesh.id
            ));
        }
    }
    let diag = sub(hi, lo).iter().map(|x| x * x).sum::<f32>().sqrt().max(0.01);
    let step = verts.len().div_ceil(MAX_NORMALS).max(1);
    let mut normal_lines = Vec::new();
    for v in verts.iter().step_by(step) {
        for pos in [v.pos, std::array::from_fn(|k| v.pos[k] + v.nrm[k] * diag * 0.018)] {
            normal_lines.push(LineVertex {
                pos,
                color: [0.35, 1.0, 0.4, 0.9],
            });
        }
    }
    let info = ModelInfo {
        path: path.display().to_string(),
        bytes: b.len() as u64,
        version,
        meshes: meshes.len(),
        vertices: verts.len(),
        triangles: indices.len() / 3,
        bbox: (lo, hi),
    };
    Ok(Model {
        info,
        verts,
        indices,
        meshes,
        materials,
        bones,
        edges,
        normals: normal_lines,
        notes,
    })
}

fn read_materials(f: &Fmdl, notes: &mut Vec<String>) -> Result<Vec<Material>, String> {
    let names = f.entries(22);
    let paths = f.entries(21);
    let params = f.entries(7);
    let textures = f.entries(6);
    let label_hash = |s: &str| foxcore::hash::strcode64(s.as_bytes());
    let mut out = Vec::new();
    for (id, m) in f.entries(4).iter().enumerate() {
        let mut mat = Material {
            id,
            base_hash: None,
            base_label: String::new(),
            linear: false,
            path: None,
            image: None,
            status: "no albedo binding".into(),
        };
        for k in 0..m[6] as usize {
            let p = params
                .get(u16at(m, 8)? as usize + k)
                .ok_or("material texture parameter out of range")?;
            let label = u64at(
                names
                    .get(u16at(p, 0)? as usize)
                    .ok_or("material name index out of range")?,
                0,
            )? & 0xffff_ffff_ffff;
            let semantic = if label == label_hash("Base_Tex_SRGB") {
                "Base_Tex_SRGB"
            } else if label == label_hash("Base_Tex_LIN") {
                "Base_Tex_LIN"
            } else {
                continue;
            };
            let t = textures
                .get(u16at(p, 2)? as usize)
                .ok_or("material texture reference out of range")?;
            let hash = u64at(
                paths
                    .get(u16at(t, 2)? as usize)
                    .ok_or("material path index out of range")?,
                0,
            )? & foxcore::dict::PATH_MASK;
            if mat.base_hash.is_some() {
                notes.push(format!("material {id}: multiple base bindings; first retained"));
                continue;
            }
            mat.base_hash = Some(hash);
            mat.base_label = semantic.into();
            mat.linear = semantic.ends_with("LIN");
            mat.status = format!("unresolved albedo {hash:013x}");
        }
        out.push(mat);
    }
    Ok(out)
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|k| a[k] - b[k])
}
fn unit(a: [f32; 3]) -> [f32; 3] {
    let n = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 1e-12 { a.map(|x| x / n) } else { [0.0, 1.0, 0.0] }
}

/// Canonical asset path, not a basename. Hashed extraction filenames are supported separately.
pub fn asset_path(path: &Path) -> Option<String> {
    let mut parts: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let i = parts.iter().rposition(|s| s.eq_ignore_ascii_case("Assets"))?;
    *parts.last_mut()? = path.file_stem()?.to_string_lossy().into_owned();
    Some(format!("/{}", parts[i..].join("/")))
}

impl Model {
    fn resolve_textures(&mut self, path: &Path, options: &LoadOptions) {
        if !self.materials.iter().any(|m| m.base_hash.is_some()) {
            return;
        }
        let mut roots = options.texture_roots.clone();
        if let Some(assets) = path
            .ancestors()
            .find(|p| p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("Assets")))
        {
            if let Some(archive) = assets.parent() {
                roots.push(archive.to_path_buf());
            }
        } else if let Some(parent) = path.parent() {
            roots.push(parent.to_path_buf());
        }
        let files = crate::preview_view::list_files(&roots, &["ftex", "dds", "png"], 20_000);
        if files.len() >= 20_000 {
            self.notes
                .push("texture search reached its 20,000-file limit; narrow the project texture folders".into());
        }
        let wanted: HashSet<u64> = self.materials.iter().filter_map(|m| m.base_hash).collect();
        let mut found: HashMap<u64, PathBuf> = HashMap::new();
        for file in files {
            let hash = asset_path(&file)
                .map(|p| foxcore::qar::path_hash(&p) & foxcore::dict::PATH_MASK)
                .or_else(|| {
                    file.file_stem()
                        .and_then(|s| u64::from_str_radix(s.to_str()?, 16).ok())
                        .map(|h| h & foxcore::dict::PATH_MASK)
                });
            if let Some(h) = hash.filter(|h| wanted.contains(h)) {
                found.entry(h).or_insert(file);
            }
        }
        let mut images: HashMap<PathBuf, Result<Arc<Vec<Image>>, String>> = HashMap::new();
        let mut used = 0;
        for m in &mut self.materials {
            let Some(file) = m.base_hash.and_then(|h| found.get(&h)) else {
                continue;
            };
            let image = images.entry(file.clone()).or_insert_with(|| {
                check_albedo_header(file, MAX_TEXTURE_BYTES.saturating_sub(used))?;
                let t = texture::load(file, 1)?;
                let img = t.levels.into_iter().next().ok_or("no texture level decoded")?;
                if img.width > 4096 || img.height > 4096 || used + img.rgba.len() > MAX_TEXTURE_BYTES {
                    return Err("texture exceeds 4096-side / 96 MiB model budget".into());
                }
                if (img.width, img.height) != (t.width, t.height) {
                    self.notes.push(format!(
                        "{}: highest available mip {} x {}, declared {} x {}",
                        file.display(),
                        img.width,
                        img.height,
                        t.width,
                        t.height
                    ));
                }
                let levels = mip_levels(img);
                let bytes = levels.iter().map(|i| i.rgba.len()).sum::<usize>();
                if used + bytes > MAX_TEXTURE_BYTES {
                    return Err("model texture mip budget exceeded".into());
                }
                used += bytes;
                Ok(Arc::new(levels))
            });
            m.path = Some(file.clone());
            match image {
                Ok(img) => {
                    m.status = format!("{} x {} · {}", img[0].width, img[0].height, m.base_label);
                    m.image = Some(img.clone());
                }
                Err(e) => {
                    m.status = format!("albedo decode failed: {e}");
                }
            }
        }
    }

    /// Distinct image/colour-space pairs, shared across materials before any GPU allocation.
    pub fn unique_texture_materials(&self) -> Vec<&Material> {
        let mut seen = HashSet::new();
        self.materials
            .iter()
            .filter(|m| {
                m.image
                    .as_ref()
                    .is_some_and(|img| seen.insert((Arc::as_ptr(img) as usize, m.linear)))
            })
            .collect()
    }

    /// X-ray bind-pose links and small joint crosses. There is no inferred animation or IK.
    pub fn bone_lines(&self, selected: Option<usize>) -> Vec<LineVertex> {
        let diag = sub(self.info.bbox.1, self.info.bbox.0)
            .iter()
            .map(|x| x * x)
            .sum::<f32>()
            .sqrt()
            .max(0.01);
        let mut lines = Vec::new();
        for bone in &self.bones {
            let color = if selected == Some(bone.id) {
                [1.0, 0.9, 0.2, 1.0]
            } else {
                [1.0, 0.45, 0.12, 0.8]
            };
            if let Some(p) = bone.parent {
                for pos in [self.bones[p].world, bone.world] {
                    lines.push(LineVertex { pos, color });
                }
            }
            for axis in 0..3 {
                for s in [-1.0, 1.0] {
                    let mut pos = bone.world;
                    pos[axis] += s * diag * 0.004;
                    lines.push(LineVertex { pos, color });
                }
            }
        }
        lines
    }
}

/// Preserve rectangular atlas dimensions and row order; all CPU mip work stays on the load worker.
pub fn mip_levels(base: Image) -> Vec<Image> {
    let mut levels = vec![base];
    while levels.last().unwrap().width > 1 || levels.last().unwrap().height > 1 {
        let src = levels.last().unwrap();
        let (w, h) = ((src.width / 2).max(1), (src.height / 2).max(1));
        let mut rgba = vec![0; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let (xa, xb) = (x * src.width / w, (x + 1) * src.width / w);
                let (ya, yb) = (y * src.height / h, (y + 1) * src.height / h);
                for c in 0..4 {
                    let mut sum = 0u32;
                    for yy in ya..yb {
                        for xx in xa..xb {
                            sum += src.rgba[4 * (yy * src.width + xx) + c] as u32;
                        }
                    }
                    rgba[4 * (y * w + x) + c] = (sum / ((xb - xa) * (yb - ya)) as u32) as u8;
                }
            }
        }
        levels.push(Image {
            width: w,
            height: h,
            rgba,
        });
    }
    levels
}

fn check_albedo_header(path: &Path, available: usize) -> Result<(), String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if f.metadata().map_err(|e| e.to_string())?.len() > MAX_FILE {
        return Err("albedo file exceeds inspection limit".into());
    }
    let mut b = [0u8; 32];
    let n = f.read(&mut b).map_err(|e| e.to_string())?;
    let (w, h) = if n >= 16 && &b[..4] == b"FTEX" {
        if u16at(&b, 14)? > 1 {
            return Err("3-D albedo is unsupported on a model".into());
        }
        (u16at(&b, 10)? as usize, u16at(&b, 12)? as usize)
    } else if n >= 24 && &b[..8] == b"\x89PNG\r\n\x1a\n" {
        (
            u32::from_be_bytes(b[16..20].try_into().unwrap()) as usize,
            u32::from_be_bytes(b[20..24].try_into().unwrap()) as usize,
        )
    } else if n >= 20 && &b[..4] == b"DDS " {
        (u32at(&b, 16)? as usize, u32at(&b, 12)? as usize)
    } else {
        return Err("unrecognised albedo header".into());
    };
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return Err(format!("albedo {w} x {h} exceeds 4096-side inspection budget"));
    }
    // Conservative mip estimate before decoding; actual bytes are checked after CPU mip preparation.
    let (mut mw, mut mh, mut need) = (w, h, 0);
    loop {
        need += mw * mh * 4;
        if mw == 1 && mh == 1 {
            break;
        }
        mw = (mw / 2).max(1);
        mh = (mh / 2).max(1);
    }
    if need > available {
        return Err("albedo exceeds remaining model memory budget".into());
    }
    Ok(())
}
