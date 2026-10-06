//! Placement counts over the terrain's horizontal world rectangle.
//! Density is instances per hectare, not canopy coverage or a surface-area estimate.
use super::terrain::Terrain;
use super::texture::Image;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub const MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_RECORDS: usize = 2_000_000;
pub const MAX_GRID_CELLS: usize = 4_194_304;
pub const MAX_MAP_PIXELS: usize = 4_194_304;
pub const MAX_NAV_EDGES: usize = 2_000_000;
pub const MAX_NAV_FILES: usize = 256;

/// Callers can lower these caps. Raising them past the hard bounds is rejected.
#[derive(Clone, Copy, Debug)]
pub struct ReadLimits {
    pub bytes: u64,
    pub records: usize,
    pub cells: usize,
    pub edges: usize,
    pub files: usize,
}

impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            bytes: MAX_INPUT_BYTES,
            records: MAX_RECORDS,
            cells: MAX_GRID_CELLS,
            edges: MAX_NAV_EDGES,
            files: MAX_NAV_FILES,
        }
    }
}

impl ReadLimits {
    pub fn validate(self) -> Result<(), String> {
        if self.bytes == 0
            || self.bytes > MAX_INPUT_BYTES
            || self.records == 0
            || self.records > MAX_RECORDS
            || self.cells == 0
            || self.cells > MAX_GRID_CELLS
            || self.edges == 0
            || self.edges > MAX_NAV_EDGES
            || self.files == 0
            || self.files > MAX_NAV_FILES
        {
            return Err("preview read limits must be positive and within the hard caps".into());
        }
        Ok(())
    }
}

pub(crate) fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("cancelled".into())
    } else {
        Ok(())
    }
}

/// Used by both placement and navmesh loaders. Rechecks growth instead of trusting
/// metadata, reads at most one 64 KiB chunk between cancellation checks, and
/// rejects devices/directories. The caller also imposes a total multi-file cap.
pub(crate) fn read_bounded(
    path: &Path,
    max_bytes: u64,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> Result<Vec<u8>, String> {
    check_cancel(cancel)?;
    if max_bytes == 0 || max_bytes > MAX_INPUT_BYTES {
        return Err("invalid preview byte limit".into());
    }
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{}: expected a regular file", path.display()));
    }
    if meta.len() > max_bytes {
        return Err(format!(
            "{}: {} bytes exceeds the {max_bytes} byte limit",
            path.display(),
            meta.len()
        ));
    }
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let opened = file
        .metadata()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if !opened.is_file() || opened.len() > max_bytes {
        return Err(format!(
            "{}: opened input is not a regular file within the byte limit",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        check_cancel(cancel)?;
        let count = file
            .read(&mut chunk)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if count == 0 {
            break;
        }
        let new_len = bytes
            .len()
            .checked_add(count)
            .ok_or("preview input size overflow")?;
        if new_len as u64 > max_bytes {
            return Err(format!(
                "{}: file grew past the {max_bytes} byte limit",
                path.display()
            ));
        }
        bytes
            .try_reserve(count)
            .map_err(|_| "not enough memory for preview input")?;
        bytes.extend_from_slice(&chunk[..count]);
        progress(bytes.len() as u64, meta.len());
    }
    check_cancel(cancel)?;
    Ok(bytes)
}

/// All world lengths are metres; +z is north. Bounds cover the grid vertices,
/// from the first sample to the last, rather than adding a phantom outer cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapBounds {
    pub x0: f64,
    pub z0: f64,
    pub x1: f64,
    pub z1: f64,
}

impl MapBounds {
    pub fn from_terrain(terrain: &Terrain) -> Result<Self, String> {
        let samples = terrain
            .rows
            .checked_mul(terrain.cols)
            .ok_or("terrain size overflow")?;
        if terrain.rows < 2
            || terrain.cols < 2
            || samples != terrain.h.len()
            || samples > 32 * 1024 * 1024
        {
            return Err(
                "terrain needs at least two rows/columns and at most 32 million samples".into(),
            );
        }
        if !terrain.cell.is_finite()
            || terrain.cell <= 0.0
            || !terrain.x0.is_finite()
            || !terrain.z0.is_finite()
        {
            return Err("terrain origin and positive cell size must be finite metres".into());
        }
        let bounds = Self {
            x0: f64::from(terrain.x0),
            z0: f64::from(terrain.z0),
            x1: f64::from(terrain.x0) + (terrain.cols - 1) as f64 * f64::from(terrain.cell),
            z1: f64::from(terrain.z0) + (terrain.rows - 1) as f64 * f64::from(terrain.cell),
        };
        bounds.validate()?;
        Ok(bounds)
    }

    pub fn validate(self) -> Result<(), String> {
        if ![self.x0, self.z0, self.x1, self.z1]
            .iter()
            .all(|v| v.is_finite())
            || self.x1 <= self.x0
            || self.z1 <= self.z0
        {
            return Err("map bounds must be finite with positive width and depth".into());
        }
        Ok(())
    }
}

pub(crate) fn map_size(width: usize, height: usize) -> Result<usize, String> {
    let count = width.checked_mul(height).ok_or("map size overflow")?;
    if width == 0 || height == 0 || count > MAX_MAP_PIXELS {
        return Err(format!("map must contain 1..{MAX_MAP_PIXELS} pixels"));
    }
    count
        .checked_mul(4)
        .ok_or_else(|| "map byte size overflow".into())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Selection {
    #[default]
    Vegetation,
    All,
    Brush,
    Trees,
    Plants,
}

impl Selection {
    pub const ALL: [Self; 5] = [
        Self::Vegetation,
        Self::All,
        Self::Brush,
        Self::Trees,
        Self::Plants,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Vegetation => "Vegetation",
            Self::All => "All placements",
            Self::Brush => "Brush",
            Self::Trees => "Trees and palms",
            Self::Plants => "Plants",
        }
    }

    /// Uses the authored placement classifications. No guessed model/path names.
    pub fn includes(self, layer: &str, kind: &str) -> bool {
        let tree = matches!(kind, "tree" | "tree_big" | "tree_small" | "palm");
        match self {
            Self::Vegetation => layer == "brush" || tree || kind == "plant",
            Self::All => true,
            Self::Brush => layer == "brush",
            Self::Trees => tree,
            Self::Plants => kind == "plant",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DensityOptions {
    pub bin_m: f64,
    pub selection: Selection,
    pub limits: ReadLimits,
}

impl Default for DensityOptions {
    fn default() -> Self {
        Self {
            bin_m: 16.0,
            selection: Selection::Vegetation,
            limits: ReadLimits::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DensityStats {
    pub records: usize,
    pub excluded: usize,
    pub nonfinite: usize,
    pub outside: usize,
    pub counted: usize,
}

/// A borrowed decoded placement, for integrations which already have records.
/// The iterator is consumed once and stopped at the configured record cap.
#[derive(Clone, Copy, Debug)]
pub struct PlacementRecord<'a> {
    pub position: [f64; 3],
    pub layer: &'a str,
    pub kind: &'a str,
}

#[derive(Clone, Debug)]
pub struct DensityGrid {
    pub bounds: MapBounds,
    pub rows: usize,
    pub cols: usize,
    pub bin_m: f64,
    pub selection: Selection,
    pub stats: DensityStats,
    counts: Vec<u32>,
    max_per_ha: f64,
}

impl DensityGrid {
    fn empty(
        terrain: &Terrain,
        options: DensityOptions,
        cancel: &AtomicBool,
    ) -> Result<Self, String> {
        options.limits.validate()?;
        check_cancel(cancel)?;
        let bounds = MapBounds::from_terrain(terrain)?;
        for chunk in terrain.h.chunks(4096) {
            check_cancel(cancel)?;
            if chunk.iter().any(|h| !h.is_finite()) {
                return Err("terrain contains nonfinite heights".into());
            }
        }
        if !options.bin_m.is_finite() || options.bin_m <= 0.0 {
            return Err("density cell size must be positive finite metres".into());
        }
        let cols = ((bounds.x1 - bounds.x0) / options.bin_m).ceil();
        let rows = ((bounds.z1 - bounds.z0) / options.bin_m).ceil();
        if rows < 1.0
            || cols < 1.0
            || rows > options.limits.cells as f64
            || cols > options.limits.cells as f64
        {
            return Err("density cell size produces too many cells".into());
        }
        let (rows, cols) = (rows as usize, cols as usize);
        let cells = rows.checked_mul(cols).ok_or("density grid size overflow")?;
        if cells > options.limits.cells {
            return Err("density grid exceeds the cell limit".into());
        }
        let smallest_area = ((bounds.x1 - bounds.x0) - (cols - 1) as f64 * options.bin_m)
            * ((bounds.z1 - bounds.z0) - (rows - 1) as f64 * options.bin_m);
        if !smallest_area.is_finite()
            || smallest_area <= 0.0
            || !(MAX_RECORDS as f64 * 10_000.0 / smallest_area).is_finite()
        {
            return Err("density cells have an unrepresentable area".into());
        }
        Ok(Self {
            bounds,
            rows,
            cols,
            bin_m: options.bin_m,
            selection: options.selection,
            stats: DensityStats::default(),
            counts: vec![0; cells],
            max_per_ha: 0.0,
        })
    }

    fn add(&mut self, position: [f64; 3], selected: bool) -> Result<(), String> {
        self.stats.records += 1;
        if !selected {
            self.stats.excluded += 1;
            return Ok(());
        }
        if position.iter().any(|v| !v.is_finite()) {
            self.stats.nonfinite += 1;
            return Ok(());
        }
        let [x, _, z] = position;
        if x < self.bounds.x0 || x > self.bounds.x1 || z < self.bounds.z0 || z > self.bounds.z1 {
            self.stats.outside += 1;
            return Ok(());
        }
        // Internal cells are half open; the final cell includes the far edge.
        let c = (((x - self.bounds.x0) / self.bin_m).floor() as usize).min(self.cols - 1);
        let r = (((z - self.bounds.z0) / self.bin_m).floor() as usize).min(self.rows - 1);
        let count = &mut self.counts[r * self.cols + c];
        *count = count.checked_add(1).ok_or("placement count overflow")?;
        self.stats.counted += 1;
        self.max_per_ha = self
            .max_per_ha
            .max(self.per_hectare(r, c).ok_or("invalid density cell")?);
        Ok(())
    }

    pub fn counts(&self) -> &[u32] {
        &self.counts
    }

    /// Area of a horizontal bin. Partial cells at +x/+z use their clipped area.
    pub fn cell_area_m2(&self, row: usize, col: usize) -> Option<f64> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        let width = (self.bounds.x1 - (self.bounds.x0 + col as f64 * self.bin_m)).min(self.bin_m);
        let depth = (self.bounds.z1 - (self.bounds.z0 + row as f64 * self.bin_m)).min(self.bin_m);
        let area = width * depth;
        if area.is_finite() && area > 0.0 {
            Some(area)
        } else {
            None
        }
    }

    pub fn per_hectare(&self, row: usize, col: usize) -> Option<f64> {
        let area = self.cell_area_m2(row, col)?;
        let index = row.checked_mul(self.cols)?.checked_add(col)?;
        Some(f64::from(*self.counts.get(index)?) * 10_000.0 / area)
    }

    pub fn max_per_hectare(&self) -> f64 {
        self.max_per_ha
    }

    fn validate(&self) -> Result<(), String> {
        self.bounds.validate()?;
        let cells = self
            .rows
            .checked_mul(self.cols)
            .ok_or("density grid size overflow")?;
        if self.rows == 0
            || self.cols == 0
            || cells > MAX_GRID_CELLS
            || cells != self.counts.len()
            || !self.bin_m.is_finite()
            || self.bin_m <= 0.0
            || ((self.bounds.x1 - self.bounds.x0) / self.bin_m).ceil() != self.cols as f64
            || ((self.bounds.z1 - self.bounds.z0) / self.bin_m).ceil() != self.rows as f64
            || self.cell_area_m2(self.rows - 1, self.cols - 1).is_none()
        {
            return Err("density grid has invalid dimensions, cells or bounds".into());
        }
        Ok(())
    }

    /// RGBA overlay, north (+z) up, using the same complete bounds as terrain maps.
    /// Pixel centres choose bins. Empty bins stay transparent.
    pub fn raster(
        &self,
        width: usize,
        height: usize,
        ceiling_per_ha: f64,
        opacity: u8,
        cancel: &AtomicBool,
    ) -> Result<Image, String> {
        check_cancel(cancel)?;
        self.validate()?;
        validate_ceiling(ceiling_per_ha)?;
        let mut rgba = vec![0; map_size(width, height)?];
        for y in 0..height {
            check_cancel(cancel)?;
            let z = self.bounds.z1
                - (y as f64 + 0.5) / height as f64 * (self.bounds.z1 - self.bounds.z0);
            let row = (((z - self.bounds.z0) / self.bin_m) as usize).min(self.rows - 1);
            for x in 0..width {
                let wx = self.bounds.x0
                    + (x as f64 + 0.5) / width as f64 * (self.bounds.x1 - self.bounds.x0);
                let col = (((wx - self.bounds.x0) / self.bin_m) as usize).min(self.cols - 1);
                let colour = density_colour(
                    self.per_hectare(row, col).unwrap_or(0.0),
                    ceiling_per_ha,
                    opacity,
                )?;
                let offset = 4 * (y * width + x);
                rgba[offset..offset + 4].copy_from_slice(&colour);
            }
        }
        check_cancel(cancel)?;
        Ok(Image {
            width,
            height,
            rgba,
        })
    }
}

/// Already decoded world positions; no classification is inferred for this API.
pub fn from_positions(
    terrain: &Terrain,
    positions: &[[f64; 3]],
    mut options: DensityOptions,
    cancel: &AtomicBool,
) -> Result<DensityGrid, String> {
    options.limits.validate()?;
    if positions.len() > options.limits.records {
        return Err("placement count exceeds the record limit".into());
    }
    options.selection = Selection::All;
    let mut grid = DensityGrid::empty(terrain, options, cancel)?;
    for (i, position) in positions.iter().enumerate() {
        if i % 4096 == 0 {
            check_cancel(cancel)?;
        }
        grid.add(*position, true)?;
    }
    check_cancel(cancel)?;
    Ok(grid)
}

pub fn from_records<'a>(
    terrain: &Terrain,
    records: impl IntoIterator<Item = PlacementRecord<'a>>,
    options: DensityOptions,
    cancel: &AtomicBool,
) -> Result<DensityGrid, String> {
    let mut grid = DensityGrid::empty(terrain, options, cancel)?;
    for (i, record) in records.into_iter().enumerate() {
        if i >= options.limits.records {
            return Err("placement count exceeds the record limit".into());
        }
        if i % 4096 == 0 {
            check_cancel(cancel)?;
        }
        grid.add(
            record.position,
            options.selection.includes(record.layer, record.kind),
        )?;
    }
    check_cancel(cancel)?;
    Ok(grid)
}

/// Plain N x 2 XZ or N x 3 XYZ arrays, float32/float64 in either byte/storage
/// order. Such arrays contain no authored classification: callers must select
/// All. Structured placement arrays use the shared record reader instead.
pub fn load_xyz_npy(
    path: &Path,
    terrain: &Terrain,
    options: DensityOptions,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f32, &str),
) -> Result<DensityGrid, String> {
    options.limits.validate()?;
    if options.selection != Selection::All {
        return Err(
            "Numeric position arrays have no classifications; select All placements.".into(),
        );
    }
    let bytes = read_bounded(path, options.limits.bytes, cancel, |done, total| {
        progress(
            0.6 * (done as f32 / total.max(1) as f32).min(1.0),
            "Reading positions",
        );
    })?;
    check_cancel(cancel)?;
    let array =
        foxcore::npy::Npy::read(&bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    aggregate_numeric(&array, terrain, options, cancel, &mut progress)
}

fn aggregate_numeric(
    array: &foxcore::npy::Npy,
    terrain: &Terrain,
    options: DensityOptions,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(f32, &str),
) -> Result<DensityGrid, String> {
    if options.selection != Selection::All {
        return Err(
            "Numeric position arrays have no classifications; select All placements.".into(),
        );
    }
    let [rows, cols] = array.shape.as_slice() else {
        return Err("positions must be a two-dimensional N x 2 or N x 3 array".into());
    };
    let (rows, cols) = (*rows, *cols);
    if !matches!(cols, 2 | 3) || rows > options.limits.records {
        return Err("positions need 2 or 3 columns within the record limit".into());
    }
    let values: Vec<f64> = match array.descr.get(1..) {
        Some("f4") => array.f32s().into_iter().map(f64::from).collect(),
        Some("f8") => array.f64s(),
        _ => return Err("positions must use float32 or float64 coordinates".into()),
    };
    check_cancel(cancel)?;
    progress(0.7, "Counting positions");
    let at = |row, column| {
        values[if array.fortran {
            column * rows + row
        } else {
            row * cols + column
        }]
    };
    let records = (0..rows).map(|row| PlacementRecord {
        position: [
            at(row, 0),
            if cols == 3 { at(row, 1) } else { 0.0 },
            at(row, cols - 1),
        ],
        layer: "",
        kind: "",
    });
    let grid = from_records(terrain, records, options, cancel)?;
    check_cancel(cancel)?;
    progress(1.0, "Density ready");
    check_cancel(cancel)?;
    Ok(grid)
}

/// Real structured placement arrays (including TP_DTYPE), or explicitly selected
/// All-placement numeric XYZ/XZ arrays. One bounded read and one shared parser;
/// no dependency on the private pipeline's placement types or Python runtime.
pub fn load(
    path: &Path,
    terrain: &Terrain,
    options: DensityOptions,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f32, &str),
) -> Result<DensityGrid, String> {
    options.limits.validate()?;
    let bytes = read_bounded(path, options.limits.bytes, cancel, |done, total| {
        progress(
            0.6 * (done as f32 / total.max(1) as f32).min(1.0),
            "Reading placements",
        );
    })?;
    check_cancel(cancel)?;
    let records = match foxcore::npy::RecordArray::read(&bytes) {
        Ok(records) => records,
        Err(record_error) => {
            let array = foxcore::npy::Npy::read(&bytes).map_err(|numeric_error| {
                format!("{}: {record_error}; {numeric_error}", path.display())
            })?;
            return aggregate_numeric(&array, terrain, options, cancel, &mut progress);
        }
    };
    if records.shape().len() != 1 || records.len() > options.limits.records {
        return Err(
            "placements must be a one-dimensional record array within the record limit".into(),
        );
    }
    records.require_scalar_floats(&["x", "y", "z"])?;
    let needs_layer = matches!(options.selection, Selection::Vegetation | Selection::Brush);
    let needs_kind = matches!(
        options.selection,
        Selection::Vegetation | Selection::Trees | Selection::Plants
    );
    for (name, needed) in [("layer", needs_layer), ("kind", needs_kind)] {
        if needed {
            let field = records.field(name)?;
            if !field.shape.is_empty()
                || !matches!(field.descr.as_bytes().get(1), Some(b'U' | b'S'))
            {
                return Err(format!(
                    "placement {name} must be a scalar Unicode or byte string"
                ));
            }
        }
    }
    let mut grid = DensityGrid::empty(terrain, options, cancel)?;
    for index in 0..records.len() {
        if index % 4096 == 0 {
            check_cancel(cancel)?;
            progress(
                0.7 + 0.3 * index as f32 / records.len().max(1) as f32,
                "Counting placements",
            );
            check_cancel(cancel)?;
        }
        let record = records.record(index)?;
        let layer = if needs_layer {
            record.string("layer")?
        } else {
            String::new()
        };
        let kind = if needs_kind {
            record.string("kind")?
        } else {
            String::new()
        };
        grid.add(
            [record.f64("x")?, record.f64("y")?, record.f64("z")?],
            options.selection.includes(&layer, &kind),
        )?;
    }
    check_cancel(cancel)?;
    progress(1.0, "Density ready");
    check_cancel(cancel)?;
    Ok(grid)
}

pub fn validate_ceiling(ceiling_per_ha: f64) -> Result<(), String> {
    if !ceiling_per_ha.is_finite() || ceiling_per_ha <= 0.0 {
        Err("density colour ceiling must be a positive finite number per hectare".into())
    } else {
        Ok(())
    }
}

pub fn density_colour(per_ha: f64, ceiling_per_ha: f64, opacity: u8) -> Result<[u8; 4], String> {
    validate_ceiling(ceiling_per_ha)?;
    if !per_ha.is_finite() || per_ha < 0.0 {
        return Err("density must be finite and nonnegative".into());
    }
    if per_ha == 0.0 {
        return Ok([0; 4]);
    }
    let palette = [
        [56u8, 123, 217],
        [32, 177, 155],
        [237, 216, 86],
        [241, 145, 52],
        [206, 64, 69],
    ];
    let value = (per_ha / ceiling_per_ha).clamp(0.0, 1.0) * 4.0;
    let low = (value.floor() as usize).min(3);
    let blend = value - low as f64;
    let mut rgba = [0, 0, 0, opacity];
    for channel in 0..3 {
        rgba[channel] = (f64::from(palette[low][channel]) * (1.0 - blend)
            + f64::from(palette[low + 1][channel]) * blend)
            .round() as u8;
    }
    Ok(rgba)
}

pub fn legend(ceiling_per_ha: f64, opacity: u8) -> Result<[(f64, [u8; 4]); 5], String> {
    validate_ceiling(ceiling_per_ha)?;
    let mut stops = [(0.0, [0; 4]); 5];
    for (i, stop) in stops.iter_mut().enumerate() {
        let value = ceiling_per_ha * (i as f64 / 4.0);
        *stop = (value, density_colour(value, ceiling_per_ha, opacity)?);
    }
    Ok(stops)
}
