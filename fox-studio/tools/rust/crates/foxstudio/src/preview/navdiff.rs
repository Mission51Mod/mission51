//! Read-only navmesh comparison in world metres. File names, polygon indices and
//! edge direction do not affect geometry identity. Endpoints are snapped to a
//! documented lattice; this is not an arbitrary-distance or reachability proof.
use super::density::{MapBounds, ReadLimits, check_cancel, map_size, read_bounded};
use super::nav::{self, NavOverlay};
use super::terrain::Terrain;
use super::texture::Image;
use foxcore::nav2;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

pub type NavEdge = ([f32; 3], [f32; 3], bool);
type PointKey = [i64; 3];
type EdgeKey = (PointKey, PointKey);

pub const ADDED_COLOUR: [u8; 4] = [50, 196, 170, 230];
pub const REMOVED_COLOUR: [u8; 4] = [235, 101, 78, 230];
pub const BOUNDARY_COLOUR: [u8; 4] = [242, 194, 73, 240];

#[derive(Clone, Copy, Debug)]
pub struct DiffOptions {
    /// Per-coordinate rounding lattice in metres, including height.
    pub quantum_m: f64,
    pub limits: ReadLimits,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            quantum_m: 0.01,
            limits: ReadLimits::default(),
        }
    }
}

impl DiffOptions {
    fn validate(self) -> Result<(), String> {
        self.limits.validate()?;
        if !self.quantum_m.is_finite() || self.quantum_m <= 0.0 || self.quantum_m > 1.0 {
            return Err("navmesh rounding must be positive finite metres, at most 1 m".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct NavDiff {
    pub quantum_m: f64,
    pub added: Vec<NavEdge>,
    pub removed: Vec<NavEdge>,
    /// Geometry exists on both sides, but its boundary/interior status changed.
    pub boundary_changed: Vec<NavEdge>,
    pub unchanged: usize,
    pub current_unique: usize,
    pub baseline_unique: usize,
    pub current_duplicates: usize,
    pub baseline_duplicates: usize,
    pub current_degenerate: usize,
    pub baseline_degenerate: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingBaseline {
    pub path: Option<PathBuf>,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub enum Baseline {
    Missing(MissingBaseline),
    Loaded(NavOverlay),
}

#[derive(Clone, Debug)]
pub enum Comparison {
    Missing(MissingBaseline),
    Ready(NavDiff),
}

struct Normalized {
    edges: BTreeMap<EdgeKey, bool>,
    duplicates: usize,
    degenerate: usize,
}

fn point_key(point: [f32; 3], quantum_m: f64) -> Result<PointKey, String> {
    let mut key = [0; 3];
    for (i, value) in point.iter().enumerate() {
        let scaled = f64::from(*value) / quantum_m;
        if !value.is_finite() || !scaled.is_finite() || scaled.abs() > (i64::MAX / 4) as f64 {
            return Err("navmesh has a nonfinite or unrepresentable world coordinate".into());
        }
        key[i] = scaled.round() as i64;
    }
    Ok(key)
}

fn normalize(
    overlay: &NavOverlay,
    options: DiffOptions,
    cancel: &AtomicBool,
) -> Result<Normalized, String> {
    if !overlay.errors.is_empty() {
        return Err(format!(
            "navmesh overlay is incomplete: {}",
            overlay.errors.join("; ")
        ));
    }
    if overlay.edges.len() > options.limits.edges {
        return Err("navmesh exceeds the edge limit".into());
    }
    let mut normalized = Normalized {
        edges: BTreeMap::new(),
        duplicates: 0,
        degenerate: 0,
    };
    for (i, &(a, b, boundary)) in overlay.edges.iter().enumerate() {
        if i % 4096 == 0 {
            check_cancel(cancel)?;
        }
        let (a, b) = (
            point_key(a, options.quantum_m)?,
            point_key(b, options.quantum_m)?,
        );
        if a == b {
            normalized.degenerate += 1;
            continue;
        }
        let key = if a < b { (a, b) } else { (b, a) };
        if let Some(existing) = normalized.edges.get_mut(&key) {
            // Coincident tile/polygon edges form a union. Any boundary copy wins.
            *existing |= boundary;
            normalized.duplicates += 1;
        } else {
            normalized.edges.insert(key, boundary);
        }
    }
    check_cancel(cancel)?;
    Ok(normalized)
}

fn world_edge(key: EdgeKey, boundary: bool, quantum_m: f64) -> NavEdge {
    let world = |p: PointKey| p.map(|coordinate| (coordinate as f64 * quantum_m) as f32);
    (world(key.0), world(key.1), boundary)
}

pub fn compare(
    current: &NavOverlay,
    baseline: &NavOverlay,
    options: DiffOptions,
    cancel: &AtomicBool,
) -> Result<NavDiff, String> {
    options.validate()?;
    check_cancel(cancel)?;
    let current = normalize(current, options, cancel)?;
    let baseline = normalize(baseline, options, cancel)?;
    let mut diff = NavDiff {
        quantum_m: options.quantum_m,
        added: Vec::new(),
        removed: Vec::new(),
        boundary_changed: Vec::new(),
        unchanged: 0,
        current_unique: current.edges.len(),
        baseline_unique: baseline.edges.len(),
        current_duplicates: current.duplicates,
        baseline_duplicates: baseline.duplicates,
        current_degenerate: current.degenerate,
        baseline_degenerate: baseline.degenerate,
    };
    for (i, (&key, &boundary)) in current.edges.iter().enumerate() {
        if i % 4096 == 0 {
            check_cancel(cancel)?;
        }
        let edge = world_edge(key, boundary, options.quantum_m);
        match baseline.edges.get(&key) {
            None => diff.added.push(edge),
            Some(old) if *old != boundary => diff.boundary_changed.push(edge),
            Some(_) => diff.unchanged += 1,
        }
    }
    for (i, (&key, &boundary)) in baseline.edges.iter().enumerate() {
        if i % 4096 == 0 {
            check_cancel(cancel)?;
        }
        if !current.edges.contains_key(&key) {
            diff.removed
                .push(world_edge(key, boundary, options.quantum_m));
        }
    }
    check_cancel(cancel)?;
    Ok(diff)
}

/// Explicitly selected prior .nav2 files, never an implicit current-output
/// fallback or a directory crawl. The byte cap applies to the whole selection.
pub fn load_baseline(
    paths: &[PathBuf],
    limits: ReadLimits,
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize, usize),
) -> Result<Baseline, String> {
    limits.validate()?;
    check_cancel(cancel)?;
    if paths.is_empty() {
        return Ok(Baseline::Missing(MissingBaseline {
            path: None,
            reason: "Select a prior .nav2 baseline.".into(),
        }));
    }
    if paths.len() > limits.files {
        return Err("baseline selection exceeds the file limit".into());
    }
    let paths: BTreeSet<_> = paths.iter().cloned().collect();
    let mut total = 0u64;
    for path in &paths {
        check_cancel(cancel)?;
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("nav2"))
        {
            return Err(format!("{}: select .nav2 files", path.display()));
        }
        let meta = match std::fs::metadata(path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Baseline::Missing(MissingBaseline {
                    path: Some(path.clone()),
                    reason: format!("Baseline is missing: {}", path.display()),
                }));
            }
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        if !meta.is_file() {
            return Err(format!("{}: select a regular .nav2 file", path.display()));
        }
        total = total
            .checked_add(meta.len())
            .ok_or("baseline size overflow")?;
        if total > limits.bytes {
            return Err("baseline selection exceeds the total byte limit".into());
        }
    }
    let mut overlay = NavOverlay::default();
    let mut remaining = limits.bytes;
    for (i, path) in paths.iter().enumerate() {
        check_cancel(cancel)?;
        progress(i, paths.len());
        let bytes = read_bounded(path, remaining, cancel, |_, _| {})?;
        remaining -= bytes.len() as u64;
        validate_container_bounds(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        check_cancel(cancel)?;
        // The existing format reader indexes bytes. Keep malformed input out of
        // the UI thread and turn its bounds panics into an explicit error.
        let nav = std::panic::catch_unwind(|| nav2::read(&bytes))
            .map_err(|_| format!("{}: damaged or truncated .nav2", path.display()))?
            .map_err(|e| format!("{}: {e}", path.display()))?;
        validate_mesh(&nav, limits.edges - overlay.edges.len(), cancel)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let bounds = nav::edges_of(&nav, &mut overlay);
        if bounds[0] <= bounds[2] {
            overlay.tiles.push((path.display().to_string(), bounds));
        }
        overlay.files += 1;
        check_cancel(cancel)?;
        progress(i + 1, paths.len());
    }
    check_cancel(cancel)?;
    Ok(Baseline::Loaded(overlay))
}

/// Minimal container-chain guard before the existing core parser: rejects
/// zero-offset/repeated chunks that could otherwise multiply a bounded input.
/// This does not implement or replace the .nav2 format reader.
fn validate_container_bounds(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 96 {
        return Err("nav2: too short".into());
    }
    let u32_at = |offset: usize| -> Result<usize, String> {
        let data = bytes
            .get(offset..offset.checked_add(4).ok_or("nav2 offset overflow")?)
            .ok_or("nav2: truncated chunk header")?;
        Ok(u32::from_le_bytes(data.try_into().unwrap()) as usize)
    };
    let count = u32_at(12)?;
    if count > 4096 || count > bytes.len() / 16 {
        return Err("nav2: unreasonable chunk count".into());
    }
    let mut offset = u32_at(8)?;
    for i in 0..count {
        let next = u32_at(offset.checked_add(4).ok_or("nav2 offset overflow")?)?;
        let data = u32_at(offset.checked_add(8).ok_or("nav2 offset overflow")?)?;
        if data < 16 || offset.checked_add(data).is_none_or(|v| v >= bytes.len()) {
            return Err("nav2: chunk data offset outside the file".into());
        }
        if i + 1 < count && next < 16 {
            return Err("nav2: non-advancing chunk chain".into());
        }
        offset = offset.checked_add(next).ok_or("nav2 offset overflow")?;
    }
    Ok(())
}

fn validate_mesh(nav: &nav2::Nav2, edge_limit: usize, cancel: &AtomicBool) -> Result<(), String> {
    if ![nav.origin.0, nav.origin.1, nav.origin.2]
        .iter()
        .all(|v| v.is_finite())
        || nav.denominator.0 == 0
        || nav.denominator.1 == 0
        || nav.denominator.2 == 0
    {
        return Err("nav2: invalid world origin or denominator".into());
    }
    let mut count = 0usize;
    for chunk in &nav.chunks {
        for segment in &chunk.segments {
            check_cancel(cancel)?;
            let start = segment.base_polygon as usize;
            let end = start
                .checked_add(segment.polygon_count as usize)
                .ok_or("nav2 polygon range overflow")?;
            let polygons = chunk
                .mesh
                .polygons
                .get(start..end)
                .ok_or("nav2 polygon range outside mesh")?;
            let positions_end = (segment.base_position as usize)
                .checked_add(segment.position_count as usize)
                .ok_or("nav2 position range overflow")?;
            if positions_end > chunk.mesh.positions.len() {
                return Err("nav2 position range outside mesh".into());
            }
            for (i, polygon) in polygons.iter().enumerate() {
                if i % 4096 == 0 {
                    check_cancel(cancel)?;
                }
                if polygon.vertices.len() < 3
                    || polygon.neighbors.len() != polygon.vertices.len()
                    || polygon
                        .vertices
                        .iter()
                        .any(|&v| v >= segment.position_count)
                {
                    return Err("nav2 polygon has invalid vertices or neighbors".into());
                }
                count = count
                    .checked_add(polygon.vertices.len())
                    .ok_or("nav2 edge count overflow")?;
                if count > edge_limit {
                    return Err("navmesh exceeds the edge limit".into());
                }
            }
        }
    }
    Ok(())
}

pub fn compare_selected(
    current: &NavOverlay,
    paths: &[PathBuf],
    options: DiffOptions,
    cancel: &AtomicBool,
    progress: impl FnMut(usize, usize),
) -> Result<Comparison, String> {
    options.validate()?;
    match load_baseline(paths, options.limits, cancel, progress)? {
        Baseline::Missing(missing) => Ok(Comparison::Missing(missing)),
        Baseline::Loaded(baseline) => {
            compare(current, &baseline, options, cancel).map(Comparison::Ready)
        }
    }
}

/// Rasterize the current navmesh with the existing preview colours. Boundary
/// edges take priority, regardless of file order. All work is clipped and bounded.
pub fn raster_overlay(
    terrain: &Terrain,
    overlay: &NavOverlay,
    width: usize,
    height: usize,
    cancel: &AtomicBool,
) -> Result<Image, String> {
    if overlay.edges.len() > super::density::MAX_NAV_EDGES {
        return Err("navmesh exceeds the edge limit".into());
    }
    let mut raster = MapRaster::new(terrain, width, height, cancel)?;
    for boundary in [false, true] {
        for (i, &(a, b, is_boundary)) in overlay.edges.iter().enumerate() {
            if i % 1024 == 0 {
                check_cancel(cancel)?;
            }
            if boundary == is_boundary {
                let colour = if boundary {
                    [255, 120, 20, 255]
                } else {
                    [40, 190, 255, 150]
                };
                raster.edge(a, b, colour, cancel)?;
            }
        }
    }
    raster.finish(cancel)
}

impl NavDiff {
    /// North-up additions, removals and boundary changes, clipped before stepping.
    pub fn raster(
        &self,
        terrain: &Terrain,
        width: usize,
        height: usize,
        cancel: &AtomicBool,
    ) -> Result<Image, String> {
        let edges = self
            .added
            .len()
            .checked_add(self.removed.len())
            .and_then(|count| count.checked_add(self.boundary_changed.len()))
            .ok_or("nav diff size overflow")?;
        if edges > super::density::MAX_NAV_EDGES * 2 {
            return Err("nav difference exceeds the edge limit".into());
        }
        let mut raster = MapRaster::new(terrain, width, height, cancel)?;
        for (edges, colour) in [
            (&self.removed, REMOVED_COLOUR),
            (&self.added, ADDED_COLOUR),
            (&self.boundary_changed, BOUNDARY_COLOUR),
        ] {
            for (i, &(a, b, _)) in edges.iter().enumerate() {
                if i % 1024 == 0 {
                    check_cancel(cancel)?;
                }
                raster.edge(a, b, colour, cancel)?;
            }
        }
        raster.finish(cancel)
    }
}

struct MapRaster {
    bounds: MapBounds,
    image: Image,
}

impl MapRaster {
    fn new(
        terrain: &Terrain,
        width: usize,
        height: usize,
        cancel: &AtomicBool,
    ) -> Result<Self, String> {
        check_cancel(cancel)?;
        Ok(Self {
            bounds: MapBounds::from_terrain(terrain)?,
            image: Image {
                width,
                height,
                rgba: vec![0; map_size(width, height)?],
            },
        })
    }

    fn edge(
        &mut self,
        a: [f32; 3],
        b: [f32; 3],
        colour: [u8; 4],
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        if !a.iter().chain(&b).all(|v| v.is_finite()) {
            return Err("nonfinite navmesh edge".into());
        }
        let bounds = self.bounds;
        let Some((a, b)) = clip(
            [f64::from(a[0]), f64::from(a[2])],
            [f64::from(b[0]), f64::from(b[2])],
            bounds,
        ) else {
            return Ok(());
        };
        let (width, height) = (self.image.width, self.image.height);
        let pixel = |p: [f64; 2]| {
            [
                (p[0] - bounds.x0) / (bounds.x1 - bounds.x0) * (width - 1) as f64,
                (bounds.z1 - p[1]) / (bounds.z1 - bounds.z0) * (height - 1) as f64,
            ]
        };
        let (a, b) = (pixel(a), pixel(b));
        let steps = (b[0] - a[0]).abs().max((b[1] - a[1]).abs()).ceil().max(1.0) as usize;
        for step in 0..=steps {
            if step % 4096 == 0 {
                check_cancel(cancel)?;
            }
            let f = step as f64 / steps as f64;
            let x = (a[0] + (b[0] - a[0]) * f)
                .round()
                .clamp(0.0, (width - 1) as f64) as usize;
            let y = (a[1] + (b[1] - a[1]) * f)
                .round()
                .clamp(0.0, (height - 1) as f64) as usize;
            let offset = 4 * (y * width + x);
            self.image.rgba[offset..offset + 4].copy_from_slice(&colour);
        }
        Ok(())
    }

    fn finish(self, cancel: &AtomicBool) -> Result<Image, String> {
        check_cancel(cancel)?;
        Ok(self.image)
    }
}

fn clip(a: [f64; 2], b: [f64; 2], bounds: MapBounds) -> Option<([f64; 2], [f64; 2])> {
    let delta = [b[0] - a[0], b[1] - a[1]];
    // Preserve exact map borders for horizontal/vertical lines, even when the
    // endpoints are so far away that their clipping fractions round together.
    if delta[1] == 0.0 {
        if a[1] < bounds.z0
            || a[1] > bounds.z1
            || a[0].max(b[0]) < bounds.x0
            || a[0].min(b[0]) > bounds.x1
        {
            return None;
        }
        return Some((
            [a[0].clamp(bounds.x0, bounds.x1), a[1]],
            [b[0].clamp(bounds.x0, bounds.x1), b[1]],
        ));
    }
    if delta[0] == 0.0 {
        if a[0] < bounds.x0
            || a[0] > bounds.x1
            || a[1].max(b[1]) < bounds.z0
            || a[1].min(b[1]) > bounds.z1
        {
            return None;
        }
        return Some((
            [a[0], a[1].clamp(bounds.z0, bounds.z1)],
            [b[0], b[1].clamp(bounds.z0, bounds.z1)],
        ));
    }
    let (mut enter, mut leave) = (0.0f64, 1.0f64);
    for (p, q) in [
        (-delta[0], a[0] - bounds.x0),
        (delta[0], bounds.x1 - a[0]),
        (-delta[1], a[1] - bounds.z0),
        (delta[1], bounds.z1 - a[1]),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else if p < 0.0 {
            enter = enter.max(q / p);
        } else {
            leave = leave.min(q / p);
        }
        if enter > leave {
            return None;
        }
    }
    Some((
        [a[0] + enter * delta[0], a[1] + enter * delta[1]],
        [a[0] + leave * delta[0], a[1] + leave * delta[1]],
    ))
}
