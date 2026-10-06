//! Navmesh overlay: every polygon edge of the .nav2 files under the given paths (foxcore::nav2 reader), in world
//! coordinates, marked boundary (no neighbour) or interior. Read only, on a worker.
use foxcore::nav2::{self, Nav2};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Debug, Default)]
pub struct NavOverlay {
    pub files: usize,
    pub polygons: usize,
    /// (a, b, boundary) world positions (x, y, z)
    pub edges: Vec<([f32; 3], [f32; 3], bool)>,
    /// per-file world bounds (x0, z0, x1, z1): tile borders
    pub tiles: Vec<(String, [f32; 4])>,
    pub errors: Vec<String>,
}

/// every *.nav2 below the paths (files are taken as they are), sorted
pub fn find_nav2(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = BTreeSet::new();
    for p in paths {
        if p.is_file() {
            out.insert(p.clone());
        } else if p.is_dir() {
            walk(p, &mut out, 0);
        }
    }
    out.into_iter().collect()
}

fn walk(d: &Path, out: &mut BTreeSet<PathBuf>, depth: usize) {
    if depth > 24 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(d) else { return };
    for e in rd.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(&p, out, depth + 1),
            Ok(_) if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("nav2")) => {
                out.insert(p);
            }
            _ => {}
        }
    }
}

/// the edges of one navmesh
pub fn edges_of(nav: &Nav2, out: &mut NavOverlay) -> [f32; 4] {
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    let w = |v: nav2::V3| {
        let (x, y, z) = nav.world(v);
        [x as f32, y as f32, z as f32]
    };
    for dc in &nav.chunks {
        let m = &dc.mesh;
        for s in &dc.segments {
            let (bp, np) = (s.base_position as usize, s.position_count as usize);
            for pi in s.base_polygon as usize..(s.base_polygon as usize + s.polygon_count as usize).min(m.polygons.len()) {
                let p = &m.polygons[pi];
                out.polygons += 1;
                let n = p.vertices.len();
                for k in 0..n {
                    let (ia, ib) = (p.vertices[k] as usize, p.vertices[(k + 1) % n] as usize);
                    if ia >= np || ib >= np || bp + ia.max(ib) >= m.positions.len() {
                        continue;
                    }
                    let (a, bb) = (w(m.positions[bp + ia]), w(m.positions[bp + ib]));
                    let boundary = p.neighbors.get(k).is_some_and(|&x| x == nav2::NO_NEIGHBOR);
                    // interior edges are shared by two polygons: keep one copy
                    if !boundary && (a[0], a[2]) > (bb[0], bb[2]) {
                        continue;
                    }
                    for q in [a, bb] {
                        b[0] = b[0].min(q[0]);
                        b[1] = b[1].min(q[2]);
                        b[2] = b[2].max(q[0]);
                        b[3] = b[3].max(q[2]);
                    }
                    out.edges.push((a, bb, boundary));
                }
            }
        }
    }
    b
}

/// Load every .nav2 under `paths`; `progress(done, total)`; stops early when `cancel` is set.
pub fn load(paths: &[PathBuf], cancel: &AtomicBool, mut progress: impl FnMut(usize, usize)) -> NavOverlay {
    let files = find_nav2(paths);
    let mut o = NavOverlay::default();
    for (i, f) in files.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            o.errors.push("cancelled".into());
            break;
        }
        progress(i, files.len());
        // the reader indexes the bytes directly: a damaged file may panic, which is reported like any error
        let read = |b: Vec<u8>| {
            std::panic::catch_unwind(|| nav2::read(&b)).unwrap_or_else(|_| Err("damaged or truncated .nav2".into()))
        };
        match std::fs::read(f).map_err(|e| e.to_string()).and_then(read) {
            Ok(nav) => {
                let b = edges_of(&nav, &mut o);
                if b[0] <= b[2] {
                    o.tiles.push((f.file_name().unwrap_or_default().to_string_lossy().into_owned(), b));
                }
                o.files += 1;
            }
            Err(e) => o.errors.push(format!("{}: {e}", f.display())),
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_paths_give_an_empty_overlay() {
        let c = AtomicBool::new(false);
        let o = load(&[PathBuf::from("Z:/no/such/dir")], &c, |_, _| {});
        assert_eq!((o.files, o.edges.len(), o.errors.len()), (0, 0, 0));
    }

    #[test]
    fn broken_file_is_reported() {
        let d = std::env::temp_dir().join(format!("foxstudio_nav_{}", std::process::id()));
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("sub/x.nav2"), b"not a navmesh").unwrap();
        let c = AtomicBool::new(false);
        let o = load(std::slice::from_ref(&d), &c, |_, _| {});
        assert_eq!(o.files, 0);
        assert_eq!(o.errors.len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }
}
