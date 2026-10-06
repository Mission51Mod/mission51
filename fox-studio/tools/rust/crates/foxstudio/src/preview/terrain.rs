//! Terrain data for previews: a height grid (heights.npy as float32 / float64, or one .htre tile), a CPU hillshade
//! (works without a GPU and in the headless tests), and the downsampled mesh the 3-D view draws.
//! Grid convention: h[row][col] sits at world (x0 + col * d, z0 + row * d).
use super::gpu::{LineVertex, TerrainVertex};
use super::texture::Image;
use std::path::Path;

#[derive(Clone, Debug)]
pub struct Terrain {
    pub rows: usize,
    pub cols: usize,
    pub h: Vec<f32>,
    pub cell: f32,
    pub x0: f32,
    pub z0: f32,
    pub min: f32,
    pub max: f32,
}

impl Terrain {
    pub fn new(rows: usize, cols: usize, h: Vec<f32>, cell: f32, x0: f32, z0: f32) -> Result<Terrain, String> {
        if rows < 2 || cols < 2 || h.len() != rows * cols {
            return Err(format!("a {rows} x {cols} grid needs {} heights, got {}", rows * cols, h.len()));
        }
        let finite = h.iter().copied().filter(|v| v.is_finite());
        let (min, max) = finite.fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(v), b.max(v)));
        if min > max {
            return Err("no finite heights".into());
        }
        Ok(Terrain { rows, cols, h, cell, x0, z0, min, max })
    }

    #[inline]
    pub fn at(&self, r: usize, c: usize) -> f32 {
        let v = self.h[r * self.cols + c];
        if v.is_finite() { v } else { self.min }
    }

    /// world extent: (x0, z0, x1, z1)
    pub fn extent(&self) -> (f32, f32, f32, f32) {
        (self.x0, self.z0, self.x0 + (self.cols - 1) as f32 * self.cell, self.z0 + (self.rows - 1) as f32 * self.cell)
    }

    /// bilinear height at a world position (clamped to the grid)
    pub fn height(&self, x: f32, z: f32) -> f32 {
        let fx = ((x - self.x0) / self.cell).clamp(0.0, (self.cols - 1) as f32);
        let fz = ((z - self.z0) / self.cell).clamp(0.0, (self.rows - 1) as f32);
        let (c, r) = ((fx.floor() as usize).min(self.cols - 2), (fz.floor() as usize).min(self.rows - 2));
        let (tx, tz) = (fx - c as f32, fz - r as f32);
        (self.at(r, c) * (1.0 - tx) + self.at(r, c + 1) * tx) * (1.0 - tz) + (self.at(r + 1, c) * (1.0 - tx) + self.at(r + 1, c + 1) * tx) * tz
    }
}

/// heights.npy (2-D float32 / float64)
pub fn load_npy(path: &Path, cell: f32, origin: Option<(f32, f32)>) -> Result<Terrain, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let array = foxcore::npy::Npy::read(&bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    if array.shape.len() != 2 {
        return Err(format!("{}: expected a 2-D array, got shape {:?}", path.display(), array.shape));
    }
    // The checked reader converts both endian orders and Fortran storage to the preview's row-major grid.
    let heights = array.to_f32_c_order().map_err(|error| format!("{}: {error}", path.display()))?;
    let (rows, cols) = (array.shape[0], array.shape[1]);
    let (x0, z0) = origin.unwrap_or((-(cols as f32) * cell / 2.0, -(rows as f32) * cell / 2.0));
    Terrain::new(rows, cols, heights, cell, x0, z0)
}

/// One .htre block (64 x 64 heights, four clusters in file order).
pub fn load_htre(path: &Path, cell: f32) -> Result<Terrain, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let tile = foxcore::terrain::read_htre(&bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    let grid = foxcore::terrain::file_to_grid(&tile.heights)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Terrain::new(64, 64, grid, cell, 0.0, 0.0)
}

pub fn load(path: &Path, cell: f32, origin: Option<(f32, f32)>) -> Result<Terrain, String> {
    match path.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref() {
        Some("npy") => load_npy(path, cell, origin),
        Some("htre") => load_htre(path, cell),
        _ => Err(format!("{}: terrain previews read .npy height grids and .htre blocks", path.display())),
    }
}

/// The part worth framing: cells above the water (or above the lowest 2 % of the height range), plus a margin.
/// The whole grid when nothing stands out.
pub fn focus(t: &Terrain, water: Option<f32>) -> (f32, f32, f32, f32) {
    let level = water.unwrap_or(t.min + 0.02 * (t.max - t.min));
    let (mut r0, mut r1, mut c0, mut c1) = (usize::MAX, 0usize, usize::MAX, 0usize);
    let step = step_for(t, 512);
    for r in (0..t.rows).step_by(step) {
        for c in (0..t.cols).step_by(step) {
            if t.at(r, c) > level {
                r0 = r0.min(r);
                r1 = r1.max(r);
                c0 = c0.min(c);
                c1 = c1.max(c);
            }
        }
    }
    let full = t.extent();
    if r0 > r1 {
        return full;
    }
    let (x0, z0) = (t.x0 + c0 as f32 * t.cell, t.z0 + r0 as f32 * t.cell);
    let (x1, z1) = (t.x0 + c1 as f32 * t.cell, t.z0 + r1 as f32 * t.cell);
    let m = 0.12 * (x1 - x0).max(z1 - z0).max(50.0);
    ((x0 - m).max(full.0), (z0 - m).max(full.1), (x1 + m).min(full.2), (z1 + m).min(full.3))
}

/// sample step so the larger side spans at most `max_side` intervals
pub fn step_for(t: &Terrain, max_side: usize) -> usize {
    (t.rows.max(t.cols) - 1).div_ceil(max_side.max(1)).max(1)
}

/// Shaded relief (multi-directional hillshade over a height ramp, water below `water`), north (+z) up.
/// One output pixel per `step` grid cells.
pub fn hillshade(t: &Terrain, max_side: usize, water: Option<f32>, exaggeration: f32) -> Image {
    hillshade_with(t, max_side, water, exaggeration, None)
}

/// Shaded relief with an optional world texture as the base colour (`tex` covers the grid; row 0 = smallest z).
pub fn hillshade_with(t: &Terrain, max_side: usize, water: Option<f32>, exaggeration: f32, tex: Option<&Image>) -> Image {
    let step = step_for(t, max_side);
    let (w, h) = (t.cols.div_ceil(step), t.rows.div_ceil(step));
    let mut rgba = vec![0u8; w * h * 4];
    let range = (t.max - t.min).max(1e-3);
    let lights: [(f32, f32, f32); 3] = [(315.0, 45.0, 0.6), (270.0, 40.0, 0.2), (0.0, 50.0, 0.2)];
    let lv: Vec<[f32; 3]> = lights
        .iter()
        .map(|&(az, el, _)| {
            let (a, e) = (az.to_radians(), el.to_radians());
            [e.cos() * a.sin(), e.cos() * a.cos(), e.sin()]
        })
        .collect();
    for oy in 0..h {
        // north up: the top row of the picture is the largest z
        let r = ((h - 1 - oy) * step).min(t.rows - 1);
        for ox in 0..w {
            let c = (ox * step).min(t.cols - 1);
            let (cl, cr) = (c.saturating_sub(step), (c + step).min(t.cols - 1));
            let (rd, ru) = (r.saturating_sub(step), (r + step).min(t.rows - 1));
            let dzdx = (t.at(r, cr) - t.at(r, cl)) * exaggeration / (((cr - cl).max(1) as f32) * t.cell);
            let dzdy = (t.at(ru, c) - t.at(rd, c)) * exaggeration / (((ru - rd).max(1) as f32) * t.cell);
            let n = {
                let v = [-dzdx, -dzdy, 1.0];
                let l = (v[0] * v[0] + v[1] * v[1] + 1.0).sqrt();
                [v[0] / l, v[1] / l, v[2] / l]
            };
            let mut shade = 0.0;
            for (k, l) in lv.iter().enumerate() {
                shade += lights[k].2 * (n[0] * l[0] + n[1] * l[1] + n[2] * l[2]).max(0.0);
            }
            let hv = t.at(r, c);
            let base = if let Some(tx) = tex {
                let u = c as f32 / (t.cols - 1) as f32;
                let v = r as f32 / (t.rows - 1) as f32;
                let (px, py) = ((u * (tx.width - 1) as f32).round() as usize, (v * (tx.height - 1) as f32).round() as usize);
                let i = 4 * (py * tx.width + px);
                let col = [tx.rgba[i] as f32 / 255.0, tx.rgba[i + 1] as f32 / 255.0, tx.rgba[i + 2] as f32 / 255.0];
                if water.is_some_and(|wl| hv < wl) {
                    let d = ((water.unwrap() - hv) / 12.0).clamp(0.0, 1.0);
                    lerp3(col, lerp3([0.30, 0.52, 0.60], [0.10, 0.28, 0.42], d), 0.55 + 0.4 * d)
                } else {
                    // the bake already carries its own cavity shading: keep the relief light
                    [col[0] * 1.25, col[1] * 1.25, col[2] * 1.25]
                }
            } else if water.is_some_and(|wl| hv < wl) {
                let d = ((water.unwrap() - hv) / 12.0).clamp(0.0, 1.0);
                lerp3([0.36, 0.62, 0.70], [0.12, 0.33, 0.48], d)
            } else {
                ramp(((hv - t.min) / range).clamp(0.0, 1.0))
            };
            let k = 0.35 + 0.85 * shade;
            let i = 4 * (oy * w + ox);
            for ch in 0..3 {
                rgba[i + ch] = ((base[ch] * k).clamp(0.0, 1.0) * 255.0) as u8;
            }
            rgba[i + 3] = 255;
        }
    }
    Image { width: w, height: h, rgba }
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// the same ramp as the 3-D shader (sRGB-ish values for the CPU image)
fn ramp(t: f32) -> [f32; 3] {
    let c = [[0.86, 0.80, 0.62], [0.45, 0.62, 0.36], [0.62, 0.64, 0.45], [0.66, 0.62, 0.56], [0.93, 0.92, 0.89]];
    let stops = [0.0, 0.08, 0.45, 0.80, 1.0];
    for k in 0..4 {
        if t <= stops[k + 1] {
            return lerp3(c[k], c[k + 1], (t - stops[k]) / (stops[k + 1] - stops[k]));
        }
    }
    c[4]
}

/// Downsampled triangle mesh with normals (smooth, from central differences).
pub fn mesh(t: &Terrain, max_side: usize) -> (Vec<TerrainVertex>, Vec<u32>) {
    let step = step_for(t, max_side);
    let rs: Vec<usize> = (0..t.rows).step_by(step).chain(std::iter::once(t.rows - 1)).collect::<Vec<_>>();
    let cs: Vec<usize> = (0..t.cols).step_by(step).chain(std::iter::once(t.cols - 1)).collect::<Vec<_>>();
    let rs = dedup_sorted(rs);
    let cs = dedup_sorted(cs);
    let (nr, nc) = (rs.len(), cs.len());
    let mut v = Vec::with_capacity(nr * nc);
    for &r in &rs {
        for &c in &cs {
            let (cl, cr) = (c.saturating_sub(step), (c + step).min(t.cols - 1));
            let (rd, ru) = (r.saturating_sub(step), (r + step).min(t.rows - 1));
            let dx = (t.at(r, cr) - t.at(r, cl)) / (((cr - cl).max(1) as f32) * t.cell);
            let dz = (t.at(ru, c) - t.at(rd, c)) / (((ru - rd).max(1) as f32) * t.cell);
            let l = (dx * dx + dz * dz + 1.0).sqrt();
            v.push(TerrainVertex {
                pos: [t.x0 + c as f32 * t.cell, t.at(r, c), t.z0 + r as f32 * t.cell],
                nrm: [-dx / l, 1.0 / l, -dz / l],
            });
        }
    }
    let mut idx = Vec::with_capacity((nr - 1) * (nc - 1) * 6);
    for i in 0..nr - 1 {
        for j in 0..nc - 1 {
            let a = (i * nc + j) as u32;
            let b = a + 1;
            let c = a + nc as u32;
            let d = c + 1;
            idx.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    (v, idx)
}

fn dedup_sorted(mut v: Vec<usize>) -> Vec<usize> {
    v.dedup();
    v
}

/// Grid lines every `spacing` metres draped on the terrain (orientation aid in the 3-D view).
pub fn grid_lines(t: &Terrain, spacing: f32, color: [f32; 4]) -> Vec<LineVertex> {
    let (x0, z0, x1, z1) = t.extent();
    let mut out = vec![];
    let seg = (spacing / 8.0).max(t.cell);
    let mut x = (x0 / spacing).ceil() * spacing;
    while x <= x1 {
        let mut z = z0;
        while z < z1 {
            let z2 = (z + seg).min(z1);
            out.push(LineVertex { pos: [x, t.height(x, z) + 0.3, z], color });
            out.push(LineVertex { pos: [x, t.height(x, z2) + 0.3, z2], color });
            z = z2;
        }
        x += spacing;
    }
    let mut z = (z0 / spacing).ceil() * spacing;
    while z <= z1 {
        let mut x = x0;
        while x < x1 {
            let x2 = (x + seg).min(x1);
            out.push(LineVertex { pos: [x, t.height(x, z) + 0.3, z], color });
            out.push(LineVertex { pos: [x2, t.height(x2, z) + 0.3, z], color });
            x = x2;
        }
        z += spacing;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bowl(n: usize) -> Terrain {
        let mut h = vec![0.0f32; n * n];
        for r in 0..n {
            for c in 0..n {
                let (x, z) = (c as f32 - n as f32 / 2.0, r as f32 - n as f32 / 2.0);
                h[r * n + c] = (x * x + z * z) * 0.1;
            }
        }
        Terrain::new(n, n, h, 2.0, -(n as f32), -(n as f32)).unwrap()
    }

    #[test]
    fn mesh_covers_the_grid_and_normals_point_up() {
        let t = bowl(65);
        let (v, idx) = mesh(&t, 16);
        assert_eq!(v.len(), 17 * 17);
        assert_eq!(idx.len(), 16 * 16 * 6);
        assert!(idx.iter().all(|&i| (i as usize) < v.len()));
        assert!(v.iter().all(|x| x.nrm[1] > 0.0));
        let (x0, z0, x1, z1) = t.extent();
        assert_eq!(v[0].pos[0], x0);
        assert_eq!(v.last().unwrap().pos[0], x1);
        assert_eq!(v.last().unwrap().pos[2], z1);
        assert_eq!(v[0].pos[2], z0);
        // the bowl's normals lean towards its centre
        assert!(v[0].nrm[0] > 0.0 && v[0].nrm[2] > 0.0);
    }

    #[test]
    fn hillshade_size_water_and_bilinear() {
        let t = bowl(100);
        let img = hillshade(&t, 50, Some(5.0), 1.0);
        assert_eq!((img.width, img.height), (50, 50));
        // the centre is below the water level: blue-ish
        let c = &img.rgba[4 * (25 * 50 + 25)..4 * (25 * 50 + 25) + 4];
        assert!(c[2] > c[0], "{c:?}");
        let h = t.height(t.x0 + 3.0, t.z0 + 4.0); // between grid points
        assert!(h > 0.0);
        assert_eq!(t.height(t.x0, t.z0), t.at(0, 0));
        // the bowl's rim stands above the water: the focus is the whole grid minus nothing to cut
        let f = focus(&t, Some(5.0));
        assert!(f.0 >= t.x0 && f.2 <= t.extent().2);
        // an island: only the middle stands out
        let mut h = vec![0.0f32; 100 * 100];
        for r in 40..60 {
            for c in 30..50 {
                h[r * 100 + c] = 10.0;
            }
        }
        let isl = Terrain::new(100, 100, h, 1.0, 0.0, 0.0).unwrap();
        let f = focus(&isl, Some(1.0));
        assert!(f.0 > 20.0 && f.2 < 60.0 && f.1 > 30.0 && f.3 < 70.0, "{f:?}");
    }

    #[test]
    fn npy_round_trip_and_errors() {
        let d = std::env::temp_dir().join(format!("foxstudio_npy_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("h.npy");
        let v: Vec<f32> = (0..12).map(|i| i as f32).collect();
        std::fs::write(&p, foxcore::npy::Npy::from_f32(vec![3, 4], &v).write()).unwrap();
        let t = load(&p, 2.0, None).unwrap();
        assert_eq!((t.rows, t.cols, t.min, t.max), (3, 4, 0.0, 11.0));
        assert_eq!((t.x0, t.z0), (-4.0, -3.0));
        std::fs::write(&p, foxcore::npy::Npy::from_f32(vec![12], &v).write()).unwrap();
        assert!(load(&p, 2.0, None).unwrap_err().contains("2-D"));
        assert!(load(&d.join("x.png"), 2.0, None).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
