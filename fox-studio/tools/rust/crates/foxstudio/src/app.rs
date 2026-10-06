//! The window: top bar with tabs and machine state, the views (Project, Setup, Build, Previews, Mods, Settings), a
//! status bar, settings persistence and a safe close (a running build or install is never dropped silently).
use crate::about::AboutView;
use crate::build_view::{self, BuildView};
use crate::mods_view::{self, ModsView};
use crate::preview_view::{self, PreviewView};
use crate::project::{self, Poller, ProbeConfig, ProjectInfo};
use crate::project_editor::{LocationEditor, LocationForm};
use crate::projects::{self, Project};
use crate::runner::BuildContext;
use crate::settings::{self, Settings, Tab, ThemeChoice};
use crate::setup_view::{self, SetupView};
use crate::tasks::Task;
use crate::theme::{self, pal};
use eframe::egui::{self, Align, Color32, Layout, RichText};
use eframe::egui_wgpu;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

pub struct AppOptions {
    /// where settings are saved; None = never saved (tests)
    pub settings_path: Option<PathBuf>,
    /// false: real (non dry-run) builds are refused outright (headless tests)
    pub allow_real_builds: bool,
    pub probe_every: Duration,
}

impl Default for AppOptions {
    fn default() -> Self {
        AppOptions {
            settings_path: Some(settings::default_path()),
            allow_real_builds: true,
            probe_every: Duration::from_secs(2),
        }
    }
}

/// the "New project" dialog
#[derive(Clone, Debug, Default)]
pub struct NewProject {
    pub name: String,
    pub folder: String,
    pub kind: String,
    pub error: Option<String>,
    pub location: LocationForm,
}

#[derive(Clone)]
enum ProjectAction {
    Select { root_setting: String, file: String },
    Reload,
    Create(NewProject),
    Quit,
}

#[derive(PartialEq)]
enum CloseAsk {
    None,
    Build,
    ModOp,
}

pub struct FoxStudio {
    pub settings: Settings,
    saved: Settings,
    settings_note: Option<String>,
    opts: AppOptions,
    poller: Poller,
    pub build: BuildView,
    pub mods: ModsView,
    pub setup: SetupView,
    pub previews: PreviewView,
    /// the open project (settings.project_file), loaded when the setting changes
    pub project: Option<Result<Project, String>>,
    project_for: Option<(PathBuf, String)>,
    project_root_setting: String,
    root_draft: String,
    pub location_editor: Option<LocationEditor>,
    editor_open: bool,
    pub editor_error: Option<String>,
    pending_project: Option<ProjectAction>,
    project_refresh: bool,
    reload_assets: bool,
    pub about: Option<AboutView>,
    pub bundle_error: Option<String>,
    bundle_for: Option<Option<PathBuf>>,
    bundle_generation: u64,
    bundle_task: Option<(u64, Task<BuildContext>)>,
    bundle_context: Option<BuildContext>,
    pub new_project: Option<NewProject>,
    /// eframe's wgpu device (None in headless tests and with other renderers): the 3-D previews use it
    wgpu: Option<egui_wgpu::RenderState>,
    applied: Option<(ThemeChoice, f32, bool, bool)>,
    close_ask: CloseAsk,
    may_close: bool,
    close_after_drafts: bool,
    last_save_try: Instant,
}

impl FoxStudio {
    pub fn new(ctx: &egui::Context, settings: Settings, settings_note: Option<String>, opts: AppOptions) -> FoxStudio {
        let cfg = probe_config(&settings, None);
        let poller = Poller::start(cfg, ctx, opts.probe_every);
        let mut app = FoxStudio {
            root_draft: settings.repo_root.clone(),
            saved: settings.clone(),
            settings,
            settings_note,
            opts,
            poller,
            build: BuildView::default(),
            mods: ModsView::default(),
            setup: SetupView::default(),
            previews: PreviewView::default(),
            project: None,
            project_for: None,
            project_root_setting: String::new(),
            location_editor: None,
            editor_open: false,
            editor_error: None,
            pending_project: None,
            project_refresh: false,
            reload_assets: false,
            about: None,
            bundle_error: None,
            bundle_for: None,
            bundle_generation: 0,
            bundle_task: None,
            bundle_context: None,
            new_project: None,
            wgpu: None,
            applied: None,
            close_ask: CloseAsk::None,
            may_close: false,
            close_after_drafts: false,
            last_save_try: Instant::now(),
        };
        app.apply_look(ctx);
        app.load_project();
        app
    }

    /// give the app eframe's wgpu state (from the creation context) for the 3-D previews
    pub fn with_wgpu(mut self, rs: Option<egui_wgpu::RenderState>) -> Self {
        self.wgpu = rs;
        self
    }

    pub fn gpu_name(&self) -> Option<String> {
        self.wgpu.as_ref().map(|r| {
            let i = r.adapter.get_info();
            format!("{} ({:?})", i.name, i.backend)
        })
    }

    /// the open project, if it loaded
    pub fn open_project(&self) -> Option<&Project> {
        self.project.as_ref().and_then(|p| p.as_ref().ok())
    }

    /// Root and authored file identify the document; a same-file Reload is an explicit action.
    fn load_project(&mut self) {
        let key = (
            self.settings.resolved_root(),
            self.settings.project_file.trim().to_string(),
        );
        if self.project_for.as_ref() == Some(&key) {
            return;
        }
        if self.location_has_changes() && self.project_for.is_some() {
            let action = ProjectAction::Select {
                root_setting: self.settings.repo_root.clone(),
                file: key.1.clone(),
            };
            if let Some((_, previous_file)) = &self.project_for {
                self.settings.project_file = previous_file.clone();
                self.settings.repo_root = self.project_root_setting.clone();
            }
            self.pending_project = Some(action);
            return;
        }
        self.load_project_now();
    }

    fn load_project_now(&mut self) {
        let root = self.settings.resolved_root();
        let file = self.settings.project_file.trim().to_string();
        if self.project_for.is_some() {
            self.previews.reset_models();
            self.settings.preview_model.clear();
        }
        self.project_for = Some((root.clone(), file.clone()));
        self.project_root_setting = self.settings.repo_root.clone();
        self.root_draft = self.settings.repo_root.clone();
        self.location_editor = None;
        self.editor_error = None;
        self.editor_open = false;
        self.project = if file.is_empty() {
            None
        } else {
            Some(projects::load_in(&root, Path::new(&file)))
        };
        if let Some(Ok(project)) = &self.project {
            projects::push_recent(
                &mut self.settings.recent_projects,
                &project.file.display().to_string(),
                settings::MAX_RECENT,
            );
            if project.m3.is_some() {
                match LocationEditor::open_in(&project.root, &project.file) {
                    Ok(editor) => self.location_editor = Some(editor),
                    Err(error) => self.editor_error = Some(error),
                }
            }
        }
        self.project_refresh = true;
    }

    pub fn location_has_changes(&self) -> bool {
        self.location_editor.as_ref().is_some_and(LocationEditor::has_changes)
    }

    fn request_project_action(&mut self, action: ProjectAction) {
        if self.location_has_changes() {
            self.pending_project = Some(action);
        } else {
            self.perform_project_action(action);
        }
    }

    fn perform_project_action(&mut self, action: ProjectAction) {
        match action {
            ProjectAction::Select { root_setting, file } => {
                self.settings.repo_root = root_setting;
                self.settings.project_file = file;
                self.load_project_now();
            }
            ProjectAction::Reload => {
                self.load_project_now();
                self.reload_assets = true;
            }
            ProjectAction::Create(new) => {
                let created = if new.kind == "location" {
                    new.location.create(Path::new(new.folder.trim()))
                } else {
                    projects::create(Path::new(new.folder.trim()), &new.name, &new.kind)
                };
                match created {
                    Ok(project) => {
                        self.settings.repo_root = project.root.display().to_string();
                        self.settings.project_file = project.file.display().to_string();
                        self.new_project = None;
                        self.load_project_now();
                    }
                    Err(error) => {
                        self.new_project = Some(NewProject {
                            error: Some(error),
                            ..new
                        })
                    }
                }
            }
            ProjectAction::Quit => {
                self.location_editor = None;
                self.close_after_drafts = true;
            }
        }
    }

    /// Explicit roots stay explicit; a standalone file outside a checkout uses its own folder.
    pub fn open_project_path(&mut self, path: &Path) {
        let root_setting = if self.settings.repo_root.trim().is_empty() {
            let folder = if path.is_dir() {
                path
            } else {
                path.parent().unwrap_or(Path::new("."))
            };
            folder
                .ancestors()
                .find(|parent| settings::is_repo(parent))
                .unwrap_or(folder)
                .display()
                .to_string()
        } else {
            self.settings.repo_root.clone()
        };
        self.request_project_action(ProjectAction::Select {
            root_setting,
            file: path.display().to_string(),
        });
    }

    pub fn close_project(&mut self) {
        self.request_project_action(ProjectAction::Select {
            root_setting: self.settings.repo_root.clone(),
            file: String::new(),
        });
    }

    pub fn reload_project(&mut self) {
        self.request_project_action(ProjectAction::Reload);
    }

    pub fn request_quit(&mut self) {
        self.close_after_drafts = true;
    }

    pub fn reload_bundle(&mut self) {
        self.bundle_for = None;
        self.bundle_context = None;
        self.bundle_error = None;
    }

    fn accept_project(&mut self, project: Project) {
        projects::push_recent(
            &mut self.settings.recent_projects,
            &project.file.display().to_string(),
            settings::MAX_RECENT,
        );
        self.project = Some(Ok(project));
        self.editor_error = None;
        self.project_refresh = true;
    }

    fn selected_writer(&self) -> Option<u32> {
        let project = self.open_project()?;
        foxbuild::lock_holder(&log_dir_of(&project.root, &project.graph))
    }

    pub fn save_location(&mut self) -> Result<(), String> {
        if let Some(pid) = self.selected_writer() {
            return Err(format!(
                "A build is using this project (pid {pid}); wait before saving location settings."
            ));
        }
        let result = self
            .location_editor
            .as_mut()
            .ok_or("No location document is open.")?
            .save();
        match result {
            Ok(project) => {
                self.accept_project(project);
                Ok(())
            }
            Err(error) => {
                if let Some(editor) = &mut self.location_editor {
                    editor.error = Some(error.clone());
                }
                self.editor_error = Some(error.clone());
                Err(error)
            }
        }
    }

    fn projects_card(&mut self, ui: &mut egui::Ui) {
        let p = pal(ui);
        let mut open: Option<PathBuf> = None;
        let mut close = false;
        let mut reload = false;
        let mut edit = false;
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::section(ui, "Project");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("New project…").clicked() {
                        self.new_project = Some(NewProject { kind: "location".into(), ..Default::default() });
                    }
                    if ui.button("Open…").on_hover_text("Open a Fox Studio project or location spec").clicked()
                        && let Some(file) = rfd::FileDialog::new().add_filter("Fox Studio project", &["toml"]).set_title("Open a Fox Studio project").pick_file()
                    {
                        open = Some(file);
                    }
                    // FLYK, example #1: its M3 location spec when the repository has one, else Fox Studio's own file
                    let repo = self.settings.resolved_root();
                    let flyk = [projects::flyk_spec(&repo), projects::flyk_example(&repo)].into_iter().find(|f| f.is_file());
                    if let Some(file) = flyk
                        && ui.button("Open the FLYK example").on_hover_text(file.display().to_string()).clicked()
                    {
                        open = Some(file);
                    }
                });
            });
            match &self.project {
                None => {
                    ui.label(RichText::new("No project open: the Build tab uses the repository's graph file (Settings). Open the FLYK example or create a project.").color(p.muted));
                }
                Some(Err(e)) => {
                    ui.colored_label(p.err, format!("Could not open the project: {e}"));
                    if ui.small_button("Close it").clicked() {
                        close = true;
                    }
                }
                Some(Ok(pr)) => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(pr.name()).size(18.0).strong());
                        theme::pill(ui, &pr.spec.project.kind, p.info);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("Close project").clicked() { close = true; }
                            if ui.small_button("Reload project").clicked() { reload = true; }
                            if pr.m3.is_some() && ui.small_button("Edit location").clicked() { edit = true; }
                        });
                    });
                    if !pr.spec.project.description.is_empty() {
                        ui.label(RichText::new(&pr.spec.project.description).color(p.muted));
                    }
                    if let Some(m) = &pr.m3 {
                        ui.horizontal_wrapped(|ui| {
                            theme::pill(ui, "M3 location spec", p.ok);
                            theme::pill(ui, &format!("code {}", m.code), p.muted);
                            theme::pill(ui, &format!("{} x {} cells of {} m", m.grid, m.grid, m.cell_m), p.muted);
                            theme::pill(ui, &format!("{} stages generated", m.stages), p.muted);
                            if let Some(c) = &m.compat {
                                theme::pill(ui, &format!("compat {c}"), p.info)
                                    .on_hover_text("legacy (Python) stage implementations allowed; the generated graph equals tools/build/flyk_stages.toml");
                            }
                        });
                        for i in m.issues.iter().take(5) {
                            ui.label(RichText::new(format!("warning: {i}")).small().color(p.warn));
                        }
                        if let Some(b) = &m.real_build_block {
                            ui.label(RichText::new(format!("Real builds: {b}")).small().color(p.warn));
                        }
                    }
                    egui::Grid::new("project_grid").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
                        let mut kv = |k: &str, v: String, ok: bool| {
                            ui.label(RichText::new(k).color(p.muted));
                            ui.label(RichText::new(v).monospace().color(if ok { ui.visuals().text_color() } else { p.err }));
                            ui.end_row();
                        };
                        kv("File", pr.file.display().to_string(), true);
                        kv("Root", pr.root.display().to_string(), pr.root.is_dir());
                        kv("Graph", pr.graph.display().to_string(), pr.graph.is_file());
                        if let Some(h) = pr.heights() {
                            let ok = h.is_file();
                            kv("Heights", h.display().to_string(), ok);
                        }
                        let nav = pr.nav();
                        if !nav.is_empty() {
                            let ok = nav.iter().all(|n| n.exists());
                            kv("Navmesh", nav.iter().map(|n| n.display().to_string()).collect::<Vec<_>>().join("; "), ok);
                        }
                    });
                }
            }
            let recent: Vec<String> = self
                .settings
                .recent_projects
                .iter()
                .filter(|r| self.open_project().is_none_or(|p| !crate::steam::same_path(Path::new(r), &p.file)))
                .cloned()
                .collect();
            if !recent.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new("Recent").small().color(p.muted));
                for r in recent {
                    if ui.link(RichText::new(&r).monospace().small()).clicked() {
                        open = Some(PathBuf::from(r));
                    }
                }
            }
        });
        if edit {
            self.editor_open = true;
        }
        if reload {
            self.reload_project();
        }
        if close {
            self.close_project();
        }
        if let Some(file) = open {
            self.open_project_path(&file);
        }
        if let Some(error) = &self.editor_error {
            ui.colored_label(p.err, error);
        }
        if self.editor_open {
            let writer = self.selected_writer();
            if let Some(pid) = writer {
                ui.colored_label(
                    p.warn,
                    format!("Build {pid} is using this project; location editing waits."),
                );
            }
            let saved = self.location_editor.as_mut().and_then(|editor| {
                theme::card(ui)
                    .show(ui, |ui| ui.add_enabled_ui(writer.is_none(), |ui| editor.ui(ui)).inner)
                    .inner
            });
            if let Some(project) = saved {
                self.accept_project(project);
            }
        }
    }

    fn new_project_modal(&mut self, ctx: &egui::Context) {
        let Some(np) = self.new_project.as_mut() else { return };
        let mut done: Option<bool> = None;
        egui::Modal::new(egui::Id::new("new_project")).show(ctx, |ui| {
            let p = pal(ui);
            ui.set_width(480.0);
            ui.heading("New project");
            ui.label(
                RichText::new("Creates a native project in a chosen folder. Existing project files are preserved.")
                    .color(p.muted),
            );
            ui.add_space(6.0);
            egui::Grid::new("new_project_grid")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    if np.kind != "location" {
                        let label = ui.label("Name");
                        ui.add(
                            egui::TextEdit::singleline(&mut np.name)
                                .hint_text("My project")
                                .desired_width(320.0),
                        )
                        .labelled_by(label.id);
                        ui.end_row();
                    }
                    ui.label("Folder");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut np.folder).desired_width(250.0));
                        if ui.button("Browse…").clicked()
                            && let Some(folder) = rfd::FileDialog::new().set_title("Project folder").pick_folder()
                        {
                            np.folder = folder.display().to_string();
                        }
                    });
                    ui.end_row();
                    ui.label("Kind");
                    ui.horizontal(|ui| {
                        for k in ["location", "mission", "mod"] {
                            ui.selectable_value(&mut np.kind, k.to_string(), k);
                        }
                    });
                    ui.end_row();
                });
            if np.kind == "location" {
                np.location.ui(ui);
            }
            if let Some(e) = &np.error {
                ui.colored_label(p.err, e);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    done = Some(false);
                }
                let named = if np.kind == "location" {
                    !np.location.name.trim().is_empty()
                } else {
                    !np.name.trim().is_empty()
                };
                let ok = named && !np.folder.trim().is_empty();
                if theme::primary_button(ui, "Create", ok).clicked() {
                    done = Some(true);
                }
            });
        });
        match done {
            Some(false) => self.new_project = None,
            Some(true) => {
                let new = self.new_project.take().unwrap();
                self.request_project_action(ProjectAction::Create(new));
            }
            None => {}
        }
    }

    pub fn project(&self) -> ProjectInfo {
        self.poller.info()
    }

    fn apply_look(&mut self, ctx: &egui::Context) {
        let want = (
            self.settings.theme,
            self.settings.ui_scale,
            self.settings.system_fonts,
            self.settings.high_contrast,
        );
        if self.applied.map(|a| a.2) != Some(want.2) {
            theme::install_fonts(ctx, want.2);
        }
        if self.applied != Some(want) {
            theme::apply_with_contrast(
                ctx,
                want.0,
                want.1,
                if want.3 {
                    theme::Contrast::High
                } else {
                    theme::Contrast::Standard
                },
            );
            self.applied = Some(want);
        }
        // keep the chosen theme even if something else (a host, a test harness) reset the preference
        let pref = theme::preference(want.0);
        if ctx.options(|o| o.theme_preference) != pref {
            ctx.set_theme(pref);
        }
    }

    pub fn bundle_ready(&self) -> bool {
        self.bundle_context.as_ref().is_some_and(BuildContext::is_packaged)
    }

    fn sync_bundle(&mut self, ctx: &egui::Context) {
        let path = self.settings.resolved_bundle_manifest();
        if self.bundle_for.as_ref() != Some(&path) {
            self.bundle_for = Some(path.clone());
            self.bundle_generation = self.bundle_generation.wrapping_add(1);
            self.bundle_context = None;
            self.bundle_error = path
                .is_none()
                .then(|| "Select a native tool bundle in Settings to enable real builds.".into());
            self.build.set_context(Some(BuildContext::json_v1()));
        }
        if let Some((generation, task)) = self.bundle_task.as_mut()
            && task.poll()
        {
            let generation = *generation;
            let result = task.take_result();
            self.bundle_task = None;
            if generation == self.bundle_generation
                && let Some(result) = result
            {
                match result {
                    Ok(context) => {
                        self.build.set_context(Some(context.clone()));
                        self.bundle_context = Some(context);
                        self.bundle_error = None;
                    }
                    Err(error) => self.bundle_error = Some(error),
                }
            }
        }
        if let Some(manifest) = path
            && self.bundle_context.is_none()
            && self.bundle_error.is_none()
            && self.bundle_task.is_none()
        {
            let task = Task::spawn_serial("Validating native tool bundle", ctx, move |_| {
                BuildContext::packaged(&manifest)
            });
            self.bundle_task = Some((self.bundle_generation, task));
        }
    }

    fn public_build_block(&self) -> Option<String> {
        if self
            .open_project()
            .and_then(|project| project.m3.as_ref())
            .is_some_and(|location| location.compat.is_some())
        {
            return Some("Compatibility recipes are unavailable in the public editor. Open a native project.".into());
        }
        if let Some(reason) = self.open_project().and_then(Project::real_build_block) {
            return Some(reason.into());
        }
        if !self.bundle_ready() {
            return Some("Select a validated native tool bundle in Settings before a real build.".into());
        }
        None
    }

    fn build_env(&self, info: &ProjectInfo) -> build_view::Env {
        let tools = self.settings.resolved_root();
        let (root, graph) = match self.open_project() {
            Some(p) => (p.root.clone(), p.graph.clone()),
            None => (tools.clone(), tools.join(&self.settings.graph_file)),
        };
        build_view::Env {
            graph,
            project_spec: self.open_project().filter(|p| p.m3.is_some()).map(|p| p.file.clone()),
            fox_exe: self.settings.resolved_fox_exe(&tools),
            python: self.settings.resolved_python(),
            game_running: info.game_running.clone(),
            lock_pid: info.build_running,
            confirm_builds: self.settings.confirm_builds,
            allow_real_builds: self.opts.allow_real_builds,
            real_build_block: self.public_build_block(),
            paused: info.build_paused,
            root,
        }
    }

    fn save_settings(&mut self) {
        if self.settings == self.saved || self.last_save_try.elapsed() < Duration::from_millis(500) {
            return;
        }
        self.last_save_try = Instant::now();
        self.settings.sanitize();
        if let Some(p) = &self.opts.settings_path {
            match self.settings.save(p) {
                Ok(()) => {
                    self.saved = self.settings.clone();
                    self.settings_note = None;
                }
                Err(e) => self.settings_note = Some(format!("could not save settings: {e}")),
            }
        } else {
            self.saved = self.settings.clone();
        }
    }

    /// one frame of the whole app
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.apply_look(&ctx);
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::Q)) {
            self.request_quit();
        }
        let drops = ctx.input(|i| i.raw.dropped_files.clone());
        if !drops.is_empty() {
            self.mods.receive_drop(&ctx, &mut self.settings, &drops);
            self.settings.last_tab = Tab::Mods;
        }
        self.load_project();
        self.sync_bundle(&ctx);
        self.poller
            .reconfigure(probe_config(&self.settings, self.open_project()));
        let info = self.poller.info();
        let env = self.build_env(&info);
        let selected_project = self.open_project().cloned();
        self.previews
            .synchronize_dataset(&self.settings.resolved_root(), selected_project.as_ref());
        if self.reload_assets {
            if let Some(project) = &selected_project
                && project.heights().is_some_and(|path| path.is_file())
            {
                self.previews.reload_terrain(&ctx, project);
            }
            self.reload_assets = false;
        }
        if self.project_refresh {
            self.build.project_changed(&ctx, &env);
            self.project_refresh = false;
        }
        // Target changes and the initial status load are handled even when Build is not the visible tab.
        self.build.poll(&ctx, &env);
        self.mods.sync_target(&ctx, &self.settings);
        self.mods.poll();
        self.setup.poll();
        self.previews.poll();
        self.handle_close(&ctx);

        egui::Panel::top("top_bar")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ui, |ui| self.top_bar(ui, &info));
        egui::Panel::bottom("status_bar")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .inner_margin(egui::Margin::symmetric(12, 3)),
            )
            .show(ui, |ui| self.status_bar(ui, &info, &env));
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ui, |ui| match self.settings.last_tab {
                Tab::Project => self.project_tab(ui, &info, &env),
                Tab::Setup => {
                    let senv = setup_view::Env {
                        fox_exe: env.fox_exe.clone(),
                        python: env.python.clone(),
                        gpu: self.gpu_name(),
                        game_running: info.game_running.clone(),
                        needs_python: self.build.report.as_ref().is_some_and(|report| {
                            report.config == env.graph
                                && report.stages.iter().any(|stage| command_needs_python(&stage.cmd))
                        }),
                    };
                    self.setup.show(ui, &mut self.settings, &senv);
                }
                Tab::Build => {
                    if let Some(reason) = &env.real_build_block {
                        ui.colored_label(pal(ui).warn, reason);
                    }
                    self.build.show(ui, &env);
                }
                Tab::Previews => {
                    let penv = preview_view::Env {
                        project: self.project.as_ref().and_then(|p| p.as_ref().ok()),
                        wgpu: self.wgpu.as_ref(),
                        game_running: info.game_running.clone(),
                    };
                    self.previews.show(ui, &mut self.settings, &penv);
                }
                Tab::Mods => {
                    let menv = mods_view::Env {
                        game_running: info.game_running.clone(),
                    };
                    self.mods.show(ui, &mut self.settings, &menv);
                }
                Tab::Settings => self.settings_tab(ui, &env),
            });
        self.close_modal(&ctx);
        self.new_project_modal(&ctx);
        self.project_draft_modal(&ctx);
        self.save_settings();
        if self.settings != self.saved {
            ctx.request_repaint_after(Duration::from_millis(600));
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, info: &ProjectInfo) {
        let p = pal(ui);
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
            ui.painter().rect_filled(r, 5u8, p.accent);
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                "F",
                egui::FontId::proportional(15.0),
                Color32::from_rgb(0x1A, 0x1C, 0x20),
            );
            ui.label(RichText::new(crate::APP_NAME).strong().size(17.0));
            ui.add_space(16.0);
            for (t, label) in [
                (Tab::Project, "Project"),
                (Tab::Setup, "Setup"),
                (Tab::Build, "Build"),
                (Tab::Previews, "Previews"),
                (Tab::Mods, "Mods"),
                (Tab::Settings, "Settings"),
            ] {
                let sel = self.settings.last_tab == t;
                let text = if sel {
                    RichText::new(label).strong().color(p.accent)
                } else {
                    RichText::new(label)
                };
                if ui.selectable_label(sel, text).clicked() {
                    self.settings.last_tab = t;
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let dark = ui.visuals().dark_mode;
                if ui
                    .button(if dark { "☀" } else { "🌙" })
                    .on_hover_text("Switch between dark and light")
                    .clicked()
                {
                    self.settings.theme = if dark { ThemeChoice::Light } else { ThemeChoice::Dark };
                }
                if let Some(g) = &info.game_running {
                    theme::pill(ui, &format!("{g} running"), p.warn).on_hover_text(
                        "Builds go one stage at a time at Idle priority; installs wait for the game to close",
                    );
                }
                if self.build.run_busy() {
                    theme::pill(ui, "building", p.info);
                } else if let Some(pid) = info.build_running {
                    theme::pill(ui, &format!("build running (pid {pid})"), p.info);
                }
                if self.mods.busy() {
                    theme::pill(ui, "mod operation", p.info);
                }
                if self.setup.busy() {
                    theme::pill(ui, "unpacking", p.info);
                }
                if let Some(pr) = self.open_project() {
                    theme::pill(ui, pr.name(), p.accent).on_hover_text(pr.file.display().to_string());
                }
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, info: &ProjectInfo, env: &build_view::Env) {
        let p = pal(ui);
        ui.horizontal(|ui| {
            ui.label(RichText::new(env.root.display().to_string()).small().color(p.muted));
            if let Some(s) = &info.stamp {
                ui.label(RichText::new(format!("· tools {s}")).small().color(p.muted));
            }
            if let Some(n) = &self.settings_note {
                ui.label(RichText::new(n).small().color(p.warn));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{} {}", crate::APP_NAME, crate::VERSION))
                        .small()
                        .color(p.muted),
                );
            });
        });
    }

    fn project_tab(&mut self, ui: &mut egui::Ui, info: &ProjectInfo, env: &build_view::Env) {
        let p = pal(ui);
        egui::ScrollArea::vertical().id_salt("project_scroll").auto_shrink([false, false]).show(ui, |ui| {
            if !self.settings.setup_done {
                egui::Frame::new().fill(p.accent.gamma_multiply(0.12)).stroke(egui::Stroke::new(1.0, p.accent)).corner_radius(6u8).inner_margin(10i8).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("First run:").strong());
                        ui.label("find the game, pick a mod test install and unpack the vanilla data.");
                        if ui.button("Open setup").clicked() {
                            self.settings.last_tab = Tab::Setup;
                        }
                    });
                });
                ui.add_space(8.0);
            }
            self.projects_card(ui);
            ui.add_space(8.0);
            let probing = !info.probed;
            let row = |ui: &mut egui::Ui, ok: Option<bool>, k: &str, v: String| {
                let (c, mark) = match ok {
                    Some(true) => (p.ok, "●"),
                    Some(false) => (p.err, "●"),
                    None => (p.muted, "○"),
                };
                ui.label(RichText::new(mark).color(c));
                ui.label(RichText::new(k).color(p.muted));
                ui.add(egui::Label::new(RichText::new(v).monospace()).selectable(true).wrap());
                ui.end_row();
            };
            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                theme::section(ui, "Project and build graph");
                egui::Grid::new("repo_grid").num_columns(3).spacing([10.0, 6.0]).show(ui, |ui| {
                    let tools = self.settings.resolved_root();
                    row(ui, if probing { None } else { Some(tools.is_dir()) }, "Selected root", tools.display().to_string());
                    if !crate::steam::same_path(&tools, &env.root) {
                        row(ui, Some(env.root.is_dir()), "Project root", env.root.display().to_string());
                    }
                    row(ui, if probing { None } else { Some(info.graph_exists) }, "Build graph", rel(&env.graph, &env.root));
                });
                if !probing && !env.root.is_dir() { ui.colored_label(p.err, "Select an existing project root in Settings."); }
                if let Some(r) = &self.build.report {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(format!("{} stages, {} pinned inputs", r.stages.len(), r.pinned.len()));
                        if ui.link("Open the build view").clicked() {
                            self.settings.last_tab = Tab::Build;
                        }
                    });
                }
            });
            ui.add_space(8.0);
            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                theme::section(ui, "Rust tools");
                egui::Grid::new("tools_grid").num_columns(3).spacing([10.0, 6.0]).show(ui, |ui| {
                    if let Some(stamp) = &info.stamp { row(ui, Some(true), "Developer tool stamp", stamp.clone()); }
                    row(ui, Some(self.bundle_ready()), "Native bundle", if self.bundle_ready() {
                        "validated native tools".into()
                    } else { self.bundle_error.clone().unwrap_or_else(|| "validating…".into()) });
                    let exe = match (info.fox_exe_size, info.fox_exe_modified) {
                        (Some(sz), Some(m)) => format!("{}  ({:.1} MB, built {})", rel(&env.fox_exe, &env.root), sz as f64 / 1e6, ago(m)),
                        _ => format!("{}  (not built)", rel(&env.fox_exe, &env.root)),
                    };
                    row(ui, if probing { None } else { Some(info.fox_exe_size.is_some()) }, "fox executable", exe);
                    if self.build.report.as_ref().is_some_and(|report| report.stages.iter().any(|stage| command_needs_python(&stage.cmd))) {
                        row(ui, None, "Legacy Python launcher", env.python.clone());
                    }
                });
                if !probing && info.fox_exe_size.is_none() {
                    ui.label(RichText::new("Select the native fox executable from the release bundle in Settings.").color(p.warn));
                }
            });
            ui.add_space(8.0);
            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                theme::section(ui, "This machine");
                egui::Grid::new("machine_grid").num_columns(3).spacing([10.0, 6.0]).show(ui, |ui| {
                    match (&info.game_running, probing) {
                        (_, true) => row(ui, None, "Game", "checking…".into()),
                        (Some(g), _) => row(ui, Some(false), "Game", format!("{g} is running: builds are polite (one stage, Idle, no GPU stages); installs wait")),
                        (None, _) => row(ui, Some(true), "Game", "not running".into()),
                    }
                    match info.build_running {
                        Some(pid) => row(ui, Some(true), "Build", format!("running (pid {pid})")),
                        None => row(ui, None, "Build", "idle".into()),
                    }
                    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0);
                    row(ui, None, "Logical cores", threads.to_string());
                });
            });
        });
    }

    fn settings_tab(&mut self, ui: &mut egui::Ui, env: &build_view::Env) {
        let p = pal(ui);
        let mut apply_root = false;
        let mut reload_bundle = false;
        let s = &mut self.settings;
        egui::ScrollArea::vertical().id_salt("settings_scroll").auto_shrink([false, false]).show(ui, |ui| {
            ui.heading("Settings");
            ui.add_space(8.0);
            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                theme::section(ui, "Appearance");
                ui.horizontal(|ui| {
                    ui.label("Theme");
                    ui.selectable_value(&mut s.theme, ThemeChoice::System, "System");
                    ui.selectable_value(&mut s.theme, ThemeChoice::Dark, "Dark");
                    ui.selectable_value(&mut s.theme, ThemeChoice::Light, "Light");
                });
                ui.horizontal(|ui| {
                    ui.label("Interface size");
                    let mut pct = (s.ui_scale * 100.0).round() as i32;
                    if ui.add(egui::Slider::new(&mut pct, 60..=200).suffix(" %").step_by(5.0)).changed() {
                        s.ui_scale = pct as f32 / 100.0;
                    }
                    if ui.small_button("Reset").clicked() {
                        s.ui_scale = 1.0;
                    }
                });
                ui.checkbox(&mut s.high_contrast, "High contrast");
                ui.checkbox(&mut s.system_fonts, "Use the Windows interface fonts (Segoe UI, Consolas)");
            });
            ui.add_space(8.0);
            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                theme::section(ui, "Build");
                egui::Grid::new("paths_grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    let label = ui.label("Project root");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.root_draft).hint_text(env.root.display().to_string()).desired_width(ui.available_width() - 100.0)).labelled_by(label.id);
                        if ui.add_enabled(self.root_draft != s.repo_root, egui::Button::new("Apply root")).clicked() {
                            apply_root = true;
                        }
                    });
                    ui.end_row();
                    ui.label("Graph file");
                    ui.add(egui::TextEdit::singleline(&mut s.graph_file).desired_width(f32::INFINITY));
                    ui.end_row();
                    ui.label("fox executable");
                    ui.add(egui::TextEdit::singleline(&mut s.fox_exe).hint_text(env.fox_exe.display().to_string()).desired_width(f32::INFINITY));
                    ui.end_row();
                    let label = ui.label("Native bundle manifest");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut s.bundle_manifest).hint_text("fox-tools.json next to Fox Studio").desired_width(ui.available_width() - 130.0)).labelled_by(label.id);
                        if ui.button("Reload bundle").clicked() { reload_bundle = true; }
                    });
                    ui.end_row();
                    ui.label("Legacy Python launcher");
                    ui.add(egui::TextEdit::singleline(&mut s.python).hint_text(foxbuild::default_python()).desired_width(f32::INFINITY));
                    ui.end_row();
                });
                ui.label(RichText::new("Native projects use bundled Rust tools. Python applies only to legacy developer graphs; those compatibility recipes cannot run in the public editor.").small().color(p.muted));
                ui.checkbox(&mut s.confirm_builds, "Ask before starting a real build");
            });
            ui.add_space(8.0);
            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                theme::section(ui, "About");
                self.about.get_or_insert_with(AboutView::load_bundled).show(ui);
                ui.label(format!("{} {} — desktop front end of the Fox Engine tools.", crate::APP_NAME, crate::VERSION));
                ui.label(RichText::new("Built with egui / eframe (wgpu renderer). Reads and writes only what you point it at; never game data it was not given.").color(p.muted));
                if let Some(path) = &self.opts.settings_path {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Settings file").color(p.muted));
                        ui.label(RichText::new(path.display().to_string()).monospace().small());
                    });
                }
            });
        });
        if apply_root {
            self.request_project_action(ProjectAction::Select {
                root_setting: self.root_draft.trim().to_string(),
                file: self.settings.project_file.clone(),
            });
        }
        if reload_bundle {
            self.reload_bundle();
        }
    }

    fn handle_close(&mut self, ctx: &egui::Context) {
        let requested = ctx.input(|i| i.viewport().close_requested()) || self.close_after_drafts;
        if !requested || self.may_close {
            return;
        }
        self.close_after_drafts = false;
        if self.location_has_changes() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending_project = Some(ProjectAction::Quit);
            return;
        }
        let writing = self.mods.op.as_ref().is_some_and(|(op, _)| op.writes_game());
        if self.build.run_busy() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_ask = CloseAsk::Build;
        } else if writing {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_ask = CloseAsk::ModOp;
        } else {
            self.may_close = true;
            self.settings.sanitize();
            if let Some(path) = &self.opts.settings_path {
                let _ = self.settings.save(path);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn project_draft_modal(&mut self, ctx: &egui::Context) {
        let Some(action) = self.pending_project.clone() else {
            return;
        };
        let mut decision = None;
        egui::Modal::new(egui::Id::new("project_draft")).show(ctx, |ui| {
            ui.heading("Unsaved location changes");
            ui.label("Save this document before continuing, or discard the draft and keep the saved file.");
            if let Some(editor) = &self.location_editor {
                ui.label(editor.path().display().to_string());
                if let Some(error) = &editor.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            }
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    decision = Some(0);
                }
                if ui.button("Discard changes").clicked() {
                    decision = Some(1);
                }
                if theme::primary_button(ui, "Save changes", self.selected_writer().is_none()).clicked() {
                    decision = Some(2);
                }
            });
        });
        match decision {
            Some(0) => {
                self.pending_project = None;
                self.root_draft = self.settings.repo_root.clone();
                if let ProjectAction::Create(new) = action {
                    self.new_project = Some(new);
                }
            }
            Some(1) => {
                self.pending_project = None;
                self.location_editor = None;
                self.perform_project_action(action);
            }
            Some(2) => {
                self.pending_project = None;
                if self.save_location().is_ok() {
                    self.perform_project_action(action);
                } else {
                    self.editor_open = true;
                }
            }
            _ => {}
        }
    }

    fn close_modal(&mut self, ctx: &egui::Context) {
        if self.close_ask == CloseAsk::None {
            return;
        }
        let mut quit = false;
        let mut cancel = false;
        egui::Modal::new(egui::Id::new("confirm_close")).show(ctx, |ui| {
            let p = pal(ui);
            ui.set_max_width(460.0);
            match self.close_ask {
                CloseAsk::Build => {
                    ui.heading("A build is running");
                    ui.label("Stop it, or leave it running in the background (it keeps writing work/build/build.log; Fox Studio shows it again when you reopen it).");
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                        if ui.button("Leave it running and quit").clicked() {
                            if let Some(r) = self.build.run.as_mut() {
                                r.detach();
                            }
                            quit = true;
                        }
                        if ui.button(RichText::new("Stop the build and quit").color(p.err)).clicked() {
                            if let Some(r) = self.build.run.as_mut() {
                                r.stop();
                            }
                            quit = true;
                        }
                    });
                }
                _ => {
                    ui.heading("A mod operation is writing to the game folder");
                    ui.label("Wait for it to finish. (Quitting now is recovered on the next run from the installer's journal, but waiting is safer.)");
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Wait").clicked() {
                            cancel = true;
                        }
                        if ui.button(RichText::new("Quit anyway").color(p.err)).clicked() {
                            quit = true;
                        }
                    });
                }
            }
        });
        if cancel {
            self.close_ask = CloseAsk::None;
        }
        if quit {
            self.close_ask = CloseAsk::None;
            self.may_close = true;
            self.settings.sanitize();
            if let Some(p) = &self.opts.settings_path {
                let _ = self.settings.save(p);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for FoxStudio {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }
}

fn probe_config(s: &Settings, pr: Option<&Project>) -> ProbeConfig {
    let tools = s.resolved_root();
    let (root, graph) = match pr {
        Some(p) => (p.root.clone(), p.graph.clone()),
        None => (tools.clone(), tools.join(&s.graph_file)),
    };
    ProbeConfig {
        log_dir: log_dir_of(&root, &graph),
        graph,
        fox_exe: s.resolved_fox_exe(&tools),
        games: project::GAME_EXES.iter().map(|x| x.to_string()).collect(),
        root,
    }
}

/// the build lock and logs live in the graph's [settings] log_dir (default work/build)
fn log_dir_of(root: &Path, graph: &Path) -> PathBuf {
    match foxbuild::config::Graph::load(graph) {
        Ok(g) => root.join(&g.settings.log_dir),
        Err(_) => root.join("work").join("build"),
    }
}

fn rel(p: &std::path::Path, root: &std::path::Path) -> String {
    p.strip_prefix(root)
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| p.display().to_string())
}

/// "3 min ago", "2 h ago", "4 days ago"
pub fn ago(t: SystemTime) -> String {
    let Ok(d) = SystemTime::now().duration_since(t) else {
        return "just now".into();
    };
    let s = d.as_secs();
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86_400),
    }
}

/// Only effective stage launchers require Python; a native tool's input/output filename cannot require it.
fn command_needs_python(command: &[String]) -> bool {
    let Some(program) = command.first() else { return false };
    let name = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    let executable = name.strip_suffix(".exe").unwrap_or(&name);
    matches!(executable, "py" | "python" | "pythonw")
        || executable.strip_prefix("python").is_some_and(|version| {
            !version.is_empty()
                && version
                    .chars()
                    .all(|character| character.is_ascii_digit() || character == '.')
        })
        || name.ends_with(".py")
        || name.ends_with(".pyw")
}

#[cfg(test)]
mod setup_prerequisite_tests {
    use super::command_needs_python;
    #[test]
    fn setup_prerequisite_checks_effective_launcher_only() {
        let command = |parts: &[&str]| parts.iter().map(|part| (*part).into()).collect::<Vec<String>>();
        for parts in [
            vec!["fox", "project", "check"],
            vec!["fox-place.exe", "--out", "python.py"],
            vec!["python-helper.exe", "preview"],
            vec![],
        ] {
            assert!(!command_needs_python(&command(&parts)), "{parts:?}");
        }
        for parts in [
            vec![r"C:\Python310\python.exe", "stage.py"],
            vec!["/usr/bin/python3.10", "stage.py"],
            vec!["py.exe", "-3", "stage.py"],
            vec!["tools/build/stage.py"],
        ] {
            assert!(command_needs_python(&command(&parts)), "{parts:?}");
        }
    }
}
