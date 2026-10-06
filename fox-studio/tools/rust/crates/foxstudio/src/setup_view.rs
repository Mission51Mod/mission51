//! The Setup tab: a four-step first-run wizard (PLAN.md §4.4).
//!   1 Game          find MGSV:TPP through Steam (read only) or pick its folder
//!   2 Test install  the game folder the Mods tab installs into (a copy is recommended)
//!   3 Unpack        cache folder, archive survey with size estimate, name dictionary, unpack / index
//!   4 Check         prerequisites, then finish
//! Every scan, survey, unpack and check runs on a worker; the game folder is never written here.
use crate::settings::{Settings, Tab};
use crate::setup::{self, ArchiveSurvey, Check, GameCheck, Level, PrereqInputs, UnpackReport};
use crate::steam::{self, Candidate};
use crate::tasks::Task;
use crate::theme::{self, pal};
use eframe::egui::{self, Align, Layout, RichText};
use egui_extras::{Column, TableBuilder};
use foxcore::runtime_data::RuntimeProfile;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Game,
    TestInstall,
    Unpack,
    Check,
}

impl Step {
    pub const ALL: [Step; 4] = [Step::Game, Step::TestInstall, Step::Unpack, Step::Check];
    pub fn title(self) -> &'static str {
        match self {
            Step::Game => "Find the game",
            Step::TestInstall => "Mod test install",
            Step::Unpack => "Prepare game data",
            Step::Check => "Check and finish",
        }
    }
}

/// what the view needs from the app
pub struct Env {
    pub fox_exe: PathBuf,
    pub python: String,
    pub gpu: Option<String>,
    pub game_running: Option<String>,
    /// The selected build workflow includes legacy Python stages.
    pub needs_python: bool,
}

/// a value computed on a worker for a key (re-run when the key changes)
struct Keyed<T> {
    key: String,
    task: Option<Task<T>>,
    task_key: String,
    generation: u64,
    task_generation: u64,
    value: Option<Result<T, String>>,
}

impl<T: Send + 'static> Keyed<T> {
    fn new() -> Self {
        Keyed {
            key: String::new(),
            task: None,
            task_key: String::new(),
            generation: 0,
            task_generation: 0,
            value: None,
        }
    }
    fn ensure(
        &mut self,
        key: &str,
        ctx: &egui::Context,
        label: &str,
        f: impl FnOnce() -> T + Send + 'static,
    ) {
        if self.key != key {
            self.key = key.to_string();
            self.generation = self.generation.wrapping_add(1);
            self.value = None;
        }
        if self.task.is_some() || self.value.is_some() {
            return;
        }
        self.task_key = key.to_string();
        self.task_generation = self.generation;
        self.task = Some(Task::spawn_serial(label, ctx, move |_| Ok(f())));
    }
    fn poll(&mut self) {
        if let Some(t) = self.task.as_mut()
            && t.poll()
        {
            let result = t.take_result();
            if self.task_key == self.key && self.task_generation == self.generation {
                self.value = result;
            }
            self.task = None;
        }
    }
    fn get(&self) -> Option<&T> {
        self.value.as_ref().and_then(|v| v.as_ref().ok())
    }
    fn busy(&self) -> bool {
        self.task.is_some()
    }
    fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.key.clear();
        self.value = None;
    }
}

/// A worker is bound to the paths reviewed when it started, never to later settings.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ProfileTarget {
    game: PathBuf,
    path: PathBuf,
    cache: Option<PathBuf>,
}

impl ProfileTarget {
    fn from_settings(settings: &Settings) -> Option<Self> {
        (!settings.game_install.trim().is_empty()).then_some(())?;
        Some(Self {
            game: PathBuf::from(settings.game_install.trim()),
            path: settings.resolved_runtime_profile()?,
            cache: (!settings.cache_dir.trim().is_empty())
                .then(|| PathBuf::from(settings.cache_dir.trim())),
        })
    }
}

struct SelectedTask<T> {
    target: ProfileTarget,
    generation: u64,
    task: Task<T>,
    cancel: Arc<AtomicBool>,
}

pub struct SetupView {
    pub step: Step,
    profile_target: Option<ProfileTarget>,
    profile_generation: u64,
    profile_task: Option<SelectedTask<RuntimeProfile>>,
    profile_result: Option<Result<RuntimeProfile, String>>,
    scan: Keyed<Vec<Candidate>>,
    game_check: Keyed<GameCheck>,
    test_check: Keyed<GameCheck>,
    survey: Keyed<Vec<ArchiveSurvey>>,
    prereqs: Keyed<Vec<Check>>,
    unpack: Option<SelectedTask<UnpackReport>>,
    pub unpack_result: Option<Result<UnpackReport, String>>,
    dict: Option<SelectedTask<String>>,
    pub dict_result: Option<Result<String, String>>,
    free: Keyed<Option<u64>>,
}

impl Default for SetupView {
    fn default() -> Self {
        SetupView {
            step: Step::Game,
            profile_target: None,
            profile_generation: 0,
            profile_task: None,
            profile_result: None,
            scan: Keyed::new(),
            game_check: Keyed::new(),
            test_check: Keyed::new(),
            survey: Keyed::new(),
            prereqs: Keyed::new(),
            unpack: None,
            unpack_result: None,
            dict: None,
            dict_result: None,
            free: Keyed::new(),
        }
    }
}

fn gb(b: u64) -> String {
    if b >= 1_000_000_000 {
        format!("{:.2} GB", b as f64 / 1e9)
    } else {
        format!("{:.0} MB", b as f64 / 1e6)
    }
}

impl SetupView {
    pub fn busy(&self) -> bool {
        self.profile_task.is_some() || self.unpack.is_some() || self.dict.is_some()
    }

    /// the Steam scan result (tests)
    pub fn candidates(&self) -> Option<&Vec<Candidate>> {
        self.scan.get()
    }

    pub fn surveys(&self) -> Option<&Vec<ArchiveSurvey>> {
        self.survey.get()
    }

    /// the check of the game folder in use (tests)
    pub fn game_check(&self) -> Option<&GameCheck> {
        self.game_check.get()
    }

    pub fn checks(&self) -> Option<&Vec<Check>> {
        self.prereqs.get()
    }

    pub fn poll(&mut self) {
        self.scan.poll();
        self.game_check.poll();
        self.test_check.poll();
        self.survey.poll();
        self.prereqs.poll();
        self.free.poll();
        if let Some(worker) = self.profile_task.as_mut()
            && worker.task.poll()
        {
            let result = worker.task.take_result();
            if self.profile_target.as_ref() == Some(&worker.target)
                && worker.generation == self.profile_generation
            {
                self.profile_result = result;
                self.prereqs.reset();
                self.survey.reset();
            }
            self.profile_task = None;
        }
        if let Some(worker) = self.unpack.as_mut()
            && worker.task.poll()
        {
            let result = worker.task.take_result();
            if self.profile_target.as_ref() == Some(&worker.target)
                && worker.generation == self.profile_generation
            {
                self.unpack_result = result;
            }
            self.unpack = None;
        }
        if let Some(worker) = self.dict.as_mut()
            && worker.task.poll()
        {
            let result = worker.task.take_result();
            if self.profile_target.as_ref() == Some(&worker.target)
                && worker.generation == self.profile_generation
            {
                self.dict_result = result;
            }
            self.dict = None;
        }
    }

    pub fn profile_ready(&self) -> bool {
        self.profile_result.as_ref().is_some_and(Result::is_ok)
    }

    pub fn profile_error(&self) -> Option<&str> {
        self.profile_result
            .as_ref()
            .and_then(|result| result.as_ref().err())
            .map(String::as_str)
    }

    fn select_profile(&mut self, target: Option<ProfileTarget>) {
        if self.profile_target == target {
            return;
        }
        self.profile_target = target;
        self.profile_generation = self.profile_generation.wrapping_add(1);
        self.profile_result = None;
        self.survey.reset();
        self.prereqs.reset();
        self.unpack_result = None;
        self.dict_result = None;
        if let Some(worker) = &self.profile_task {
            worker.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(worker) = &self.unpack {
            worker.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(worker) = &self.dict {
            worker.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn sync_profile(&mut self, settings: &Settings, ctx: &egui::Context) {
        self.select_profile(ProfileTarget::from_settings(settings));
        if self.busy() || self.profile_result.is_some() {
            return;
        }
        let Some(target) = self.profile_target.clone() else {
            return;
        };
        let captured = target.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        self.profile_task = Some(SelectedTask {
            target,
            generation: self.profile_generation,
            cancel,
            task: Task::spawn_serial("Checking local game data", ctx, move |_| {
                setup::check_cache(&captured.path, Some(&captured.game))?;
                let profile = setup::load_profile(&captured.game, &captured.path)?;
                if worker_cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".into());
                }
                Ok(profile)
            }),
        });
    }

    /// Explicit preparation action, also exposed for headless setup fixtures.
    pub fn prepare_profile(
        &mut self,
        settings: &Settings,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        self.select_profile(ProfileTarget::from_settings(settings));
        if self.busy() {
            return Err("wait for the current setup operation to finish".into());
        }
        let target = self
            .profile_target
            .clone()
            .ok_or("choose the game and a local metadata file first")?;
        setup::check_cache(&target.path, Some(&target.game))?;
        let captured = target.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        self.profile_result = None;
        self.profile_task = Some(SelectedTask {
            target,
            generation: self.profile_generation,
            cancel,
            task: Task::spawn_serial("Preparing local game data", ctx, move |task| {
                setup::prepare_profile(
                    &captured.game,
                    &captured.path,
                    &worker_cancel,
                    &mut |fraction, phase| task.progress(fraction, phase),
                )
            }),
        });
        Ok(())
    }

    /// start the Steam scan (also used by tests)
    pub fn scan(&mut self, ctx: &egui::Context) {
        self.scan.reset();
        self.scan.ensure("scan", ctx, "Finding the game", || {
            let roots = steam::steam_roots();
            let libs = steam::libraries(&roots);
            steam::find_game(&libs, true)
        });
    }

    pub fn show(&mut self, ui: &mut egui::Ui, s: &mut Settings, env: &Env) {
        let ctx = ui.ctx().clone();
        let p = pal(ui);
        self.sync_profile(s, &ctx);
        // background checks for the paths in use
        let game = s.game_install.trim().to_string();
        if !game.is_empty() {
            let g = PathBuf::from(&game);
            self.game_check
                .ensure(&game, &ctx, "Checking the game folder", move || {
                    setup::check_game(&g)
                });
        }
        let test = s.game_dir.trim().to_string();
        if !test.is_empty() {
            let g = PathBuf::from(&test);
            self.test_check
                .ensure(&test, &ctx, "Checking the test install", move || {
                    setup::check_game(&g)
                });
        }
        let cache = s.cache_dir.trim().to_string();
        if !cache.is_empty() {
            let c = PathBuf::from(&cache);
            self.free
                .ensure(&cache, &ctx, "Free space", move || setup::free_space(&c));
        }

        egui::Panel::left("setup_steps").resizable(false).exact_size(210.0).show(ui, |ui| {
            ui.add_space(4.0);
            ui.heading("Setup");
            ui.label(RichText::new("First run: find the game, choose where mods are tested and where vanilla data is unpacked.").small().color(p.muted));
            ui.add_space(10.0);
            for (i, st) in Step::ALL.iter().enumerate() {
                let done = self.step_done(*st, s);
                let sel = self.step == *st;
                ui.horizontal(|ui| {
                    let (c, mark) = if done { (p.ok, "●") } else if sel { (p.accent, "●") } else { (p.muted, "○") };
                    ui.label(RichText::new(mark).color(c));
                    let t = format!("{}  {}", i + 1, st.title());
                    let text = if sel { RichText::new(t).strong().color(p.accent) } else { RichText::new(t) };
                    if ui.selectable_label(sel, text).clicked() {
                        self.step = *st;
                    }
                });
            }
            ui.add_space(12.0);
            if s.setup_done {
                theme::pill(ui, "setup finished", p.ok);
            }
        });
        egui::CentralPanel::no_frame().show(ui, |ui| {
            egui::Panel::bottom("setup_nav").show(ui, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let k = Step::ALL.iter().position(|x| *x == self.step).unwrap_or(0);
                    if ui.add_enabled(k > 0, egui::Button::new("Back")).clicked() {
                        self.step = Step::ALL[k - 1];
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if k + 1 < Step::ALL.len() {
                            if theme::primary_button(ui, "Next", true).clicked() {
                                self.step = Step::ALL[k + 1];
                            }
                        } else {
                            if theme::primary_button(
                                ui,
                                "Finish setup",
                                !self.busy() && self.step_done(Step::Check, s),
                            )
                            .clicked()
                            {
                                s.setup_done = true;
                                s.last_tab = Tab::Project;
                            }
                        }
                        if !s.setup_done
                            && ui
                                .button("Skip for now")
                                .on_hover_text("Set things up later from this tab")
                                .clicked()
                        {
                            s.setup_done = true;
                            s.last_tab = Tab::Project;
                        }
                    });
                });
                ui.add_space(4.0);
            });
            egui::ScrollArea::vertical()
                .id_salt("setup_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(4.0);
                    match self.step {
                        Step::Game => self.step_game(ui, s),
                        Step::TestInstall => self.step_test(ui, s, env),
                        Step::Unpack => self.step_unpack(ui, s, env),
                        Step::Check => self.step_check(ui, s, env),
                    }
                });
        });
    }

    fn step_done(&self, st: Step, s: &Settings) -> bool {
        match st {
            Step::Game => {
                self.game_check.get().is_some_and(|g| g.ok()) && !s.game_install.trim().is_empty()
            }
            Step::TestInstall => {
                self.test_check.get().is_some_and(|g| g.master00) && !s.game_dir.trim().is_empty()
            }
            Step::Unpack => !s.cache_dir.trim().is_empty() && self.profile_ready(),
            Step::Check => {
                self.profile_ready()
                    && self
                        .prereqs
                        .get()
                        .is_some_and(|c| c.iter().all(|x| x.level != Level::Fail))
            }
        }
    }

    fn step_game(&mut self, ui: &mut egui::Ui, s: &mut Settings) {
        let ctx = ui.ctx().clone();
        let p = pal(ui);
        if self.scan.key.is_empty() && s.game_install.trim().is_empty() {
            self.scan(&ctx);
        }
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::section(ui, "Find MGSV:TPP");
            ui.label("Fox Studio reads your game's archives to unpack the vanilla data your projects build on. It only ever reads this folder.");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.scan.busy(), egui::Button::new("Scan Steam libraries")).clicked() {
                    self.scan(&ctx);
                }
                if self.scan.busy() {
                    ui.spinner();
                    ui.label(RichText::new("looking in the Steam libraries…").color(p.muted));
                }
            });
            if let Some(c) = self.scan.get().cloned() {
                ui.add_space(4.0);
                if c.is_empty() {
                    ui.label(RichText::new("No Steam install of MGSV:TPP found. Pick its folder below.").color(p.warn));
                }
                for cand in &c {
                    let sel = steam::same_path(Path::new(s.game_install.trim()), &cand.dir);
                    ui.horizontal(|ui| {
                        if ui.radio(sel, RichText::new(cand.dir.display().to_string()).monospace()).clicked() {
                            s.game_install = cand.dir.display().to_string();
                            s.game_buildid = cand.manifest.as_ref().and_then(|m| m.buildid);
                        }
                        if let Some(b) = cand.manifest.as_ref().and_then(|m| m.buildid) {
                            theme::pill(ui, &format!("build {b}"), p.info);
                        }
                        theme::pill(ui, if cand.source == steam::Source::Manifest { "Steam manifest" } else { "found on drive" }, p.muted);
                        if !cand.has_exe {
                            theme::pill(ui, "mgsvtpp.exe missing", p.err);
                        }
                    });
                }
                if c.len() == 1 && s.game_install.trim().is_empty() {
                    s.game_install = c[0].dir.display().to_string();
                    s.game_buildid = c[0].manifest.as_ref().and_then(|m| m.buildid);
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Game folder");
                ui.add(egui::TextEdit::singleline(&mut s.game_install).hint_text("…\\steamapps\\common\\MGS_TPP").desired_width(ui.available_width() - 90.0));
                if ui.button("Browse…").clicked()
                    && let Some(d) = rfd::FileDialog::new().set_title("MGSV:TPP game folder (read only)").pick_folder()
                {
                    s.game_install = d.display().to_string();
                    s.game_buildid = None;
                }
            });
            ui.add_space(4.0);
            game_check_lines(ui, &self.game_check, s.game_install.trim());
        });
    }

    fn step_test(&mut self, ui: &mut egui::Ui, s: &mut Settings, env: &Env) {
        let p = pal(ui);
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::section(ui, "Choose the mod test install");
            ui.label("The Mods tab installs into this game folder. Use a copy of the game for testing (the installer keeps a backup of the base archives and can restore them, but a copy keeps your real install untouched).");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Test install");
                ui.add(egui::TextEdit::singleline(&mut s.game_dir).hint_text("a copy of MGS_TPP").desired_width(ui.available_width() - 90.0));
                if ui.button("Browse…").clicked()
                    && let Some(d) = rfd::FileDialog::new().set_title("Game folder for mod tests").pick_folder()
                {
                    s.game_dir = d.display().to_string();
                }
            });
            if !s.game_install.trim().is_empty() && ui.small_button("Use the Steam install (not recommended)").clicked() {
                s.game_dir = s.game_install.clone();
            }
            ui.add_space(4.0);
            game_check_lines(ui, &self.test_check, s.game_dir.trim());
            if !s.game_dir.trim().is_empty() && steam::same_path(Path::new(s.game_dir.trim()), Path::new(s.game_install.trim())) {
                ui.colored_label(p.warn, "This is the Steam install itself. Mods will be installed into your real game.");
            }
            if let Some(g) = &env.game_running {
                ui.colored_label(p.warn, format!("{g} is running: installs wait until it closes."));
            }
        });
    }

    fn step_unpack(&mut self, ui: &mut egui::Ui, s: &mut Settings, _env: &Env) {
        let ctx = ui.ctx().clone();
        let p = pal(ui);
        let game = PathBuf::from(s.game_install.trim());
        let have_game =
            self.game_check.get().is_some_and(|g| g.ok()) && !s.game_install.trim().is_empty();
        // Survey only with metadata validated for the captured install.
        if have_game && self.profile_ready() && !self.busy() {
            let archives: Vec<PathBuf> = self
                .game_check
                .get()
                .map(|g| g.archives.iter().map(|a| a.0.clone()).collect())
                .unwrap_or_default();
            let target = self.profile_target.clone().unwrap();
            let key = format!("{target:?}");
            self.survey
                .ensure(
                    &key,
                    &ctx,
                    "Reading archive tables",
                    move || match setup::load_profile(&target.game, &target.path) {
                        Ok(profile) => {
                            let context = profile.qar_context();
                            archives
                                .iter()
                                .map(|a| setup::survey_with_context(&context, &target.game, a))
                                .collect()
                        }
                        Err(error) => archives
                            .iter()
                            .map(|a| ArchiveSurvey {
                                path: a.clone(),
                                id: setup::archive_id(&target.game, a),
                                error: Some(error.clone()),
                                ..Default::default()
                            })
                            .collect(),
                    },
                );
        }
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::section(ui, "Cache folder");
            ui.label("Vanilla files are unpacked here (one folder per archive, plus an index). Pick a drive with room; it never goes into the game folder.");
            ui.horizontal(|ui| {
                ui.label("Cache");
                let r = ui.add(egui::TextEdit::singleline(&mut s.cache_dir).hint_text("e.g. H:\\fox-cache").desired_width(ui.available_width() - 90.0));
                if r.changed() {
                    self.free.reset();
                }
                if ui.button("Browse…").clicked()
                    && let Some(d) = rfd::FileDialog::new().set_title("Cache folder for unpacked vanilla data").pick_folder()
                {
                    s.cache_dir = d.display().to_string();
                    self.free.reset();
                }
            });
            let cache = PathBuf::from(s.cache_dir.trim());
            if !s.cache_dir.trim().is_empty() {
                match setup::check_cache(&cache, have_game.then_some(game.as_path())) {
                    Err(e) => {
                        ui.colored_label(p.err, e);
                    }
                    Ok(()) => {
                        if let Some(Some(f)) = self.free.get() {
                            ui.label(RichText::new(format!("{} free on that drive", gb(*f))).color(p.muted));
                        }
                    }
                }
            }
        });
        ui.add_space(8.0);
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::section(ui, "Local game data");
            ui.label("Prepare the metadata needed to read and rebuild your own archives. This reads the game and saves a small local file.");
            ui.horizontal(|ui| {
                ui.label("Metadata file");
                let hint = s.resolved_runtime_profile().map(|path| path.display().to_string()).unwrap_or_else(|| "choose a cache folder first".into());
                ui.add(egui::TextEdit::singleline(&mut s.runtime_profile).hint_text(hint).desired_width(ui.available_width()));
            });
            if let Some(worker) = &self.profile_task {
                ui.add(egui::ProgressBar::new(worker.task.progress.max(0.0)).show_percentage().text(worker.task.phase.clone()));
                if ui.button("Cancel preparation").clicked() { worker.cancel.store(true, Ordering::Relaxed); }
            } else {
                if self.profile_ready() { ui.colored_label(p.ok, "Local game data is ready."); }
                else if let Some(error) = self.profile_error() { ui.colored_label(p.err, error); }
                ui.horizontal(|ui| {
                    let can = have_game && s.resolved_runtime_profile().is_some() && !self.busy();
                    if ui.add_enabled(can, egui::Button::new("Prepare game data")).clicked()
                        && let Err(error) = self.prepare_profile(s, &ctx)
                    {
                        self.profile_result = Some(Err(error));
                    }
                    if ui.add_enabled(can, egui::Button::new("Reload metadata")).clicked() {
                        self.profile_result = None;
                        self.survey.reset();
                        self.prereqs.reset();
                    }
                });
            }
        });
        // Path fields above may have changed during this frame. Rebind before any archive action.
        self.sync_profile(s, &ctx);
        ui.add_space(8.0);
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::section(ui, "Archives");
            if !have_game {
                ui.label(RichText::new("Find the game first (step 1).").color(p.muted));
                return;
            }
            if !self.profile_ready() {
                ui.label(
                    RichText::new("Prepare game data above to read the archive tables.")
                        .color(p.muted),
                );
                return;
            }
            if self.survey.busy() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Reading the archive tables (sizes only)…");
                });
                return;
            }
            let Some(sv) = self.survey.get().cloned() else {
                return;
            };
            if s.unpack_select.is_empty() {
                // a sensible default: the data archive and the first chunk
                s.unpack_select = sv
                    .iter()
                    .filter(|a| a.id == "data1" || a.id == "chunk0")
                    .map(|a| a.id.clone())
                    .collect();
            }
            let row_h = 22.0;
            TableBuilder::new(ui)
                .id_salt("archive_table")
                .striped(true)
                .auto_shrink([false, true])
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::exact(24.0))
                .column(Column::initial(140.0).at_least(90.0))
                .column(Column::initial(90.0))
                .column(Column::initial(80.0))
                .column(Column::initial(110.0))
                .column(Column::remainder())
                .header(20.0, |mut h| {
                    for t in ["", "Archive", "File", "Entries", "Unpacked", ""] {
                        h.col(|ui| {
                            ui.label(RichText::new(t).strong());
                        });
                    }
                })
                .body(|body| {
                    body.rows(row_h, sv.len(), |mut row| {
                        let a = &sv[row.index()];
                        let mut on = s.unpack_select.contains(&a.id);
                        row.col(|ui| {
                            if ui.checkbox(&mut on, "").changed() {
                                if on {
                                    s.unpack_select.push(a.id.clone());
                                } else {
                                    s.unpack_select.retain(|x| x != &a.id);
                                }
                            }
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(&a.id).monospace());
                        });
                        row.col(|ui| {
                            ui.label(gb(a.bytes));
                        });
                        row.col(|ui| {
                            ui.label(a.entries.to_string());
                        });
                        row.col(|ui| {
                            ui.label(gb(a.content_bytes));
                        });
                        row.col(|ui| {
                            if let Some(e) = &a.error {
                                ui.colored_label(p.err, e);
                            }
                        });
                    });
                });
            let need: u64 = sv
                .iter()
                .filter(|a| s.unpack_select.contains(&a.id))
                .map(|a| a.content_bytes)
                .sum();
            let free = self.free.get().copied().flatten();
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(format!(
                    "Selected: {} archive(s), about {} unpacked",
                    s.unpack_select.len(),
                    gb(need)
                ));
                if let Some(f) = free
                    && f < need
                {
                    theme::pill(ui, &format!("only {} free", gb(f)), p.err);
                }
            });
        });
        ui.add_space(8.0);
        // ---- name dictionary
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::section(ui, "Name dictionary");
            ui.label("Archives store hashes, not file names. A dictionary built from your own game names most files; without one they are named by hash.");
            let dict = s.resolved_dict();
            ui.horizontal(|ui| {
                ui.label("Dictionary");
                ui.add(egui::TextEdit::singleline(&mut s.dict_file).hint_text(dict.as_ref().map(|d| d.display().to_string()).unwrap_or_else(|| "<cache>\\dictionary.txt".into()))
                    .desired_width(ui.available_width() - 90.0));
                if ui.button("Browse…").clicked()
                    && let Some(f) = rfd::FileDialog::new().add_filter("text", &["txt"]).pick_file()
                {
                    s.dict_file = f.display().to_string();
                }
            });
            let exists = dict.as_ref().is_some_and(|d| d.is_file());
            ui.horizontal(|ui| {
                if exists {
                    theme::pill(ui, "dictionary found", p.ok);
                } else {
                    theme::pill(ui, "no dictionary yet", p.warn);
                }
                let can = have_game && self.profile_ready() && dict.is_some() && !self.busy();
                if ui.add_enabled(can, egui::Button::new("Build from your game")).on_hover_text("Reads every archive and the game exe for file names (several minutes); writes only the dictionary file").clicked() {
                    let cancel = Arc::new(AtomicBool::new(false));
                    let c2 = cancel.clone();
                    let (g, out) = (game.clone(), dict.clone().unwrap());
                    let archives: Vec<PathBuf> = self.game_check.get().map(|gc| gc.archives.iter().map(|a| a.0.clone()).collect()).unwrap_or_default();
                    self.dict_result = None;
                    let target = self.profile_target.clone().unwrap();
                    let profile_path = target.path.clone();
                    self.dict = Some(SelectedTask { generation: self.profile_generation, cancel, target, task: Task::spawn_serial("Building the name dictionary", &ctx, move |t| {
                        let profile = setup::load_profile(&g, &profile_path)?;
                        setup::build_dictionary_with_context(&profile.qar_context(), &g, &archives, &out, &c2, &mut |f, txt| t.progress(f, txt))
                    }) });
                }
                if let Some(worker) = &self.dict
                    && ui.button("Cancel").clicked()
                {
                    worker.cancel.store(true, Ordering::Relaxed);
                }
            });
            if let Some(worker) = &self.dict {
                ui.add(egui::ProgressBar::new(worker.task.progress.max(0.0)).show_percentage().text(worker.task.phase.clone()));
            }
            if let Some(r) = &self.dict_result {
                match r {
                    Ok(m) => ui.colored_label(p.ok, m),
                    Err(e) => ui.colored_label(p.err, e),
                };
            }
        });
        ui.add_space(8.0);
        // ---- unpack
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::section(ui, "Unpack and index");
            let cache = PathBuf::from(s.cache_dir.trim());
            let cache_ok = !s.cache_dir.trim().is_empty() && setup::check_cache(&cache, have_game.then_some(game.as_path())).is_ok();
            let archives: Vec<PathBuf> = self
                .survey
                .get()
                .map(|sv| sv.iter().filter(|a| s.unpack_select.contains(&a.id) && a.error.is_none()).map(|a| a.path.clone()).collect())
                .unwrap_or_default();
            let can_unpack = have_game && self.profile_ready() && cache_ok && !archives.is_empty() && !self.busy();
            match &mut self.unpack {
                Some(worker) => {
                    let t = &worker.task;
                    let frac = t.progress.max(0.0);
                    ui.add(egui::ProgressBar::new(frac).show_percentage().text(t.phase.clone()));
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{} elapsed", crate::build_view::fmt_dur(t.elapsed()))).color(p.muted));
                        if ui.button("Cancel").on_hover_text("Stop after the current file; unpacking again resumes").clicked() {
                            worker.cancel.store(true, Ordering::Relaxed);
                        }
                    });
                }
                None => {
                    let can = can_unpack;
                    if theme::primary_button(ui, &format!("Unpack {} archive(s)", archives.len()), can).clicked() {
                        let cancel = Arc::new(AtomicBool::new(false));
                        let c2 = cancel.clone();
                        let dict_path = s.resolved_dict();
                        let g = game.clone();
                        self.unpack_result = None;
                        let target = self.profile_target.clone().unwrap();
                        let profile_path = target.path.clone();
                        self.unpack = Some(SelectedTask { generation: self.profile_generation, cancel, target, task: Task::spawn_serial("Unpacking", &ctx, move |t| {
                            let profile = setup::load_profile(&g, &profile_path)?;
                            let dict = dict_path.and_then(|d| std::fs::read_to_string(d).ok()).map(|x| setup::load_dict(&x)).unwrap_or_default();
                            t.log(format!("dictionary: {} names", dict.len()));
                            setup::unpack_with_context(&profile.qar_context(), &g, &archives, &cache, &dict, &c2, &mut |f, txt| t.progress(f, txt))
                        }) });
                    }
                    if !cache_ok {
                        ui.label(RichText::new("Choose a valid cache folder above.").color(p.muted));
                    }
                }
            }
            if let Some(r) = &self.unpack_result {
                match r {
                    Ok(r) if r.cancelled => {
                        ui.colored_label(p.warn, format!("Cancelled after {} files ({} already there). Unpack again to resume.", r.written, r.skipped));
                    }
                    Ok(r) => {
                        ui.colored_label(p.ok, format!(
                            "{} archive(s): {} files written ({}), {} already there; {} named, {} by hash",
                            r.archives, r.written, gb(r.bytes_written), r.skipped, r.named, r.unnamed
                        ));
                        for e in r.errors.iter().take(8) {
                            ui.colored_label(p.err, e);
                        }
                    }
                    Err(e) => {
                        ui.colored_label(p.err, e);
                    }
                }
            }
        });
    }

    fn step_check(&mut self, ui: &mut egui::Ui, s: &mut Settings, env: &Env) {
        let ctx = ui.ctx().clone();
        let p = pal(ui);
        let need: u64 = self
            .survey
            .get()
            .map(|sv| {
                sv.iter()
                    .filter(|a| s.unpack_select.contains(&a.id))
                    .map(|a| a.content_bytes)
                    .sum()
            })
            .unwrap_or(0);
        let inputs = PrereqInputs {
            game: (!s.game_install.trim().is_empty()).then(|| PathBuf::from(s.game_install.trim())),
            buildid: s.game_buildid,
            test_install: (!s.game_dir.trim().is_empty()).then(|| PathBuf::from(s.game_dir.trim())),
            cache: (!s.cache_dir.trim().is_empty()).then(|| PathBuf::from(s.cache_dir.trim())),
            needed_bytes: if self.unpack_result.as_ref().is_some_and(|r| r.is_ok()) {
                0
            } else {
                need
            },
            fox_exe: env.fox_exe.clone(),
            python: env.python.clone(),
            dict: s.resolved_dict(),
            gpu: env.gpu.clone(),
            runtime_profile: s.resolved_runtime_profile(),
            needs_python: env.needs_python,
        };
        let key = format!(
            "{:?}",
            (
                &inputs.game,
                &inputs.test_install,
                &inputs.cache,
                inputs.needed_bytes,
                &inputs.fox_exe,
                &inputs.python,
                &inputs.dict,
                &inputs.gpu,
                &inputs.runtime_profile,
                inputs.needs_python
            )
        );
        self.prereqs
            .ensure(&key, &ctx, "Checking prerequisites", move || {
                setup::prereqs(&inputs)
            });
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::section(ui, "Prerequisites");
                if ui
                    .add_enabled(!self.prereqs.busy(), egui::Button::new("Check again"))
                    .clicked()
                {
                    self.prereqs.reset();
                }
                if self.prereqs.busy() {
                    ui.spinner();
                }
            });
            if let Some(checks) = self.prereqs.get() {
                egui::Grid::new("prereq_grid")
                    .num_columns(3)
                    .spacing([10.0, 6.0])
                    .show(ui, |ui| {
                        for c in checks {
                            let (col, mark) = match c.level {
                                Level::Ok => (p.ok, "●"),
                                Level::Info => (p.info, "●"),
                                Level::Warn => (p.warn, "▲"),
                                Level::Fail => (p.err, "✖"),
                            };
                            ui.label(RichText::new(mark).color(col));
                            ui.label(RichText::new(&c.name).color(p.muted));
                            ui.add(egui::Label::new(&c.detail).wrap());
                            ui.end_row();
                        }
                    });
                let fails = checks.iter().filter(|c| c.level == Level::Fail).count();
                ui.add_space(6.0);
                if fails == 0 {
                    ui.colored_label(p.ok, "Ready. Finish to open your project.");
                } else {
                    ui.colored_label(
                        p.warn,
                        format!(
                            "{fails} item(s) need attention; complete them or choose Skip for now."
                        ),
                    );
                }
            }
        });
    }
}

fn game_check_lines(ui: &mut egui::Ui, k: &Keyed<GameCheck>, path: &str) {
    let p = pal(ui);
    if path.is_empty() {
        return;
    }
    if k.busy() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Checking the folder…");
        });
        return;
    }
    if let Some(g) = k.get() {
        if g.ok() {
            ui.horizontal_wrapped(|ui| {
                theme::pill(ui, "mgsvtpp.exe", p.ok);
                theme::pill(
                    ui,
                    &format!("{} archives, {}", g.archives.len(), gb(g.total_bytes())),
                    p.info,
                );
                if g.foxinstall_manifest {
                    theme::pill(ui, "set up for Fox Studio mods", p.info);
                }
                if g.snakebite {
                    theme::pill(ui, "SnakeBite-managed", p.warn);
                }
            });
        } else {
            ui.colored_label(
                p.err,
                format!("Not an MGSV:TPP install: {}", g.problems.join("; ")),
            );
        }
    }
}

#[cfg(test)]
mod profile_tests {
    use super::*;
    use foxcore::runtime_data::{ArchiveSource, KindOrder, PackOrder, QarKeys};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn profile() -> RuntimeProfile {
        let kind = KindOrder {
            packages: 5,
            rules: Vec::new(),
            rank: Vec::new(),
        };
        RuntimeProfile {
            schema_version: 1,
            keys: QarKeys {
                header_masks: [11, 22, 33, 44],
                layer1: [1, 2, 3, 4, 5, 6, 7, 8],
            },
            order: PackOrder {
                fpk: kind.clone(),
                fpkd: kind,
            },
            sources: vec![ArchiveSource {
                relative_path: "master/data1.dat".into(),
                bytes: 1024,
                index_sha256: "0".repeat(64),
            }],
        }
    }

    fn target(cache: &str) -> ProfileTarget {
        ProfileTarget {
            game: PathBuf::from("fixture-game"),
            path: PathBuf::from("explicit-profile.json"),
            cache: Some(PathBuf::from(cache)),
        }
    }

    #[test]
    fn setup_profile_late_success_and_error_cannot_restore_an_old_cache_selection() {
        let ctx = egui::Context::default();
        for (succeed, return_to_first) in
            [(true, false), (false, false), (true, true), (false, true)]
        {
            let (send, receive) = mpsc::channel();
            let mut view = SetupView::default();
            let old = target("old-cache");
            view.select_profile(Some(old.clone()));
            let cancelled = Arc::new(AtomicBool::new(false));
            view.profile_task = Some(SelectedTask {
                target: old,
                generation: view.profile_generation,
                cancel: cancelled.clone(),
                task: Task::spawn_serial("delayed profile fixture", &ctx, move |_| {
                    receive.recv().unwrap();
                    if succeed {
                        Ok(profile())
                    } else {
                        Err("old install failure".into())
                    }
                }),
            });
            let current = target(if return_to_first {
                "old-cache"
            } else {
                "new-cache"
            });
            view.select_profile(Some(target("new-cache")));
            view.select_profile(Some(current.clone()));
            assert!(cancelled.load(Ordering::Relaxed));
            send.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while view.busy() && Instant::now() < deadline {
                view.poll();
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(!view.busy());
            assert_eq!(view.profile_target, Some(current));
            assert!(view.profile_result.is_none());
            assert!(!view.profile_ready());
        }
    }

    #[test]
    fn setup_keyed_probe_finishes_old_worker_before_starting_latest_request() {
        let ctx = egui::Context::default();
        let (send, receive) = mpsc::channel();
        let mut keyed = Keyed::new();
        keyed.ensure("first", &ctx, "old probe", move || {
            receive.recv().unwrap();
            1
        });
        keyed.ensure("second", &ctx, "latest probe", || {
            panic!("a second worker must not start yet")
        });
        send.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while keyed.busy() && Instant::now() < deadline {
            keyed.poll();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!keyed.busy());
        assert!(keyed.get().is_none());
        keyed.ensure("second", &ctx, "latest probe", || 2);
        let deadline = Instant::now() + Duration::from_secs(3);
        while keyed.busy() && Instant::now() < deadline {
            keyed.poll();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(keyed.get(), Some(&2));
    }

    #[test]
    fn setup_keyed_refresh_does_not_accept_an_old_result_for_the_same_key() {
        let ctx = egui::Context::default();
        let (send, receive) = mpsc::channel();
        let mut keyed = Keyed::new();
        keyed.ensure("same path", &ctx, "old probe", move || {
            receive.recv().unwrap();
            1
        });
        keyed.reset();
        keyed.ensure("same path", &ctx, "refresh", || {
            panic!("the old probe still owns the worker")
        });
        send.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while keyed.busy() && Instant::now() < deadline {
            keyed.poll();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!keyed.busy());
        assert!(keyed.get().is_none());
    }

    #[test]
    fn setup_archive_results_cannot_return_after_a_cache_selection_round_trip() {
        let ctx = egui::Context::default();
        let mut view = SetupView::default();
        let old = target("old-cache");
        view.select_profile(Some(old.clone()));
        let (unpack_send, unpack_receive) = mpsc::channel();
        let (dict_send, dict_receive) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        view.unpack = Some(SelectedTask {
            target: old.clone(),
            generation: view.profile_generation,
            cancel: cancelled.clone(),
            task: Task::spawn_serial("old unpack", &ctx, move |_| {
                unpack_receive.recv().unwrap();
                Ok(UnpackReport::default())
            }),
        });
        view.dict = Some(SelectedTask {
            target: old.clone(),
            generation: view.profile_generation,
            cancel: cancelled.clone(),
            task: Task::spawn_serial("old dictionary", &ctx, move |_| {
                dict_receive.recv().unwrap();
                Err("old dictionary failure".into())
            }),
        });
        view.select_profile(Some(target("new-cache")));
        view.select_profile(Some(old));
        assert!(cancelled.load(Ordering::Relaxed));
        unpack_send.send(()).unwrap();
        dict_send.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while view.busy() && Instant::now() < deadline {
            view.poll();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!view.busy());
        assert!(view.unpack_result.is_none());
        assert!(view.dict_result.is_none());
    }
}
