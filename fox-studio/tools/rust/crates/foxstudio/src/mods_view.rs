//! Mods view: a game folder's installed mods (foxinstall manifest), verify, install from a .mgsv or a staging
//! folder, uninstall, and `.mgsv` packing (foxinstall::mgsvpack). Every operation runs on a worker; the installer
//! itself refuses while the game runs, and the view says so before anyone tries.
use crate::mods_conflicts::{ConflictReport, ConflictsView};
use crate::settings::Settings;
use crate::tasks::Task;
use crate::theme::{self, pal};
use eframe::egui::{self, Align, Layout, RichText, Sense};
use egui_extras::{Column, TableBuilder};
use foxcore::runtime_data::RuntimeProfile;
use foxinstall::{Game, Manifest};
use std::path::{Path, PathBuf};

/// what a finished operation reports
#[derive(Clone, Debug, Default)]
pub struct OpResult {
    pub ok: bool,
    pub summary: String,
    pub log: Vec<String>,
    /// the game's manifest after the operation (list / install / uninstall / setup)
    pub manifest: Option<Manifest>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    List,
    Verify,
    Setup,
    Install { source: PathBuf, replace: bool },
    Uninstall { name: String },
    Pack { stage: PathBuf, out: PathBuf, level: u32 },
}

impl Op {
    pub fn title(&self) -> String {
        match self {
            Op::List => "Read installed mods".into(),
            Op::Verify => "Verify".into(),
            Op::Setup => "Set up game folder".into(),
            Op::Install { source, .. } => format!("Install {}", file_name(source)),
            Op::Uninstall { name } => format!("Uninstall {name}"),
            Op::Pack { out, .. } => format!("Pack {}", file_name(out)),
        }
    }
    /// writes into the game folder
    pub fn writes_game(&self) -> bool {
        matches!(self, Op::Setup | Op::Install { .. } | Op::Uninstall { .. })
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Long paths stay readable and selectable; wrap continuously rather than leaving a short prefix on its own line.
fn review_path(ui: &mut egui::Ui, label: &str, path: &Path) {
    let mut job = egui::text::LayoutJob::simple(
        format!("{label}: {}", path.display()),
        egui::FontId::monospace(13.0),
        ui.visuals().strong_text_color(),
        ui.available_width(),
    );
    job.wrap.break_anywhere = true;
    ui.add(egui::Label::new(job).selectable(true));
}

/// `;`-separated folder list (empty entries dropped)
pub fn split_dirs(s: &str) -> Vec<PathBuf> {
    s.split(';')
        .map(|x| x.trim())
        .filter(|x| !x.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Throttle for installer progress reports: passes phase changes, the final report and 1 % steps (the UI repaints
/// on each message, so a byte-level stream would cost frames for nothing). Returns (overall 0..1, phase text).
#[derive(Default)]
pub struct ProgressGate {
    last: Option<(String, f32)>,
}

impl ProgressGate {
    pub fn pass(&mut self, p: &foxinstall::Progress) -> Option<(f32, String)> {
        let fresh = match &self.last {
            None => true,
            Some((ph, f)) => p.done || *ph != p.phase || p.overall - *f >= 0.01,
        };
        if !fresh {
            return None;
        }
        self.last = Some((p.phase.clone(), p.overall));
        Some((p.overall, p.phase.clone()))
    }
}

/// "write 00.dat  42 %  (about 12 s left)": the remaining time once the estimate is past 10 % and 2 s in
pub fn progress_text(phase: &str, overall: f32, elapsed: std::time::Duration) -> String {
    let mut s = format!("{phase}  {:.0} %", overall.clamp(0.0, 1.0) * 100.0);
    let el = elapsed.as_secs_f32();
    if (0.1..1.0).contains(&overall) && el >= 2.0 {
        let left = el * (1.0 - overall) / overall;
        s.push_str(&format!(
            "  (about {} left)",
            crate::build_view::fmt_dur(std::time::Duration::from_secs_f32(left.max(1.0)))
        ));
    }
    s
}

/// All archive context belongs to the selected source install, never an ambient global profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModTarget {
    pub game: PathBuf,
    pub source_game: PathBuf,
    pub runtime_profile: Option<PathBuf>,
}

impl ModTarget {
    pub fn from_settings(settings: &Settings) -> Option<Self> {
        let game = settings.game_dir.trim();
        if game.is_empty() {
            return None;
        }
        let source = settings.game_install.trim();
        Some(Self {
            game: PathBuf::from(game),
            source_game: PathBuf::from(if source.is_empty() { game } else { source }),
            runtime_profile: settings.resolved_runtime_profile(),
        })
    }

    /// Read the profile afresh before every archive operation and validate its source provenance.
    pub fn load_profile(&self) -> Result<RuntimeProfile, String> {
        let path = self.runtime_profile.as_deref().ok_or_else(|| {
            "Prepare game data in Setup and select its runtime profile before archive operations.".to_string()
        })?;
        let profile =
            RuntimeProfile::load(path).map_err(|error| format!("Runtime profile {}: {error}", path.display()))?;
        profile.validate_for_game(&self.source_game).map_err(|error| {
            format!(
                "Runtime profile does not match source install {}: {error}",
                self.source_game.display()
            )
        })?;
        Ok(profile)
    }

    /// A loose GameDir entry must never replace the profile used to rebuild archives.
    /// Resolve both paths afresh immediately before any write action.
    pub fn validate_write_profile(&self) -> Result<(), String> {
        let path = self.runtime_profile.as_deref().ok_or_else(|| {
            "Prepare game data in Setup and select its runtime profile before archive operations.".to_string()
        })?;
        let game = self
            .game
            .canonicalize()
            .map_err(|error| format!("Target game folder {}: {error}", self.game.display()))?;
        let profile = path
            .canonicalize()
            .map_err(|error| format!("Runtime profile {}: {error}", path.display()))?;
        if profile.starts_with(&game) {
            return Err("Move the runtime profile outside the target game folder before installing, uninstalling or setting up mods.".into());
        }
        Ok(())
    }

    fn without_profile(game: &Path) -> Self {
        Self {
            game: game.to_path_buf(),
            source_game: game.to_path_buf(),
            runtime_profile: None,
        }
    }
}

/// Profile-free entry point for manifest reads and package creation. Archive actions require run_op_for_target.
pub fn run_op(game: &Path, vanilla_dirs: &[PathBuf], op: &Op) -> Result<OpResult, String> {
    run_op_with(game, vanilla_dirs, op, None)
}

/// Profile-free compatibility entry point; it never falls back to built-in archive keys.
pub fn run_op_with(
    game: &Path,
    vanilla_dirs: &[PathBuf],
    op: &Op,
    progress: Option<foxinstall::ProgressFn>,
) -> Result<OpResult, String> {
    run_op_for_target(&ModTarget::without_profile(game), vanilla_dirs, op, progress)
}

/// Blocking worker entry point. List and Pack need no archive profile; every other operation validates it first.
pub fn run_op_for_target(
    target: &ModTarget,
    vanilla_dirs: &[PathBuf],
    op: &Op,
    progress: Option<foxinstall::ProgressFn>,
) -> Result<OpResult, String> {
    if op.writes_game() {
        target.validate_write_profile()?;
    }
    let mut installer = if matches!(op, Op::List | Op::Pack { .. }) {
        Game::new(&target.game, Some(&target.source_game))
    } else {
        let profile = target.load_profile()?;
        Game::with_profile(&target.game, Some(&target.source_game), &profile)?
    };
    installer.vanilla_dirs = vanilla_dirs.to_vec();
    if let Some(callback) = progress {
        installer.set_progress(callback);
    }
    let mut output = OpResult::default();
    let result: Result<String, String> = match op {
        Op::List => installer.load_manifest().map(|manifest| {
            let summary = format!("{} mod(s) installed", manifest.mods.len());
            output.manifest = Some(manifest);
            summary
        }),
        Op::Verify => installer.verify().and_then(|valid| {
            if valid {
                Ok("verify: OK".to_string())
            } else {
                Err("verify found problems (see the log)".to_string())
            }
        }),
        Op::Setup => installer
            .setup()
            .map(|_| "set up: the current archives are the base now".to_string()),
        Op::Install { source, replace } => installer
            .install_opts(source, false, *replace)
            .map(|_| format!("installed {}", file_name(source))),
        Op::Uninstall { name } => installer.uninstall(name).map(|_| format!("uninstalled {name}")),
        Op::Pack { stage, out, level } => {
            let started = std::time::Instant::now();
            foxinstall::mgsvpack::write_mgsv(stage, out, *level).map(|stats| {
                format!(
                    "{}: {} files, {:.1} MB -> {:.1} MB in {:.1} s",
                    file_name(out),
                    stats.files,
                    stats.bytes_in as f64 / 1e6,
                    stats.bytes_out as f64 / 1e6,
                    started.elapsed().as_secs_f64()
                )
            })
        }
    };
    output.log = std::mem::take(&mut installer.log);
    match result {
        Ok(summary) => {
            output.ok = true;
            output.summary = summary;
            if output.manifest.is_none() && op.writes_game() {
                output.manifest = installer.load_manifest().ok();
            }
            Ok(output)
        }
        Err(mut error) => {
            if !output.log.is_empty() {
                error.push_str("\n\n");
                error.push_str(&output.log.join("\n"));
            }
            Err(error)
        }
    }
}

/// a folder that looks like an MGSV:TPP install (master/0/00.dat)
pub fn looks_like_game(p: &Path) -> bool {
    p.join("master").join("0").join("00.dat").is_file()
}

/// the installer's "game is running" refusal
pub fn is_game_running_error(e: &str) -> bool {
    e.contains("is running") || e.contains("the game is running")
}

#[derive(Clone, Debug, PartialEq)]
enum Confirm {
    Uninstall(String),
    Setup,
    Install(PathBuf, bool, String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProfileRequest {
    target: ModTarget,
    generation: u64,
}

#[derive(Default)]
struct ProfileValidation {
    write_error: Option<String>,
}

struct PackagePreview {
    source: PathBuf,
    task: Option<Task<String>>,
    result: Option<Result<String, String>>,
}

#[derive(Default)]
pub struct ModsView {
    pub manifest: Option<Result<Manifest, String>>,
    loaded_for: Option<ModTarget>,
    pub op: Option<(Op, Task<OpResult>)>,
    pub last: Option<(Op, Result<OpResult, String>, std::time::Duration)>,
    pub selected_mod: Option<String>,
    pub conflicts: Option<ConflictReport>,
    conflicts_view: ConflictsView,
    /// Destination, profile and action as reviewed, never later values from Settings.
    confirm: Option<(ProfileRequest, Confirm)>,
    target: Option<ModTarget>,
    generation: u64,
    profile: Option<Result<ProfileValidation, String>>,
    profile_task: Option<(ProfileRequest, Task<ProfileValidation>)>,
    op_request: Option<ProfileRequest>,
    last_target: Option<ModTarget>,
    drop_error: Option<String>,
    preview_after: Option<std::time::Instant>,
    preview: Option<PackagePreview>,
    vanilla_dirs: Vec<PathBuf>,
}

pub struct Env {
    pub game_running: Option<String>,
}

/// Validate the whole drop before changing the selected package. No install is started here.
pub fn dropped_package(files: &[egui::DroppedFileHandle]) -> Result<PathBuf, String> {
    if files.len() != 1 {
        return Err("Drop one .mgsv package at a time.".into());
    }
    let path = files[0].path();
    if !path.is_absolute() {
        return Err("Drop a local .mgsv file from your file browser.".into());
    }
    if !path
        .extension()
        .is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("mgsv"))
    {
        return Err("Only .mgsv packages can be dropped here. Use Pick folder for a staging folder.".into());
    }
    if !path.is_file() {
        return Err(format!(
            "The dropped package is not a readable file: {}",
            path.display()
        ));
    }
    Ok(path.to_path_buf())
}

impl ModsView {
    /// Select and inspect a drop, even while the game runs; installation still requires its normal review.
    pub fn receive_drop(&mut self, ctx: &egui::Context, s: &mut Settings, files: &[egui::DroppedFileHandle]) {
        match dropped_package(files) {
            Ok(path) => {
                self.drop_error = None;
                self.confirm = None;
                self.preview_after = None;
                s.install_source = path.display().to_string();
                self.preview_package(ctx, path);
            }
            Err(error) => self.drop_error = Some(error),
        }
    }

    /// Only metadata for this exact source can make its Install button ready.
    pub fn package_name(&self, path: &Path) -> Option<&str> {
        self.preview.as_ref().and_then(|preview| {
            if preview.source == path && preview.task.is_none() {
                preview
                    .result
                    .as_ref()
                    .and_then(|result| result.as_ref().ok())
                    .map(String::as_str)
            } else {
                None
            }
        })
    }

    fn sync_package(&mut self, ctx: &egui::Context, path: PathBuf, edited: bool) {
        if self.preview.as_ref().is_some_and(|preview| preview.source != path) || edited {
            self.preview = None;
            self.confirm = None;
        }
        if edited {
            self.drop_error = None;
            self.preview_after = Some(std::time::Instant::now() + std::time::Duration::from_millis(200));
        }
        if path.as_os_str().is_empty() {
            self.preview = None;
            self.preview_after = None;
            return;
        }
        if let Some(deadline) = self.preview_after
            && let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now())
        {
            ctx.request_repaint_after(remaining);
            return;
        }
        self.preview_after = None;
        self.preview_package(ctx, path);
    }

    fn current_request(&self) -> Option<ProfileRequest> {
        self.target.clone().map(|target| ProfileRequest {
            target,
            generation: self.generation,
        })
    }

    fn invalidate_target(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.loaded_for = None;
        self.manifest = None;
        self.selected_mod = None;
        self.conflicts = None;
        self.conflicts_view.reset_selection();
        self.confirm = None;
        self.profile = None;
        // Keep the old worker until it drains; only one profile validation can run at a time.
    }

    /// Called even while Mods is hidden, so old results cannot restore a previous Settings selection.
    pub fn sync_target(&mut self, ctx: &egui::Context, settings: &Settings) {
        self.vanilla_dirs = split_dirs(&settings.vanilla_dirs);
        let target = ModTarget::from_settings(settings);
        if self.target != target {
            self.target = target;
            self.invalidate_target();
        }
        if self.profile.is_none()
            && self.profile_task.is_none()
            && let Some(request) = self.current_request()
            && looks_like_game(&request.target.game)
        {
            if request.target.runtime_profile.is_none() {
                self.profile = Some(Err(
                    "Prepare game data in Setup to create a runtime profile for the source install.".into(),
                ));
            } else {
                let target = request.target.clone();
                let task = Task::spawn_serial("Validate game data", ctx, move |_| {
                    target.load_profile()?;
                    Ok(ProfileValidation {
                        write_error: target.validate_write_profile().err(),
                    })
                });
                self.profile_task = Some((request, task));
            }
        }
    }

    pub fn profile_ready(&self) -> bool {
        matches!(self.profile, Some(Ok(_)))
    }

    pub fn profile_write_error(&self) -> Option<&str> {
        self.profile
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .and_then(|validation| validation.write_error.as_deref())
    }

    pub fn writes_ready(&self) -> bool {
        self.profile_ready() && self.profile_write_error().is_none()
    }

    pub fn profile_error(&self) -> Option<&str> {
        self.profile
            .as_ref()
            .and_then(|result| result.as_ref().err())
            .map(String::as_str)
    }

    pub fn busy(&self) -> bool {
        self.op.is_some()
    }

    pub fn start(&mut self, ctx: &egui::Context, game: &Path, op: Op) {
        if self.op.is_some() {
            return;
        }
        let target = self
            .target
            .as_ref()
            .filter(|target| target.game == game)
            .cloned()
            .unwrap_or_else(|| ModTarget::without_profile(game));
        self.op_request = self.current_request().filter(|request| request.target == target);
        let vanilla_dirs = self.vanilla_dirs.clone();
        let worker_op = op.clone();
        let task = Task::spawn_serial(op.title(), ctx, move |task| {
            task.progress(-1.0, worker_op.title());
            let task = task.clone();
            let mut gate = ProgressGate::default();
            let callback: foxinstall::ProgressFn = Box::new(move |progress| {
                if let Some((fraction, phase)) = gate.pass(progress) {
                    task.progress(fraction, phase);
                }
            });
            run_op_for_target(&target, &vanilla_dirs, &worker_op, Some(callback))
        });
        self.op = Some((op, task));
    }

    pub fn poll(&mut self) {
        if let Some((request, task)) = self.profile_task.as_mut()
            && task.poll()
        {
            let request = request.clone();
            let result = task.take_result();
            self.profile_task = None;
            if self.current_request().as_ref() == Some(&request) {
                self.profile = result;
            }
        }
        let mut completed = None;
        if let Some((op, task)) = self.op.as_mut()
            && task.poll()
            && let Some(result) = task.take_result()
        {
            completed = Some((op.clone(), result, task.elapsed()));
        }
        if let Some((op, result, elapsed)) = completed {
            self.op = None;
            let request = self.op_request.take();
            let current = request.is_some() && request == self.current_request();
            if current
                && let Ok(output) = &result
                && let Some(manifest) = &output.manifest
            {
                self.conflicts = Some(ConflictReport::from_manifest(manifest));
                self.conflicts_view.reset_selection();
                self.manifest = Some(Ok(manifest.clone()));
            }
            if op == Op::List
                && current
                && let Err(error) = &result
            {
                self.manifest = Some(Err(error.clone()));
            }
            if op != Op::List || result.is_err() {
                self.last_target = request.map(|request| request.target);
                self.last = Some((op, result, elapsed));
            }
        }
        if let Some(preview) = self.preview.as_mut()
            && let Some(task) = preview.task.as_mut()
            && task.poll()
        {
            preview.result = task.take_result();
            preview.task = None;
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, s: &mut Settings, env: &Env) {
        let ctx = ui.ctx().clone();
        let p = pal(ui);
        self.sync_target(&ctx, s);
        self.sync_package(&ctx, PathBuf::from(s.install_source.trim()), false);
        let game = PathBuf::from(s.game_dir.trim());
        let game_ok = !s.game_dir.trim().is_empty() && looks_like_game(&game);
        if game_ok && self.loaded_for.as_ref() != self.target.as_ref() && self.op.is_none() {
            self.loaded_for = self.target.clone();
            self.manifest = None;
            self.selected_mod = None;
            self.start(&ctx, &game, Op::List);
        }
        egui::ScrollArea::vertical().id_salt("mods_scroll").auto_shrink([false, false]).show(ui, |ui| {
            if let Some(g) = &env.game_running {
                egui::Frame::new().fill(p.warn.gamma_multiply(0.15)).stroke(egui::Stroke::new(1.0, p.warn)).corner_radius(6u8).inner_margin(10i8).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(format!("{g} is running")).strong().color(p.warn));
                    ui.label("Install, uninstall and setup are paused until the game closes: the installer never rewrites archives the game has open. Reading the mod list, verifying and packing still work.");
                });
                ui.add_space(8.0);
            }
            // ---- game folder
            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                theme::section(ui, "Game folder");
                ui.horizontal(|ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut s.game_dir).hint_text("…\\steamapps\\common\\MGS_TPP").desired_width(ui.available_width() - 190.0));
                    if r.lost_focus() {
                        self.invalidate_target();
                    }
                    if ui.button("Browse…").clicked()
                        && let Some(folder) = rfd::FileDialog::new().set_title("MGSV:TPP game folder").pick_folder()
                    {
                        s.game_dir = folder.display().to_string();
                        self.invalidate_target();
                    }
                    if ui.add_enabled(game_ok && !self.busy(), egui::Button::new("Reload")).clicked() {
                        self.invalidate_target();
                    }
                });
                if s.game_dir.trim().is_empty() {
                    ui.label(RichText::new("Pick the folder that holds mgsvtpp.exe and master\\0\\00.dat.").color(p.muted));
                } else if !game_ok {
                    ui.colored_label(p.err, "This folder does not look like an MGSV:TPP install (no master\\0\\00.dat).");
                } else {
                    match &self.manifest {
                        None => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("Reading the mod manifest…");
                            });
                        }
                        Some(Ok(m)) => {
                            ui.horizontal_wrapped(|ui| {
                                theme::pill(ui, "managed by Fox Studio", p.ok);
                                theme::pill(ui, &format!("mode: {}", m.mode), p.info);
                                theme::pill(ui, &format!("layout: {}", m.layout), p.info);
                                theme::pill(ui, &format!("{} mod(s)", m.mods.len()), p.accent);
                            });
                        }
                        Some(Err(e)) if e.starts_with("not set up") => {
                            if game.join("snakebite.xml").exists() {
                                ui.colored_label(p.warn, "SnakeBite manages this game (snakebite.xml).");
                                ui.label("Adopt its installed mods first (writes nothing to the game files):");
                                ui.add(egui::Label::new(RichText::new(format!("fox mod setup --snakebite --game \"{}\"", game.display())).monospace()).selectable(true));
                            } else {
                                ui.label("Not set up yet. Setup keeps the current 00.dat / 01.dat as the base (a one-time copy next to them), so every uninstall restores them byte for byte.");
                                if ui.add_enabled(self.writes_ready() && !self.busy() && env.game_running.is_none(), egui::Button::new("Set up this game folder…")).clicked()
                                    && let Some(request) = self.current_request()
                                {
                                    self.confirm = Some((request, Confirm::Setup));
                                }
                            }
                        }
                        Some(Err(e)) => {
                            ui.colored_label(p.err, e);
                        }
                    }
                }
            });
            ui.add_space(8.0);
            // Validate changed selections before any action can become ready in this same frame.
            self.sync_target(&ctx, s);
            if game_ok {
                theme::card(ui).show(ui, |ui| {
                    theme::section(ui, "Game data");
                    if let Some(target) = &self.target {
                        review_path(ui, "Source install", &target.source_game);
                        if let Some(profile) = &target.runtime_profile { review_path(ui, "Runtime profile", profile); }
                    }
                    match &self.profile {
                        Some(Ok(validation)) => {
                            ui.colored_label(p.ok, "Runtime profile matches the source install.");
                            if let Some(error) = &validation.write_error { ui.colored_label(p.warn, error); }
                        }
                        Some(Err(error)) => {
                            ui.colored_label(p.warn, error);
                            if ui.button("Open Setup").clicked() { s.last_tab = crate::settings::Tab::Setup; }
                        }
                        None => { ui.horizontal(|ui| { ui.spinner(); ui.label("Validating game data…"); }); }
                    }
                });
                ui.add_space(8.0);
            }
            // ---- installed mods
            let game = PathBuf::from(s.game_dir.trim());
            let game_ok = !s.game_dir.trim().is_empty() && looks_like_game(&game);
            let can_write = game_ok && self.loaded_for.as_ref() == self.target.as_ref()
                && self.writes_ready() && !self.busy() && env.game_running.is_none() && matches!(self.manifest, Some(Ok(_)));
            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    theme::section(ui, "Installed mods");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let sel = self.selected_mod.clone();
                        if ui.add_enabled(can_write && sel.is_some(), egui::Button::new("Uninstall…")).clicked()
                            && let Some(name) = sel
                            && let Some(request) = self.current_request()
                        {
                            self.confirm = Some((request, Confirm::Uninstall(name)));
                        }
                        if ui.add_enabled(game_ok && self.profile_ready() && !self.busy() && matches!(self.manifest, Some(Ok(_))), egui::Button::new("Verify"))
                            .on_hover_text("Check the archives against the manifest: every base entry intact, every mod file present with its content")
                            .clicked()
                        {
                            self.start(&ctx, &game, Op::Verify);
                        }
                    });
                });
                match &self.manifest {
                    Some(Ok(m)) if m.mods.is_empty() => {
                        ui.label(RichText::new("No mods installed: the archives are the base.").color(p.muted));
                    }
                    Some(Ok(m)) => {
                        let mut clicked = None;
                        let sel = self.selected_mod.clone();
                        ui.push_id("mods_table", |ui| {
                            ui.style_mut().interaction.selectable_labels = false;
                            TableBuilder::new(ui)
                                .striped(true)
                                .resizable(true)
                                .sense(Sense::click())
                                .vscroll(false)
                                .cell_layout(Layout::left_to_right(Align::Center))
                                .column(Column::exact(28.0))
                                .column(Column::initial(260.0).at_least(120.0).clip(true))
                                .column(Column::initial(90.0).clip(true))
                                .column(Column::initial(150.0).clip(true))
                                .column(Column::initial(60.0))
                                .column(Column::initial(60.0))
                                .column(Column::initial(60.0))
                                .column(Column::remainder().at_least(120.0))
                                .header(22.0, |mut h| {
                                    for t in ["#", "Name", "Version", "Author", "Files", "Packs", "Loose", "Installed"] {
                                        h.col(|ui| {
                                            ui.label(RichText::new(t).strong());
                                        });
                                    }
                                })
                                .body(|mut body| {
                                    for (k, md) in m.mods.iter().enumerate() {
                                        body.row(24.0, |mut row| {
                                            row.set_selected(sel.as_deref() == Some(md.name.as_str()));
                                            row.col(|ui| {
                                                ui.label(RichText::new(format!("{}", k + 1)).color(p.muted));
                                            });
                                            row.col(|ui| {
                                                ui.label(RichText::new(&md.name).strong()).on_hover_text(if md.description.is_empty() { md.name.as_str() } else { md.description.as_str() });
                                            });
                                            row.col(|ui| {
                                                ui.label(&md.version);
                                            });
                                            row.col(|ui| {
                                                ui.label(&md.author);
                                            });
                                            row.col(|ui| {
                                                ui.label(md.files.len().to_string());
                                            });
                                            row.col(|ui| {
                                                ui.label(md.packs.len().to_string());
                                            });
                                            row.col(|ui| {
                                                ui.label(md.loose.len().to_string());
                                            });
                                            row.col(|ui| {
                                                ui.label(&md.installed);
                                            });
                                            if row.response().clicked() {
                                                clicked = Some(md.name.clone());
                                            }
                                        });
                                    }
                                });
                        });
                        if let Some(c) = clicked {
                            self.selected_mod = if self.selected_mod.as_ref() == Some(&c) { None } else { Some(c) };
                        }
                        ui.label(RichText::new("Install order: later mods win where two mods change the same file.").small().color(p.muted));
                    }
                    _ => {
                        ui.label(RichText::new("—").color(p.muted));
                    }
                }
            });
            ui.add_space(8.0);
            if let Some(report) = &self.conflicts {
                egui::CollapsingHeader::new("File ownership and conflicts").id_salt("mods_conflicts").show(ui, |ui| {
                    self.conflicts_view.show(ui, report);
                });
                ui.add_space(8.0);
            }
            // ---- install
            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                theme::section(ui, "Install a mod");
                ui.label(RichText::new("Drop a .mgsv package anywhere to review it, or choose a source below.").color(p.muted));
                if let Some(error) = &self.drop_error {
                    ui.colored_label(p.err, error);
                }
                ui.horizontal(|ui| {
                    let source_label = ui.label("Source");
                    let r = ui.add(egui::TextEdit::singleline(&mut s.install_source).hint_text(".mgsv file or staging folder (metadata.xml + Assets/ …)").desired_width(ui.available_width() - 230.0)).labelled_by(source_label.id);
                    let mut changed = r.changed();
                    if ui.button("Pick .mgsv…").clicked()
                        && let Some(file) = rfd::FileDialog::new().add_filter("MGSV mod", &["mgsv"]).pick_file()
                    {
                        s.install_source = file.display().to_string();
                        changed = true;
                    }
                    if ui.button("Pick folder…").clicked()
                        && let Some(folder) = rfd::FileDialog::new().set_title("Staging folder (metadata.xml)").pick_folder()
                    {
                        s.install_source = folder.display().to_string();
                        changed = true;
                    }
                    if changed {
                        self.sync_package(&ctx, PathBuf::from(s.install_source.trim()), true);
                    }
                });
                let source = PathBuf::from(s.install_source.trim());
                let pkg_name = self.package_name(&source).map(str::to_owned);
                if self.preview_after.is_some() {
                    ui.label(RichText::new("Waiting for the source path…").color(p.muted));
                }
                if let Some(preview) = &self.preview {
                    match (&preview.task, &preview.result) {
                        (Some(_), _) => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("Reading the package…");
                            });
                        }
                        (None, Some(Ok(name))) => {
                            let installed = matches!(&self.manifest, Some(Ok(m)) if m.mods.iter().any(|x| &x.name == name));
                            ui.horizontal(|ui| {
                                ui.label("Package:");
                                ui.label(RichText::new(name).strong());
                                if installed {
                                    theme::pill(ui, "installed", p.warn);
                                }
                            });
                        }
                        (None, Some(Err(e))) => {
                            ui.colored_label(p.err, e);
                        }
                        _ => {}
                    }
                }
                ui.horizontal(|ui| {
                    ui.checkbox(&mut s.install_replace, "Replace if already installed")
                        .on_hover_text("Swap the installed version for this one in one step, keeping its place in the install order");
                    let src = PathBuf::from(s.install_source.trim());
                    let ok = can_write && pkg_name.is_some();
                    if theme::primary_button(ui, "Install", ok).clicked()
                        && let Some(request) = self.current_request()
                    {
                        self.confirm = Some((request, Confirm::Install(src, s.install_replace, pkg_name.unwrap())));
                    }
                });
            });
            ui.add_space(8.0);
            // ---- pack
            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                theme::section(ui, "Build a .mgsv package");
                ui.label(RichText::new("From a staging folder (metadata.xml, Assets/… with the packs built, GameDir/… loose files). Deterministic: the same folder always gives the same bytes.").color(p.muted));
                ui.add_space(4.0);
                let field_w = (ui.available_width() - 230.0).max(200.0);
                egui::Grid::new("pack_grid").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("Staging folder");
                    ui.add(egui::TextEdit::singleline(&mut s.pack_stage).desired_width(field_w));
                    if ui.button("Browse…").clicked()
                        && let Some(folder) = rfd::FileDialog::new().set_title("Staging folder").pick_folder()
                    {
                        s.pack_stage = folder.display().to_string();
                    }
                    ui.end_row();
                    ui.label("Output .mgsv");
                    ui.add(egui::TextEdit::singleline(&mut s.pack_out).desired_width(field_w));
                    if ui.button("Save as…").clicked()
                        && let Some(mut file) = rfd::FileDialog::new().add_filter("MGSV mod", &["mgsv"]).save_file()
                    {
                        if file.extension().is_none() { file.set_extension("mgsv"); }
                        s.pack_out = file.display().to_string();
                    }
                    ui.end_row();
                    ui.label("Compression");
                    ui.add(egui::Slider::new(&mut s.pack_level, 0..=9).text("deflate level (MakeBite: 6)"));
                    ui.end_row();
                });
                let stage = PathBuf::from(s.pack_stage.trim());
                let out = PathBuf::from(s.pack_out.trim());
                let ready = stage.join("metadata.xml").is_file() && !s.pack_out.trim().is_empty();
                ui.horizontal(|ui| {
                    if ui.add_enabled(ready && !self.busy(), egui::Button::new("Build .mgsv")).clicked() {
                        self.start(&ctx, Path::new(""), Op::Pack { stage: stage.clone(), out: out.clone(), level: s.pack_level });
                    }
                    if !s.pack_stage.trim().is_empty() && !stage.join("metadata.xml").is_file() {
                        ui.colored_label(p.err, "no metadata.xml in the staging folder");
                    }
                });
            });
            ui.add_space(8.0);
            self.op_card(ui);
            ui.add_space(8.0);
            egui::CollapsingHeader::new("Advanced").id_salt("mods_advanced").show(ui, |ui| {
                ui.label("Vanilla pack folders (development): extracted game folders, each holding Assets/…, separated by ';'.                           Installs merge packs from these instead of the game's chunk archives (for test copies of a game without them).");
                ui.add(egui::TextEdit::singleline(&mut s.vanilla_dirs).hint_text("empty = the game folder's own archives").desired_width(f32::INFINITY));
            });
        });
        self.confirm_modal(&ctx, s, env);
    }

    fn preview_package(&mut self, ctx: &egui::Context, path: PathBuf) {
        if self.preview.as_ref().is_some_and(|preview| preview.source == path) {
            return;
        }
        if path.as_os_str().is_empty() {
            self.preview = None;
            return;
        }
        let p2 = path.clone();
        let t = Task::spawn("Read package", ctx, move |_| foxinstall::package_name(&p2));
        self.preview = Some(PackagePreview {
            source: path,
            task: Some(t),
            result: None,
        });
    }

    fn op_card(&mut self, ui: &mut egui::Ui) {
        let p = pal(ui);
        if let Some((op, t)) = &self.op {
            if *op != Op::List {
                theme::card(ui).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new(op.title()).strong());
                        ui.label(RichText::new(crate::build_view::fmt_dur(t.elapsed())).color(p.muted));
                    });
                    // live installer progress (install / uninstall / setup); other operations keep the spinner only
                    if t.progress >= 0.0 {
                        ui.add(
                            egui::ProgressBar::new(t.progress.clamp(0.0, 1.0))
                                .desired_width(ui.available_width().min(520.0))
                                .text(progress_text(&t.phase, t.progress, t.elapsed())),
                        );
                    }
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
                });
            }
            return;
        }
        let Some((op, r, el)) = &self.last else { return };
        theme::card(ui).show(ui, |ui| {
            if let Some(target) = &self.last_target {
                review_path(ui, "Destination", &target.game);
            }
            ui.set_width(ui.available_width());
            match r {
                Ok(res) => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("✔").color(p.ok));
                        ui.label(RichText::new(&res.summary).strong());
                        ui.label(RichText::new(format!("({})", crate::build_view::fmt_dur(*el))).color(p.muted));
                    });
                    if !res.log.is_empty() {
                        egui::CollapsingHeader::new(format!("{} log ({} lines)", op.title(), res.log.len()))
                            .id_salt("op_log")
                            .show(ui, |ui| {
                                for l in &res.log {
                                    ui.label(RichText::new(l).monospace().small());
                                }
                            });
                    }
                }
                Err(e) if is_game_running_error(e) => {
                    ui.label(RichText::new("The game is running").strong().color(p.warn));
                    ui.label("Close MGSV and try again: the installer never rewrites archives the game has open.");
                    ui.label(RichText::new(e.lines().next().unwrap_or("")).small().color(p.muted));
                }
                Err(e) => {
                    ui.label(RichText::new(format!("{} failed", op.title())).strong().color(p.err));
                    for (k, l) in e.lines().enumerate() {
                        let t = RichText::new(l).monospace().small();
                        ui.label(if k == 0 { t.color(p.err) } else { t });
                    }
                }
            }
        });
    }

    fn confirm_modal(&mut self, ctx: &egui::Context, s: &Settings, env: &Env) {
        let Some((request, c)) = self.confirm.clone() else {
            return;
        };
        let game = &request.target.game;
        let source = PathBuf::from(s.install_source.trim());
        if self.current_request().as_ref() != Some(&request)
            || matches!(&c, Confirm::Install(path, replace, name)
                if *path != source || *replace != s.install_replace || self.package_name(path) != Some(name.as_str()))
        {
            self.confirm = None;
            return;
        }
        let ready = !self.busy()
            && env.game_running.is_none()
            && looks_like_game(game)
            && self.loaded_for.as_ref() == Some(&request.target)
            && self.writes_ready()
            && (matches!(c, Confirm::Setup) || matches!(self.manifest, Some(Ok(_))));
        let mut decision: Option<bool> = None;
        egui::Modal::new(egui::Id::new("confirm_mod_op")).show(ctx, |ui| {
            let p = pal(ui);
            ui.set_min_width(460.0);
            ui.set_max_width(560.0);
            let (title, body, action) = match &c {
                Confirm::Uninstall(n) => (format!("Uninstall {n}?"), "Its files are removed from the archives and every file it replaced is restored exactly.".to_string(), "Uninstall"),
                Confirm::Setup => ("Set up this game folder?".to_string(), "The current 00.dat and 01.dat are kept as the base (00.dat.foxbase / 01.dat.foxbase, a one-time copy of a few GB). Make sure the game files are unmodified (verify them in Steam first).".to_string(), "Set up"),
                Confirm::Install(_, replace, name) => (format!("Install {name}?"), if *replace { "An installed mod of the same name is replaced in place (it keeps its position in the install order).".to_string() } else { "The mod is added on top of the installed ones.".to_string() }, "Install"),
            };
            ui.label(RichText::new(title).heading().strong());
            ui.add_space(6.0);
            ui.label(body);
            ui.add_space(4.0);
            if let Confirm::Install(src, _, _) = &c {
                review_path(ui, "Source", src);
            }
            review_path(ui, "Destination", game);
            review_path(ui, "Source install", &request.target.source_game);
            if let Some(profile) = &request.target.runtime_profile { review_path(ui, "Runtime profile", profile); }
            if let Some(running) = &env.game_running {
                ui.colored_label(p.warn, format!("{running} is running. Close the game before continuing."));
            } else if !ready {
                ui.colored_label(p.warn, "Wait for the destination to be ready before continuing.");
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    decision = Some(false);
                }
                if theme::primary_button(ui, action, ready).clicked() {
                    decision = Some(true);
                }
            });
        });
        match decision {
            Some(true) if ready => {
                self.confirm = None;
                let op = match c {
                    Confirm::Uninstall(name) => {
                        self.selected_mod = None;
                        Op::Uninstall { name }
                    }
                    Confirm::Setup => Op::Setup,
                    Confirm::Install(source, replace, _) => Op::Install { source, replace },
                };
                self.start(ctx, game, op);
            }
            Some(false) => self.confirm = None,
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "../tests/common/runtime_fixture.rs"]
mod runtime_fixture;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_running_message() {
        assert!(is_game_running_error("mgsvtpp.exe is running: close the game first"));
        assert!(is_game_running_error("the game is running"));
        assert!(!is_game_running_error("not set up: no x"));
    }

    #[test]
    fn op_kinds() {
        assert!(
            Op::Install {
                source: "a.mgsv".into(),
                replace: false
            }
            .writes_game()
        );
        assert!(Op::Uninstall { name: "x".into() }.writes_game());
        assert!(Op::Setup.writes_game());
        assert!(!Op::Verify.writes_game());
        assert!(!Op::List.writes_game());
        assert!(
            !Op::Pack {
                stage: "s".into(),
                out: "o.mgsv".into(),
                level: 6
            }
            .writes_game()
        );
    }

    fn rep(phase: &str, overall: f32, done: bool) -> foxinstall::Progress {
        foxinstall::Progress {
            op: "install",
            phase: phase.into(),
            fraction: None,
            overall,
            done,
            ok: done,
        }
    }

    #[test]
    fn progress_gate_throttles() {
        let mut g = ProgressGate::default();
        assert!(g.pass(&rep("write 00.dat", 0.30, false)).is_some()); // first
        assert!(g.pass(&rep("write 00.dat", 0.305, false)).is_none()); // < 1 %
        assert!(g.pass(&rep("write 00.dat", 0.315, false)).is_some()); // 1 % step
        assert!(g.pass(&rep("write 01.dat", 0.316, false)).is_some()); // phase change
        assert_eq!(g.pass(&rep("done", 1.0, true)), Some((1.0, "done".into()))); // final
    }

    #[test]
    fn progress_text_eta() {
        use std::time::Duration;
        assert_eq!(
            progress_text("vanilla tables", 0.05, Duration::from_secs(1)),
            "vanilla tables  5 %"
        );
        let t = progress_text("write 00.dat", 0.5, Duration::from_secs(10));
        assert!(t.starts_with("write 00.dat  50 %  (about 10.0 s left)"), "{t}");
        assert_eq!(progress_text("done", 1.0, Duration::from_secs(20)), "done  100 %");
    }

    /// the Mods view's worker path: run_op_with passes the installer's reports through (setup, install, uninstall)
    #[test]
    fn run_op_reports_progress() {
        use std::sync::{Arc, Mutex};
        let root = std::env::temp_dir().join(format!("foxstudio_progress_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let game = root.join("game");
        let profile = runtime_fixture::write_game(&game, 0);
        let profile_path = root.join("cache/runtime_profile.json");
        profile.save(&profile_path).unwrap();
        let target = ModTarget {
            game: game.clone(),
            source_game: game,
            runtime_profile: Some(profile_path),
        };
        let stage = root.join("stage");
        std::fs::create_dir_all(stage.join("Assets/tpp/script/t")).unwrap();
        std::fs::write(
            stage.join("metadata.xml"),
            r#"<?xml version="1.0" encoding="utf-8"?>
<ModEntry Name="gui progress" Version="1" Author="t" Website=""><Description>t</Description></ModEntry>"#,
        )
        .unwrap();
        std::fs::write(stage.join("Assets/tpp/script/t/a.lua"), b"print(1)").unwrap();
        for op in [
            Op::Setup,
            Op::Install {
                source: stage.clone(),
                replace: false,
            },
            Op::Uninstall {
                name: "gui progress".into(),
            },
        ] {
            let seen: Arc<Mutex<Vec<(f32, String)>>> = Arc::default();
            let s2 = seen.clone();
            let mut gate = ProgressGate::default();
            let cb: foxinstall::ProgressFn = Box::new(move |p| {
                if let Some(x) = gate.pass(p) {
                    s2.lock().unwrap().push(x);
                }
            });
            let r = run_op_for_target(&target, &[], &op, Some(cb)).unwrap();
            assert!(r.ok, "{op:?}: {r:?}");
            let v = seen.lock().unwrap();
            assert!(v.len() >= 3, "{op:?}: {v:?}");
            assert!(v.windows(2).all(|w| w[1].0 >= w[0].0), "{op:?}: {v:?}");
            assert_eq!(v.last().unwrap(), &(1.0, "done".to_string()), "{op:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    fn poll_until(view: &mut ModsView, finished: impl Fn(&ModsView) -> bool) {
        let started = std::time::Instant::now();
        while !finished(view) {
            view.poll();
            assert!(
                started.elapsed() < std::time::Duration::from_secs(5),
                "delayed fixture worker did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn target_settings(game: &str, profile: &str) -> Settings {
        Settings {
            game_dir: game.into(),
            runtime_profile: profile.into(),
            ..Default::default()
        }
    }

    #[test]
    fn mods_profile_ignores_old_validation_after_a_b_a() {
        let context = egui::Context::default();
        let a = target_settings("missing-a", "missing-a/profile.json");
        let b = target_settings("missing-b", "missing-b/profile.json");
        let target = ModTarget::from_settings(&a).unwrap();
        let request = ProfileRequest {
            target: target.clone(),
            generation: 1,
        };
        let (release, wait) = std::sync::mpsc::channel();
        let task = Task::spawn_serial("delayed validation", &context, move |_| {
            wait.recv().unwrap();
            Ok(ProfileValidation::default())
        });
        let mut view = ModsView {
            target: Some(target),
            generation: 1,
            profile_task: Some((request, task)),
            ..Default::default()
        };
        view.sync_target(&context, &b);
        view.sync_target(&context, &a);
        assert_eq!(view.generation, 3);
        release.send(()).unwrap();
        poll_until(&mut view, |view| view.profile_task.is_none());
        assert!(
            view.profile.is_none(),
            "old success must not validate the new A selection"
        );
    }

    #[test]
    fn mods_profile_stale_operation_keeps_destination_and_never_replaces_manifest() {
        let context = egui::Context::default();
        let a = target_settings("missing-a", "missing-a/profile.json");
        let b = target_settings("missing-b", "missing-b/profile.json");
        let target = ModTarget::from_settings(&a).unwrap();
        let request = ProfileRequest {
            target: target.clone(),
            generation: 1,
        };
        let (release, wait) = std::sync::mpsc::channel();
        // A mock completion only: no installer or archive mutation is invoked.
        let task = Task::spawn_serial("delayed operation", &context, move |_| {
            wait.recv().unwrap();
            Ok(OpResult {
                ok: true,
                summary: "mock result".into(),
                manifest: Some(Manifest {
                    tool: "previous install".into(),
                    ..Default::default()
                }),
                ..Default::default()
            })
        });
        let mut view = ModsView {
            target: Some(target.clone()),
            generation: 1,
            op_request: Some(request),
            op: Some((
                Op::Install {
                    source: "mock.mgsv".into(),
                    replace: false,
                },
                task,
            )),
            ..Default::default()
        };
        view.sync_target(&context, &b);
        release.send(()).unwrap();
        poll_until(&mut view, |view| !view.busy());
        assert!(view.manifest.is_none());
        assert!(view.conflicts.is_none());
        assert_eq!(view.last_target, Some(target));
        assert_eq!(view.last.as_ref().unwrap().1.as_ref().unwrap().summary, "mock result");
    }

    #[test]
    fn mods_profile_source_change_invalidates_review_and_ownership_selection() {
        let context = egui::Context::default();
        let settings = target_settings("missing-a", "profile.json");
        let target = ModTarget::from_settings(&settings).unwrap();
        let request = ProfileRequest {
            target: target.clone(),
            generation: 1,
        };
        let mut view = ModsView {
            target: Some(target.clone()),
            generation: 1,
            loaded_for: Some(target),
            profile: Some(Ok(ProfileValidation::default())),
            confirm: Some((request, Confirm::Setup)),
            conflicts: Some(ConflictReport::default()),
            selected_mod: Some("previous".into()),
            ..Default::default()
        };
        let changed = Settings {
            game_install: "another-source".into(),
            ..settings
        };
        view.sync_target(&context, &changed);
        assert!(view.confirm.is_none());
        assert!(!view.profile_ready());
        assert!(view.manifest.is_none());
        assert!(view.conflicts.is_none());
        assert!(view.selected_mod.is_none());
    }
}
