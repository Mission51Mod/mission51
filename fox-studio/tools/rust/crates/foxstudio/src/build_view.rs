//! Build view: every stage with its last run and why it would run (foxbuild's own `--status` logic, called as a
//! library), the dependency graph, and run controls that start `fox build ...` with its output streamed.
use crate::build_events::{BuildEvent, KnownEventKind};
use crate::graph;
use crate::logtail::LogTail;
use crate::output_diff::{OutputDiff, OutputDiffView, OutputSnapshot, SnapshotSource};
use crate::runner::{self, BuildCmd, BuildContext, BuildRun, LineKind, RunTarget};
use crate::tasks::Task;
use crate::theme::{self, pal};
use crate::timeline::{self, Timeline};
use eframe::egui::{self, Align, Color32, Layout, RichText, Sense, Stroke, StrokeKind, Vec2};
use egui_extras::{Column, TableBuilder};
use foxbuild::{StageStatus, StatusOptions, StatusReport};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// what the view needs from the app each frame
#[derive(Clone)]
pub struct Env {
    pub root: PathBuf,
    pub graph: PathBuf,
    /// Explicit M3 identity for the child-only environment; None for a standalone graph.
    pub project_spec: Option<PathBuf>,
    pub fox_exe: PathBuf,
    pub python: String,
    pub game_running: Option<String>,
    /// a foxbuild (from anywhere) holds the repo lock
    pub lock_pid: Option<u32>,
    pub confirm_builds: bool,
    /// false: real builds are refused (headless tests)
    pub allow_real_builds: bool,
    /// why real builds are not offered for the open project (an M3 spec before `fox build --project`)
    pub real_build_block: Option<String>,
    /// work/build/PAUSE exists (no new stage starts)
    pub paused: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StatusSource {
    root: PathBuf,
    graph: PathBuf,
    python: String,
    project_spec: Option<PathBuf>,
    fox_exe: PathBuf,
    context: Option<String>,
    generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RunSource {
    status: StatusSource,
    fox_exe: PathBuf,
}

impl Env {
    fn status_source(&self) -> StatusSource {
        StatusSource {
            root: self.root.clone(),
            graph: self.graph.clone(),
            python: self.python.clone(),
            project_spec: self.project_spec.clone(),
            fox_exe: self.fox_exe.clone(),
            context: None,
            generation: 0,
        }
    }

    fn run_source(&self) -> RunSource {
        RunSource {
            status: self.status_source(),
            fox_exe: self.fox_exe.clone(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SubView {
    Table,
    Graph,
    Timeline,
    Outputs,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogTab {
    Output,
    BuildLog,
    StageLog,
}

/// a stage's state in the current / last run, from the run's output
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Live {
    WouldRun,
    Maybe,
    Running,
    Done,
    Skipped,
    Failed,
    Cancelled,
    Blocked,
}

/// Session state comes only from validated stdout events, never human stderr.
pub fn live_event(event: &BuildEvent) -> Option<(Live, String)> {
    let state = match event.kind? {
        KnownEventKind::StagePlanned => match event.data.get("result")?.as_str()? {
            "run" => Live::WouldRun,
            "maybe" => Live::Maybe,
            "skip" => Live::Skipped,
            _ => return None,
        },
        KnownEventKind::StageStarted => Live::Running,
        KnownEventKind::StageFinished => match event.data.get("result")?.as_str()? {
            "succeeded" => Live::Done,
            "failed" => Live::Failed,
            "skipped" => Live::Skipped,
            "cancelled" => Live::Cancelled,
            "blocked" => Live::Blocked,
            _ => return None,
        },
        _ => return None,
    };
    Some((state, event.data.get("name")?.as_str()?.to_owned()))
}

/// "[  0:01] start  nav.ground (est ...)" -> (Running, "nav.ground")
pub fn parse_live(line: &str) -> Option<(Live, String)> {
    let body = match line.find("] ") {
        Some(i) if line.starts_with('[') => &line[i + 2..],
        _ => line,
    };
    let mut it = body.split_whitespace();
    let verb = it.next()?;
    let name = it.next()?;
    let st = match verb {
        "start" => Live::Running,
        "done" => Live::Done,
        "FAILED" => Live::Failed,
        "skip" => Live::Skipped,
        "RUN" => Live::WouldRun,
        "MAYBE" => Live::Maybe,
        _ => return None,
    };
    Some((st, name.to_string()))
}

/// the static state of a stage (from the status report)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StageState {
    Checking,
    UpToDate,
    NeverBuilt,
    Failed,
    Dirty,
}

pub fn stage_state(s: &StageStatus, checked: bool) -> StageState {
    if !checked {
        return StageState::Checking;
    }
    match s.dirty.as_deref() {
        None => StageState::UpToDate,
        Some("never built") => StageState::NeverBuilt,
        Some("last run failed") => StageState::Failed,
        Some(_) => StageState::Dirty,
    }
}

pub struct BuildView {
    generation: u64,
    context: Option<BuildContext>,
    output_before: Option<OutputSnapshot>,
    output_source: Option<SnapshotSource>,
    pub output_diff: Option<OutputDiff>,
    pub output_error: Option<String>,
    output_view: OutputDiffView,
    pub report: Option<StatusReport>,
    /// the report has dirty reasons (not just the quick structural load)
    pub checked: bool,
    status_task: Option<Task<StatusReport>>,
    status_source: Option<StatusSource>,
    status_for: Option<StatusSource>,
    refresh_pending: bool,
    confirmation_for: Option<RunSource>,
    confirmation_source: Option<Vec<u8>>,
    confirmation_graph: Option<PathBuf>,
    run_for: Option<RunSource>,
    pub status_error: Option<String>,
    status_at: Option<Instant>,
    pub selected: Option<String>,
    filter: String,
    pub sub: SubView,
    pub run: Option<BuildRun>,
    pub run_error: Option<String>,
    pub pending_confirm: Option<BuildCmd>,
    pub live: BTreeMap<String, Live>,
    live_lines: usize,
    pub log_tab: LogTab,
    build_log: Option<LogTail>,
    stage_log: Option<LogTail>,
    scene_rect: egui::Rect,
    /// the next graph frame fits the whole graph (Fit button); otherwise a fresh view opens readable, at the left
    fit_all: bool,
    layout: Option<(Vec<String>, graph::Layout, Vec<Vec<usize>>)>,
    last_lock: Option<u32>,
    /// stage starts / ends / results of the current run (or of the last run in build.log)
    pub timeline: Timeline,
    timeline_from_run: bool,
    timeline_log: Option<LogTail>,
    timeline_sig: (usize, usize),
    /// Stop pressed: ask before killing the build
    pub confirm_stop: bool,
    /// log panes show only problem lines (failures, fallbacks, notes)
    pub problems_only: bool,
}

impl Default for BuildView {
    fn default() -> Self {
        BuildView {
            generation: 0,
            context: None,
            output_before: None,
            output_source: None,
            output_diff: None,
            output_error: None,
            output_view: OutputDiffView::default(),
            report: None,
            checked: false,
            status_task: None,
            status_source: None,
            status_for: None,
            refresh_pending: false,
            confirmation_for: None,
            confirmation_source: None,
            confirmation_graph: None,
            run_for: None,
            status_error: None,
            status_at: None,
            selected: None,
            filter: String::new(),
            sub: SubView::Table,
            run: None,
            run_error: None,
            pending_confirm: None,
            live: BTreeMap::new(),
            live_lines: 0,
            log_tab: LogTab::Output,
            build_log: None,
            stage_log: None,
            scene_rect: egui::Rect::ZERO,
            fit_all: false,
            layout: None,
            last_lock: None,
            timeline: Timeline::default(),
            timeline_from_run: false,
            timeline_log: None,
            timeline_sig: (0, 0),
            confirm_stop: false,
            problems_only: false,
        }
    }
}

impl BuildView {
    /// Explicit opt-in after scheduler proof. None preserves the legacy CLI.
    /// A context change invalidates pending review/status, never a running child.
    pub fn set_context(&mut self, context: Option<BuildContext>) {
        if self.context.as_ref().map(BuildContext::key) != context.as_ref().map(BuildContext::key) {
            self.context = context;
            self.status_source = None;
            self.refresh_pending = true;
            self.pending_confirm = None;
            self.confirmation_for = None;
            self.confirmation_source = None;
            self.confirmation_graph = None;
        }
    }

    /// App calls this after a successful Save/Reload, including while Build is hidden.
    pub fn invalidate_project(&mut self) {
        self.generation = self.generation.saturating_add(1);
        self.output_before = None;
        self.output_source = None;
        self.output_diff = None;
        self.output_error = None;
        self.status_source = None;
        self.refresh_pending = true;
        self.pending_confirm = None;
        self.confirmation_for = None;
        self.confirmation_source = None;
        self.confirmation_graph = None;
    }

    pub fn project_changed(&mut self, ctx: &egui::Context, env: &Env) {
        self.invalidate_project();
        self.refresh_status(ctx, env);
    }

    fn source(&self, env: &Env) -> StatusSource {
        let mut source = env.status_source();
        source.context = self.context.as_ref().map(BuildContext::key);
        source.generation = self.generation;
        source
    }

    fn run_source(&self, env: &Env) -> RunSource {
        let mut source = env.run_source();
        source.status.context = self.context.as_ref().map(BuildContext::key);
        source.status.generation = self.generation;
        source
    }

    pub fn status_busy(&self) -> bool {
        self.status_task.is_some()
    }

    pub fn run_busy(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.is_running())
    }

    /// Invalidate only target-specific state. A running child is never dropped by changing projects/settings.
    fn sync_target(&mut self, env: &Env) {
        let source = self.source(env);
        if self.status_source.as_ref() != Some(&source) {
            self.status_source = Some(source);
            self.report = None;
            self.checked = false;
            self.status_error = None;
            self.status_at = None;
            self.refresh_pending = true;
            self.selected = None;
            self.filter.clear();
            self.layout = None;
            self.fit_all = false;
            self.build_log = None;
            self.stage_log = None;
            self.timeline_log = None;
            self.timeline = Timeline::default();
            self.timeline_from_run = false;
            self.timeline_sig = (0, 0);
            self.output_diff = None;
            self.output_error = None;
            self.last_lock = None;
            self.live.clear();
            self.live_lines = 0;
            self.pending_confirm = None;
            self.confirmation_for = None;
            self.confirmation_source = None;
            self.confirmation_graph = None;
            self.run_error = None;
            if !self.run_busy() {
                self.output_before = None;
                self.output_source = None;
                self.run = None;
                self.run_for = None;
                self.confirm_stop = false;
            }
        }
        if self.pending_confirm.is_some()
            && self.confirmation_for.as_ref() != Some(&self.run_source(env))
        {
            self.pending_confirm = None;
            self.confirmation_for = None;
            self.confirmation_source = None;
            self.confirmation_graph = None;
        }
    }

    /// Coalesce refreshes behind one worker. Older-target results are discarded, never displayed or saved.
    pub fn refresh_status(&mut self, ctx: &egui::Context, env: &Env) {
        self.sync_target(env);
        self.refresh_pending = true;
        self.checked = false;
        self.status_error = None;
        if self.status_task.is_some() {
            return;
        }
        let source = self.source(env);
        self.status_for = Some(source.clone());
        self.refresh_pending = false;
        let context = self.context.clone();
        self.status_task = Some(Task::spawn_serial("Reading build status", ctx, move |t| {
            let read = |check_dirty| {
                let opts = StatusOptions {
                    python: source.python.clone(),
                    check_dirty,
                    save_cache: false,
                };
                runner::read_status(
                    RunTarget {
                        root: &source.root,
                        graph: &source.graph,
                        python: &source.python,
                        fox_exe: &source.fox_exe,
                        project_spec: source.project_spec.as_deref(),
                    },
                    context.as_ref(),
                    &opts,
                )
            };
            t.progress(-1.0, "loading the graph");
            t.partial(read(false)?);
            t.progress(-1.0, "checking inputs (hashes)");
            read(true)
        }));
    }

    /// Start a `fox build` run (real builds only when allowed; the confirmation happens before this).
    pub fn start_run(&mut self, ctx: &egui::Context, env: &Env, cmd: BuildCmd) {
        self.sync_target(env);
        self.run_error = None;
        if self.run_busy() {
            self.run_error = Some("a run is already in progress".into());
            return;
        }
        if cmd.is_real_build() {
            if let Some(why) = &env.real_build_block {
                self.run_error = Some(format!(
                    "real builds are not available for this project: {why}"
                ));
                return;
            }
            if !env.allow_real_builds {
                self.run_error = Some("real builds are disabled in this session".into());
                return;
            }
            if let Some(pid) = env.lock_pid {
                self.run_error = Some(format!("another build (pid {pid}) is running on this repo"));
                return;
            }
        }
        self.output_before = None;
        self.output_source = None;
        self.output_diff = None;
        self.output_error = None;
        let target = RunTarget {
            fox_exe: &env.fox_exe,
            root: &env.root,
            graph: &env.graph,
            python: &env.python,
            project_spec: env.project_spec.as_deref(),
        };
        match BuildRun::start_with_context(target, cmd, ctx, self.context.as_ref()) {
            Ok(mut r) => {
                self.output_source = r.output_source.clone();
                if let Some(before) = r.before_output.take() {
                    match before {
                        Ok(snapshot) => self.output_before = snapshot,
                        Err(error) => self.output_error = Some(error),
                    }
                }
                self.run = Some(r);
                self.run_for = Some(self.run_source(env));
                self.timeline_from_run = false;
                self.timeline_sig = (0, 0);
                self.live.clear();
                self.live_lines = 0;
                self.log_tab = LogTab::Output;
            }
            Err(e) => self.run_error = Some(e),
        }
    }

    /// a run button was pressed: real builds ask first (unless the user turned that off)
    pub fn request_run(&mut self, ctx: &egui::Context, env: &Env, cmd: BuildCmd) {
        self.sync_target(env);
        self.run_error = None;
        if cmd.is_real_build() && env.confirm_builds {
            let target = RunTarget {
                fox_exe: &env.fox_exe,
                root: &env.root,
                graph: &env.graph,
                python: &env.python,
                project_spec: env.project_spec.as_deref(),
            };
            match runner::prepare_command(target, &cmd, self.context.as_ref(), None) {
                Ok(prepared) => {
                    self.confirmation_graph = Some(
                        prepared
                            .plan
                            .as_ref()
                            .map_or_else(|| env.graph.clone(), |plan| plan.graph_path().to_owned()),
                    )
                }
                Err(error) => {
                    self.run_error = Some(error);
                    return;
                }
            }
            match runner::review_source(&env.root, &env.graph, env.project_spec.as_deref()) {
                Ok(source) => self.confirmation_source = Some(source),
                Err(error) => {
                    self.run_error = Some(error);
                    return;
                }
            }
            self.confirmation_for = Some(self.run_source(env));
            self.pending_confirm = Some(cmd);
        } else {
            self.start_run(ctx, env, cmd);
        }
    }

    /// per-frame bookkeeping (also when the tab is not shown)
    pub fn poll(&mut self, ctx: &egui::Context, env: &Env) {
        self.sync_target(env);
        let current_status = self.status_for == self.status_source && !self.refresh_pending;
        if let Some(t) = self.status_task.as_mut() {
            let done = t.poll();
            if let Some(p) = t.take_partial()
                && current_status
            {
                self.report = Some(p);
                self.checked = false;
            }
            if done {
                let result = t.take_result();
                match result.filter(|_| current_status) {
                    Some(Ok(r)) => {
                        self.report = Some(r);
                        self.checked = true;
                        self.status_error = None;
                        self.status_at = Some(Instant::now());
                    }
                    Some(Err(e)) => {
                        self.report = None;
                        self.checked = false;
                        self.status_error = Some(e);
                    }
                    None => {}
                }
                self.status_task = None;
                self.status_for = None;
            }
        }
        let current_run = self.run_for.as_ref() == Some(&self.run_source(env));
        let mut finished_now = false;
        if let Some(r) = self.run.as_mut() {
            let was_running = r.is_running();
            let done = r.poll();
            if current_run && r.is_json() {
                self.live = r.live.clone();
            } else if current_run && r.lines.len() + r.dropped > self.live_lines {
                let skip = self.live_lines.saturating_sub(r.dropped);
                for l in r.lines.iter().skip(skip) {
                    if let Some((st, name)) = parse_live(l) {
                        self.live.insert(name, st);
                    }
                }
                self.live_lines = r.lines.len() + r.dropped;
            }
            finished_now = was_running && done;
        }
        self.update_timeline(ctx, env);
        if finished_now && current_run {
            self.finish_output_diff();
            // what the run changed shows in the status straight away
            self.refresh_status(ctx, env);
        }
        // an outside build finished (its lock went away): refresh
        if self.last_lock.is_some() && env.lock_pid.is_none() {
            self.refresh_status(ctx, env);
        }
        self.last_lock = env.lock_pid;
        if self.refresh_pending && self.status_task.is_none() {
            self.refresh_status(ctx, env);
        }
    }

    /// the timeline follows this session's run, else the last run in build.log (re-read when the file changes)
    fn update_timeline(&mut self, ctx: &egui::Context, env: &Env) {
        // a run started in this session (dry or real) is what the timeline shows
        if let Some(r) = self
            .run
            .as_ref()
            .filter(|_| self.run_for.as_ref() == Some(&self.run_source(env)))
        {
            let sig = if r.is_json() {
                (
                    usize::try_from(r.event_count).unwrap_or(usize::MAX),
                    usize::from(r.exit.is_some()),
                )
            } else {
                (r.lines.len(), r.dropped)
            };
            if sig != self.timeline_sig || !self.timeline_from_run {
                self.timeline = if r.is_json() {
                    r.timeline.clone()
                } else {
                    timeline::parse(r.lines.iter().map(|s| s.as_str()))
                };
                self.timeline_sig = sig;
                self.timeline_from_run = true;
            }
            return;
        }
        let log_dir = self
            .report
            .as_ref()
            .map(|r| r.log_dir.clone())
            .unwrap_or_else(|| env.root.join("work").join("build"));
        let path = log_dir.join("build.log");
        let tail = self
            .timeline_log
            .get_or_insert_with(|| LogTail::new(path.clone()));
        if tail.path() != path {
            *tail = LogTail::new(path);
        }
        tail.refresh(ctx, Duration::from_millis(2000));
        let sig = (
            tail.lines.len(),
            tail.lines.last().map(|l| l.len()).unwrap_or(0),
        );
        if sig != self.timeline_sig || self.timeline_from_run {
            self.timeline = timeline::parse(tail.lines.iter().map(|s| s.as_str()));
            self.timeline_sig = sig;
            self.timeline_from_run = false;
        }
    }

    fn finish_output_diff(&mut self) {
        let Some(mut source) = self.output_source.clone() else {
            return;
        };
        source.label = "After this run".into();
        let mut after = match OutputSnapshot::read(source) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.output_error = Some(error);
                None
            }
        };
        if let Some(run) = &self.run
            && !run.succeeded()
            && let Some(snapshot) = after.as_mut()
        {
            for (stage, state) in &self.live {
                if matches!(
                    state,
                    Live::Running | Live::Done | Live::Failed | Live::Cancelled | Live::Blocked
                ) {
                    snapshot.mark_stale(stage);
                }
            }
        }
        self.output_diff = Some(OutputDiff::compare(
            self.output_before.as_ref(),
            after.as_ref(),
        ));
    }

    pub fn show(&mut self, ui: &mut egui::Ui, env: &Env) {
        let ctx = ui.ctx().clone();
        self.sync_target(env);
        if self.refresh_pending && self.status_task.is_none() {
            self.refresh_status(&ctx, env);
        }
        self.toolbar(ui, env);
        ui.add_space(4.0);
        egui::Panel::bottom("build_logs")
            .resizable(true)
            .default_size(260.0)
            .min_size(120.0)
            .show(ui, |ui| {
                self.logs(ui, env);
            });
        egui::Panel::right("stage_details")
            .resizable(true)
            .default_size(340.0)
            .min_size(240.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("details_scroll")
                    .show(ui, |ui| self.details(ui, env));
            });
        egui::CentralPanel::no_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.sub, SubView::Table, "Stages");
                ui.selectable_value(&mut self.sub, SubView::Graph, "Graph");
                ui.selectable_value(&mut self.sub, SubView::Timeline, "Timeline");
                ui.selectable_value(&mut self.sub, SubView::Outputs, "Outputs");
                ui.separator();
                ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter stages / owners").desired_width(220.0));
                if !self.filter.is_empty() && ui.small_button("Clear").clicked() {
                    self.filter.clear();
                }
                if self.sub == SubView::Graph && ui.button("Fit").on_hover_text("Fit the whole graph in view").clicked() {
                    self.fit_all = true;
                }
            });
            ui.add_space(2.0);
            match self.sub {
                SubView::Table => self.table(ui),
                SubView::Graph => self.graph(ui),
                SubView::Outputs => {
                    if let Some(error) = &self.output_error { ui.colored_label(pal(ui).warn, error); }
                    if let Some(diff) = &self.output_diff { self.output_view.show(ui, diff); }
                    else { ui.label("Output comparison appears after a real build for this target. Missing metadata is shown explicitly."); }
                }
                SubView::Timeline => {
                    let p = pal(ui);
                    self.run_summary(ui, false);
                    let now = self.run.as_ref().filter(|r| r.is_running() && self.timeline_from_run).map(|r| r.elapsed().as_secs_f32());
                    if let Some(n) = timeline::chart(ui, &self.timeline, now, self.selected.as_deref(), &p) {
                        self.select(Some(n));
                    }
                }
            }
        });
        self.confirm_modal(&ctx, env);
        self.stop_modal(ui.ctx());
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, env: &Env) {
        let ctx = ui.ctx().clone();
        let p = pal(ui);
        ui.horizontal(|ui| {
            let busy = self.status_busy();
            if ui.add_enabled(!busy, egui::Button::new("Refresh")).on_hover_text("Re-read the graph and the build state; check every stage's inputs").clicked() {
                self.refresh_status(&ctx, env);
            }
            let running = self.run_busy();
            if ui.add_enabled(!running, egui::Button::new("Dry run")).on_hover_text("Show what a build would run, and why (runs nothing)").clicked() {
                self.request_run(&ctx, env, BuildCmd::DryRun { only: vec![], from: vec![] });
            }
            let can_build = !running && env.lock_pid.is_none() && env.real_build_block.is_none();
            let tip = env.real_build_block.clone().unwrap_or_else(|| "Incremental build of every default stage (fox build)".into());
            if theme::primary_button(ui, "Build", can_build).on_hover_text(tip).on_disabled_hover_text(env.real_build_block.clone().unwrap_or_default()).clicked() {
                self.request_run(&ctx, env, BuildCmd::Build { only: vec![], from: vec![] });
            }
            let sel = self.selected.clone();
            ui.add_enabled_ui(!running && sel.is_some(), |ui| {
                ui.menu_button("Selected stage", |ui| {
                    if let Some(s) = &sel {
                        if ui.button(format!("Dry run from {s}")).clicked() {
                            self.request_run(&ctx, env, BuildCmd::DryRun { only: vec![], from: vec![s.clone()] });
                            ui.close();
                        }
                        if ui.button(format!("Dry run only {s}")).clicked() {
                            self.request_run(&ctx, env, BuildCmd::DryRun { only: vec![s.clone()], from: vec![] });
                            ui.close();
                        }
                        ui.separator();
                        if ui.add_enabled(can_build, egui::Button::new(format!("Build only {s}"))).clicked() {
                            self.request_run(&ctx, env, BuildCmd::Build { only: vec![s.clone()], from: vec![] });
                            ui.close();
                        }
                        if ui.add_enabled(can_build, egui::Button::new(format!("Build from {s} (forces it and everything after)"))).clicked() {
                            self.request_run(&ctx, env, BuildCmd::Build { only: vec![], from: vec![s.clone()] });
                            ui.close();
                        }
                    }
                });
            });
            if running {
                let requested = self.run.as_ref().is_some_and(|run| run.cancel_requested);
                if requested { ui.colored_label(p.warn, "Cancellation requested"); }
                let force = self.run.as_ref().is_some_and(BuildRun::can_force_stop);
                if ui.add_enabled(!requested || force, egui::Button::new(RichText::new(if force { "Force stop" } else { "Stop" }).color(p.err))).on_hover_text("Request cancellation; force stop is available if the build cannot stop safely").clicked() {
                    self.confirm_stop = true;
                }
            }
            if busy {
                ui.spinner();
                let phase = if self.status_for != self.status_source {
                    "Finishing the previous target's check".to_string()
                } else {
                    self.status_task.as_ref().map(|t| t.phase.clone()).unwrap_or_default()
                };
                ui.label(RichText::new(phase).color(p.muted));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                match self.report.as_ref().and_then(|r| r.rust_tools.as_ref()) {
                    Some(Ok(false)) => {
                        theme::pill(ui, "Rust tools stale", p.warn)
                            .on_hover_text("sources changed since the last build of fox.exe / fox-place.exe: the next build rebuilds them first (python tools/rust/build.py)");
                    }
                    Some(Err(e)) => {
                        theme::pill(ui, "Rust tools: unknown", p.warn).on_hover_text(e.clone());
                    }
                    _ => {}
                }
                if env.paused {
                    theme::pill(ui, "paused", p.warn).on_hover_text(
                        "work/build/PAUSE exists: running stages finish, no new stage starts (tools/build/pause.py; a watchdog resumes after at most 20 min)");
                }
                let pause_tool = env.root.join("tools").join("build").join("pause.py");
                if (env.lock_pid.is_some() || env.paused) && pause_tool.is_file() {
                    let (label, arg, tip) = if env.paused {
                        ("Resume", "off", "Remove the pause: stages start again (pause.py off)")
                    } else {
                        ("Pause", "on", "No new stage starts until you resume; the running ones finish (pause.py on; auto-resume after 20 min)")
                    };
                    if ui.small_button(label).on_hover_text(tip).clicked() {
                        self.run_error = run_pause_tool(&env.python, &pause_tool, &env.root, arg).err();
                    }
                }
                if let Some(pid) = env.lock_pid {
                    let mine = self.run.as_ref().and_then(|r| r.pid()) == Some(pid);
                    theme::pill(ui, &if mine { "building".to_string() } else { format!("build running elsewhere (pid {pid})") }, p.info);
                }
                if let Some(r) = &self.report {
                    let mut c = [0usize; 5];
                    for s in &r.stages {
                        c[stage_state(s, self.checked) as usize] += 1;
                    }
                    if self.checked {
                        let (failed, never, dirty) =
                            (c[StageState::Failed as usize], c[StageState::NeverBuilt as usize], c[StageState::Dirty as usize]);
                        if failed > 0 {
                            theme::pill(ui, &format!("{failed} failed"), p.err);
                        }
                        theme::pill(ui, &format!("{} would run", failed + never + dirty), p.warn)
                            .on_hover_text(format!("{dirty} changed, {never} never built, {failed} failed last time"));
                        theme::pill(ui, &format!("{} up to date", c[StageState::UpToDate as usize]), p.ok);
                    }
                    ui.label(RichText::new(format!("{} stages", r.stages.len())).color(p.muted));
                }
            });
        });
        if let Some(source) = self
            .run_for
            .as_ref()
            .filter(|s| *s != &self.run_source(env))
        {
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "Run output belongs to {} · {}",
                        source.status.root.display(),
                        source.status.graph.display()
                    ))
                    .monospace(),
                )
                .wrap()
                .selectable(true),
            );
        }
        if self
            .run_for
            .as_ref()
            .is_some_and(|source| source.status.generation != self.generation)
        {
            ui.label("This run uses project settings from before the last Save or Reload.");
        }
        if let Some(e) = &self.status_error {
            ui.colored_label(p.err, format!("Could not read the build status: {e}"));
        }
        if let Some(e) = &self.run_error {
            ui.colored_label(p.err, e);
        }
        if let Some(error) = self.run.as_ref().and_then(|run| run.control_error.as_ref()) {
            ui.colored_label(p.err, error);
        }
    }

    fn visible(&self, s: &StageStatus) -> bool {
        if self.filter.trim().is_empty() {
            return true;
        }
        let f = self.filter.trim().to_lowercase();
        s.name.to_lowercase().contains(&f) || s.owner.to_lowercase().contains(&f)
    }

    /// colour + short text for a stage, the live run state first
    fn state_of(&self, ui: &egui::Ui, s: &StageStatus) -> (Color32, String) {
        let p = pal(ui);
        if let Some(l) = self.live.get(&s.name) {
            return match l {
                Live::Running => (p.info, "running".into()),
                Live::Done => (p.ok, "built (this run)".into()),
                Live::Skipped => (p.ok, "up to date (this run)".into()),
                Live::Failed => (p.err, "FAILED (this run)".into()),
                Live::Cancelled => (p.warn, "cancelled (this run)".into()),
                Live::Blocked => (p.warn, "blocked (this run)".into()),
                Live::WouldRun => (
                    p.warn,
                    format!("would run: {}", s.dirty.as_deref().unwrap_or("forced")),
                ),
                Live::Maybe => (p.warn, "maybe (if upstream changes its inputs)".into()),
            };
        }
        let (c, t) = match stage_state(s, self.checked) {
            StageState::Checking => (p.muted, "checking…".to_string()),
            StageState::UpToDate => (p.ok, "up to date".to_string()),
            StageState::NeverBuilt => (p.muted, "never built".to_string()),
            StageState::Failed => (p.err, "last run failed".to_string()),
            StageState::Dirty => (p.warn, s.dirty.clone().unwrap_or_default()),
        };
        // a Rust-routed step fell back to its Python reference last time: never silent
        if self.has_fallback(s) {
            (p.err, format!("{t} · ran Python fallback"))
        } else {
            (c, t)
        }
    }

    /// the stage's last run (status) or this run (timeline) used a Python fallback
    pub fn has_fallback(&self, s: &StageStatus) -> bool {
        !s.fallbacks.is_empty()
            || self
                .timeline
                .span(&s.name)
                .is_some_and(|x| !x.fallbacks.is_empty())
    }

    /// summary of the run shown in the timeline: result line, failures, fallbacks, blocked stages
    fn run_summary(&mut self, ui: &mut egui::Ui, compact: bool) {
        let p = pal(ui);
        let tl = &self.timeline;
        if tl.header.is_none() {
            return;
        }
        let mut goto: Option<String> = None;
        let failed: Vec<(String, String)> = tl
            .failed()
            .iter()
            .map(|s| (s.name.clone(), s.failure.clone().unwrap_or_default()))
            .collect();
        let fb: Vec<(String, Vec<String>)> = tl
            .with_fallbacks()
            .iter()
            .map(|s| (s.name.clone(), s.fallbacks.clone()))
            .collect();
        ui.horizontal_wrapped(|ui| {
            let src = if self.timeline_from_run {
                "this run"
            } else {
                "last run (build.log)"
            };
            ui.label(RichText::new(src).strong());
            if tl.dry_run {
                theme::pill(ui, "dry run", p.muted);
            }
            match &tl.summary {
                Some(sm) => {
                    ui.label(RichText::new(sm).color(if sm.contains("FAILURES") {
                        p.err
                    } else {
                        p.muted
                    }));
                }
                None if self.run_busy() && self.timeline_from_run => {
                    ui.label(
                        RichText::new(format!("{} stage(s) started", tl.spans.len())).color(p.info),
                    );
                }
                None => {
                    ui.label(
                        RichText::new("did not finish (stopped, or still running elsewhere)")
                            .color(p.warn),
                    );
                }
            }
            if !failed.is_empty() {
                theme::pill(ui, &format!("{} failed", failed.len()), p.err);
            }
            if !tl.blocked.is_empty() {
                theme::pill(ui, &format!("{} blocked", tl.blocked.len()), p.warn)
                    .on_hover_text(tl.blocked.join("\n"));
            }
            if !fb.is_empty() {
                theme::pill(ui, &format!("{} Python fallback", fb.len()), p.err).on_hover_text(
                    "Rust-routed steps ran their Python reference (stale or missing Rust tools)",
                );
            }
        });
        if !compact {
            for (n, why) in &failed {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("✖").color(p.err));
                    if ui.link(RichText::new(n).monospace()).clicked() {
                        goto = Some(n.clone());
                    }
                    ui.label(RichText::new(why).small().color(p.err));
                });
            }
            for (n, f) in &fb {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("PY").small().strong().color(p.err));
                    if ui.link(RichText::new(n).monospace()).clicked() {
                        goto = Some(n.clone());
                    }
                    ui.label(RichText::new(f.join("; ")).small().color(p.muted));
                });
            }
            ui.add_space(4.0);
        }
        if let Some(g) = goto {
            self.select(Some(g));
        }
    }

    fn stop_modal(&mut self, ctx: &egui::Context) {
        if !self.confirm_stop {
            return;
        }
        if !self.run_busy() {
            self.confirm_stop = false;
            return;
        }
        let mut decision: Option<bool> = None;
        egui::Modal::new(egui::Id::new("confirm_stop")).show(ctx, |ui| {
            let p = pal(ui);
            ui.set_max_width(460.0);
            ui.heading("Stop the build?");
            let cooperative = self.run.as_ref().is_some_and(|run| run.is_json() && !run.can_force_stop());
            ui.label(if cooperative { "Request cancellation and wait for the scheduler to stop. Finished stages keep their results; the actual exit confirms cancellation." }
                else { "Stages in flight are killed (their outputs may be partial and they re-run next time). Stages that already finished keep their results." });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Keep building").clicked() {
                    decision = Some(false);
                }
                if ui.button(RichText::new("Stop the build").strong().color(p.err)).clicked() {
                    decision = Some(true);
                }
            });
        });
        match decision {
            Some(true) => {
                if let Some(r) = self.run.as_mut() {
                    if r.can_force_stop() {
                        r.force_stop();
                    } else {
                        r.stop();
                    }
                }
                self.confirm_stop = false;
            }
            Some(false) => self.confirm_stop = false,
            None => {}
        }
    }

    fn table(&mut self, ui: &mut egui::Ui) {
        let Some(report) = &self.report else {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                if self.status_error.is_none() {
                    ui.spinner();
                    ui.label("Reading the build graph…");
                }
            });
            return;
        };
        let rows: Vec<usize> = (0..report.stages.len())
            .filter(|&i| self.visible(&report.stages[i]))
            .collect();
        let row_h = 24.0;
        let mut clicked: Option<String> = None;
        let selected = self.selected.clone();
        let states: Vec<(Color32, String)> = rows
            .iter()
            .map(|&i| self.state_of(ui, &report.stages[i]))
            .collect();
        let fallbacks: Vec<bool> = rows
            .iter()
            .map(|&i| self.has_fallback(&report.stages[i]))
            .collect();
        let p = pal(ui);
        // row clicks select; labels must not swallow them as text selection
        ui.style_mut().interaction.selectable_labels = false;
        TableBuilder::new(ui)
            .id_salt("stage_table")
            .striped(true)
            .resizable(true)
            .sense(Sense::click())
            .auto_shrink([false, false])
            .max_scroll_height(f32::INFINITY)
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::exact(18.0))
            .column(Column::exact(22.0))
            .column(Column::initial(190.0).at_least(110.0).clip(true))
            .column(Column::initial(80.0).at_least(50.0).clip(true))
            .column(Column::initial(150.0).at_least(60.0).clip(true))
            .column(Column::initial(120.0).at_least(70.0))
            .column(Column::initial(60.0).at_least(45.0))
            .column(Column::initial(70.0).at_least(50.0))
            .column(Column::remainder().at_least(120.0).clip(true))
            .header(22.0, |mut h| {
                for t in ["", "", "Stage", "Owner", "Depends on", "Last run", "Seconds", "Peak MB", "State"] {
                    h.col(|ui| {
                        ui.label(RichText::new(t).strong());
                    });
                }
            })
            .body(|body| {
                body.rows(row_h, rows.len(), |mut row| {
                    let k = row.index();
                    let s = &report.stages[rows[k]];
                    row.set_selected(selected.as_deref() == Some(s.name.as_str()));
                    let (color, text) = &states[k];
                    row.col(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 4.5, *color);
                    });
                    row.col(|ui| {
                        if fallbacks[k] {
                            ui.label(RichText::new("PY").small().strong().color(p.err))
                                .on_hover_text("ran its Python fallback: a Rust-routed step used the Python reference");
                        }
                    });
                    row.col(|ui| {
                        let mut t = RichText::new(&s.name).monospace();
                        if !s.default {
                            t = t.italics().color(p.muted);
                        }
                        ui.label(t).on_hover_text(if s.default { s.name.clone() } else { format!("{} (on demand only)", s.name) });
                    });
                    row.col(|ui| {
                        ui.label(&s.owner);
                    });
                    row.col(|ui| {
                        let d = if s.deps.is_empty() { "—".to_string() } else { s.deps.join(", ") };
                        ui.label(RichText::new(d).small()).on_hover_text(s.deps.join("\n"));
                    });
                    row.col(|ui| {
                        ui.label(s.last_run.as_deref().map(short_stamp).unwrap_or_else(|| "—".into()));
                    });
                    row.col(|ui| {
                        ui.label(if s.seconds > 0.0 { format!("{:.0}", s.seconds) } else { "—".into() });
                    });
                    row.col(|ui| {
                        ui.label(if s.peak_mb > 0.0 { format!("{:.0}", s.peak_mb) } else { "—".into() });
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(text).color(*color)).on_hover_text(text);
                    });
                    if row.response().clicked() {
                        clicked = Some(s.name.clone());
                    }
                });
            });
        if let Some(c) = clicked {
            self.select(Some(c));
        }
    }

    pub fn select(&mut self, name: Option<String>) {
        if self.selected != name {
            self.selected = name;
            self.stage_log = None;
        }
    }

    fn graph(&mut self, ui: &mut egui::Ui) {
        let Some(report) = &self.report else {
            ui.spinner();
            return;
        };
        let names: Vec<String> = report.stages.iter().map(|s| s.name.clone()).collect();
        let deps_now: Vec<Vec<usize>> = {
            let idx: BTreeMap<&str, usize> = names
                .iter()
                .enumerate()
                .map(|(i, n)| (n.as_str(), i))
                .collect();
            report
                .stages
                .iter()
                .map(|s| {
                    s.deps
                        .iter()
                        .filter_map(|d| idx.get(d.as_str()).copied())
                        .collect()
                })
                .collect()
        };
        if self
            .layout
            .as_ref()
            .map(|(n, _, d)| n != &names || d != &deps_now)
            .unwrap_or(true)
        {
            self.layout = Some((names.clone(), graph::layout(&deps_now), deps_now.clone()));
        }
        let (_, lay, deps) = self.layout.as_ref().unwrap();
        let sel = self
            .selected
            .as_ref()
            .and_then(|s| names.iter().position(|n| n == s));
        let (up, down) = match sel {
            Some(i) => (graph::reach(deps, i, true), graph::reach(deps, i, false)),
            None => (vec![false; names.len()], vec![false; names.len()]),
        };
        let states: Vec<(Color32, String)> =
            report.stages.iter().map(|s| self.state_of(ui, s)).collect();
        let fallbacks: Vec<bool> = report.stages.iter().map(|s| self.has_fallback(s)).collect();
        let visible: Vec<bool> = report.stages.iter().map(|s| self.visible(s)).collect();
        let filtering = !self.filter.trim().is_empty();
        let p = pal(ui);
        let (w, h, gx, gy) = (200.0f32, 40.0f32, 70.0f32, 14.0f32);
        let pos =
            |i: usize| egui::pos2(lay.layer[i] as f32 * (w + gx), lay.row[i] as f32 * (h + gy));
        let mut clicked: Option<usize> = None;
        let frame = egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .corner_radius(6);
        frame.show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            let content = egui::Rect::from_min_size(
                egui::pos2(-24.0, -24.0),
                Vec2::new(
                    lay.layers as f32 * (w + gx) - gx + 48.0,
                    lay.max_rows as f32 * (h + gy) - gy + 48.0,
                ),
            );
            if self.fit_all {
                self.scene_rect = content;
                self.fit_all = false;
            } else if self.scene_rect == egui::Rect::ZERO || !self.scene_rect.is_finite() {
                // first view: readable zoom, starting at the graph's sources, centred vertically
                let size = ui.available_size().max(Vec2::splat(100.0)) / 0.75;
                let mut min = content.min;
                if content.height() < size.y {
                    min.y = content.center().y - size.y / 2.0;
                }
                self.scene_rect = egui::Rect::from_min_size(min, size);
            }
            let mut rect = self.scene_rect;
            egui::Scene::new()
                .zoom_range(0.15..=2.0)
                .show(ui, &mut rect, |ui| {
                    let painter = ui.painter().clone();
                    // edges first
                    for (i, ds) in deps.iter().enumerate() {
                        for &d in ds {
                            let a = pos(d) + Vec2::new(w, h / 2.0);
                            let b = pos(i) + Vec2::new(0.0, h / 2.0);
                            let hot = sel.is_some_and(|s| {
                                (s == i && up[d])
                                    || (s == d && down[i])
                                    || (up[i] && up[d])
                                    || (down[i] && down[d])
                                    || (s == i)
                                    || (s == d)
                            });
                            let faded = filtering && !(visible[i] && visible[d]);
                            let color = if hot {
                                p.accent
                            } else if faded {
                                p.muted.gamma_multiply(0.15)
                            } else {
                                p.muted.gamma_multiply(0.45)
                            };
                            let dx = ((b.x - a.x) * 0.5).max(30.0);
                            let bez = egui::epaint::CubicBezierShape::from_points_stroke(
                                [a, a + Vec2::new(dx, 0.0), b - Vec2::new(dx, 0.0), b],
                                false,
                                Color32::TRANSPARENT,
                                Stroke::new(if hot { 2.0 } else { 1.0 }, color),
                            );
                            painter.add(bez);
                        }
                    }
                    for (i, s) in report.stages.iter().enumerate() {
                        let r = egui::Rect::from_min_size(pos(i), Vec2::new(w, h));
                        let resp = ui.interact(r, ui.id().with(("stage_node", i)), Sense::click());
                        let (color, text) = &states[i];
                        let dim = filtering && !visible[i];
                        let is_sel = sel == Some(i);
                        let fill = if is_sel {
                            p.accent.gamma_multiply(0.22)
                        } else if resp.hovered() {
                            p.node.gamma_multiply(1.15)
                        } else {
                            p.node
                        };
                        painter.rect_filled(
                            r,
                            6,
                            if dim { fill.gamma_multiply(0.4) } else { fill },
                        );
                        painter.rect_stroke(
                            r,
                            6,
                            Stroke::new(
                                if is_sel { 2.0 } else { 1.0 },
                                if is_sel {
                                    p.accent
                                } else {
                                    color.gamma_multiply(0.8)
                                },
                            ),
                            StrokeKind::Inside,
                        );
                        painter.rect_filled(
                            egui::Rect::from_min_size(r.min, Vec2::new(5.0, h)),
                            egui::CornerRadius {
                                nw: 6,
                                sw: 6,
                                ne: 0,
                                se: 0,
                            },
                            *color,
                        );
                        let tc = if dim {
                            p.muted.gamma_multiply(0.6)
                        } else {
                            ui.visuals().strong_text_color()
                        };
                        painter.text(
                            r.min + Vec2::new(12.0, 7.0),
                            egui::Align2::LEFT_TOP,
                            &s.name,
                            egui::FontId::monospace(12.5),
                            tc,
                        );
                        let small: String = text.chars().take(30).collect();
                        painter.text(
                            r.min + Vec2::new(12.0, 23.0),
                            egui::Align2::LEFT_TOP,
                            small,
                            egui::FontId::proportional(10.5),
                            color.gamma_multiply(if dim { 0.5 } else { 1.0 }),
                        );
                        if fallbacks[i] {
                            let b = egui::Rect::from_min_size(
                                r.right_top() + Vec2::new(-26.0, 4.0),
                                Vec2::new(22.0, 13.0),
                            );
                            painter.rect_filled(b, 3, p.err);
                            painter.text(
                                b.center(),
                                egui::Align2::CENTER_CENTER,
                                "PY",
                                egui::FontId::proportional(9.5),
                                Color32::WHITE,
                            );
                        }
                        if resp.clicked() {
                            clicked = Some(i);
                        }
                        resp.on_hover_ui(|ui| {
                            ui.label(RichText::new(&s.name).monospace().strong());
                            ui.label(text);
                            if !s.deps.is_empty() {
                                ui.label(format!("after: {}", s.deps.join(", ")));
                            }
                        });
                    }
                });
            self.scene_rect = rect;
        });
        if let Some(i) = clicked {
            self.select(Some(names[i].clone()));
        }
    }

    fn details(&mut self, ui: &mut egui::Ui, env: &Env) {
        let ctx = ui.ctx().clone();
        let p = pal(ui);
        let Some(report) = &self.report else { return };
        let Some(name) = self.selected.clone() else {
            ui.add_space(8.0);
            ui.label(
                RichText::new("Select a stage to see its command, dependencies and last run.")
                    .color(p.muted),
            );
            ui.add_space(12.0);
            if self.timeline.header.is_some() {
                theme::section(ui, "Run");
                self.run_summary(ui, false);
                ui.add_space(8.0);
            }
            let Some(report) = &self.report else { return };
            theme::section(ui, "Graph");
            ui.label(RichText::new(rel(&report.config, &report.root)).monospace());
            if !report.pinned.is_empty() {
                ui.add_space(8.0);
                theme::section(ui, "Pinned inputs");
                for pn in &report.pinned {
                    ui.label(RichText::new(&pn.path).monospace().small());
                    ui.label(
                        RichText::new(format!(
                            "{}  ({})",
                            if pn.hash.is_empty() { "…" } else { &pn.hash },
                            pn.owner
                        ))
                        .small()
                        .color(p.muted),
                    );
                }
            }
            if !report.notes.is_empty() {
                ui.add_space(8.0);
                theme::section(ui, "Notes");
                for n in &report.notes {
                    ui.label(RichText::new(n).small().color(p.warn));
                }
            }
            return;
        };
        let Some(s) = report.stages.iter().find(|s| s.name == name).cloned() else {
            ui.label("The selected stage is not in the graph any more.");
            return;
        };
        let users: Vec<String> = report
            .stages
            .iter()
            .filter(|x| x.deps.contains(&s.name))
            .map(|x| x.name.clone())
            .collect();
        let (color, text) = self.state_of(ui, &s);
        ui.label(RichText::new(&s.name).monospace().size(17.0).strong());
        ui.horizontal_wrapped(|ui| {
            if !s.owner.is_empty() {
                theme::pill(ui, &s.owner, p.info);
            }
            if !s.default {
                theme::pill(ui, "on demand", p.muted);
            }
            if s.gpu {
                theme::pill(ui, "GPU", p.warn);
            }
            if s.verify {
                theme::pill(ui, "verify", p.ok);
            }
            if !s.deterministic {
                theme::pill(ui, "non-deterministic", p.warn);
            }
            if s.rust {
                theme::pill(ui, "Rust tools", p.info).on_hover_text(
                    "routes work into fox.exe / fox-place.exe: a build makes them fresh first",
                );
            }
        });
        let span = self.timeline.span(&s.name).cloned();
        let run_fb: Vec<String> = span
            .as_ref()
            .map(|x| x.fallbacks.clone())
            .unwrap_or_default();
        if !s.fallbacks.is_empty() || !run_fb.is_empty() {
            ui.add_space(6.0);
            ui.colored_label(p.err, RichText::new("Python fallback used").strong())
                .on_hover_text("a Rust-routed step ran its Python reference instead (stale or missing Rust tools)");
            for f in s
                .fallbacks
                .iter()
                .chain(run_fb.iter().filter(|f| !s.fallbacks.contains(f)))
            {
                ui.label(RichText::new(f).small().color(p.err));
            }
        }
        let failed_now = span.as_ref().is_some_and(|x| x.state == Live::Failed);
        if failed_now || s.last_ok == Some(false) {
            ui.add_space(6.0);
            egui::Frame::new()
                .fill(p.err.gamma_multiply(0.10))
                .stroke(Stroke::new(1.0, p.err.gamma_multiply(0.6)))
                .corner_radius(6)
                .inner_margin(8)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("Failed").strong().color(p.err));
                    if let Some(why) = span.as_ref().and_then(|x| x.failure.clone()) {
                        ui.add(egui::Label::new(RichText::new(why).small()).wrap());
                    }
                    // the end of the stage's own log, where the error usually is
                    let log_dir = report.log_dir.clone();
                    let path = log_dir.join("logs").join(format!("{}.log", s.name));
                    let tail = self
                        .stage_log
                        .get_or_insert_with(|| LogTail::new(path.clone()));
                    if tail.path() != path {
                        *tail = LogTail::new(path);
                    }
                    tail.refresh(&ctx, Duration::from_millis(1500));
                    let errs: Vec<&String> = tail.lines.iter().rev().take(14).collect();
                    if !errs.is_empty() {
                        egui::Frame::new()
                            .fill(ui.visuals().code_bg_color)
                            .corner_radius(4)
                            .inner_margin(6)
                            .show(ui, |ui| {
                                for l in errs.into_iter().rev() {
                                    let c = if runner::classify(l) == LineKind::Failed
                                        || l.contains("Error")
                                        || l.contains("Traceback")
                                    {
                                        p.err
                                    } else {
                                        ui.visuals().text_color()
                                    };
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(l).monospace().size(11.0).color(c),
                                        )
                                        .wrap(),
                                    );
                                }
                            });
                    }
                    if ui.small_button("Open the stage log").clicked() {
                        self.log_tab = LogTab::StageLog;
                    }
                });
        }
        ui.add_space(6.0);
        ui.label(RichText::new(&text).color(color));
        ui.add_space(8.0);
        egui::Grid::new("stage_facts")
            .num_columns(2)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                let mut fact = |k: &str, v: String| {
                    ui.label(RichText::new(k).color(p.muted));
                    ui.label(v);
                    ui.end_row();
                };
                fact(
                    "Last run",
                    match (&s.last_run, s.last_ok) {
                        (Some(t), Some(true)) => t.clone(),
                        (Some(t), Some(false)) => format!("{t} (then failed)"),
                        (None, Some(false)) => "failed".into(),
                        _ => "never".into(),
                    },
                );
                fact(
                    "Duration",
                    if s.seconds > 0.0 {
                        format!("{:.1} s", s.seconds)
                    } else {
                        "—".into()
                    },
                );
                fact(
                    "Peak memory",
                    if s.peak_mb > 0.0 {
                        format!("{:.0} MB", s.peak_mb)
                    } else {
                        format!("estimate {:.1} GB", s.mem_gb)
                    },
                );
                fact(
                    "Traced I/O",
                    format!("{} inputs, {} outputs", s.inputs, s.outputs),
                );
                if !s.locks.is_empty() {
                    fact("Locks", s.locks.join(", "));
                }
            });
        ui.add_space(8.0);
        theme::section(ui, "Command");
        egui::Frame::new()
            .fill(ui.visuals().code_bg_color)
            .corner_radius(4)
            .inner_margin(6)
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(RichText::new(s.cmd.join(" ")).monospace().size(12.0))
                        .selectable(true)
                        .wrap(),
                );
            });
        ui.add_space(8.0);
        let mut goto: Option<String> = None;
        theme::section(ui, "Runs after");
        if s.deps.is_empty() {
            ui.label(RichText::new("nothing (a root stage)").color(p.muted));
        }
        ui.horizontal_wrapped(|ui| {
            for d in &s.deps {
                let learned = !s.declared.contains(d);
                let r = ui
                    .link(RichText::new(d).monospace())
                    .on_hover_text(if learned {
                        "learned from traced I/O (not declared in the graph file)"
                    } else {
                        "declared"
                    });
                if learned {
                    ui.label(RichText::new("learned").small().color(p.warn));
                }
                if r.clicked() {
                    goto = Some(d.clone());
                }
            }
        });
        ui.add_space(6.0);
        theme::section(ui, "Needed by");
        if users.is_empty() {
            ui.label(RichText::new("nothing").color(p.muted));
        }
        ui.horizontal_wrapped(|ui| {
            for u in &users {
                if ui.link(RichText::new(u).monospace()).clicked() {
                    goto = Some(u.clone());
                }
            }
        });
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            let can = !self.run_busy();
            if ui
                .add_enabled(can, egui::Button::new("Dry run from here"))
                .clicked()
            {
                self.request_run(
                    &ctx,
                    env,
                    BuildCmd::DryRun {
                        only: vec![],
                        from: vec![s.name.clone()],
                    },
                );
            }
            let can_build = can && env.lock_pid.is_none() && env.real_build_block.is_none();
            if ui
                .add_enabled(can_build, egui::Button::new("Build only this"))
                .clicked()
            {
                self.request_run(
                    &ctx,
                    env,
                    BuildCmd::Build {
                        only: vec![s.name.clone()],
                        from: vec![],
                    },
                );
            }
            if ui
                .add_enabled(can_build, egui::Button::new("Build from here"))
                .on_hover_text("Forces this stage and every default stage after it")
                .clicked()
            {
                self.request_run(
                    &ctx,
                    env,
                    BuildCmd::Build {
                        only: vec![],
                        from: vec![s.name.clone()],
                    },
                );
            }
            if ui.button("Stage log").clicked() {
                self.log_tab = LogTab::StageLog;
            }
        });
        if let Some(g) = goto {
            self.select(Some(g));
        }
    }

    fn logs(&mut self, ui: &mut egui::Ui, env: &Env) {
        let ctx = ui.ctx().clone();
        let p = pal(ui);
        let log_dir = self
            .report
            .as_ref()
            .map(|r| r.log_dir.clone())
            .unwrap_or_else(|| env.root.join("work").join("build"));
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.log_tab, LogTab::Output, "Run output");
            ui.selectable_value(&mut self.log_tab, LogTab::BuildLog, "build.log");
            let sl = match &self.selected {
                Some(s) => format!("Stage log: {s}"),
                None => "Stage log".into(),
            };
            ui.selectable_value(&mut self.log_tab, LogTab::StageLog, sl);
            ui.separator();
            ui.checkbox(&mut self.problems_only, "Problems only")
                .on_hover_text("Failures, Python fallbacks, notes and warnings");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(r) = &self.run {
                    let (c, t) = if let Some(completion) = &r.completion {
                        let color = match completion.outcome {
                            crate::build_events::CompletionOutcome::Succeeded => p.ok,
                            crate::build_events::CompletionOutcome::Failed => p.err,
                            _ => p.warn,
                        };
                        (color, runner::completion_message(completion))
                    } else {
                        match &r.exit {
                            None => (p.info, format!("running {}", fmt_dur(r.elapsed()))),
                            Some(Ok(0)) => (p.ok, format!("finished in {}", fmt_dur(r.elapsed()))),
                            Some(Ok(code)) if r.stopped_by_user => (
                                p.warn,
                                format!("stopped after {} (exit {code})", fmt_dur(r.elapsed())),
                            ),
                            Some(Ok(code)) => {
                                (p.err, format!("exit {code} after {}", fmt_dur(r.elapsed())))
                            }
                            Some(Err(e)) => (p.err, e.clone()),
                        }
                    };
                    if r.is_running() {
                        ui.spinner();
                    }
                    ui.label(RichText::new(t).color(c));
                    ui.label(RichText::new(r.cmd.title()).strong());
                }
            });
        });
        ui.separator();
        match self.log_tab {
            LogTab::Output => match &self.run {
                None => {
                    ui.label(RichText::new("No run yet in this session. Dry run shows what a build would do without running anything.").color(p.muted));
                }
                Some(r) => {
                    ui.add(
                        egui::Label::new(
                            RichText::new(&r.command_line)
                                .monospace()
                                .small()
                                .color(p.muted),
                        )
                        .selectable(true),
                    );
                    let lines: Vec<&str> = r
                        .lines
                        .iter()
                        .map(|s| s.as_str())
                        .filter(|l| !self.problems_only || is_problem(l))
                        .collect();
                    log_lines(ui, "run_output", &lines, r.dropped > 0);
                }
            },
            LogTab::BuildLog => {
                let path = log_dir.join("build.log");
                let tail = self
                    .build_log
                    .get_or_insert_with(|| LogTail::new(path.clone()));
                if tail.path() != path {
                    *tail = LogTail::new(path);
                }
                tail.refresh(&ctx, Duration::from_millis(1000));
                file_tail(ui, tail, self.problems_only);
            }
            LogTab::StageLog => match self.selected.clone() {
                None => {
                    ui.label(
                        RichText::new(
                            "Select a stage to see its own log (work/build/logs/<stage>.log).",
                        )
                        .color(p.muted),
                    );
                }
                Some(s) => {
                    let path = log_dir.join("logs").join(format!("{s}.log"));
                    let tail = self
                        .stage_log
                        .get_or_insert_with(|| LogTail::new(path.clone()));
                    if tail.path() != path {
                        *tail = LogTail::new(path);
                    }
                    tail.refresh(&ctx, Duration::from_millis(1000));
                    file_tail(ui, tail, self.problems_only);
                }
            },
        }
    }

    fn confirm_modal(&mut self, ctx: &egui::Context, env: &Env) {
        self.sync_target(env);
        let Some(cmd) = self.pending_confirm.clone() else {
            return;
        };
        let mut decision: Option<bool> = None;
        egui::Modal::new(egui::Id::new("confirm_build")).show(ctx, |ui| {
            let p = pal(ui);
            ui.set_max_width(520.0);
            ui.label(RichText::new(format!("Start a real build? ({})", cmd.title())).heading().strong());
            ui.add(egui::Label::new(RichText::new(format!("Root: {}", env.root.display())).monospace()).wrap().selectable(true));
            ui.add(egui::Label::new(RichText::new(format!("Graph: {}", self.confirmation_graph.as_ref().unwrap_or(&env.graph).display())).monospace()).wrap().selectable(true));
            ui.add(egui::Label::new(RichText::new(format!("Interpreter: {}", env.python)).monospace()).wrap().selectable(true));
            if let Some(spec) = &env.project_spec {
                ui.add(egui::Label::new(RichText::new(format!("Project spec: {}", spec.display())).monospace()).wrap().selectable(true));
            }
            ui.add_space(6.0);
            ui.label("Stages run their pipeline commands and rewrite their outputs under work/. The build runs in the \
                      background; Stop ends it and every stage it started.");
            ui.add_space(6.0);
            egui::Frame::new().fill(ui.visuals().code_bg_color).corner_radius(4).inner_margin(6).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(runner::target_command_line(&env.fox_exe, &env.root, &env.graph, &env.python, &cmd)).monospace().small()).wrap());
            });
            if let Some(g) = &env.game_running {
                ui.add_space(6.0);
                ui.colored_label(p.warn, format!("{g} is running: the build goes one stage at a time at Idle priority, and GPU stages wait for the game to close."));
            }
            if let Some(pid) = env.lock_pid {
                ui.add_space(6.0);
                ui.colored_label(p.err, format!("Another build (pid {pid}) is running on this repo: wait for it to finish."));
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    decision = Some(false);
                }
                let ready = !self.run_busy() && env.lock_pid.is_none() && env.allow_real_builds && env.real_build_block.is_none();
                if theme::primary_button(ui, "Start build", ready).clicked() {
                    decision = Some(true);
                }
            });
        });
        match decision {
            Some(true) => {
                let current =
                    runner::review_source(&env.root, &env.graph, env.project_spec.as_deref());
                if current.as_ref().ok() != self.confirmation_source.as_ref() {
                    self.pending_confirm = None;
                    self.confirmation_for = None;
                    self.confirmation_source = None;
                    self.confirmation_graph = None;
                    self.run_error = Some(current.err().unwrap_or_else(|| {
                        "Build source changed after review; refresh and review it again.".into()
                    }));
                    return;
                }
                self.pending_confirm = None;
                self.confirmation_for = None;
                self.confirmation_source = None;
                self.confirmation_graph = None;
                self.start_run(ctx, env, cmd);
            }
            Some(false) => {
                self.pending_confirm = None;
                self.confirmation_for = None;
                self.confirmation_source = None;
                self.confirmation_graph = None;
            }
            None => {}
        }
    }
}

/// `python tools/build/pause.py on|off` (detached: it returns at once; the probe shows the PAUSE file)
fn run_pause_tool(
    python: &str,
    tool: &std::path::Path,
    root: &std::path::Path,
    arg: &str,
) -> Result<(), String> {
    let mut c = std::process::Command::new(python);
    c.arg(tool)
        .arg(arg)
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // no console window
    }
    c.spawn()
        .map(|_| ())
        .map_err(|e| format!("pause.py {arg}: {e}"))
}

/// lines worth seeing when something went wrong
pub fn is_problem(l: &str) -> bool {
    matches!(runner::classify(l), LineKind::Failed | LineKind::Note)
        || l.contains("PYTHON FALLBACK")
        || l.contains("Traceback")
        || l.contains("Error")
        || l.contains("error:")
        || l.contains("WARNING")
        || l.contains("warning:")
}

fn file_tail(ui: &mut egui::Ui, tail: &LogTail, problems_only: bool) {
    let p = pal(ui);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(tail.path().display().to_string())
                .monospace()
                .small()
                .color(p.muted),
        );
        if tail.busy() {
            ui.spinner();
        }
        if tail.path().exists() && ui.small_button("Show in folder").clicked() {
            open_in_explorer(tail.path());
        }
    });
    if tail.missing && !tail.busy() {
        ui.label(RichText::new("No log file yet.").color(p.muted));
        return;
    }
    let lines: Vec<&str> = tail
        .lines
        .iter()
        .map(|s| s.as_str())
        .filter(|l| !problems_only || is_problem(l))
        .collect();
    log_lines(
        ui,
        &tail.path().display().to_string(),
        &lines,
        tail.truncated && !problems_only,
    );
}

/// a virtualised, coloured, bottom-sticking log
fn log_lines(ui: &mut egui::Ui, id: &str, lines: &[&str], truncated: bool) {
    let p = pal(ui);
    let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
    egui::Frame::new()
        .fill(ui.visuals().code_bg_color)
        .corner_radius(4)
        .inner_margin(6)
        .show(ui, |ui| {
            egui::ScrollArea::both()
                .id_salt(id)
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show_rows(ui, row_h, lines.len() + truncated as usize, |ui, range| {
                    for k in range {
                        if truncated && k == 0 {
                            ui.label(
                                RichText::new("… (earlier lines not shown)")
                                    .monospace()
                                    .color(p.muted),
                            );
                            continue;
                        }
                        let l = lines[k - truncated as usize];
                        let c = if l.contains("PYTHON FALLBACK") {
                            p.err
                        } else {
                            match runner::classify(l) {
                                LineKind::Header => ui.visuals().strong_text_color(),
                                LineKind::Run => p.info,
                                LineKind::Done => p.ok,
                                LineKind::Skip => p.muted,
                                LineKind::Maybe | LineKind::Note => p.warn,
                                LineKind::Failed => p.err,
                                LineKind::Plain => ui.visuals().text_color(),
                            }
                        };
                        ui.add(egui::Label::new(RichText::new(l).monospace().color(c)).extend());
                    }
                });
        });
}

pub fn open_in_explorer(path: &std::path::Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }
    #[cfg(not(windows))]
    let _ = path;
}

/// "2026-10-05 09:20:11" -> "10-05 09:20"
fn short_stamp(s: &str) -> String {
    s.get(5..16)
        .map(|x| x.to_string())
        .unwrap_or_else(|| s.to_string())
}

pub fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}h {:02}m", s / 3600, (s / 60) % 60)
    } else if s >= 60 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{:.1} s", d.as_secs_f64())
    }
}

fn rel(p: &std::path::Path, root: &std::path::Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_states_from_foxbuild_lines() {
        assert_eq!(
            parse_live("[  0:01] start  nav.ground (est 2048 MB; never built)"),
            Some((Live::Running, "nav.ground".into()))
        );
        assert_eq!(
            parse_live("[  1:01] done   nav.ground   61 s   900 MB peak  3 outputs"),
            Some((Live::Done, "nav.ground".into()))
        );
        assert_eq!(
            parse_live("[  1:01] FAILED nav.sky rc Some(1) after 2 s"),
            Some((Live::Failed, "nav.sky".into()))
        );
        assert_eq!(
            parse_live("[  0:00] skip   veg.dense up to date"),
            Some((Live::Skipped, "veg.dense".into()))
        );
        assert_eq!(
            parse_live("[  0:00] RUN    a.b      0 s   2048 MB  never built"),
            Some((Live::WouldRun, "a.b".into()))
        );
        assert_eq!(
            parse_live("[  0:00] MAYBE  a.c  0 s"),
            Some((Live::Maybe, "a.c".into()))
        );
        assert_eq!(parse_live("[  0:00] === foxbuild"), None);
        assert_eq!(parse_live("[  2:00] nav.ground   61.0   900  ran"), None);
    }

    #[test]
    fn states() {
        let mut s = StageStatus::default();
        assert_eq!(stage_state(&s, false), StageState::Checking);
        assert_eq!(stage_state(&s, true), StageState::UpToDate);
        s.dirty = Some("never built".into());
        assert_eq!(stage_state(&s, true), StageState::NeverBuilt);
        s.dirty = Some("last run failed".into());
        assert_eq!(stage_state(&s, true), StageState::Failed);
        s.dirty = Some("input changed: x".into());
        assert_eq!(stage_state(&s, true), StageState::Dirty);
    }

    #[test]
    fn real_builds_refused_when_not_allowed() {
        let ctx = egui::Context::default();
        let env = Env {
            root: PathBuf::from("."),
            graph: PathBuf::from("none.toml"),
            project_spec: None,
            fox_exe: PathBuf::from("Z:/no/such/fox.exe"),
            python: "python".into(),
            game_running: None,
            lock_pid: None,
            confirm_builds: false,
            allow_real_builds: false,
            real_build_block: None,
            paused: false,
        };
        let mut v = BuildView::default();
        v.request_run(
            &ctx,
            &env,
            BuildCmd::Build {
                only: vec![],
                from: vec![],
            },
        );
        assert!(v.run.is_none());
        assert!(v.run_error.as_deref().unwrap_or("").contains("disabled"));
        // with confirmation on, a real build only asks
        let graph = std::env::temp_dir().join(format!(
            "foxstudio_disabled_review_{}_{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::write(&graph, b"[settings]\n").unwrap();
        let env2 = Env {
            root: std::env::temp_dir(),
            graph,
            confirm_builds: true,
            ..env
        };
        v.request_run(
            &ctx,
            &env2,
            BuildCmd::Build {
                only: vec![],
                from: vec![],
            },
        );
        assert!(v.pending_confirm.is_some());
        assert!(v.run.is_none());
        std::fs::remove_file(&env2.graph).unwrap();
    }
}

/// Deterministic delayed-worker regressions; no stage process or GPU is used.
#[cfg(test)]
mod target_tests {
    use super::*;
    use std::sync::mpsc::{Sender, channel};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "foxstudio_build_status_{}_{nonce}",
                std::process::id()
            ));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }

        fn env(&self, name: &str) -> Env {
            let graph = self.0.join(format!("{name}.toml"));
            std::fs::write(&graph, format!("[settings]\nlog_dir = \"{name}_logs\"\n[[stage]]\nname = \"{name}\"\ncmd = [\"never_execute\"]\noutputs = [\"missing\"]\n")).unwrap();
            Env {
                root: self.0.clone(),
                graph,
                project_spec: None,
                fox_exe: self.0.join("no_fox"),
                python: self.0.join("no_python").display().to_string(),
                game_running: None,
                lock_pid: None,
                confirm_builds: true,
                allow_real_builds: false,
                real_build_block: None,
                paused: false,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
            if let Ok(target) = std::fs::canonicalize(&self.0) {
                assert!(target.starts_with(&temp) && target != temp);
                let _ = std::fs::remove_dir_all(target);
            }
        }
    }

    fn blocked_status(v: &mut BuildView, ctx: &egui::Context, env: &Env, fail: bool) -> Sender<()> {
        v.sync_target(env);
        v.refresh_pending = false;
        v.status_for = Some(v.source(env));
        let snapshot = foxbuild::status(
            &env.root,
            &env.graph,
            &StatusOptions {
                python: env.python.clone(),
                check_dirty: false,
                save_cache: false,
            },
        )
        .unwrap();
        let (release, gate) = channel();
        let (entered, ready) = channel();
        v.status_task = Some(Task::spawn_serial(
            "Delayed fixture status",
            ctx,
            move |t| {
                entered.send(()).unwrap();
                gate.recv_timeout(Duration::from_secs(10))
                    .map_err(|e| e.to_string())?;
                t.partial(snapshot.clone());
                if fail {
                    Err("obsolete target failed".into())
                } else {
                    Ok(snapshot)
                }
            },
        ));
        ready.recv_timeout(Duration::from_secs(10)).unwrap();
        release
    }

    fn finish(v: &mut BuildView, ctx: &egui::Context, env: &Env) {
        let started = Instant::now();
        while !v.checked || v.status_busy() {
            v.poll(ctx, env);
            if let Some(report) = &v.report {
                assert_eq!(
                    report.config, env.graph,
                    "late report replaced the latest target"
                );
            }
            assert!(v.status_error.is_none(), "{:?}", v.status_error);
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "status did not finish"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn target_retry_late_status_is_discarded_and_changes_coalesce_to_latest_target() {
        let f = Fixture::new();
        let old = f.env("old");
        let middle = f.env("middle");
        let latest = f.env("latest");
        let ctx = egui::Context::default();
        let mut v = BuildView::default();
        let release = blocked_status(&mut v, &ctx, &old, false);
        let worker_started = v.status_task.as_ref().unwrap().started;
        v.poll(&ctx, &middle);
        v.poll(&ctx, &latest);
        assert!(v.report.is_none() && !v.checked);
        assert_eq!(
            v.status_task.as_ref().unwrap().started,
            worker_started,
            "target change spawned a second status worker"
        );
        release.send(()).unwrap();
        finish(&mut v, &ctx, &latest);
        assert_eq!(v.report.as_ref().unwrap().stages[0].name, "latest");
        for name in ["old", "middle", "latest"] {
            assert!(
                !f.0.join(format!("{name}_logs/hashcache.json")).exists(),
                "status wrote a cache"
            );
        }
    }

    #[test]
    fn target_retry_late_error_from_previous_target_is_ignored() {
        let f = Fixture::new();
        let old = f.env("old");
        let latest = f.env("latest");
        let ctx = egui::Context::default();
        let mut v = BuildView::default();
        let release = blocked_status(&mut v, &ctx, &old, true);
        v.poll(&ctx, &latest);
        release.send(()).unwrap();
        finish(&mut v, &ctx, &latest);
        assert_eq!(v.report.as_ref().unwrap().stages[0].name, "latest");
        assert!(v.status_error.is_none());
    }

    #[test]
    fn target_retry_refresh_requested_while_busy_rechecks_changed_graph() {
        let f = Fixture::new();
        let env = f.env("old");
        let ctx = egui::Context::default();
        let mut v = BuildView::default();
        let release = blocked_status(&mut v, &ctx, &env, false);
        std::fs::write(&env.graph, "[settings]\nlog_dir = \"old_logs\"\n[[stage]]\nname = \"edited\"\ncmd = [\"never_execute\"]\noutputs = [\"missing\"]\n").unwrap();
        v.refresh_status(&ctx, &env);
        assert!(v.refresh_pending && !v.checked);
        release.send(()).unwrap();
        finish(&mut v, &ctx, &env);
        assert_eq!(v.report.as_ref().unwrap().stages[0].name, "edited");
    }
}
