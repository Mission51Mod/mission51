//! Synthetic FMDL with independent position/half-normal/half-UV streams, two material bindings,
//! two bind bones and asymmetric rectangular PNG atlases. No game data or installer is involved.
#![allow(dead_code)]
use foxcore::fmdl::{Block, Fmdl};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
pub const RED_PATH: &str = "/Assets/tpp/mecha/fixture/Pictures/panel";
pub const BLUE_PATH: &str = "/Assets/tpp/mecha/other/Pictures/panel";

pub struct ModelFixture {
    pub root: PathBuf,
    pub model: PathBuf,
    pub cache: PathBuf,
    pub broken: PathBuf,
    before: Vec<(PathBuf, Vec<u8>)>,
}

fn put16(b: &mut [u8], i: usize, n: u16) {
    b[i..i + 2].copy_from_slice(&n.to_le_bytes());
}
fn put32(b: &mut [u8], i: usize, n: u32) {
    b[i..i + 4].copy_from_slice(&n.to_le_bytes());
}
fn putf(b: &mut [u8], i: usize, n: f32) {
    put32(b, i, n.to_bits());
}

/// The expected values in tests are hand-written, not derived through the viewer decoder.
pub fn container() -> Fmdl {
    let mut f = Fmdl {
        head: [0; 32],
        s0off: 0x400,
        order: vec![0, 3, 4, 6, 7, 9, 10, 11, 14, 21, 22],
        info_order: vec![0, 3, 4, 6, 7, 9, 10, 11, 14, 21, 22],
        blocks: BTreeMap::new(),
        s1_infos: vec![(2, 16, 216)],
        section1: vec![0xab; 16],
        tail: vec![],
    };
    f.head[..4].copy_from_slice(b"FMDL");
    putf(&mut f.head, 4, 2.04);
    put32(&mut f.head, 8, 0x40);
    let block = |f: &mut Fmdl, id, rows: Vec<Vec<u8>>| {
        f.blocks.insert(
            id,
            Block {
                num: rows.len() as u16,
                raw: rows.concat(),
            },
        );
    };
    let mut bones = Vec::new();
    for (i, parent, local, world) in [
        (1u16, -1i16, [-1.0, -0.5, 0.0], [-1.0, -0.5, 0.0]),
        (2, 0, [2.2, 1.1, 0.0], [1.2, 0.6, 0.0]),
    ] {
        let mut b = vec![0; 48];
        put16(&mut b, 0, i);
        put16(&mut b, 2, parent as u16);
        for k in 0..3 {
            putf(&mut b, 16 + 4 * k, local[k]);
            putf(&mut b, 32 + 4 * k, world[k]);
        }
        bones.push(b);
    }
    block(&mut f, 0, bones);
    let mut md = Vec::new();
    for (id, material) in [(0, 1), (1, 0)] {
        let mut b = vec![0; 48];
        put16(&mut b, 4, material);
        put16(&mut b, 8, id);
        put16(&mut b, 10, 4);
        put32(&mut b, 16, id as u32 * 6);
        put32(&mut b, 20, 6);
        md.push(b);
    }
    block(&mut f, 3, md);
    let mut materials = Vec::new();
    for id in 0..2 {
        let mut b = vec![0; 16];
        b[6] = 1;
        put16(&mut b, 8, id);
        materials.push(b);
    }
    block(&mut f, 4, materials);
    block(&mut f, 6, vec![vec![0, 0, 0, 0], vec![0, 0, 1, 0]]);
    block(&mut f, 7, vec![vec![0, 0, 0, 0], vec![0, 0, 1, 0]]);
    let mut layouts = Vec::new();
    let mut headers = Vec::new();
    let mut elements = Vec::new();
    for id in 0..2 {
        let mut l = vec![2, 3, 0, 1, 0, 0, 0, 0];
        put16(&mut l, 4, id * 2);
        put16(&mut l, 6, id * 3);
        layouts.push(l);
        for (file, n) in [(0, 1), (1, 2)] {
            let mut h = vec![file, n, 12, 0, 0, 0, 0, 0];
            put32(&mut h, 4, id as u32 * 48);
            headers.push(h);
        }
        elements.extend([vec![0, 1, 0, 0], vec![2, 6, 0, 0], vec![8, 7, 8, 0]]);
    }
    block(&mut f, 9, layouts);
    block(&mut f, 10, headers);
    block(&mut f, 11, elements);
    let mut files = Vec::new();
    for (kind, len, off) in [(0, 96, 0), (0, 96, 96), (1, 24, 192)] {
        let mut b = vec![0; 16];
        put32(&mut b, 0, kind);
        put32(&mut b, 4, len);
        put32(&mut b, 8, off);
        files.push(b);
    }
    block(&mut f, 14, files);
    block(
        &mut f,
        21,
        [RED_PATH, BLUE_PATH]
            .iter()
            .map(|p| foxcore::qar::path_hash(p).to_le_bytes().to_vec())
            .collect(),
    );
    block(
        &mut f,
        22,
        ["Base_Tex_SRGB", "fixture_root", "fixture_child"]
            .iter()
            .map(|p| foxcore::hash::strcode64(p.as_bytes()).to_le_bytes().to_vec())
            .collect(),
    );
    for p in [
        [-2.0, -1.0, 0.0],
        [-0.2, -1.0, 0.0],
        [-0.2, 1.0, 0.0],
        [-2.0, 1.0, 0.0],
        [0.2, -0.6, 0.1],
        [2.0, -0.6, 0.1],
        [2.0, 0.6, 0.1],
        [0.2, 0.6, 0.1],
    ] {
        for v in p {
            f.section1.extend_from_slice(&f32::to_le_bytes(v));
        }
    }
    for id in 0..2 {
        for uv in [[0u16, 0], [0x3c00, 0], [0x3c00, 0x3c00], [0, 0x3c00]] {
            let normal = if id == 0 { [0, 0, 0x3c00, 0] } else { [0xbc00, 0, 0, 0] };
            for h in normal.into_iter().chain(uv) {
                f.section1.extend_from_slice(&u16::to_le_bytes(h));
            }
        }
    }
    for _ in 0..2 {
        for i in [0u16, 1, 2, 0, 2, 3] {
            f.section1.extend_from_slice(&i.to_le_bytes());
        }
    }
    assert_eq!(f.section1.len(), 232);
    f
}

impl ModelFixture {
    pub fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "foxstudio_model_{}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let model = root.join("model/Assets/tpp/mecha/fixture/Scenes/viewer.fmdl");
        let cache = root.join("cache");
        std::fs::create_dir_all(model.parent().unwrap()).unwrap();
        std::fs::write(&model, container().build()).unwrap();
        let red = cache.join("patch/Assets/tpp/mecha/fixture/Pictures/panel.png");
        let blue = cache.join("patch/Assets/tpp/mecha/other/Pictures/panel.png");
        for file in [&red, &blue] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        }
        // Top red/green, bottom blue/yellow. UV V=0 must sample the bottom row in the render proof.
        let colors = [
            [230, 35, 20, 255],
            [230, 35, 20, 255],
            [25, 210, 55, 255],
            [25, 210, 55, 255],
            [25, 55, 235, 255],
            [25, 55, 235, 255],
            [235, 220, 25, 255],
            [235, 220, 25, 255],
        ];
        image::save_buffer(&red, &colors.concat(), 4, 2, image::ExtendedColorType::Rgba8).unwrap();
        image::save_buffer(
            &blue,
            &[35, 100, 220, 255].repeat(8),
            8,
            1,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
        let broken = root.join("broken.fmdl");
        std::fs::write(&broken, b"broken model").unwrap();
        let mut f = Self {
            root,
            model,
            cache,
            broken,
            before: vec![],
        };
        f.before = snapshot(&f.root);
        f
    }
    pub fn options(&self) -> foxstudio::preview::model::LoadOptions {
        foxstudio::preview::model::LoadOptions {
            texture_roots: vec![self.cache.clone()],
            name_files: vec![],
        }
    }
    pub fn assert_unchanged(&self) {
        assert_eq!(snapshot(&self.root), self.before, "model/texture fixture was changed");
    }
}
fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(d: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let mut es: Vec<_> = std::fs::read_dir(d).unwrap().map(Result::unwrap).collect();
        es.sort_by_key(|e| e.path());
        for e in es {
            if e.file_type().unwrap().is_dir() {
                walk(&e.path(), out);
            } else {
                out.push((e.path(), std::fs::read(e.path()).unwrap()));
            }
        }
    }
    let mut out = vec![];
    walk(root, &mut out);
    out
}
impl Drop for ModelFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
