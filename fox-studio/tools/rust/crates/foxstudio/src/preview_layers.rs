//! Reusable terrain-layer controls. This component does no file I/O, parsing or
//! CPU raster work on the UI thread. Its typed actions go to the preview owner's
//! cancellable worker tasks; results and loading/error states come back as views.
use crate::preview::density::{self, DensityGrid, DensityOptions, Selection};
use crate::preview::navdiff::{self, Comparison, DiffOptions};
use eframe::egui::{self, Color32, RichText};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub enum LayerAction {
    LoadDensity {
        path: PathBuf,
        options: DensityOptions,
    },
    CompareNav {
        paths: Vec<PathBuf>,
        options: DiffOptions,
    },
    CancelDensity,
    CancelNav,
}

#[derive(Clone, Copy, Debug, Default)]
pub enum LayerState<'a, T> {
    #[default]
    Idle,
    Loading {
        fraction: f32,
    },
    Ready(&'a T),
    Error(&'a str),
}

#[derive(Debug)]
pub struct LayerStatus<'a> {
    pub terrain_available: bool,
    pub nav_available: bool,
    pub density: LayerState<'a, DensityGrid>,
    pub comparison: LayerState<'a, Comparison>,
}

impl Default for LayerStatus<'_> {
    fn default() -> Self {
        Self {
            terrain_available: false,
            nav_available: false,
            density: LayerState::Idle,
            comparison: LayerState::Idle,
        }
    }
}

/// Keep one instance per project preview. Reset it when changing projects and
/// cancel/discard previous tasks before adopting a result for a different dataset.
pub struct PreviewLayers {
    pub density_path: String,
    /// One explicitly selected prior .nav2 file per line.
    pub baseline_paths: String,
    pub bin_m: f64,
    pub selection: Selection,
    pub ceiling_per_ha: f64,
    pub opacity: u8,
    pub quantum_m: f64,
    pub show_density: bool,
    pub show_nav_diff: bool,
}

impl Default for PreviewLayers {
    fn default() -> Self {
        Self {
            density_path: String::new(),
            baseline_paths: String::new(),
            bin_m: 16.0,
            selection: Selection::Vegetation,
            ceiling_per_ha: 1_000.0,
            opacity: 160,
            quantum_m: 0.01,
            show_density: true,
            show_nav_diff: true,
        }
    }
}

impl PreviewLayers {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn show(&mut self, ui: &mut egui::Ui, status: LayerStatus<'_>) -> Vec<LayerAction> {
        let mut actions = Vec::new();
        ui.push_id("terrain_preview_layers", |ui| {
            self.density_controls(ui, &status, &mut actions);
            ui.separator();
            self.nav_controls(ui, &status, &mut actions);
        });
        actions
    }

    fn density_controls(
        &mut self,
        ui: &mut egui::Ui,
        status: &LayerStatus<'_>,
        actions: &mut Vec<LayerAction>,
    ) {
        ui.label(RichText::new("Vegetation density").strong());
        let busy = matches!(status.density, LayerState::Loading { .. });
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal(|ui| {
                ui.label("Placement array");
                ui.add(
                    egui::TextEdit::singleline(&mut self.density_path)
                        .hint_text("placements.npy")
                        .desired_width(260.0),
                );
                if ui.button("Choose placements…").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("NumPy placements", &["npy"])
                        .pick_file()
                {
                    self.density_path = path.display().to_string();
                }
            });
            ui.horizontal(|ui| {
                ui.label("Density cell");
                ui.add(
                    egui::DragValue::new(&mut self.bin_m)
                        .range(0.1..=4096.0)
                        .speed(1.0)
                        .suffix(" m"),
                );
                egui::ComboBox::from_id_salt("placement_selection")
                    .selected_text(self.selection.label())
                    .show_ui(ui, |ui| {
                        for selection in Selection::ALL {
                            ui.selectable_value(&mut self.selection, selection, selection.label());
                        }
                    });
                let valid = self.bin_m.is_finite() && self.bin_m > 0.0;
                if ui
                    .add_enabled(
                        status.terrain_available && valid && !self.density_path.trim().is_empty(),
                        egui::Button::new("Load density"),
                    )
                    .clicked()
                {
                    actions.push(LayerAction::LoadDensity {
                        path: PathBuf::from(self.density_path.trim()),
                        options: DensityOptions {
                            bin_m: self.bin_m,
                            selection: self.selection,
                            ..Default::default()
                        },
                    });
                }
            });
        });
        match &status.density {
            LayerState::Idle => {
                ui.label(if status.terrain_available {
                    "Select a placement array to measure its density."
                } else {
                    "Load terrain to measure placement density."
                });
            }
            LayerState::Loading { fraction } => {
                loading(ui, *fraction, "Reading placements");
                if ui.button("Cancel density").clicked() {
                    actions.push(LayerAction::CancelDensity);
                }
            }
            LayerState::Error(error) => {
                ui.colored_label(Color32::LIGHT_RED, *error);
            }
            LayerState::Ready(grid) => {
                ui.label(format!(
                    "{} counted · {} excluded · {} outside · {} nonfinite",
                    grid.stats.counted,
                    grid.stats.excluded,
                    grid.stats.outside,
                    grid.stats.nonfinite
                ));
                ui.label(format!(
                    "{} × {} cells · {:.2} m · {}",
                    grid.cols,
                    grid.rows,
                    grid.bin_m,
                    grid.selection.label()
                ));
                ui.label("Instances per hectare of horizontal ground; +z is north.");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.show_density, "Show density");
                    ui.label("Colour ceiling");
                    ui.add(
                        egui::DragValue::new(&mut self.ceiling_per_ha)
                            .range(0.01..=1e12)
                            .speed(10.0)
                            .suffix(" / ha"),
                    );
                    if ui.button("Use observed maximum").clicked() {
                        self.ceiling_per_ha = grid.max_per_hectare().max(1.0);
                    }
                    ui.add(egui::Slider::new(&mut self.opacity, 0..=255).text("Opacity"));
                });
                match density::legend(self.ceiling_per_ha, self.opacity) {
                    Ok(stops) => {
                        ui.horizontal_wrapped(|ui| {
                            for (i, (value, colour)) in stops.into_iter().enumerate() {
                                swatch(ui, colour);
                                ui.label(if i == 4 {
                                    format!("≥{value:.1} / ha")
                                } else {
                                    format!("{value:.1} / ha")
                                });
                            }
                        });
                        ui.label("Empty cells are transparent; values above the ceiling use the final colour.");
                    }
                    Err(error) => {
                        ui.colored_label(Color32::LIGHT_RED, error);
                    }
                }
            }
        }
    }

    fn nav_controls(
        &mut self,
        ui: &mut egui::Ui,
        status: &LayerStatus<'_>,
        actions: &mut Vec<LayerAction>,
    ) {
        ui.label(RichText::new("Navmesh difference").strong());
        let busy = matches!(status.comparison, LayerState::Loading { .. });
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal(|ui| {
                ui.label("Prior .nav2 files");
                ui.add(
                    egui::TextEdit::multiline(&mut self.baseline_paths)
                        .desired_rows(2)
                        .hint_text("One selected baseline file per line")
                        .desired_width(260.0),
                );
                if ui.button("Choose baseline…").clicked()
                    && let Some(paths) = rfd::FileDialog::new()
                        .add_filter("Navmesh baseline", &["nav2"])
                        .pick_files()
                {
                    self.baseline_paths = paths
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join("\n");
                }
            });
            ui.horizontal(|ui| {
                ui.label("World rounding");
                ui.add(
                    egui::DragValue::new(&mut self.quantum_m)
                        .range(0.0001..=1.0)
                        .speed(0.001)
                        .max_decimals(4)
                        .suffix(" m"),
                );
                let valid =
                    self.quantum_m.is_finite() && self.quantum_m > 0.0 && self.quantum_m <= 1.0;
                if ui
                    .add_enabled(
                        status.nav_available && valid,
                        egui::Button::new("Compare navmesh"),
                    )
                    .clicked()
                {
                    let paths = self
                        .baseline_paths
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(PathBuf::from)
                        .collect();
                    actions.push(LayerAction::CompareNav {
                        paths,
                        options: DiffOptions {
                            quantum_m: self.quantum_m,
                            ..Default::default()
                        },
                    });
                }
            });
        });
        match &status.comparison {
            LayerState::Idle => {
                ui.label(if status.nav_available {
                    "Select a prior .nav2 baseline to compare world edges."
                } else {
                    "Load the current navmesh to compare a baseline."
                });
            }
            LayerState::Loading { fraction } => {
                loading(ui, *fraction, "Comparing navmesh");
                if ui.button("Cancel nav comparison").clicked() {
                    actions.push(LayerAction::CancelNav);
                }
            }
            LayerState::Error(error) => {
                ui.colored_label(Color32::LIGHT_RED, *error);
            }
            LayerState::Ready(Comparison::Missing(missing)) => {
                ui.colored_label(Color32::YELLOW, &missing.reason);
            }
            LayerState::Ready(Comparison::Ready(diff)) => {
                ui.checkbox(&mut self.show_nav_diff, "Show navmesh difference");
                ui.label(format!(
                    "{} added · {} removed · {} boundary changes · {} unchanged",
                    diff.added.len(),
                    diff.removed.len(),
                    diff.boundary_changed.len(),
                    diff.unchanged
                ));
                ui.horizontal_wrapped(|ui| {
                    for (label, colour) in [
                        ("Added", navdiff::ADDED_COLOUR),
                        ("Removed", navdiff::REMOVED_COLOUR),
                        ("Boundary changed", navdiff::BOUNDARY_COLOUR),
                    ] {
                        swatch(ui, colour);
                        ui.label(label);
                    }
                });
                ui.label(format!(
                    "World edges rounded to {:.4} m, including height.",
                    diff.quantum_m
                ));
                if diff.current_degenerate + diff.baseline_degenerate > 0 {
                    ui.label(format!(
                        "{} current and {} prior edges collapse at this rounding.",
                        diff.current_degenerate, diff.baseline_degenerate
                    ));
                }
            }
        }
    }
}

fn loading(ui: &mut egui::Ui, fraction: f32, label: &str) {
    if fraction.is_finite() && fraction >= 0.0 {
        ui.add(egui::ProgressBar::new(fraction.clamp(0.0, 1.0)).text(label));
    } else {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(label);
        });
    }
}

fn swatch(ui: &mut egui::Ui, rgba: [u8; 4]) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 12.0), egui::Sense::hover());
    ui.painter().rect_filled(
        rect,
        2.0,
        Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]),
    );
    ui.painter().rect_stroke(
        rect,
        2.0,
        (1.0, ui.visuals().text_color()),
        egui::StrokeKind::Inside,
    );
}
