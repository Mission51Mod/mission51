//! Real Preview tab/action/state verification with synthetic arrays and navmeshes.
//! No GPU context, game, private fixtures or display capture is used.
use eframe::egui;
use egui_kittest::{Harness, kittest::Queryable};
use foxcore::{
    nav2::{self, DataChunk, Nav2, Navmesh, Polygon, Segment},
    npy::Npy,
};
use foxstudio::{
    preview::{
        density::{DensityOptions, Selection},
        nav::NavOverlay,
        navdiff::{self, Comparison, DiffOptions},
        terrain::Terrain,
    },
    preview_layers::{LayerAction, LayerState},
    preview_view::{Env, PreviewView, TerrainMode},
    projects::{Meta, PreviewPaths, Project, ProjectFile},
    settings::Settings,
};
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Fixture {
    root: PathBuf,
    project: Project,
}
impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "foxstudio_preview_l_{}_{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let project = Project {
            file: root.join("foxproject.toml"),
            root: root.clone(),
            graph: root.join("graph.toml"),
            m3: None,
            spec: ProjectFile {
                format: 1,
                project: Meta {
                    name: "Synthetic L preview".into(),
                    kind: "location".into(),
                    description: String::new(),
                    root: ".".into(),
                    graph: "graph.toml".into(),
                },
                preview: PreviewPaths {
                    heights: "heights.npy".into(),
                    cell_m: Some(10.0),
                    origin: Some([0.0, 0.0]),
                    nav: vec!["current.nav2".into()],
                    ..Default::default()
                },
            },
        };
        let f = Self { root, project };
        f.heights(0.0);
        f.write(
            "placements.npy",
            &Npy::from_f32(vec![2, 3], &[0.0, 0.0, 0.0, 10.0, 0.0, 10.0])
                .try_write()
                .unwrap(),
        );
        f.write("current.nav2", &triangle(0.0));
        f.write("prior.nav2", &triangle(1.0));
        f
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.root.join(name), bytes).unwrap();
    }
    fn heights(&self, height: f32) {
        self.write(
            "heights.npy",
            &Npy::from_f32(vec![2, 2], &[height; 4]).try_write().unwrap(),
        );
    }
    fn settings(&self) -> Settings {
        Settings {
            repo_root: self.root.display().to_string(),
            ..Default::default()
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn triangle(y: f32) -> Vec<u8> {
    nav2::write(&Nav2 {
        origin: (0.0, y as f64, 0.0),
        denominator: (1, 1, 1),
        chunks: vec![DataChunk {
            mesh: Navmesh {
                positions: vec![[0, 0, 0], [10, 0, 0], [0, 0, 10]],
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
    })
}
fn show(
    view: &mut PreviewView,
    ctx: &egui::Context,
    settings: &mut Settings,
    project: Option<&Project>,
) {
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        view.show(
            ui,
            settings,
            &Env {
                project,
                wgpu: None,
                game_running: Some("synthetic running-game guard".into()),
            },
        );
    });
    // This test host deliberately has no renderer; consume its texture deltas.
    output.textures_delta.clear();
}
fn drain(view: &mut PreviewView) {
    let start = Instant::now();
    while view.busy() {
        view.poll();
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "Preview worker stalled"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn loaded(f: &Fixture, ctx: &egui::Context) -> PreviewView {
    let mut view = PreviewView::default();
    show(&mut view, ctx, &mut f.settings(), Some(&f.project));
    drain(&mut view);
    assert!(view.terrain.as_ref().unwrap().is_ok());
    assert!(view.nav.as_ref().unwrap().is_ok());
    view
}
fn density_action() -> LayerAction {
    LayerAction::LoadDensity {
        path: "placements.npy".into(),
        options: DensityOptions {
            selection: Selection::All,
            bin_m: 10.0,
            ..Default::default()
        },
    }
}

#[test]
fn real_actions_load_project_relative_density_and_compare_height_geometry() {
    let f = Fixture::new();
    let ctx = egui::Context::default();
    let mut view = loaded(&f, &ctx);
    view.apply_layer_action(&ctx, density_action());
    view.apply_layer_action(
        &ctx,
        LayerAction::CompareNav {
            paths: vec!["prior.nav2".into()],
            options: DiffOptions::default(),
        },
    );
    drain(&mut view);
    assert_eq!(view.mode, TerrainMode::Map);
    let status = view.layer_status();
    let LayerState::Ready(grid) = status.density else {
        panic!("{:?}", status.density)
    };
    assert_eq!(grid.stats.counted, 2);
    assert_eq!(grid.max_per_hectare(), 200.0);
    let LayerState::Ready(Comparison::Ready(diff)) = status.comparison else {
        panic!("{:?}", status.comparison)
    };
    assert_eq!(
        (diff.added.len(), diff.removed.len(), diff.unchanged),
        (3, 3, 0)
    );
    let input_before = std::fs::read(f.root.join("placements.npy")).unwrap();
    show(&mut view, &ctx, &mut f.settings(), Some(&f.project));
    assert_eq!(
        std::fs::read(f.root.join("placements.npy")).unwrap(),
        input_before
    );
}

#[test]
fn same_path_reload_clears_layers_and_reads_new_terrain_generation() {
    let f = Fixture::new();
    let ctx = egui::Context::default();
    let mut view = loaded(&f, &ctx);
    view.apply_layer_action(&ctx, density_action());
    drain(&mut view);
    let first = view.terrain.as_ref().unwrap().as_ref().unwrap().clone();
    f.heights(7.0);
    view.reload_terrain(&ctx, &f.project);
    assert!(view.terrain.is_none());
    assert!(matches!(view.layer_status().density, LayerState::Idle));
    drain(&mut view);
    let second = view.terrain.as_ref().unwrap().as_ref().unwrap();
    assert!(!std::sync::Arc::ptr_eq(&first, second));
    assert_eq!(second.terrain.min, 7.0);
    assert_eq!(first.terrain.min, 0.0);
}

#[test]
fn root_project_and_preview_identity_invalidate_even_identical_height_paths() {
    let f = Fixture::new();
    let ctx = egui::Context::default();
    let mut view = loaded(&f, &ctx);
    view.apply_layer_action(&ctx, density_action());
    drain(&mut view);
    view.synchronize_dataset(&f.root, Some(&f.project));
    assert!(matches!(view.layer_status().density, LayerState::Ready(_)));
    let mut changed = f.project.clone();
    changed.file = f.root.join("another.toml");
    view.synchronize_dataset(&f.root, Some(&changed));
    assert!(!view.layer_status().terrain_available);
    assert!(matches!(view.layer_status().density, LayerState::Idle));
    show(&mut view, &ctx, &mut f.settings(), Some(&f.project));
    drain(&mut view);
    view.synchronize_dataset(&f.root.join("different-tools"), Some(&f.project));
    assert!(view.terrain.is_none());
    show(&mut view, &ctx, &mut f.settings(), Some(&f.project));
    drain(&mut view);
    changed = f.project.clone();
    changed.spec.preview.origin = Some([20.0, -20.0]);
    view.synchronize_dataset(&f.root, Some(&changed));
    assert!(view.terrain.is_none());
    view.synchronize_dataset(&f.root, None);
    assert!(view.nav.is_none());
}

#[test]
fn missing_corrupt_and_cancelled_actions_have_distinct_visible_states() {
    let f = Fixture::new();
    let ctx = egui::Context::default();
    let mut view = loaded(&f, &ctx);
    view.apply_layer_action(
        &ctx,
        LayerAction::CompareNav {
            paths: vec![],
            options: DiffOptions::default(),
        },
    );
    drain(&mut view);
    assert!(matches!(
        view.layer_status().comparison,
        LayerState::Ready(Comparison::Missing(_))
    ));
    f.write("bad.nav2", b"corrupt");
    view.apply_layer_action(
        &ctx,
        LayerAction::CompareNav {
            paths: vec!["bad.nav2".into()],
            options: DiffOptions::default(),
        },
    );
    drain(&mut view);
    let LayerState::Error(error) = view.layer_status().comparison else {
        panic!("expected corrupt-input error")
    };
    assert!(error.contains("bad.nav2"));
    view.apply_layer_action(&ctx, density_action());
    view.apply_layer_action(&ctx, LayerAction::CancelDensity);
    drain(&mut view);
    let LayerState::Error(error) = view.layer_status().density else {
        panic!("late density result was adopted")
    };
    assert!(error.contains("cancelled"));
    view.apply_layer_action(
        &ctx,
        LayerAction::CompareNav {
            paths: vec!["prior.nav2".into()],
            options: DiffOptions::default(),
        },
    );
    view.apply_layer_action(&ctx, LayerAction::CancelNav);
    drain(&mut view);
    assert!(
        matches!(view.layer_status().comparison, LayerState::Error(e) if e.contains("cancelled"))
    );
}

#[test]
fn current_nav_raster_clips_and_preserves_boundary_priority_and_north_up() {
    let terrain = Terrain::new(2, 2, vec![0.0; 4], 10.0, 0.0, 0.0).unwrap();
    let nav = NavOverlay {
        edges: vec![
            ([-1e30, 0.0, 10.0], [1e30, 0.0, 10.0], true),
            ([0.0, 0.0, 10.0], [10.0, 0.0, 10.0], false),
            ([0.0, 0.0, 0.0], [10.0, 0.0, 0.0], false),
        ],
        ..Default::default()
    };
    let image = navdiff::raster_overlay(&terrain, &nav, 11, 11, &AtomicBool::new(false)).unwrap();
    assert_eq!(&image.rgba[..4], &[255, 120, 20, 255]);
    assert_eq!(&image.rgba[4 * 110..4 * 111], &[40, 190, 255, 150]);
    assert!(navdiff::raster_overlay(&terrain, &nav, 0, 11, &AtomicBool::new(false)).is_err());
    assert!(navdiff::raster_overlay(&terrain, &nav, 11, 11, &AtomicBool::new(true)).is_err());
}

struct UiState {
    view: PreviewView,
    settings: Settings,
    project: Project,
}
#[test]
fn real_preview_ui_buttons_load_density_legend_and_explicit_missing_baseline() {
    let f = Fixture::new();
    let ctx = egui::Context::default();
    let mut view = loaded(&f, &ctx);
    view.layers.density_path = "placements.npy".into();
    view.layers.selection = Selection::All;
    view.layers.bin_m = 10.0;
    let mut h = Harness::builder()
        .with_size([1400.0, 1000.0])
        .build_ui_state(
            |ui, s: &mut UiState| {
                s.view.poll();
                s.view.show(
                    ui,
                    &mut s.settings,
                    &Env {
                        project: Some(&s.project),
                        wgpu: None,
                        game_running: None,
                    },
                );
            },
            UiState {
                view,
                settings: f.settings(),
                project: f.project.clone(),
            },
        );
    h.get_by_label("Density and navmesh comparison").click();
    h.run_steps(3);
    h.get_by_label("Load density").click();
    h.run_steps(2);
    assert!(
        !matches!(h.state().view.layer_status().density, LayerState::Idle),
        "Load density click was not dispatched"
    );
    drain(&mut h.state_mut().view);
    h.run_steps(3);
    h.get_by_label("2 counted · 0 excluded · 0 outside · 0 nonfinite");
    h.get_by_label("Instances per hectare of horizontal ground; +z is north.");
    h.get_by_label("Show density").click();
    h.run_steps(2);
    assert!(!h.state().view.layers.show_density);
    h.get_by_label("Compare navmesh").click();
    h.run_steps(2);
    drain(&mut h.state_mut().view);
    h.run_steps(3);
    h.get_by_label("Select a prior .nav2 baseline.");
    assert_eq!(h.state().view.mode, TerrainMode::Map);
}

#[test]
fn missing_terrain_and_action_prerequisites_do_not_keep_old_data() {
    let f = Fixture::new();
    let ctx = egui::Context::default();
    let mut view = loaded(&f, &ctx);
    let mut project = f.project.clone();
    project.spec.preview.heights.clear();
    show(&mut view, &ctx, &mut f.settings(), Some(&project));
    assert!(view.terrain.is_none() && view.nav.is_none());
    view.apply_layer_action(&ctx, density_action());
    assert!(matches!(view.layer_status().density, LayerState::Error(e) if e.contains("terrain")));
    view.apply_layer_action(
        &ctx,
        LayerAction::CompareNav {
            paths: vec![],
            options: Default::default(),
        },
    );
    assert!(
        matches!(view.layer_status().comparison, LayerState::Error(e) if e.contains("terrain"))
    );
    view.synchronize_dataset(Path::new("root"), None);
    assert!(matches!(view.layer_status().density, LayerState::Idle));
}
