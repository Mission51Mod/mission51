//! Synthetic, CPU-only verification of L's density/navdiff backends and controls.
//! Path inclusions keep this receipt independently buildable until P exports the
//! modules; no private game arrays, models, navmeshes or images are shipped here.
#[path = "../src/preview/density.rs"]
pub mod density;
#[path = "../src/preview/navdiff.rs"]
pub mod navdiff;
pub use foxstudio::preview::{nav, terrain, texture};
mod preview {
    pub use crate::{density, navdiff};
}
#[path = "../src/preview_layers.rs"]
pub mod preview_layers;

use density::{DensityOptions, MapBounds, ReadLimits, Selection};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use foxcore::nav2::{self, DataChunk, Nav2, Navmesh, Polygon, Segment};
use foxstudio::preview::{nav::NavOverlay, terrain::Terrain};
use navdiff::{Baseline, Comparison, DiffOptions, NavEdge};
use preview_layers::{LayerAction, LayerState, LayerStatus, PreviewLayers};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

fn terrain(rows: usize, cols: usize, cell: f32, origin: (f32, f32)) -> Terrain {
    Terrain::new(rows, cols, vec![0.0; rows * cols], cell, origin.0, origin.1).unwrap()
}

fn options(bin_m: f64) -> DensityOptions {
    DensityOptions {
        bin_m,
        ..Default::default()
    }
}
fn cancel() -> AtomicBool {
    AtomicBool::new(false)
}
fn edge(a: [f32; 3], b: [f32; 3], boundary: bool) -> NavEdge {
    (a, b, boundary)
}
fn overlay(edges: Vec<NavEdge>) -> NavOverlay {
    NavOverlay {
        edges,
        ..Default::default()
    }
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("foxstudio_layers_l_{}_{nonce}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }
    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn density_world_axes_boundaries_and_invalid_records() {
    let terrain = terrain(3, 3, 10.0, (-10.0, -20.0));
    let positions = [
        [-10.0, 0.0, -20.0],
        [0.0, 0.0, -10.0],
        [10.0, 0.0, 0.0],
        [-10.01, 0.0, -20.0],
        [0.0, f64::NAN, -10.0],
    ];
    let grid = density::from_positions(&terrain, &positions, options(10.0), &cancel()).unwrap();
    assert_eq!(
        grid.bounds,
        MapBounds {
            x0: -10.0,
            z0: -20.0,
            x1: 10.0,
            z1: 0.0
        }
    );
    assert_eq!((grid.rows, grid.cols), (2, 2));
    assert_eq!(grid.counts(), &[1, 0, 0, 2]);
    assert_eq!(
        (
            grid.stats.records,
            grid.stats.counted,
            grid.stats.outside,
            grid.stats.nonfinite
        ),
        (5, 3, 1, 1)
    );
    assert_eq!(grid.per_hectare(0, 0), Some(100.0));
    assert_eq!(grid.per_hectare(1, 1), Some(200.0));
}

#[test]
fn partial_bins_use_actual_horizontal_area() {
    let terrain = terrain(2, 4, 10.0, (0.0, 0.0));
    let grid = density::from_positions(
        &terrain,
        &[[0.0; 3], [30.0, 0.0, 10.0]],
        options(16.0),
        &cancel(),
    )
    .unwrap();
    assert_eq!((grid.rows, grid.cols), (1, 2));
    assert_eq!(grid.cell_area_m2(0, 0), Some(160.0));
    assert_eq!(grid.cell_area_m2(0, 1), Some(140.0));
    assert_eq!(grid.per_hectare(0, 0), Some(62.5));
    assert!((grid.per_hectare(0, 1).unwrap() - 10_000.0 / 140.0).abs() < 1e-10);
    assert_eq!(grid.per_hectare(1, 0), None);
    assert_eq!(grid.max_per_hectare(), 10_000.0 / 140.0);
}

#[test]
fn density_raster_is_north_up_with_transparent_empty_cells() {
    let terrain = terrain(3, 3, 10.0, (0.0, 0.0));
    let grid = density::from_positions(
        &terrain,
        &[[0.0; 3], [20.0, 0.0, 20.0]],
        options(10.0),
        &cancel(),
    )
    .unwrap();
    let image = grid.raster(2, 2, 100.0, 160, &cancel()).unwrap();
    let red = density::density_colour(100.0, 100.0, 160).unwrap();
    assert_eq!(image.rgba, [[0; 4], red, red, [0; 4]].concat());
    let legend = density::legend(100.0, 160).unwrap();
    assert_eq!(legend.map(|stop| stop.0), [0.0, 25.0, 50.0, 75.0, 100.0]);
    assert_eq!(legend[4].1, red);
    assert_eq!(density::density_colour(1_000.0, 100.0, 160).unwrap(), red);
}

#[test]
fn density_rejects_invalid_geometry_sizes_and_colour_units() {
    let mut terrain = terrain(3, 3, 10.0, (0.0, 0.0));
    for bin_m in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e-300] {
        assert!(density::from_positions(&terrain, &[], options(bin_m), &cancel()).is_err());
    }
    let limits = ReadLimits {
        cells: 3,
        ..Default::default()
    };
    assert!(
        density::from_positions(
            &terrain,
            &[],
            DensityOptions {
                limits,
                ..options(10.0)
            },
            &cancel()
        )
        .is_err()
    );
    terrain.cell = f32::NAN;
    assert!(MapBounds::from_terrain(&terrain).is_err());
    terrain.cell = 10.0;
    terrain.h.pop();
    assert!(MapBounds::from_terrain(&terrain).is_err());
    terrain.h.push(f32::NAN);
    assert!(density::from_positions(&terrain, &[], options(10.0), &cancel()).is_err());
    let terrain = self::terrain(2, 2, 1.0, (0.0, 0.0));
    let grid = density::from_positions(&terrain, &[], options(1.0), &cancel()).unwrap();
    assert!(grid.raster(0, 1, 1.0, 255, &cancel()).is_err());
    assert!(grid.raster(usize::MAX, 2, 1.0, 255, &cancel()).is_err());
    assert!(grid.raster(4097, 4097, 1.0, 255, &cancel()).is_err());
    assert!(grid.raster(1, 1, f64::NAN, 255, &cancel()).is_err());
    assert!(density::density_colour(f64::NAN, 100.0, 255).is_err());
    assert!(density::legend(0.0, 255).is_err());
}

#[test]
fn authored_classification_includes_flora_without_rocks_or_logs() {
    for kind in ["tree", "tree_big", "tree_small", "palm", "plant"] {
        assert!(Selection::Vegetation.includes("sma", kind));
    }
    assert!(Selection::Vegetation.includes("brush", ""));
    assert!(!Selection::Vegetation.includes("sma", "rock"));
    assert!(!Selection::Vegetation.includes("sma", "log"));
    assert!(!Selection::Plants.includes("sma", "tree_small"));
    assert!(Selection::Trees.includes("sma", "palm"));
    assert!(Selection::All.includes("", "unknown"));
}

#[test]
fn streamed_records_apply_selection_and_stop_at_the_cap() {
    use density::PlacementRecord;
    let terrain = terrain(2, 2, 10.0, (0.0, 0.0));
    let records = [
        PlacementRecord {
            position: [5.0, 1.0, 5.0],
            layer: "sma",
            kind: "tree",
        },
        PlacementRecord {
            position: [5.0, 0.0, 5.0],
            layer: "sma",
            kind: "rock",
        },
        PlacementRecord {
            position: [5.0, f64::NAN, 5.0],
            layer: "brush",
            kind: "plant",
        },
    ];
    let grid = density::from_records(&terrain, records, options(10.0), &cancel()).unwrap();
    assert_eq!(
        (
            grid.stats.records,
            grid.stats.counted,
            grid.stats.excluded,
            grid.stats.nonfinite
        ),
        (3, 1, 1, 1)
    );
    assert_eq!(grid.selection, Selection::Vegetation);
    let limits = ReadLimits {
        records: 2,
        ..Default::default()
    };
    let endless = std::iter::repeat(records[0]);
    assert!(
        density::from_records(
            &terrain,
            endless,
            DensityOptions {
                limits,
                ..options(10.0)
            },
            &cancel()
        )
        .unwrap_err()
        .contains("record limit")
    );
    assert!(
        density::legend(f64::MAX, 255)
            .unwrap()
            .iter()
            .all(|(value, _)| value.is_finite())
    );
}

#[test]
fn bounded_reader_rejects_growth_after_initial_metadata() {
    use std::io::Write;
    let scratch = Scratch::new();
    let path = scratch.write("growing.bin", b"1234");
    let result = density::read_bounded(&path, 4, &cancel(), |_, _| {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"5").unwrap();
    });
    assert!(result.unwrap_err().contains("grew past"));
}

#[test]
fn numeric_npy_positions_preserve_endian_fortran_and_f64_bounds() {
    use foxcore::npy::Npy;
    let scratch = Scratch::new();
    let terrain = terrain(2, 2, 10.0, (0.0, 0.0));
    // N x 2 Fortran XZ: each axis is contiguous. One f64 lies just outside
    // the extent; narrowing to f32 would incorrectly count it on the border.
    let values = [0.0f64, 10.0 + 1e-9, 0.0, 10.0];
    let array = Npy {
        descr_raw: "'>f8'".into(),
        descr: ">f8".into(),
        fortran: true,
        shape: vec![2, 2],
        data: values
            .iter()
            .flat_map(|value| value.to_be_bytes())
            .collect(),
    };
    let path = scratch.write("xz.npy", &array.try_write().unwrap());
    let options = DensityOptions {
        selection: Selection::All,
        ..options(10.0)
    };
    let grid = density::load_xyz_npy(&path, &terrain, options, &cancel(), |_, _| {}).unwrap();
    assert_eq!((grid.stats.counted, grid.stats.outside), (1, 1));
    let xyz = Npy::from_f32(vec![2, 3], &[0.0, 1.0, 0.0, 10.0, f32::NAN, 10.0]);
    let path = scratch.write("xyz.npy", &xyz.try_write().unwrap());
    let grid = density::load_xyz_npy(&path, &terrain, options, &cancel(), |_, _| {}).unwrap();
    assert_eq!((grid.stats.counted, grid.stats.nonfinite), (1, 1));
    assert!(
        density::load_xyz_npy(&path, &terrain, self::options(10.0), &cancel(), |_, _| {})
            .unwrap_err()
            .contains("All placements")
    );
}

#[test]
fn numeric_npy_loader_rejects_wrong_shapes_and_cancelled_reads() {
    use foxcore::npy::Npy;
    let scratch = Scratch::new();
    let terrain = terrain(2, 2, 1.0, (0.0, 0.0));
    let options = DensityOptions {
        selection: Selection::All,
        ..options(1.0)
    };
    for (index, array) in [
        Npy::from_f32(vec![3], &[0.0; 3]),
        Npy::from_f32(vec![1, 4], &[0.0; 4]),
        Npy::from_u8(vec![1, 2], &[0; 2]),
    ]
    .into_iter()
    .enumerate()
    {
        let path = scratch.write(&format!("bad{index}.npy"), &array.try_write().unwrap());
        assert!(density::load_xyz_npy(&path, &terrain, options, &cancel(), |_, _| {}).is_err());
    }
    let path = scratch.write(
        "ok.npy",
        &Npy::from_f32(vec![1, 2], &[0.0; 2]).try_write().unwrap(),
    );
    let stop = cancel();
    assert_eq!(
        density::load_xyz_npy(&path, &terrain, options, &stop, |_, _| {
            stop.store(true, Ordering::Relaxed);
        })
        .unwrap_err(),
        "cancelled"
    );
}

fn record_npy(descriptor: &str, records: usize, data: &[u8]) -> Vec<u8> {
    let body =
        format!("{{'descr': {descriptor}, 'fortran_order': False, 'shape': ({records},), }}");
    let padding = (64 - (10 + body.len() + 1) % 64) % 64;
    let header = format!("{body}{}\n", " ".repeat(padding));
    let mut bytes = b"\x93NUMPY\x01\x00".to_vec();
    bytes.extend_from_slice(&(header.len() as u16).to_le_bytes());
    bytes.extend_from_slice(header.as_bytes());
    bytes.extend_from_slice(data);
    bytes
}

fn unicode(bytes: &mut [u8], value: &str, big_endian: bool) {
    for (slot, character) in bytes.chunks_exact_mut(4).zip(value.chars()) {
        let codepoint = character as u32;
        slot.copy_from_slice(&if big_endian {
            codepoint.to_be_bytes()
        } else {
            codepoint.to_le_bytes()
        });
    }
}

#[test]
fn structured_tp_dtype_loads_real_schema_and_authored_selection() {
    let descriptor = "[('origin', '<U16'), ('layer', '<U6'), ('plugin', '<i2'), ('model', '<i2'), ('props', '<i2'), ('x', '<f8'), ('y', '<f8'), ('z', '<f8'), ('L', '<f8', (3, 3)), ('sbyte', '|u1'), ('clear', '<f4'), ('kind', '<U10'), ('tile', '<i4')]";
    let mut data = vec![0u8; 4 * 239];
    for (row, (layer, kind, position)) in [
        ("sma", "tree", [5.0f64, 1.0, 5.0]),
        ("sma", "rock", [5.0, 1.0, 5.0]),
        ("brush", "plant", [30.0, 1.0, 5.0]),
        ("sma", "plant", [5.0, f64::NAN, 5.0]),
    ]
    .into_iter()
    .enumerate()
    {
        let record = &mut data[row * 239..(row + 1) * 239];
        unicode(&mut record[64..88], layer, false);
        unicode(&mut record[195..235], kind, false);
        for (column, value) in position.into_iter().enumerate() {
            let offset = 94 + column * 8;
            record[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
    }
    let scratch = Scratch::new();
    let path = scratch.write("placements.npy", &record_npy(descriptor, 4, &data));
    let terrain = terrain(2, 2, 10.0, (0.0, 0.0));
    let grid = density::load(&path, &terrain, options(10.0), &cancel(), |_, _| {}).unwrap();
    assert_eq!(
        (
            grid.stats.records,
            grid.stats.counted,
            grid.stats.excluded,
            grid.stats.outside,
            grid.stats.nonfinite
        ),
        (4, 1, 1, 1, 1)
    );
    assert_eq!(grid.per_hectare(0, 0), Some(100.0));
    assert_eq!(grid.selection, Selection::Vegetation);
}

#[test]
fn structured_loader_uses_descriptor_order_and_mixed_scalar_formats() {
    let descriptor =
        "[('kind', '|S10'), ('z', '>f8'), ('layer', '>U6'), ('x', '<f4'), ('y', '>f4')]";
    let mut data = vec![0u8; 50];
    data[..5].copy_from_slice(b"plant");
    data[10..18].copy_from_slice(&4.0f64.to_be_bytes());
    unicode(&mut data[18..42], "sma", true);
    data[42..46].copy_from_slice(&2.0f32.to_le_bytes());
    data[46..50].copy_from_slice(&1.0f32.to_be_bytes());
    let scratch = Scratch::new();
    let path = scratch.write("mixed.npy", &record_npy(descriptor, 1, &data));
    let terrain = terrain(3, 3, 5.0, (0.0, 0.0));
    let grid = density::load(&path, &terrain, options(5.0), &cancel(), |_, _| {}).unwrap();
    assert_eq!(grid.counts(), &[1, 0, 0, 0]);
}

#[test]
fn structured_empty_arrays_validate_fields_and_selection_requirements() {
    let scratch = Scratch::new();
    let terrain = terrain(2, 2, 1.0, (0.0, 0.0));
    for descriptor in [
        "[('x', '<i4'), ('y', '<f8'), ('z', '<f8')]",
        "[('x', '<f8'), ('y', '<f8')]",
        "[('x', '<f8', (1,)), ('y', '<f8'), ('z', '<f8')]",
    ] {
        let path = scratch.write("invalid.npy", &record_npy(descriptor, 0, &[]));
        let options = DensityOptions {
            selection: Selection::All,
            ..options(1.0)
        };
        assert!(density::load(&path, &terrain, options, &cancel(), |_, _| {}).is_err());
    }
    let descriptor = "[('x', '<f8'), ('y', '<f8'), ('z', '<f8')]";
    let path = scratch.write("empty.npy", &record_npy(descriptor, 0, &[]));
    assert!(density::load(&path, &terrain, options(1.0), &cancel(), |_, _| {}).is_err());
    let options = DensityOptions {
        selection: Selection::All,
        ..options(1.0)
    };
    assert_eq!(
        density::load(&path, &terrain, options, &cancel(), |_, _| {})
            .unwrap()
            .stats
            .records,
        0
    );
}

#[test]
fn bounded_reader_cancels_between_chunks_and_enforces_caps() {
    let scratch = Scratch::new();
    let path = scratch.write("input.bin", &vec![7; 2 * 64 * 1024]);
    let stop = cancel();
    let mut callbacks = 0;
    let result = density::read_bounded(&path, 256 * 1024, &stop, |done, _| {
        callbacks += 1;
        assert_eq!(done, 64 * 1024);
        stop.store(true, Ordering::Relaxed);
    });
    assert_eq!(result.unwrap_err(), "cancelled");
    assert_eq!(callbacks, 1);
    assert!(density::read_bounded(&path, 100, &cancel(), |_, _| {}).is_err());
    assert!(density::read_bounded(&scratch.0, 100, &cancel(), |_, _| {}).is_err());
    assert!(
        ReadLimits {
            bytes: density::MAX_INPUT_BYTES + 1,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    let terrain = terrain(2, 2, 1.0, (0.0, 0.0));
    assert_eq!(
        density::from_positions(&terrain, &[], options(1.0), &AtomicBool::new(true)).unwrap_err(),
        "cancelled"
    );
}

#[test]
fn nav_diff_ignores_order_direction_and_duplicate_interior_edges() {
    let a = [0.0, 2.0, 0.0];
    let b = [10.0, 2.0, 0.0];
    let c = [10.0, 2.0, 10.0];
    let current = overlay(vec![edge(b, a, false), edge(c, b, true), edge(a, b, false)]);
    let baseline = overlay(vec![edge(b, c, true), edge(a, b, false)]);
    let diff = navdiff::compare(&current, &baseline, DiffOptions::default(), &cancel()).unwrap();
    assert_eq!(
        (
            diff.current_unique,
            diff.baseline_unique,
            diff.unchanged,
            diff.current_duplicates
        ),
        (2, 2, 2, 1)
    );
    assert!(diff.added.is_empty() && diff.removed.is_empty() && diff.boundary_changed.is_empty());
}

#[test]
fn nav_diff_detects_added_removed_height_and_boundary_changes() {
    let low = edge([0.0, 0.0, 0.0], [10.0, 0.0, 0.0], false);
    let high = edge([0.0, 1.0, 0.0], [10.0, 1.0, 0.0], false);
    let prior_boundary = edge([0.0, 0.0, 10.0], [10.0, 0.0, 10.0], true);
    let current_interior = (prior_boundary.0, prior_boundary.1, false);
    let diff = navdiff::compare(
        &overlay(vec![high, current_interior]),
        &overlay(vec![low, prior_boundary]),
        DiffOptions::default(),
        &cancel(),
    )
    .unwrap();
    assert_eq!(diff.added, vec![high]);
    assert_eq!(diff.removed, vec![low]);
    assert_eq!(diff.boundary_changed, vec![current_interior]);
    assert_eq!(diff.unchanged, 0);
}

#[test]
fn nav_diff_snaps_world_metres_and_reports_collapsed_edges() {
    let current = overlay(vec![
        edge([0.002, 0.0, 0.0], [1.003, 0.0, 0.0], false),
        edge([0.001, 0.0, 0.0], [0.002, 0.0, 0.0], true),
    ]);
    let baseline = overlay(vec![edge([0.0; 3], [1.0, 0.0, 0.0], false)]);
    let diff = navdiff::compare(&current, &baseline, DiffOptions::default(), &cancel()).unwrap();
    assert_eq!((diff.unchanged, diff.current_degenerate), (1, 1));
    assert!(diff.added.is_empty());
}

#[test]
fn nav_diff_rejects_incomplete_nonfinite_oversized_and_cancelled_inputs() {
    let mut current = overlay(vec![edge([0.0; 3], [1.0, 0.0, 0.0], false)]);
    let baseline = overlay(Vec::new());
    current.errors.push("one tile failed".into());
    assert!(navdiff::compare(&current, &baseline, DiffOptions::default(), &cancel()).is_err());
    current.errors.clear();
    current.edges[0].0[1] = f32::NAN;
    assert!(navdiff::compare(&current, &baseline, DiffOptions::default(), &cancel()).is_err());
    current.edges[0].0[1] = 0.0;
    for quantum_m in [0.0, f64::NAN, 2.0, 1e-300] {
        assert!(
            navdiff::compare(
                &current,
                &baseline,
                DiffOptions {
                    quantum_m,
                    ..Default::default()
                },
                &cancel()
            )
            .is_err()
        );
    }
    assert_eq!(
        navdiff::compare(
            &current,
            &baseline,
            DiffOptions::default(),
            &AtomicBool::new(true)
        )
        .unwrap_err(),
        "cancelled"
    );
}

fn triangle_bytes() -> Vec<u8> {
    let nav = Nav2 {
        origin: (-10.0, 2.0, -20.0),
        denominator: (2, 2, 2),
        chunks: vec![DataChunk {
            mesh: Navmesh {
                positions: vec![[0, 0, 0], [20, 0, 0], [0, 0, 20]],
                polygons: vec![Polygon {
                    vertices: vec![0, 1, 2],
                    neighbors: vec![nav2::NO_NEIGHBOR; 3],
                    ..Default::default()
                }],
            },
            segments: vec![Segment {
                position_count: 3,
                polygon_count: 1,
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    nav2::write(&nav)
}

#[test]
fn explicit_baseline_missing_and_corrupt_are_distinct_states() {
    let scratch = Scratch::new();
    match navdiff::load_baseline(&[], ReadLimits::default(), &cancel(), |_, _| {}).unwrap() {
        Baseline::Missing(missing) => assert!(missing.path.is_none()),
        _ => panic!("no selection cannot report an unchanged comparison"),
    }
    let missing_path = scratch.0.join("missing.nav2");
    match navdiff::compare_selected(
        &overlay(Vec::new()),
        std::slice::from_ref(&missing_path),
        DiffOptions::default(),
        &cancel(),
        |_, _| {},
    )
    .unwrap()
    {
        Comparison::Missing(missing) => assert_eq!(missing.path, Some(missing_path)),
        _ => panic!("missing baseline cannot report an unchanged comparison"),
    }
    let corrupt = scratch.write("broken.nav2", b"not a navmesh");
    assert!(
        navdiff::load_baseline(&[corrupt], ReadLimits::default(), &cancel(), |_, _| {}).is_err()
    );
}

#[test]
fn baseline_reads_real_format_world_transform_and_total_budget() {
    let scratch = Scratch::new();
    let bytes = triangle_bytes();
    let first = scratch.write("first.nav2", &bytes);
    let second = scratch.write("second.nav2", &bytes);
    let Baseline::Loaded(baseline) = navdiff::load_baseline(
        &[first.clone(), first.clone()],
        ReadLimits::default(),
        &cancel(),
        |_, _| {},
    )
    .unwrap() else {
        panic!("valid baseline was not loaded")
    };
    assert_eq!(
        (baseline.files, baseline.polygons, baseline.edges.len()),
        (1, 1, 3)
    );
    assert_eq!(baseline.tiles[0].1, [-10.0, -20.0, 0.0, -10.0]);
    assert!(
        baseline
            .edges
            .iter()
            .all(|edge| edge.0[1] == 2.0 && edge.1[1] == 2.0)
    );
    let diff = navdiff::compare(&baseline, &baseline, DiffOptions::default(), &cancel()).unwrap();
    assert_eq!(diff.unchanged, 3);
    let limits = ReadLimits {
        bytes: bytes.len() as u64,
        ..Default::default()
    };
    assert!(
        navdiff::load_baseline(&[first, second], limits, &cancel(), |_, _| {})
            .unwrap_err()
            .contains("total byte")
    );
}

#[test]
fn nonadvancing_nav_chunk_chain_is_rejected_before_core_decode() {
    let scratch = Scratch::new();
    let mut bytes = triangle_bytes();
    let start = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    bytes[start + 4..start + 8].copy_from_slice(&0u32.to_le_bytes());
    let bad = scratch.write("cycle.nav2", &bytes);
    assert!(
        navdiff::load_baseline(&[bad], ReadLimits::default(), &cancel(), |_, _| {})
            .unwrap_err()
            .contains("non-advancing")
    );
}

#[test]
fn nav_raster_clips_long_edges_and_preserves_north_up() {
    let terrain = terrain(3, 3, 10.0, (0.0, 0.0));
    let current = overlay(vec![edge([-1e20, 0.0, 20.0], [1e20, 0.0, 20.0], true)]);
    // Raster clipping also applies to an already-decoded diff with large world
    // coordinates; normal compare rejects coordinates outside its integer lattice.
    let mut diff = navdiff::compare(
        &overlay(Vec::new()),
        &overlay(Vec::new()),
        DiffOptions::default(),
        &cancel(),
    )
    .unwrap();
    diff.added = current.edges;
    let image = diff.raster(&terrain, 3, 3, &cancel()).unwrap();
    assert_eq!(&image.rgba[..12], &navdiff::ADDED_COLOUR.repeat(3));
    assert!(image.rgba[12..].iter().all(|&v| v == 0));
    assert!(diff.raster(&terrain, 0, 3, &cancel()).is_err());
}

#[derive(Default)]
struct UiFixture {
    layers: PreviewLayers,
    actions: Vec<LayerAction>,
    density: Option<density::DensityGrid>,
    comparison: Option<Comparison>,
    busy: bool,
}

fn ui_harness(state: UiFixture) -> Harness<'static, UiFixture> {
    Harness::builder()
        .with_size([1200.0, 900.0])
        .build_ui_state(
            |ui, state: &mut UiFixture| {
                let density = if state.busy {
                    LayerState::Loading { fraction: 0.5 }
                } else {
                    state
                        .density
                        .as_ref()
                        .map_or(LayerState::Idle, LayerState::Ready)
                };
                let comparison = state
                    .comparison
                    .as_ref()
                    .map_or(LayerState::Idle, LayerState::Ready);
                state.actions.extend(state.layers.show(
                    ui,
                    LayerStatus {
                        terrain_available: true,
                        nav_available: true,
                        density,
                        comparison,
                    },
                ));
            },
            state,
        )
}

#[test]
fn layer_controls_emit_native_load_and_explicit_empty_baseline_actions() {
    let state = UiFixture {
        layers: PreviewLayers {
            density_path: "placements.npy".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut harness = ui_harness(state);
    harness.get_by_label("Load density").click();
    harness.run_steps(2);
    assert!(
        matches!(harness.state().actions.first(), Some(LayerAction::LoadDensity { path, options })
        if path == &PathBuf::from("placements.npy") && options.bin_m == 16.0 && options.selection == Selection::Vegetation)
    );
    harness.get_by_label("Compare navmesh").click();
    harness.run_steps(2);
    assert!(
        harness.state().actions.iter().any(
            |action| matches!(action, LayerAction::CompareNav { paths, .. } if paths.is_empty())
        )
    );
}

#[test]
fn layer_controls_show_real_density_legend_and_missing_baseline() {
    let terrain = terrain(2, 2, 10.0, (0.0, 0.0));
    let grid =
        density::from_positions(&terrain, &[[5.0, 0.0, 5.0]], options(10.0), &cancel()).unwrap();
    let comparison = Comparison::Missing(navdiff::MissingBaseline {
        path: None,
        reason: "Select a prior .nav2 baseline.".into(),
    });
    let mut harness = ui_harness(UiFixture {
        density: Some(grid),
        comparison: Some(comparison),
        ..Default::default()
    });
    harness.get_by_label("1 counted · 0 excluded · 0 outside · 0 nonfinite");
    harness.get_by_label("Instances per hectare of horizontal ground; +z is north.");
    harness.get_by_label("Select a prior .nav2 baseline.");
    harness.get_by_label("Show density").click();
    harness.run_steps(2);
    assert!(!harness.state().layers.show_density);
}

#[test]
fn layer_controls_cancel_loading_without_running_io_on_ui_thread() {
    let mut harness = ui_harness(UiFixture {
        busy: true,
        ..Default::default()
    });
    harness.get_by_label("Cancel density").click();
    harness.run_steps(2);
    assert!(
        harness
            .state()
            .actions
            .iter()
            .any(|action| matches!(action, LayerAction::CancelDensity))
    );
}
