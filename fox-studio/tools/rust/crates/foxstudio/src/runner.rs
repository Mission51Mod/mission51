//! The build as a child process: `fox build <args>`, output streamed line by line.
//!
//! The build runs in its own process on purpose: foxbuild keeps its repo lock, its job object (closing it stops
//! every stage) and its log, exactly as from a terminal, and a crash of either side never takes the other down.
use crate::build_events::{
    BuildCompletion, BuildEvent, BuildEventStream, ChildExit, CompletionIssue, CompletionOutcome,
    Interruption, KnownEventKind,
};
use eframe::egui;
use std::collections::VecDeque;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// what to run; only `Build` changes pipeline outputs
#[derive(Clone, Debug, PartialEq)]
pub enum BuildCmd {
    /// the plan: what would run and why (`--dry-run`), optionally for a selection
    DryRun {
        only: Vec<String>,
        from: Vec<String>,
    },
    Status,
    List,
    Graph,
    /// a real build: the default selection, or `--only` / `--from`
    Build {
        only: Vec<String>,
        from: Vec<String>,
    },
}

impl BuildCmd {
    pub fn args(&self) -> Vec<String> {
        let sel = |only: &Vec<String>, from: &Vec<String>, v: &mut Vec<String>| {
            if !only.is_empty() {
                v.push("--only".into());
                v.push(only.join(","));
            }
            if !from.is_empty() {
                v.push("--from".into());
                v.push(from.join(","));
            }
        };
        let mut v = vec!["build".to_string()];
        match self {
            BuildCmd::DryRun { only, from } => {
                v.push("--dry-run".into());
                sel(only, from, &mut v);
            }
            BuildCmd::Status => v.push("--status".into()),
            BuildCmd::List => v.push("--list".into()),
            BuildCmd::Graph => v.push("--graph".into()),
            BuildCmd::Build { only, from } => sel(only, from, &mut v),
        }
        v
    }

    /// writes pipeline outputs (needs the user's confirmation)
    pub fn is_real_build(&self) -> bool {
        matches!(self, BuildCmd::Build { .. })
    }

    pub fn title(&self) -> String {
        match self {
            BuildCmd::DryRun { only, from } if only.is_empty() && from.is_empty() => {
                "Dry run".into()
            }
            BuildCmd::DryRun { only, from } => format!("Dry run ({})", describe_sel(only, from)),
            BuildCmd::Status => "Status".into(),
            BuildCmd::List => "List".into(),
            BuildCmd::Graph => "Graph".into(),
            BuildCmd::Build { only, from } if only.is_empty() && from.is_empty() => "Build".into(),
            BuildCmd::Build { only, from } => format!("Build ({})", describe_sel(only, from)),
        }
    }
}

fn describe_sel(only: &[String], from: &[String]) -> String {
    let mut p = vec![];
    if !only.is_empty() {
        p.push(format!("only {}", only.join(", ")));
    }
    if !from.is_empty() {
        p.push(format!("from {}", from.join(", ")));
    }
    p.join("; ")
}

/// the command line a user would type for this run (shown in the UI, copyable)
pub fn command_line(fox_exe: &Path, cmd: &BuildCmd) -> String {
    let mut s = quote(&fox_exe.to_string_lossy());
    for a in cmd.args() {
        s.push(' ');
        s.push_str(&quote(&a));
    }
    s
}

/// The reviewed invocation, including the graph/root/interpreter actually passed to the child.
pub fn target_command_line(
    fox_exe: &Path,
    root: &Path,
    graph: &Path,
    python: &str,
    cmd: &BuildCmd,
) -> String {
    format!(
        "{} --config {} --root {} --python {}",
        command_line(fox_exe, cmd),
        quote(&graph.to_string_lossy()),
        quote(&root.to_string_lossy()),
        quote(python)
    )
}

/// Prepare a child command without launching it or publishing any project files.
/// An M3 identity is applied through Q's immutable plan; the parent's environment remains untouched.
pub fn build_command(
    fox_exe: &Path,
    root: &Path,
    graph: &Path,
    python: &str,
    cmd: &BuildCmd,
    project_spec: Option<&Path>,
) -> Result<Command, String> {
    Ok(prepare_command(
        RunTarget {
            fox_exe,
            root,
            graph,
            python,
            project_spec,
        },
        cmd,
        None,
        None,
    )?
    .command)
}

fn raw_command(
    fox_exe: &Path,
    root: &Path,
    graph: &Path,
    python: &str,
    cmd: &BuildCmd,
    project_spec: Option<&Path>,
) -> Command {
    let temp = root.join("work").join("tmp");
    let mut c = Command::new(fox_exe);
    c.args(cmd.args());
    if let Some(spec) = project_spec {
        c.arg("--project").arg(spec);
    } else {
        c.arg("--config").arg(graph);
    }
    c.arg("--root")
        .arg(root)
        .arg("--python")
        .arg(python)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("FOX_REPO_ROOT", root)
        .env("FOX_PYTHON", python)
        .env("TEMP", &temp)
        .env("TMP", &temp);
    #[cfg(windows)]
    c.creation_flags(
        0x0800_0000 /* CREATE_NO_WINDOW */ | 0x0000_4000, /* BELOW_NORMAL_PRIORITY_CLASS */
    );
    c
}

/// Explicit scheduler capability. None in BuildView retains the legacy invocation.
/// Packaged tools use C's trusted compiled identity and immutable bundle snapshot.
#[derive(Clone, Debug)]
pub struct BuildContext {
    bundle_manifest: Option<std::path::PathBuf>,
    bundle: Option<foxbuild::bundle::BundledTools>,
}

impl BuildContext {
    pub fn json_v1() -> Self {
        Self {
            bundle_manifest: None,
            bundle: None,
        }
    }

    pub fn packaged(manifest: &Path) -> Result<Self, String> {
        let manifest = manifest
            .canonicalize()
            .map_err(|e| format!("Bundle manifest {}: {e}", manifest.display()))?;
        let bundle = foxbuild::bundle::BundledTools::load_compiled(&manifest)
            .map_err(|e| format!("{e:#}"))?;
        Ok(Self {
            bundle_manifest: Some(manifest),
            bundle: Some(bundle),
        })
    }

    pub fn key(&self) -> String {
        match (&self.bundle_manifest, &self.bundle) {
            (Some(path), Some(bundle)) => {
                format!("json-v1:{}:{}", path.display(), bundle.fingerprint())
            }
            _ => "json-v1".into(),
        }
    }

    fn verify(&self) -> Result<(), String> {
        if let (Some(path), Some(bundle)) = (&self.bundle_manifest, &self.bundle) {
            let current = foxbuild::bundle::BundledTools::load(path, bundle.identity())
                .map_err(|e| format!("{e:#}"))?;
            if current.fingerprint() != bundle.fingerprint() {
                return Err("Native tool bundle changed after review; select it again.".into());
            }
        }
        Ok(())
    }

    pub fn status(
        &self,
        root: &Path,
        graph: &Path,
        opts: &foxbuild::StatusOptions,
    ) -> Result<foxbuild::StatusReport, String> {
        self.verify()?;
        match &self.bundle {
            Some(bundle) => foxbuild::status_with_bundle(root, graph, opts, bundle),
            None => foxbuild::status(root, graph, opts),
        }
        .map_err(|e| format!("{e:#}"))
    }

    pub fn is_packaged(&self) -> bool {
        self.bundle.is_some()
    }
}

/// Reviewed child target; borrowed paths never change the parent environment.
#[derive(Clone, Copy)]
pub struct RunTarget<'a> {
    pub fox_exe: &'a Path,
    pub root: &'a Path,
    pub graph: &'a Path,
    pub python: &'a str,
    pub project_spec: Option<&'a Path>,
}

/// Add verified capability flags without changing the parent environment.
pub fn build_command_with_context(
    target: RunTarget<'_>,
    cmd: &BuildCmd,
    context: Option<&BuildContext>,
    cancel_file: Option<&Path>,
) -> Result<Command, String> {
    Ok(prepare_command(target, cmd, context, cancel_file)?.command)
}

/// Read-only project selection. The caller materializes only at an approved run boundary.
#[derive(Debug)]
pub struct PreparedCommand {
    pub command: Command,
    pub plan: Option<foxproject::ProjectBuildPlan>,
}

pub fn prepare_command(
    target: RunTarget<'_>,
    cmd: &BuildCmd,
    context: Option<&BuildContext>,
    cancel_file: Option<&Path>,
) -> Result<PreparedCommand, String> {
    let root = absolute_root(target.root)?;
    let selected_exe =
        std::path::absolute(target.fox_exe).map_err(|e| format!("Native build executable: {e}"))?;
    let executable = if let Some(context) = context {
        context.verify()?;
        if let Some(bundle) = &context.bundle {
            let expected = bundle
                .resolve_program("fox")
                .map_err(|e| format!("{e:#}"))?;
            let actual = selected_exe
                .canonicalize()
                .map_err(|e| format!("Native build executable: {e}"))?;
            if actual != expected {
                return Err(
                    "Reviewed executable does not match the selected native tool bundle.".into(),
                );
            }
            // Launch this exact checked path, never the caller's relative spelling.
            expected.to_path_buf()
        } else {
            selected_exe
        }
    } else {
        selected_exe
    };
    let mut plan = target
        .project_spec
        .map(|spec| foxproject::plan_build_in(&root, spec))
        .transpose()?;
    if let Some(context) = context
        && let Some(project) = plan.take()
    {
        let Some(bundle) = &context.bundle else {
            return Err("Project JSONL builds require a validated native tool bundle.".into());
        };
        let bound = project.with_tool(&executable)?;
        bundle.verify_current().map_err(|e| format!("{e:#}"))?;
        if bound
            .native_tool()
            .is_none_or(|tool| tool.path != executable)
        {
            return Err("Project native tool identity differs from its validated bundle.".into());
        }
        plan = Some(bound);
    }
    let (root, graph) = match &plan {
        Some(plan) => (plan.repo_root(), plan.graph_path().to_path_buf()),
        None => (
            root.as_path(),
            selected_path(target.root, &root, target.graph)?,
        ),
    };
    let mut command = raw_command(
        &executable,
        root,
        &graph,
        target.python,
        cmd,
        plan.as_ref().map(|plan| plan.spec_path()),
    );
    if let Some(context) = context {
        command.arg("--json");
        if let Some(path) = &context.bundle_manifest {
            command.arg("--bundled-tools").arg(path);
        }
        if let Some(path) = cancel_file {
            command.arg("--cancel-file").arg(rooted_path(root, path));
        }
    }
    if let Some(plan) = &plan {
        plan.apply_child_environment(&mut command)?;
    } else {
        command.env_remove("FOX_PROJECT");
    }
    Ok(PreparedCommand { command, plan })
}

fn absolute_root(root: &Path) -> Result<std::path::PathBuf, String> {
    if root.as_os_str().is_empty() {
        return Err("Build root must be a nonempty directory path.".into());
    }
    let root = std::path::absolute(root).map_err(|e| format!("Build root: {e}"))?;
    // Unix absolute() retains parent components. Windows already resolves them;
    // preserve its normal absolute spelling for existing GUI/report comparisons.
    #[cfg(unix)]
    let root = root
        .canonicalize()
        .map_err(|e| format!("Build root {}: {e}", root.display()))?;
    if !root.is_dir() {
        return Err(format!("Build root {} is not a directory.", root.display()));
    }
    Ok(root)
}

fn rooted_path(root: &Path, path: &Path) -> std::path::PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

/// Inspect a freshly planned project without publishing its graph or facets.
pub fn read_status(
    target: RunTarget<'_>,
    context: Option<&BuildContext>,
    opts: &foxbuild::StatusOptions,
) -> Result<foxbuild::StatusReport, String> {
    if target.project_spec.is_some() {
        let selected = prepare_command(target, &BuildCmd::Status, context, None)?;
        let plan = selected
            .plan
            .as_ref()
            .ok_or("Project status has no prepared plan")?;
        let bundle = context
            .and_then(|context| context.bundle.as_ref())
            .ok_or("Project status requires a validated native tool bundle.")?;
        let prepared = plan.prepared_graph()?;
        let mut read_only = opts.clone();
        read_only.save_cache = false;
        foxbuild::status_prepared(
            plan.repo_root(),
            plan.graph_path(),
            &read_only,
            Some(bundle),
            &prepared,
        )
        .map_err(|e| format!("{e:#}"))
    } else {
        let root = absolute_root(target.root)?;
        let graph = selected_path(target.root, &root, target.graph)?;
        match context {
            Some(context) => context.status(&root, &graph, opts),
            None => foxbuild::status(&root, &graph, opts).map_err(|e| format!("{e:#}")),
        }
    }
}

/// Capture authored/resolved source for confirmation. No cache or graph publication.
pub fn review_source(
    root: &Path,
    graph: &Path,
    project_spec: Option<&Path>,
) -> Result<Vec<u8>, String> {
    let bytes = if let Some(spec) = project_spec {
        let spec = foxpipe::project::Spec::load_in(root, spec).map_err(|e| e.to_string())?;
        serde_json::to_vec(spec.resolved_json()).map_err(|e| format!("Project review: {e}"))?
    } else {
        let path = selected_path(root, &absolute_root(root)?, graph)?;
        let metadata = std::fs::metadata(&path)
            .map_err(|e| format!("Review graph {}: {e}", path.display()))?;
        if metadata.len() > 4 * 1024 * 1024 {
            return Err("Build graph is too large for a bounded review (4 MiB).".into());
        }
        std::fs::read(&path).map_err(|e| format!("Review graph {}: {e}", path.display()))?
    };
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("Project source is too large for a bounded review (4 MiB).".into());
    }
    Ok(bytes)
}

// Settings may already prefix a relative selected root. Resolve that spelling
// against the parent's cwd once, rather than joining the root twice.
fn selected_path(
    selected_root: &Path,
    absolute_root: &Path,
    path: &Path,
) -> Result<std::path::PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else if selected_root.is_relative() && path.starts_with(selected_root) {
        let path = std::path::absolute(path).map_err(|e| format!("Selected build path: {e}"))?;
        #[cfg(unix)]
        if path.exists() {
            return path
                .canonicalize()
                .map_err(|e| format!("Selected build path {}: {e}", path.display()));
        }
        Ok(path)
    } else {
        Ok(absolute_root.join(path))
    }
}

fn quote(a: &str) -> String {
    if a.is_empty() || a.contains([' ', '\t', '"']) {
        format!("\"{}\"", a.replace('"', "\\\""))
    } else {
        a.to_string()
    }
}

const MAX_LINES: usize = 50_000;
const MAX_LOG_BYTES: usize = 4 * 1024 * 1024;
const MAX_HUMAN_LINE_BYTES: usize = 8192;
const OUTPUT_CHUNK_BYTES: usize = 4096;
const OUTPUT_QUEUE_PACKETS: usize = 64;
const POLL_PACKETS: usize = 128;
const PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
static CANCEL_NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[derive(Clone, Copy)]
enum Pipe {
    Stdout,
    Stderr,
}
impl Pipe {
    fn index(self) -> usize {
        match self {
            Self::Stdout => 0,
            Self::Stderr => 1,
        }
    }
}
enum Output {
    Bytes(Pipe, Vec<u8>),
    End(Pipe),
    Error(Pipe, String),
}

#[derive(Default)]
struct HumanLines {
    pending: Vec<u8>,
    discarding: bool,
}
impl HumanLines {
    fn push(&mut self, mut bytes: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        while !bytes.is_empty() {
            let newline = bytes.iter().position(|b| *b == b'\n');
            let count = newline.map_or(bytes.len(), |i| i + 1);
            let part = &bytes[..count];
            bytes = &bytes[count..];
            if !self.discarding {
                let available = MAX_HUMAN_LINE_BYTES - self.pending.len();
                self.pending
                    .extend_from_slice(&part[..part.len().min(available)]);
                if part.len() > available {
                    lines.push(format!(
                        "{} [line truncated]",
                        String::from_utf8_lossy(&self.pending)
                    ));
                    self.pending.clear();
                    self.discarding = true;
                }
            }
            if newline.is_some() {
                if !self.discarding {
                    lines.push(
                        String::from_utf8_lossy(&self.pending)
                            .trim_end_matches(['\r', '\n'])
                            .to_string(),
                    );
                }
                self.pending.clear();
                self.discarding = false;
            }
        }
        lines
    }
    fn finish(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            return None;
        }
        let line = String::from_utf8_lossy(&self.pending).to_string();
        self.pending.clear();
        Some(line)
    }
}

pub struct BuildRun {
    pub cmd: BuildCmd,
    pub command_line: String,
    pub started: Instant,
    pub lines: VecDeque<String>,
    pub dropped: usize,
    child: Option<Child>,
    rx: Receiver<Output>,
    pub exit: Option<Result<i32, String>>,
    pub finished: Option<Duration>,
    /// Actual forced stop; cooperative cancellation is only a request until wait.
    pub stopped_by_user: bool,
    pub cancel_requested: bool,
    pub control_error: Option<String>,
    pub completion: Option<BuildCompletion>,
    pub live: std::collections::BTreeMap<String, crate::build_view::Live>,
    pub timeline: crate::timeline::Timeline,
    decoder: Option<BuildEventStream>,
    pub event_count: u64,
    pub output_source: Option<crate::output_diff::SnapshotSource>,
    pub before_output: Option<Result<Option<crate::output_diff::OutputSnapshot>, String>>,
    human: [HumanLines; 2],
    eof: [bool; 2],
    pending_exit: Option<Result<Option<i32>, String>>,
    exit_at: Option<Instant>,
    interruption: Option<Interruption>,
    log_bytes: usize,
    cancel_file: Option<std::path::PathBuf>,
    cancel_created: bool,
    cancel_at: Option<Instant>,
    detached: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl BuildRun {
    /// Legacy entry point remains compatible until the scheduler capability is selected.
    pub fn start(
        fox_exe: &Path,
        root: &Path,
        graph: &Path,
        python: &str,
        project_spec: Option<&Path>,
        cmd: BuildCmd,
        ctx: &egui::Context,
    ) -> Result<BuildRun, String> {
        Self::start_with_context(
            RunTarget {
                fox_exe,
                root,
                graph,
                python,
                project_spec,
            },
            cmd,
            ctx,
            None,
        )
    }

    pub fn start_with_context(
        target: RunTarget<'_>,
        cmd: BuildCmd,
        ctx: &egui::Context,
        context: Option<&BuildContext>,
    ) -> Result<BuildRun, String> {
        let RunTarget { fox_exe, .. } = target;
        let root = absolute_root(target.root)?;
        if !fox_exe.is_file() {
            return Err(format!(
                "{} not found: select or build the Rust tools first",
                fox_exe.display()
            ));
        }
        let temp = root.join("work").join("tmp");
        let cancel_file = context.map(|_| {
            temp.join(format!(
                "foxstudio-cancel-{}-{}-{}",
                std::process::id(),
                CANCEL_NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ))
        });
        let prepared = prepare_command(target, &cmd, context, cancel_file.as_deref())?;
        // Project publication belongs to the CLI child's held build lease.
        // Parent planning/review/status never writes shared project inputs.
        if let Some(context) = context {
            context.verify()?;
        }
        let actual_root = prepared
            .plan
            .as_ref()
            .map_or(root.as_path(), |plan| plan.repo_root());
        let graph = match &prepared.plan {
            Some(plan) => plan.graph_path().to_path_buf(),
            None => selected_path(target.root, actual_root, target.graph)?,
        };
        let (output_source, before_output) = if cmd.is_real_build() {
            let config = prepared.plan.as_ref().map_or_else(
                || foxbuild::config::Graph::load(&graph),
                |plan| Ok(plan.graph().clone()),
            );
            match config {
                Ok(config) => {
                    let source = crate::output_diff::SnapshotSource {
                        root: actual_root.to_path_buf(),
                        graph: graph.clone(),
                        state_file: actual_root.join(config.settings.log_dir).join("state.json"),
                        label: format!("Before {}", cmd.title()),
                    };
                    let before = match std::fs::metadata(&source.state_file) {
                        Ok(metadata) if metadata.len() > 16 * 1024 * 1024 => Err(
                            "Build state exceeds the bounded 16 MiB output snapshot limit.".into(),
                        ),
                        _ => crate::output_diff::OutputSnapshot::read(source.clone()),
                    };
                    (Some(source), Some(before))
                }
                Err(error) => (None, Some(Err(format!("Output snapshot graph: {error:#}")))),
            }
        } else {
            (None, None)
        };
        let mut command = prepared.command;
        let command_line = std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|arg| quote(&arg.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ");
        std::fs::create_dir_all(&temp).map_err(|e| format!("{}: {e}", temp.display()))?;
        let mut child = command
            .spawn()
            .map_err(|e| format!("Starting {}: {e}", fox_exe.display()))?;
        let (tx, rx) = std::sync::mpsc::sync_channel::<Output>(OUTPUT_QUEUE_PACKETS);
        let detached = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let pipes: Vec<(Pipe, Box<dyn Read + Send>)> = vec![
            (
                Pipe::Stdout,
                Box::new(child.stdout.take().ok_or("Child stdout was not piped")?),
            ),
            (
                Pipe::Stderr,
                Box::new(child.stderr.take().ok_or("Child stderr was not piped")?),
            ),
        ];
        for (pipe, mut reader) in pipes {
            let sender = tx.clone();
            let repaint = ctx.clone();
            let detached_reader = detached.clone();
            let spawn = std::thread::Builder::new()
                .name(format!("fox-studio: build output {}", pipe.index()))
                .spawn(move || {
                    let mut bytes = [0u8; OUTPUT_CHUNK_BYTES];
                    loop {
                        match reader.read(&mut bytes) {
                            Ok(0) => break,
                            Ok(count) => {
                                if sender
                                    .send(Output::Bytes(pipe, bytes[..count].to_vec()))
                                    .is_err()
                                    && !detached_reader.load(std::sync::atomic::Ordering::Relaxed)
                                {
                                    return;
                                }
                                repaint.request_repaint();
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                                continue;
                            }
                            Err(error) => {
                                let _ = sender.send(Output::Error(pipe, error.to_string()));
                                break;
                            }
                        }
                    }
                    let _ = sender.send(Output::End(pipe));
                    repaint.request_repaint();
                });
            if let Err(error) = spawn {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Starting build output reader: {error}"));
            }
        }
        drop(tx);
        Ok(BuildRun {
            cmd,
            command_line,
            started: Instant::now(),
            lines: VecDeque::new(),
            dropped: 0,
            child: Some(child),
            rx,
            exit: None,
            finished: None,
            stopped_by_user: false,
            cancel_requested: false,
            control_error: None,
            completion: None,
            live: std::collections::BTreeMap::new(),
            timeline: crate::timeline::Timeline::default(),
            decoder: context.map(|_| BuildEventStream::new()),
            event_count: 0,
            output_source,
            before_output,
            human: Default::default(),
            eof: [false; 2],
            pending_exit: None,
            exit_at: None,
            interruption: None,
            log_bytes: 0,
            cancel_file,
            cancel_created: false,
            cancel_at: None,
            detached,
        })
    }

    /// Bounded, nonblocking output drain. Wait and both pipe EOFs precede completion.
    pub fn poll(&mut self) -> bool {
        if self.exit.is_some() {
            return true;
        }
        for _ in 0..POLL_PACKETS {
            match self.rx.try_recv() {
                Ok(Output::Bytes(Pipe::Stdout, bytes)) if self.decoder.is_some() => {
                    if let Some(mut decoder) = self.decoder.take() {
                        let first_error = self.control_error.is_none();
                        let result = decoder.push_bytes(&bytes, |event| self.apply_event(event));
                        self.event_count = decoder.accepted_events();
                        self.decoder = Some(decoder);
                        if let Err(error) = result
                            && first_error
                        {
                            self.push(format!("Build event stream error: {error}"));
                            self.control_error = Some(error.to_string());
                        }
                    }
                }
                Ok(Output::Bytes(pipe, bytes)) => {
                    for line in self.human[pipe.index()].push(&bytes) {
                        self.push(line);
                    }
                }
                Ok(Output::End(pipe)) => {
                    self.eof[pipe.index()] = true;
                    if let Some(line) = self.human[pipe.index()].finish() {
                        self.push(line);
                    }
                }
                Ok(Output::Error(pipe, error)) => {
                    self.interruption = Some(Interruption::BrokenOutputTransport);
                    self.push(format!(
                        "Could not read build {}: {error}",
                        if pipe.index() == 0 {
                            "stdout"
                        } else {
                            "stderr"
                        }
                    ));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    if !self.eof.iter().all(|eof| *eof) {
                        self.interruption = Some(Interruption::BrokenOutputTransport);
                        self.eof = [true; 2];
                    }
                    break;
                }
            }
        }
        if self.pending_exit.is_none()
            && let Some(child) = self.child.as_mut()
        {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if status.code().is_none() && self.interruption.is_none() {
                        self.interruption = Some(Interruption::ForcedTermination);
                    }
                    self.pending_exit = Some(Ok(status.code()));
                    self.exit_at = Some(Instant::now());
                    self.child = None;
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    self.pending_exit =
                        Some(Err(format!("Waiting for the build process: {error}")));
                    self.exit_at = Some(Instant::now());
                    self.interruption = Some(Interruption::ForcedTermination);
                    self.child = None;
                }
            }
        }
        let drained = self.eof.iter().all(|eof| *eof);
        let timed_out = self
            .exit_at
            .is_some_and(|at| at.elapsed() >= PIPE_DRAIN_TIMEOUT);
        if self.pending_exit.is_some() && (drained || timed_out) {
            if !drained {
                self.interruption = Some(Interruption::BrokenOutputTransport);
                self.push(
                    "Build exited, but its output pipes did not close; completion is incomplete."
                        .into(),
                );
            }
            let status = self
                .pending_exit
                .take()
                .unwrap_or(Err("Missing child wait result".into()));
            let code = status.as_ref().ok().copied().flatten();
            if let Some(decoder) = self.decoder.take() {
                let completion = decoder.finish(ChildExit {
                    code,
                    interruption: self.interruption,
                });
                let message = completion_message(&completion);
                self.push(message.clone());
                self.timeline.summary = Some(message);
                self.completion = Some(completion);
            }
            self.exit = Some(status.map(|code| code.unwrap_or(-1)));
            self.finished = Some(self.started.elapsed());
            self.remove_cancel_marker();
        }
        self.exit.is_some()
    }

    fn apply_event(&mut self, event: BuildEvent) {
        self.timeline.apply_event(&event);
        if let Some((state, name)) = crate::build_view::live_event(&event)
            && name.len() <= 256
            && (self.live.len() < 4096 || self.live.contains_key(&name))
        {
            self.live.insert(name, state);
        }
        if let Some(line) = event_message(&event) {
            self.push(line);
        }
    }

    fn push(&mut self, mut line: String) {
        if line.len() > MAX_HUMAN_LINE_BYTES {
            let mut end = MAX_HUMAN_LINE_BYTES - 20;
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            line.truncate(end);
            line.push_str(" [line truncated]");
        }
        while self.lines.len() >= MAX_LINES || self.log_bytes + line.len() > MAX_LOG_BYTES {
            if let Some(old) = self.lines.pop_front() {
                self.log_bytes -= old.len();
                self.dropped = self.dropped.saturating_add(1);
            } else {
                break;
            }
        }
        self.log_bytes += line.len();
        self.lines.push_back(line);
    }

    pub fn is_json(&self) -> bool {
        self.decoder.is_some() || self.completion.is_some()
    }
    pub fn is_running(&self) -> bool {
        self.exit.is_none()
    }
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// JSONL cancellation requests a marker. Success/failure/130 are decided after wait.
    pub fn stop(&mut self) {
        if !self.is_running() {
            return;
        }
        if let Some(path) = &self.cancel_file {
            if self.cancel_requested {
                return;
            }
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(_) => {
                    self.cancel_created = true;
                    self.cancel_requested = true;
                    self.cancel_at = Some(Instant::now());
                    self.push(
                        "Cancellation requested; waiting for the build to stop safely.".into(),
                    );
                }
                Err(error) => {
                    self.control_error = Some(format!(
                        "Could not request cancellation at {}: {error}",
                        path.display()
                    ));
                }
            }
        } else {
            self.force_stop();
        }
    }

    pub fn can_force_stop(&self) -> bool {
        self.is_running()
            && (self.control_error.is_some()
                || self
                    .cancel_at
                    .is_some_and(|at| at.elapsed() >= Duration::from_secs(3)))
    }

    pub fn force_stop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            match child.kill() {
                Ok(()) => {
                    self.stopped_by_user = true;
                    self.interruption = Some(Interruption::ForcedTermination);
                }
                Err(error) => {
                    self.control_error = Some(format!("Could not force-stop the build: {error}"))
                }
            }
        }
    }

    pub fn detach(&mut self) {
        self.detached
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.child = None;
        // Detached readers continue draining even when the GUI receiver goes away.
    }

    fn remove_cancel_marker(&mut self) {
        if self.cancel_created {
            if let Some(path) = &self.cancel_file
                && let Err(error) = std::fs::remove_file(path)
            {
                self.control_error = Some(format!(
                    "Could not remove this run's cancellation marker: {error}"
                ));
            }
            self.cancel_created = false;
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.finished.unwrap_or_else(|| self.started.elapsed())
    }
    pub fn succeeded(&self) -> bool {
        if let Some(completion) = &self.completion {
            completion.confirmed && completion.outcome == CompletionOutcome::Succeeded
        } else if self.decoder.is_some() {
            false
        } else {
            matches!(self.exit, Some(Ok(0)))
        }
    }
}

impl Drop for BuildRun {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if !self.detached.load(std::sync::atomic::Ordering::Relaxed) {
            self.remove_cancel_marker();
        }
    }
}

pub fn completion_message(completion: &BuildCompletion) -> String {
    let outcome = match completion.outcome {
        CompletionOutcome::Succeeded => "Succeeded",
        CompletionOutcome::Failed => "Failed",
        CompletionOutcome::Cancelled => "Cancelled",
        CompletionOutcome::Incomplete => "Incomplete",
    };
    let mut message = format!(
        "{outcome} (child exit {})",
        completion
            .child_exit
            .code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "unavailable".into())
    );
    for issue in &completion.issues {
        let detail = match issue {
            CompletionIssue::Protocol(error) => error.to_string(),
            CompletionIssue::TruncatedLine { bytes } => {
                format!("stdout ended with an incomplete {bytes}-byte line")
            }
            CompletionIssue::MissingRunStarted => "run_started was not received".into(),
            CompletionIssue::MissingTerminal => "run_finished was not received".into(),
            CompletionIssue::MissingChildExit => "no normal child exit code was available".into(),
            CompletionIssue::ExitMismatch { reported, actual } => {
                format!("terminal exit {reported} disagrees with actual exit {actual}")
            }
            CompletionIssue::Interrupted(reason) => match reason {
                Interruption::CallerCancellation => "caller interrupted cancellation".into(),
                Interruption::ForcedTermination => "process was forcibly stopped".into(),
                Interruption::BrokenOutputTransport => "output transport was interrupted".into(),
            },
        };
        message.push_str("; ");
        message.push_str(&detail);
    }
    message
}

fn event_message(event: &BuildEvent) -> Option<String> {
    let data = &event.data;
    let text = |key| {
        data.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    };
    Some(match event.kind? {
        KnownEventKind::RunStarted => format!("Starting {}", text("operation")),
        KnownEventKind::StagePlanned => {
            format!("{} {}: {}", text("result"), text("name"), text("reason"))
        }
        KnownEventKind::StageStarted => format!("Starting {}: {}", text("name"), text("reason")),
        KnownEventKind::StageFinished => format!(
            "{} {}{}",
            text("name"),
            text("result"),
            data.get("error")
                .or_else(|| data.get("reason"))
                .and_then(serde_json::Value::as_str)
                .map(|reason| format!(": {reason}"))
                .unwrap_or_default()
        ),
        KnownEventKind::Warning => format!("Warning: {}", text("message")),
        KnownEventKind::Error => format!("Build error: {}", text("message")),
        KnownEventKind::RunFinished => {
            "Build reported completion; waiting for child exit and output EOF.".into()
        }
        _ => return None,
    })
}

/// How a foxbuild output line should be coloured.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineKind {
    Header,
    Run,
    Done,
    Skip,
    Maybe,
    Failed,
    Note,
    Plain,
}

/// classify a foxbuild log line ("[  0:01] RUN    nav.ground ..." etc.)
pub fn classify(line: &str) -> LineKind {
    let body = match line.find("] ") {
        Some(i) if line.starts_with('[') => &line[i + 2..],
        _ => line,
    };
    let t = body.trim_start();
    if t.starts_with("===") || t.starts_with("---") {
        LineKind::Header
    } else if t.starts_with("FAILED")
        || t.starts_with("foxbuild: error")
        || t.contains(", FAILURES")
    {
        LineKind::Failed
    } else if t.starts_with("RUN") || t.starts_with("start") {
        LineKind::Run
    } else if t.starts_with("done") {
        LineKind::Done
    } else if t.starts_with("skip") {
        LineKind::Skip
    } else if t.starts_with("MAYBE") {
        LineKind::Maybe
    } else if t.starts_with("note:") || t.starts_with("game running") {
        LineKind::Note
    } else {
        LineKind::Plain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_and_kinds() {
        assert_eq!(
            BuildCmd::DryRun {
                only: vec![],
                from: vec![]
            }
            .args(),
            ["build", "--dry-run"]
        );
        assert_eq!(
            BuildCmd::DryRun {
                only: vec![],
                from: vec!["veg.dense".into()]
            }
            .args(),
            ["build", "--dry-run", "--from", "veg.dense"]
        );
        assert_eq!(
            BuildCmd::Build {
                only: vec!["a".into(), "b".into()],
                from: vec![]
            }
            .args(),
            ["build", "--only", "a,b"]
        );
        assert_eq!(
            BuildCmd::Build {
                only: vec![],
                from: vec![]
            }
            .args(),
            ["build"]
        );
        assert_eq!(BuildCmd::Status.args(), ["build", "--status"]);
        assert!(
            BuildCmd::Build {
                only: vec![],
                from: vec![]
            }
            .is_real_build()
        );
        for c in [
            BuildCmd::DryRun {
                only: vec![],
                from: vec![],
            },
            BuildCmd::Status,
            BuildCmd::List,
            BuildCmd::Graph,
        ] {
            assert!(!c.is_real_build());
            assert!(
                c.args().iter().any(|a| a.starts_with("--")),
                "{c:?} must carry its read-only flag"
            );
        }
    }

    #[test]
    fn command_line_quotes_spaces() {
        let s = command_line(Path::new("C:/Program Files/fox.exe"), &BuildCmd::Status);
        assert_eq!(s, "\"C:/Program Files/fox.exe\" build --status");
    }

    #[test]
    fn classify_lines() {
        assert_eq!(
            classify("[  0:00] === foxbuild 2026-10-05 x (55 stages selected) DRY RUN"),
            LineKind::Header
        );
        assert_eq!(
            classify("[  0:00] RUN    nav.ground      0 s   2048 MB  never built"),
            LineKind::Run
        );
        assert_eq!(
            classify("[  0:01] skip   veg.dense up to date"),
            LineKind::Skip
        );
        assert_eq!(
            classify("[  0:02] MAYBE  m3.packs  0 s 0 MB  if x change(s) its inputs"),
            LineKind::Maybe
        );
        assert_eq!(
            classify("[ 12:02] FAILED nav.ground rc Some(1) after 3 s: see x"),
            LineKind::Failed
        );
        assert_eq!(classify("[ 12:02] done   nav.ground   3 s"), LineKind::Done);
        assert_eq!(
            classify("[  0:00] note: learned edge: a -> b"),
            LineKind::Note
        );
        assert_eq!(
            classify("foxbuild: error: unknown stage x"),
            LineKind::Failed
        );
        assert_eq!(classify("hello"), LineKind::Plain);
    }

    #[test]
    fn missing_exe_is_an_error_not_a_panic() {
        let ctx = eframe::egui::Context::default();
        let r = BuildRun::start(
            Path::new("Z:/no/such/fox.exe"),
            Path::new("."),
            Path::new("stages.toml"),
            "python",
            None,
            BuildCmd::Status,
            &ctx,
        );
        assert!(r.is_err());
    }
}
