//! Fox Studio: the desktop GUI of the Fox Engine tools (docs/release/GUI.md).
//!
//! egui (eframe, wgpu renderer). It calls the workspace libraries directly: `foxbuild` (graph, status, learned
//! edges), `foxinstall` (mod list / verify / install / uninstall, `.mgsv` writer) and `foxpipe` (repo root). The
//! one thing it starts as a child process is the build itself (`fox build ...`), so a build keeps its own job
//! object, lock and log, exactly as from a terminal.
//!
//! Module map:
//!   app        the window: top bar, tabs, status bar, project + settings tabs
//!   project    repo / tool / game probe (background poller)
//!   projects   foxproject.toml: open / create projects, recent list (FLYK = example #1)
//!   setup      first-run setup logic: game check, archive survey, unpack / index, dictionary, prerequisites
//!   setup_view the Setup tab (wizard)
//!   steam      finding the game: registry, libraryfolders.vdf, appmanifest_287700.acf (read only)
//!   preview    terrain / navmesh / texture / model preview back ends (wgpu 3-D view)
//!   preview_view the Previews tab
//!   build_view stage table, dependency graph, run timeline, run controls, logs, failure details
//!   timeline   per-stage start / end / result parsed from foxbuild output; Gantt chart
//!   graph      layered layout of the stage graph (pure, tested)
//!   mods_view  game folder: installed mods, verify, install / uninstall, `.mgsv` packing
//!   runner     the `fox build` child process and its streamed output
//!   tasks      background work with progress / log, never blocking the UI
//!   logtail    tail of a log file, read off the UI thread
//!   settings   per-user settings (%APPDATA%\fox-studio\settings.json)
//!   theme      colours, fonts, dark / light
pub mod about;
pub mod mods_conflicts;
pub mod output_diff;
pub mod preview_layers;
pub mod app;
pub mod build_events;
pub mod build_view;
pub mod graph;
pub mod logtail;
pub mod mods_view;
pub mod preview;
pub mod preview_view;
pub mod project;
pub mod projects;
pub mod project_editor;
pub mod runner;
pub mod settings;
pub mod setup;
pub mod setup_view;
pub mod steam;
pub mod tasks;
pub mod theme;
pub mod timeline;

pub use app::{AppOptions, FoxStudio};

pub const APP_NAME: &str = "Fox Studio";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
