//! foxbuild: incremental, parallel, memory-budgeted build of a Fox Engine location from a stage graph.
//!
//!   foxbuild --root <repo> --config tools/build/flyk_stages.toml --python <python.exe> [options]
//!
//! Options:
//!   --dry-run              show the plan, run nothing
//!   --from STAGE           force STAGE and everything downstream of it
//!   --only A,B             only these stages (their upstream is assumed done)
//!   --force                run every selected stage regardless of hashes
//!   --jobs N, --mem-gb X   override max_jobs / mem_budget_gb
//!   --list | --status | --graph
//!   --bundled-tools FILE   validate packaged native tools; no Python or source snapshot
//!   --json                 emit JSONL v1 to stdout; human diagnostics to stderr
//!   --cancel-file FILE     caller creates FILE to cancel managed work (exit 130)
//!   --stop-on-error        stop starting new stages after the first failure (default: keep going with the rest)
//!   --suspend-running      while work/build/PAUSE exists also suspend the running stages (resumed when it goes, hard
//!                          auto-resume after 20 min). Without it PAUSE only stops new stages (tools/build/pause.py)
//!
//! See docs/release/BUILD.md for the model (traced I/O, early cutoff, memory budget, locks, games).
mod api;
pub mod bundle;
mod cancel;
pub mod config;
mod events;
mod graph;
mod hashing;
mod interpreter;
mod job;
mod lock;
mod options;
mod prepared;
mod process;
mod state;
mod suspend;

pub use api::{
    PinnedStatus, StageStatus, StatusOptions, StatusReport, default_python, lock_holder,
    rust_tools_fresh, rust_tools_stamp, status, status_prepared, status_with_bundle,
};

use anyhow::{Context, Result};
use cancel::Cancellation;
use config::{Graph, Stage};
use events::Events;
use hashing::HashCache;
pub use lock::PublicationLease;
use options::{Opts, parse_args};
pub use prepared::PreparedGraph;
use process::{ManagedChild, Priority};
pub use state::{StageRecord, State};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessesToUpdate, System};

struct Log {
    file: Option<std::fs::File>,
    t0: Instant,
    json: bool,
}

impl Log {
    fn line(&mut self, s: &str) {
        let t = self.t0.elapsed().as_secs();
        let msg = format!("[{:>3}:{:02}] {}", t / 60, t % 60, s);
        if self.json {
            eprintln!("{msg}");
        } else {
            println!("{msg}");
        }
        if let Some(file) = &mut self.file {
            let _ = writeln!(file, "{msg}");
            let _ = file.flush();
        }
    }
}

/// The canonical identity of a Python interpreter spec ("python", "py", a path, a venv's python.exe): the interpreter
/// itself reports os.path.realpath(sys.executable) (lowercased on Windows) and sys.version, so every way of naming
/// the same interpreter (the CLI's FOX_PYTHON / "python" on PATH, the shim's sys.executable, the GUI setting) gives
/// the same command fingerprint. Asked once per spec per process; falls back to the spec text when it cannot run.
pub fn python_identity(spec: &str) -> String {
    interpreter::identity(spec)
}

/// Command fingerprint. `python` is the interpreter IDENTITY (python_identity), not the spec text.
/// The command fingerprint foxbuild records for a stage (what `fox build` compares to decide "command changed").
/// `python_identity` = python_identity(spec) of the interpreter that replaces "python" in commands.
pub fn command_fingerprint(st: &Stage, python_identity: &str) -> String {
    cmd_fp(st, python_identity)
}

fn cmd_fp(st: &Stage, python: &str) -> String {
    let mut h = blake3::Hasher::new();
    for c in st.run_cmd() {
        let c = if c == "python" { python } else { c.as_str() };
        h.update(c.as_bytes());
        h.update(b"\0");
    }
    // the stage's own env is part of what it builds (BTreeMap: sorted); none = the fingerprint of before
    for (k, v) in &st.env {
        h.update(b"\x01");
        h.update(k.as_bytes());
        h.update(b"=");
        h.update(v.as_bytes());
    }
    h.finalize().to_hex()[..16].to_string()
}

/// rerun_with: the "finished" stamps of those stages now (what a successful run of `s` records)
fn with_runs(s: &Stage, st: &State) -> BTreeMap<String, String> {
    s.rerun_with
        .iter()
        .map(|w| {
            (
                w.clone(),
                st.stages
                    .get(w)
                    .map(|r| r.finished.clone())
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// dirty_reason plus rerun_with: a stage named there ran since this one was built
fn stage_dirty(
    s: &Stage,
    fp: &str,
    st: &State,
    root: &Path,
    cache: &mut HashCache,
) -> Option<String> {
    let rec = st.stages.get(&s.name);
    if let Some(w) = state::dirty_reason(rec, fp, root, cache) {
        return Some(w);
    }
    let rec = rec?;
    for (w, stamp) in with_runs(s, st) {
        if rec.with_runs.get(&w) != Some(&stamp) {
            return Some(format!("{w} ran since the last run (rerun_with)"));
        }
    }
    None
}

/// deps[i]: declared `after` + learned edges (stage i read a file another stage produced), when consistent.
fn build_deps(g: &Graph, st: &State, warn: &mut Vec<String>) -> Vec<Vec<usize>> {
    let n = g.stage.len();
    let idx: BTreeMap<&str, usize> = g
        .stage
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.as_str(), i))
        .collect();
    let mut deps: Vec<Vec<usize>> = g
        .stage
        .iter()
        .map(|s| {
            s.after
                .iter()
                .chain(&s.rerun_with)
                .map(|a| idx[a.as_str()])
                .collect()
        })
        .collect();
    for d in deps.iter_mut() {
        let mut seen = BTreeSet::new();
        d.retain(|x| seen.insert(*x));
    }
    // producers: recorded outputs (exact) and declared output prefixes
    let mut producer: BTreeMap<String, usize> = BTreeMap::new();
    for (i, s) in g.stage.iter().enumerate() {
        if let Some(r) = st.stages.get(&s.name) {
            for p in r.outputs.keys() {
                producer.insert(hashing::key(p), i);
            }
        }
    }
    let prefixes: Vec<(String, usize)> = g
        .stage
        .iter()
        .enumerate()
        .flat_map(|(i, s)| s.outputs.iter().map(move |o| (hashing::key(o), i)))
        .collect();
    let find = |k: &str| -> Option<usize> {
        if let Some(&i) = producer.get(k) {
            return Some(i);
        }
        prefixes
            .iter()
            .find(|(p, _)| k == p || k.starts_with(&format!("{p}/")))
            .map(|(_, i)| *i)
    };
    for (b, s) in g.stage.iter().enumerate() {
        let Some(r) = st.stages.get(&s.name) else {
            continue;
        };
        let mut learned: BTreeSet<usize> = BTreeSet::new();
        for p in r.inputs.keys() {
            if let Some(a) = find(&hashing::key(p))
                && a != b
            {
                learned.insert(a);
            }
        }
        for a in learned {
            if deps[b].contains(&a) || graph::is_ancestor(&deps, a, b) {
                continue;
            }
            if graph::is_ancestor(&deps, b, a) {
                warn.push(format!(
                    "{} reads output of {}, but is declared to run before it (edge ignored; check the graph)",
                    s.name, g.stage[a].name
                ));
                continue;
            }
            warn.push(format!(
                "learned edge: {} -> {} (add it to `after` to make it explicit)",
                g.stage[a].name, s.name
            ));
            deps[b].push(a);
        }
    }
    let _ = n;
    deps
}

fn game_running(sys: &System, games: &[String]) -> Option<String> {
    let set: BTreeSet<String> = games.iter().map(|g| g.to_lowercase()).collect();
    for p in sys.processes().values() {
        let name = p.name().to_string_lossy().to_lowercase();
        if set.contains(&name) {
            return Some(name);
        }
    }
    None
}

fn tree_memory(sys: &System, root: u32) -> u64 {
    let mut kids: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (pid, p) in sys.processes() {
        if let Some(pp) = p.parent() {
            kids.entry(pp.as_u32()).or_default().push(pid.as_u32());
        }
    }
    let mut total = 0;
    let mut stack = vec![root];
    let mut seen = BTreeSet::new();
    while let Some(p) = stack.pop() {
        if !seen.insert(p) {
            continue;
        }
        if let Some(pr) = sys.process(Pid::from_u32(p)) {
            total += pr.memory();
        }
        if let Some(k) = kids.get(&p) {
            stack.extend(k.iter().copied());
        }
    }
    total
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum St {
    Waiting,
    Running,
    Ran,
    Skipped,
    Failed,
    Cancelled,
    Blocked,
    NotSelected,
}

impl St {
    fn result(self) -> &'static str {
        match self {
            Self::Ran => "succeeded",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Blocked => "blocked",
            Self::Waiting => "waiting",
            Self::Running => "running",
            Self::NotSelected => "not_selected",
        }
    }
}

struct Running {
    i: usize,
    child: ManagedChild,
    start: Instant,
    peak: u64,
    trace_dir: PathBuf,
    est_mb: f64,
}

/// `foxbuild [options]` / `fox build [options]`: returns the process exit code
pub fn main_with(args: &[String]) -> i32 {
    main_context(args, None, || Ok(()))
}

/// Run a captured project. A real build publishes exactly once under the same
/// lease that protects state loading, stage execution and final cleanup.
pub fn main_with_prepared<F>(args: &[String], prepared: PreparedGraph, publish: F) -> i32
where
    F: FnOnce() -> Result<(), String>,
{
    main_context(args, Some(prepared), publish)
}

fn main_context<F>(args: &[String], prepared: Option<PreparedGraph>, publish: F) -> i32
where
    F: FnOnce() -> Result<(), String>,
{
    let parsed = parse_args(args);
    let mut events = Events::new(args.iter().any(|arg| arg == "--json"));
    let start = match &parsed {
        Ok(opts) => serde_json::json!({
            "operation": opts.operation(), "root": opts.root, "config": opts.config,
            "bundled_tools": opts.bundled_tools, "cancel_file": opts.cancel_file,
        }),
        Err(_) => serde_json::json!({"operation": "build"}),
    };
    if let Err(error) = events.emit("run_started", start) {
        eprintln!("foxbuild: event output failed: {error:#}");
        return 2;
    }
    let result = parsed.and_then(|opts| run(opts, &mut events, prepared, publish));
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) => {
            let message = format!("{error:#}");
            eprintln!("foxbuild: error: {message}");
            let _ = events.emit("error", serde_json::json!({"message": message}));
            RunOutcome {
                code: 2,
                summary: serde_json::json!({"error": message}),
            }
        }
    };
    if let Err(error) = events.finish(outcome.code, outcome.summary) {
        eprintln!("foxbuild: event output failed: {error:#}");
        return 2;
    }
    outcome.code
}

struct RunOutcome {
    code: i32,
    summary: serde_json::Value,
}

impl RunOutcome {
    fn success() -> Self {
        Self {
            code: 0,
            summary: serde_json::json!({}),
        }
    }
    fn cancelled() -> Self {
        Self {
            code: 130,
            summary: serde_json::json!({}),
        }
    }
}

fn run<F>(
    o: Opts,
    events: &mut Events,
    prepared: Option<PreparedGraph>,
    publish: F,
) -> Result<RunOutcome>
where
    F: FnOnce() -> Result<(), String>,
{
    let cancel = Cancellation::new(o.cancel_file.clone());
    if cancel.requested()? {
        return Ok(RunOutcome::cancelled());
    }
    if o.help {
        let help = include_str!("lib.rs")
            .lines()
            .take_while(|line| line.starts_with("//!"))
            .map(|line| line.trim_start_matches("//!"))
            .collect::<Vec<_>>()
            .join("\n");
        events.human(&help);
        return Ok(RunOutcome {
            code: 0,
            summary: serde_json::json!({"help": help}),
        });
    }
    let bundle = o
        .bundled_tools
        .as_ref()
        .map(|path| bundle::BundledTools::load_compiled(path))
        .transpose()?;
    let g = match &prepared {
        Some(prepared) => {
            prepared.validate(&o.root, &o.config)?;
            for name in o.only.iter().chain(&o.from) {
                if prepared.graph.index(name).is_none() {
                    anyhow::bail!("unknown stage {name}");
                }
            }
            prepared.graph.clone()
        }
        None => Graph::load(&o.config)?,
    };
    if let Some(bundle) = &bundle {
        for stage in &g.stage {
            bundle.command_fingerprint(stage)?;
        }
    }
    let root = o.root.clone();
    let log_dir = root.join(&g.settings.log_dir);
    let temp = root.join(&g.settings.temp_dir);
    let is_build = !o.dry_run && !o.list && !o.graph && !o.status;
    if is_build {
        std::fs::create_dir_all(log_dir.join("logs"))?;
        std::fs::create_dir_all(log_dir.join("traces"))?;
        std::fs::create_dir_all(&temp)?;
    }
    let state_path = log_dir.join("state.json");
    let cache_path = log_dir.join("hashcache.json");
    // Authoritative state, cached hashes and learned ordering belong to the
    // acquired build lease. A contender must never derive a stale snapshot
    // before a completing owner has released its records.
    let _lock = if is_build {
        Some(lock::BuildLock::acquire(&log_dir)?)
    } else {
        None
    };
    // Opening the configured log is part of setup. A failed setup must not
    // invoke the project's publisher, even after ownership was acquired.
    let prepared_log = if is_build && prepared.is_some() {
        Some(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_dir.join("build.log"))?,
        )
    } else {
        None
    };
    if cancel.requested()? {
        return Ok(RunOutcome::cancelled());
    }
    if is_build && prepared.is_some() {
        publish()
            .map_err(anyhow::Error::msg)
            .context("publishing prepared project")?;
        if cancel.requested()? {
            return Ok(RunOutcome::cancelled());
        }
    }
    let mut st = State::load(&state_path);
    let mut cache = HashCache::load(&cache_path);
    if !is_build && let Some(prepared) = &prepared {
        cache.set_planned_inputs(&prepared.input_hashes);
    }
    let mut warns = Vec::new();
    let deps = build_deps(&g, &st, &mut warns);
    let n = g.stage.len();
    let order = graph::topo_order(n, &deps).context("stage graph has a cycle")?;

    for &i in &order {
        let stage = &g.stage[i];
        events.emit(
            "stage_declared",
            serde_json::json!({
                "name": stage.name, "owner": stage.owner, "command": stage.run_cmd(),
                "dependencies": deps[i].iter().map(|&d| &g.stage[d].name).collect::<Vec<_>>(),
                "default": stage.default, "gpu": stage.gpu, "verify": stage.verify,
                "deterministic": stage.deterministic, "mem_gb": stage.mem_gb, "locks": stage.locks,
            }),
        )?;
    }

    if o.list || o.graph {
        for &i in &order {
            let s = &g.stage[i];
            let d: Vec<&str> = deps[i].iter().map(|&j| g.stage[j].name.as_str()).collect();
            if o.graph {
                events.human(&format!(
                    "{:<30} <- {}",
                    s.name,
                    if d.is_empty() {
                        "-".into()
                    } else {
                        d.join(", ")
                    }
                ));
            } else {
                events.human(&format!(
                    "{:<30} {}{}{}{} {}  ({})",
                    s.name,
                    if s.default { "" } else { "[on demand] " },
                    if s.gpu { "[gpu] " } else { "" },
                    if s.deterministic {
                        ""
                    } else {
                        "[non-deterministic] "
                    },
                    if s.verify { "[verify] " } else { "" },
                    s.run_cmd().join(" "),
                    s.owner
                ));
            }
        }
        for w in &warns {
            events.human(&format!("note: {w}"));
            events.emit("warning", serde_json::json!({"message": w}))?;
        }
        return Ok(RunOutcome::success());
    }
    if o.status {
        let options = StatusOptions {
            python: o.python.clone(),
            check_dirty: true,
            save_cache: true,
        };
        let Some(report) = api::status_context_prepared(
            &root,
            &o.config,
            &options,
            bundle.as_ref(),
            &cancel,
            prepared.as_ref(),
        )?
        else {
            return Ok(RunOutcome::cancelled());
        };
        events.emit("status", serde_json::to_value(&report)?)?;
        events.human(&format!(
            "{:<30} {:>8} {:>9} {:>7} {:>7}  state",
            "stage", "built", "seconds", "peakMB", "inputs"
        ));
        for stage in &report.stages {
            events.human(&format!(
                "{:<30} {:>8} {:>9.0} {:>7.0} {:>7}  {}",
                stage.name,
                stage
                    .last_run
                    .as_deref()
                    .and_then(|stamp| stamp.get(11..16))
                    .unwrap_or(""),
                stage.seconds,
                stage.peak_mb,
                stage.inputs,
                stage.state_text()
            ));
        }
        for pin in &report.pinned {
            events.human(&format!(
                "pinned {} = {} ({})",
                pin.path, pin.hash, pin.owner
            ));
        }
        for stage in &report.stages {
            for fallback in &stage.fallbacks {
                events.human(&format!(
                    "note: {} last ran a PYTHON FALLBACK: {fallback}",
                    stage.name
                ));
            }
        }
        match &report.rust_tools {
            Some(Ok(true)) if bundle.is_some() => events.human("note: Rust tools: validated native bundle"),
            Some(Ok(true)) => events.human("note: Rust tools: built from the current sources"),
            Some(Ok(false)) => events.human("note: Rust tools: STALE (sources changed since the last build); a build rebuilds them first"),
            Some(Err(error)) => events.human(&format!("note: Rust tools: cannot tell ({error})")),
            None => {}
        }
        if let Some(pid) = report.running_pid {
            events.human(&format!(
                "note: a build is running (pid {pid}); the hash cache was not saved"
            ));
        }
        return Ok(RunOutcome::success());
    }

    // ---- selection
    let mut selected = vec![false; n];
    let mut forced = vec![o.force; n];
    if !o.only.is_empty() {
        for name in &o.only {
            let i = g
                .index(name)
                .with_context(|| format!("unknown stage {name}"))?;
            selected[i] = true;
        }
    } else {
        for (selected, stage) in selected.iter_mut().zip(&g.stage) {
            *selected = stage.default;
        }
    }
    for name in &o.from {
        let i = g
            .index(name)
            .with_context(|| format!("unknown stage {name}"))?;
        let d = graph::descendants(n, &deps, i);
        for j in 0..n {
            if d[j] && (g.stage[j].default || j == i) {
                selected[j] = true;
                forced[j] = true;
            }
        }
    }

    let mut log = Log {
        file: if o.dry_run {
            None
        } else {
            Some(match prepared_log {
                Some(file) => file,
                None => OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(log_dir.join("build.log"))?,
            })
        },
        t0: Instant::now(),
        json: o.json,
    };
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    log.line(&format!(
        "=== foxbuild {} {} ({} stages selected){}",
        now,
        o.config.display(),
        selected.iter().filter(|&&s| s).count(),
        if o.dry_run { " DRY RUN" } else { "" }
    ));
    for w in &warns {
        log.line(&format!("note: {w}"));
        events.emit("warning", serde_json::json!({"message": w}))?;
    }
    for k in &g.settings.unset_env {
        if std::env::var_os(k).is_some() {
            log.line(&format!(
                "note: {k} is set in this shell; it is removed from every stage's environment"
            ));
        }
    }

    let budget_mb = o.mem_gb.unwrap_or(g.settings.mem_budget_gb) * 1024.0;
    let reserve_mb = g.settings.mem_reserve_gb * 1024.0;
    let mut status = vec![St::NotSelected; n];
    let mut reason: Vec<String> = vec![String::new(); n];
    for i in 0..n {
        if selected[i] {
            status[i] = St::Waiting;
        }
    }
    let py_id = if bundle.is_none() {
        let Some(identity) = interpreter::identity_cancellable(&o.python, &cancel)? else {
            return cancel_before_stages(events, &g, &selected, &log_dir, log.t0);
        };
        log.line(&format!("python: {} ({})", o.python, identity));
        identity
    } else {
        String::new()
    };
    let fps: Vec<String> = g
        .stage
        .iter()
        .map(|stage| match &bundle {
            Some(bundle) => bundle.command_fingerprint(stage),
            None => Ok(cmd_fp(stage, &py_id)),
        })
        .collect::<Result<_>>()?;

    let needs_rust = (0..n).any(|i| selected[i] && g.stage[i].needs_rust());
    // Rust tools: a real run builds them ONCE from a snapshot of tools/rust into its own target dir
    // (`build.py --out-dir work/rust/gate`), so edits other agents make during the run can never stale it and no other
    // cargo job holds its target dir. Every stage then gets FOXBUILD_RUST_STAMP / FOX_BIN_DIR / FOXRS_STAMP (foxrs
    // loads that module, foxrs.fox_exe() returns that fox.exe) and cmd_rs runs those binaries. Stages that route into
    // the tools are refused when the snapshot build fails. A dry run only reports the working tree's state.
    let mut rust_err: Option<String> = None;
    let mut rust_stamp: Option<String> = None;
    let mut rust_bin: Option<PathBuf> = None;
    if let Some(bundle) = &bundle {
        rust_stamp = Some(format!("bundle:{}", bundle.fingerprint()));
        rust_bin = Some(bundle.root().to_owned());
        log.line(&format!(
            "Rust tools: validated native bundle {} / {} in {}",
            bundle.identity().package_version,
            bundle.identity().build_id,
            bundle.root().display()
        ));
    } else if needs_rust && o.dry_run {
        let Some(fresh) = api::rust_tools_fresh_cancellable(&root, &o.python, &cancel)? else {
            return cancel_before_stages(events, &g, &selected, &log_dir, log.t0);
        };
        match fresh {
            Ok(true) => log.line("Rust tools: built from the current sources (a real run builds its own snapshot)"),
            Ok(false) => log.line("Rust tools: the working tree changed since the last build (a real run builds its own snapshot)"),
            Err(e) => log.line(&format!("Rust tools: cannot check them ({e})")),
        }
    } else if needs_rust {
        log.line("Rust tools: snapshot build (python tools/rust/build.py --out-dir work/rust/gate; log work/build/logs/_rust_tools.log)");
        let lf = std::fs::File::create(log_dir.join("logs").join("_rust_tools.log"))?;
        let mut c = Command::new(&o.python);
        c.arg(root.join("tools").join("rust").join("build.py"))
            .arg("--out-dir")
            .arg(root.join("work").join("rust").join("gate"))
            .current_dir(&root)
            .stdin(Stdio::null())
            .stdout(lf.try_clone()?)
            .stderr(lf)
            .env("TEMP", &temp)
            .env("TMP", &temp)
            .env_remove("CARGO_TARGET_DIR");
        events.emit(
            "progress",
            serde_json::json!({
                "phase": "snapshot", "selected": selected.iter().filter(|&&value| value).count(),
                "completed": 0, "running": 0,
            }),
        )?;
        let snapshot = match ManagedChild::spawn(&mut c, Priority::BelowNormal) {
            Ok(mut child) => loop {
                if cancel.requested()? {
                    child
                        .cancel()
                        .context("stopping the cancelled Rust snapshot")?;
                    return cancel_before_stages(events, &g, &selected, &log_dir, log.t0);
                }
                if let Some(status) = child.try_wait()? {
                    break Ok(status);
                }
                cancel.sleep(Duration::from_millis(100));
            },
            Err(error) => Err(error),
        };
        match snapshot {
            Ok(st_) if st_.success() => {
                let text = std::fs::read_to_string(log_dir.join("logs").join("_rust_tools.log"))
                    .unwrap_or_default();
                let line = text
                    .lines()
                    .rev()
                    .find(|l| l.starts_with("SNAPSHOT stamp="))
                    .unwrap_or("");
                let stamp = line
                    .split_whitespace()
                    .find_map(|w| w.strip_prefix("stamp="))
                    .map(|x| x.to_string());
                let bin = line
                    .split(" bin=")
                    .nth(1)
                    .map(|x| PathBuf::from(x.split(" (").next().unwrap_or(x).trim()));
                match (stamp, bin) {
                    (Some(sp), Some(b)) if b.join(if cfg!(windows) { "fox.exe" } else { "fox" }).is_file() => {
                        log.line(&format!("Rust tools: snapshot {sp} in {}", b.display()));
                        rust_stamp = Some(sp);
                        rust_bin = Some(b);
                    }
                    _ => rust_err = Some("the snapshot build reported no stamp / binaries: see work/build/logs/_rust_tools.log".into()),
                }
            }
            Ok(st_) => {
                rust_err = Some(format!(
                    "the snapshot build failed (rc {:?}): see work/build/logs/_rust_tools.log",
                    st_.code()
                ))
            }
            Err(e) => rust_err = Some(format!("could not run the snapshot build: {e}")),
        }
        if let Some(e) = &rust_err {
            log.line(&format!(
                "Rust tools: {e}: the stages that route into them are refused"
            ));
        }
    }

    if o.dry_run {
        // predicted: a stage runs if dirty now or any selected upstream would run
        let mut will = vec![false; n];
        for &i in &order {
            if !selected[i] {
                continue;
            }
            let up = deps[i]
                .iter()
                .filter(|&&d| selected[d] && will[d])
                .map(|&d| g.stage[d].name.clone())
                .collect::<Vec<_>>();
            let why = if forced[i] {
                Some("forced".to_string())
            } else {
                stage_dirty(&g.stage[i], &fps[i], &st, &root, &mut cache)
            };
            let s = &g.stage[i];
            let est = st
                .stages
                .get(&s.name)
                .filter(|r| r.peak_mb > 0.0)
                .map(|r| r.peak_mb)
                .unwrap_or(s.mem_gb * 1024.0);
            let secs = st.stages.get(&s.name).map(|r| r.seconds).unwrap_or(0.0);
            let (planned, explanation) = match &why {
                Some(reason) => ("run", reason.clone()),
                None if !up.is_empty() => (
                    "maybe",
                    format!("if {} change(s) its inputs", up.join(", ")),
                ),
                None => ("skip", "up to date".into()),
            };
            events.emit(
                "stage_planned",
                serde_json::json!({
                    "name": s.name, "result": planned, "reason": explanation, "estimate_mb": est,
                }),
            )?;
            if let Some(w) = why {
                will[i] = true;
                log.line(&format!(
                    "RUN    {:<30} {:>6.0} s {:>6.0} MB  {}",
                    s.name, secs, est, w
                ));
            } else if !up.is_empty() {
                will[i] = true;
                log.line(&format!(
                    "MAYBE  {:<30} {:>6.0} s {:>6.0} MB  if {} change(s) its inputs",
                    s.name,
                    secs,
                    est,
                    up.join(", ")
                ));
            } else {
                log.line(&format!("skip   {:<30} up to date", s.name));
            }
        }
        // Planning may compute hashes, but publishes no state, cache or logs.
        return Ok(RunOutcome::success());
    }

    let mut sys = System::new();
    let mut running: Vec<Running> = Vec::new();
    let mut locks_held: BTreeSet<String> = BTreeSet::new();
    let mut last_sys = Instant::now() - Duration::from_secs(10);
    let mut game: Option<String> = None;
    let mut failed_any = false;
    let mut cancelled = false;
    let mut timings: Vec<(usize, f64, f64, String)> = Vec::new();
    let pause_path = log_dir.join("PAUSE");
    let mut was_paused = false;
    let mut suspended: Vec<u32> = Vec::new();
    let mut suspended_at: Option<Instant> = None;
    let mut auto_resumed_for: Option<std::time::SystemTime> = None;
    // tools/build/pause.py: this foxbuild watches PAUSE itself (pause.py then never suspends the scheduler)
    let _ = std::fs::write(
        log_dir.join("foxbuild.pause-aware"),
        format!("{}", std::process::id()),
    );
    let _aware = PauseAwareGuard(log_dir.join("foxbuild.pause-aware"));

    report_progress(events, &selected, &status)?;
    loop {
        if cancel.requested()? {
            cancelled = true;
            log.line("cancel: stopping managed children; no new stages will start");
            for mut stage in running.drain(..) {
                stage.child.cancel().with_context(|| {
                    format!("stopping cancelled stage {}", g.stage[stage.i].name)
                })?;
                let seconds = stage.start.elapsed().as_secs_f64();
                let peak = stage.peak as f64 / 1048576.0;
                status[stage.i] = St::Cancelled;
                timings.push((stage.i, seconds, peak, "cancelled".into()));
                events.emit("stage_finished", serde_json::json!({
                    "name": g.stage[stage.i].name, "result": "cancelled", "seconds": seconds, "peak_mb": peak,
                }))?;
            }
            for state in &mut status {
                if *state == St::Waiting {
                    *state = St::Cancelled;
                }
            }
            // Running records were invalidated and saved before spawn.
            st.save(&state_path)?;
            break;
        }
        // refresh system view (processes: memory, games) at most twice a second
        if last_sys.elapsed() >= Duration::from_millis(500) {
            sys.refresh_processes(ProcessesToUpdate::All, true);
            sys.refresh_memory();
            last_sys = Instant::now();
            let g_now = game_running(&sys, &g.settings.games);
            if g_now != game {
                match &g_now {
                    Some(n) => log.line(&format!(
                        "game running ({n}): one stage at a time, Idle priority, no GPU stages"
                    )),
                    None => log.line("no game running: normal scheduling"),
                }
                game = g_now;
            }
            for r in running.iter_mut() {
                let m = tree_memory(&sys, r.child.id());
                r.peak = r.peak.max(m);
            }
        }

        // finished children
        let mut k = 0;
        while k < running.len() {
            let done = running[k].child.try_wait()?;
            if let Some(code) = done {
                let r = running.remove(k);
                let s = &g.stage[r.i];
                for l in &s.locks {
                    locks_held.remove(l);
                }
                let secs = r.start.elapsed().as_secs_f64();
                let peak_mb = r.peak as f64 / (1024.0 * 1024.0);
                let mut ok = code.success();
                let mut why_fail = format!("rc {:?}", code.code());
                if ok && let Some(rx) = &s.require_output {
                    let text = std::fs::read_to_string(
                        log_dir.join("logs").join(format!("{}.log", s.name)),
                    )
                    .unwrap_or_default();
                    if !regex::Regex::new(rx)
                        .map(|r| r.is_match(&text))
                        .unwrap_or(false)
                    {
                        ok = false;
                        why_fail = format!("exit 0 but the log lacks /{rx}/");
                    }
                }
                if ok {
                    let c = state::collect(
                        &root,
                        &r.trace_dir,
                        r.child.id(),
                        &s.outputs,
                        &g.settings.ignore,
                        &g.settings.static_roots,
                        &s.soft_inputs,
                        &mut cache,
                    );
                    let old_out = st
                        .stages
                        .get(&s.name)
                        .map(|x| x.outputs.clone())
                        .unwrap_or_default();
                    let mut rec: StageRecord = c.record;
                    rec.fallbacks = state::fallbacks(&r.trace_dir);
                    rec.rust_stamp = rust_stamp.clone();
                    rec.ok = true;
                    rec.cmd_fp = fps[r.i].clone();
                    rec.with_runs = with_runs(s, &st);
                    rec.seconds = (secs * 10.0).round() / 10.0;
                    rec.peak_mb = peak_mb.round();
                    rec.finished = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
                    let same = old_out == rec.outputs;
                    log.line(&format!("done   {:<30} {:>6.0} s {:>6.0} MB peak  {} outputs{}, {} inputs ({} static), {} code files",
                                      s.name, secs, peak_mb, rec.outputs.len(), if same { " (unchanged: early cutoff)" } else { "" },
                                      rec.inputs.len(), rec.static_reads, rec.code.len()));
                    for nn in c.notes {
                        events.emit(
                            "warning",
                            serde_json::json!({"message": nn, "stage": s.name}),
                        )?;
                        log.line(&format!("       {}: {nn}", s.name));
                    }
                    for f in &rec.fallbacks {
                        log.line(&format!("       {}: PYTHON FALLBACK: {f}", s.name));
                    }
                    let result = if rec.fallbacks.is_empty() {
                        "ran".to_string()
                    } else {
                        "ran (PYTHON FALLBACK)".to_string()
                    };
                    events.emit("stage_finished", serde_json::json!({
                        "name": s.name, "result": "succeeded", "seconds": secs, "peak_mb": peak_mb,
                        "exit_code": code.code(), "inputs": rec.inputs.len(), "outputs": rec.outputs.len(),
                        "trace_complete": rec.trace_complete, "changed_during_run": rec.changed_during_run,
                        "fallbacks": rec.fallbacks, "rust_stamp": rec.rust_stamp,
                    }))?;
                    st.stages.insert(s.name.clone(), rec);
                    st.save(&state_path)?;
                    cache.save(&cache_path)?;
                    status[r.i] = St::Ran;
                    timings.push((r.i, secs, peak_mb, result));
                } else {
                    let mut rec = st.stages.get(&s.name).cloned().unwrap_or_default();
                    rec.ok = false;
                    st.stages.insert(s.name.clone(), rec);
                    st.save(&state_path)?;
                    log.line(&format!(
                        "FAILED {:<30} {} after {:.0} s: see {}",
                        s.name,
                        why_fail,
                        secs,
                        log_dir
                            .join("logs")
                            .join(format!("{}.log", s.name))
                            .display()
                    ));
                    events.emit(
                        "stage_finished",
                        serde_json::json!({
                            "name": s.name, "result": "failed", "seconds": secs, "peak_mb": peak_mb,
                            "exit_code": code.code(), "error": why_fail,
                        }),
                    )?;
                    status[r.i] = St::Failed;
                    failed_any = true;
                    timings.push((r.i, secs, peak_mb, "FAILED".into()));
                    // block everything downstream
                    let d = graph::descendants(n, &deps, r.i);
                    for j in 0..n {
                        if d[j] && j != r.i && status[j] == St::Waiting {
                            status[j] = St::Blocked;
                        }
                    }
                }
            } else {
                k += 1;
            }
        }

        report_progress(events, &selected, &status)?;

        // evaluate waiting stages whose dependencies are settled
        let stop = failed_any && o.stop_on_error;
        for &i in &order {
            if status[i] != St::Waiting || !reason[i].is_empty() {
                continue;
            }
            let settled = deps[i]
                .iter()
                .all(|&d| matches!(status[d], St::Ran | St::Skipped | St::NotSelected));
            if !settled {
                continue;
            }
            let why = if forced[i] {
                Some("forced".to_string())
            } else {
                stage_dirty(&g.stage[i], &fps[i], &st, &root, &mut cache)
            };
            match why {
                None => {
                    status[i] = St::Skipped;
                    log.line(&format!("skip   {:<30} up to date", g.stage[i].name));
                    timings.push((i, 0.0, 0.0, "up to date".into()));
                    events.emit(
                        "stage_finished",
                        serde_json::json!({
                            "name": g.stage[i].name, "result": "skipped", "reason": "up to date",
                            "seconds": 0.0, "peak_mb": 0.0,
                        }),
                    )?;
                }
                Some(w) => {
                    events.emit(
                        "stage_planned",
                        serde_json::json!({"name": g.stage[i].name, "result": "run", "reason": w}),
                    )?;
                    reason[i] = w;
                }
            }
        }

        // pause (work/build/PAUSE, e.g. tools/build/pause.py): no new stage starts while it exists; with
        // --suspend-running the running stage trees are suspended too, resumed when it goes or after 20 minutes
        let paused = pause_path.exists();
        if paused != was_paused {
            log.line(if paused {
                "pause: work/build/PAUSE set: no new stage starts"
            } else {
                "pause: PAUSE removed: resuming"
            });
            was_paused = paused;
        }
        if o.suspend_running {
            let stamp_now = std::fs::metadata(&pause_path)
                .and_then(|m| m.modified())
                .ok();
            if paused
                && suspended.is_empty()
                && stamp_now != auto_resumed_for
                && !running.is_empty()
            {
                for r in &running {
                    suspended.extend(suspend::suspend_tree(&sys, r.child.id()));
                }
                suspended_at = Some(Instant::now());
                log.line(&format!(
                    "pause: --suspend-running: {} process(es) suspended (auto-resume in 20 min)",
                    suspended.len()
                ));
            }
            let timed_out =
                suspended_at.is_some_and(|t| t.elapsed() >= Duration::from_secs(20 * 60));
            if !suspended.is_empty() && (!paused || timed_out) {
                let n = suspend::resume_all(&suspended);
                suspended.clear();
                suspended_at = None;
                if timed_out && paused {
                    auto_resumed_for = stamp_now;
                    log.line(&format!("pause: auto-resume after 20 min: {n} process(es) resumed (PAUSE still set: no new stage starts)"));
                } else {
                    log.line(&format!("pause: {n} process(es) resumed"));
                }
            }
        }

        // start ready stages
        if !stop && !paused {
            let max_jobs = if game.is_some() {
                g.settings.game_max_jobs
            } else {
                o.jobs.unwrap_or(g.settings.max_jobs)
            };
            for &i in &order {
                if failed_any && o.stop_on_error {
                    break;
                }
                if status[i] != St::Waiting || reason[i].is_empty() || running.len() >= max_jobs {
                    continue;
                }
                let s = &g.stage[i];
                if s.gpu && game.is_some() {
                    continue;
                }
                if s.locks.iter().any(|l| locks_held.contains(l)) {
                    continue;
                }
                let est = st
                    .stages
                    .get(&s.name)
                    .filter(|r| r.peak_mb > 0.0)
                    .map(|r| r.peak_mb * 1.15)
                    .unwrap_or(s.mem_gb * 1024.0);
                let used: f64 = running
                    .iter()
                    .map(|r| r.est_mb.max(r.peak as f64 / 1048576.0))
                    .sum();
                let avail = sys.available_memory() as f64 / 1048576.0;
                if !running.is_empty() && (used + est > budget_mb || avail - est < reserve_mb) {
                    continue;
                }
                if let (true, Some(e)) = (s.needs_rust(), &rust_err) {
                    log.line(&format!(
                        "FAILED {:<30} not started: the Rust tools are not fresh ({e})",
                        s.name
                    ));
                    status[i] = St::Failed;
                    failed_any = true;
                    timings.push((i, 0.0, 0.0, "FAILED (stale Rust tools)".into()));
                    let mut record = st.stages.get(&s.name).cloned().unwrap_or_default();
                    record.ok = false;
                    st.stages.insert(s.name.clone(), record);
                    st.save(&state_path)?;
                    events.emit("stage_finished", serde_json::json!({
                        "name": s.name, "result": "failed", "error": e, "seconds": 0.0, "peak_mb": 0.0,
                    }))?;
                    let d = graph::descendants(n, &deps, i);
                    for j in 0..n {
                        if d[j] && j != i && status[j] == St::Waiting {
                            status[j] = St::Blocked;
                        }
                    }
                    continue;
                }
                // start
                let trace_dir = log_dir.join("traces").join(&s.name);
                let _ = std::fs::remove_dir_all(&trace_dir);
                std::fs::create_dir_all(&trace_dir)?;
                let logf =
                    std::fs::File::create(log_dir.join("logs").join(format!("{}.log", s.name)))?;
                let cmd = s.run_cmd();
                // a relative program path with a separator (work/rust/target/release/fox.exe) is the repo's
                let prog = if let Some(bundle) = &bundle {
                    bundle
                        .resolve_program(&cmd[0])?
                        .to_string_lossy()
                        .into_owned()
                } else if cmd[0] == "python" {
                    o.python.clone()
                } else if let (Some(b), true) = (
                    &rust_bin,
                    cmd[0]
                        .replace('\\', "/")
                        .starts_with("work/rust/target/release/"),
                ) {
                    // a cmd_rs binary: this run's snapshot build
                    b.join(std::path::Path::new(&cmd[0]).file_name().unwrap())
                        .to_string_lossy()
                        .into_owned()
                } else if cmd[0].contains(['/', '\\'])
                    && std::path::Path::new(&cmd[0]).is_relative()
                {
                    root.join(&cmd[0]).to_string_lossy().into_owned()
                } else {
                    cmd[0].clone()
                };
                let mut c = Command::new(&prog);
                c.args(cmd[1..].iter().map(|arg| {
                    if bundle.is_none() && arg == "python" {
                        o.python.clone()
                    } else {
                        arg.clone()
                    }
                }))
                .current_dir(&root)
                .stdin(Stdio::null())
                .stdout(logf.try_clone()?)
                .stderr(logf)
                .env("TEMP", &temp)
                .env("TMP", &temp)
                .env("FOXBUILD_TRACE_DIR", &trace_dir)
                .env("FOXBUILD_ROOT", &root)
                .env("FOXBUILD_STAGE", &s.name);
                if let Some(st_) = &rust_stamp {
                    c.env("FOXBUILD_RUST_STAMP", st_).env("FOXRS_STAMP", st_);
                }
                if let Some(b) = &rust_bin {
                    c.env("FOX_BIN_DIR", b);
                }
                for k in &g.settings.unset_env {
                    c.env_remove(k);
                }
                for (k, v) in g.settings.env.iter().chain(s.env.iter()) {
                    c.env(k, v);
                }
                if let Some(bundle) = &bundle {
                    // Controlled package bindings override inherited/configured
                    // stale internal tool paths; no Python drop-in is installed.
                    c.env_remove("PYTHONPATH")
                        .env_remove("PYTHONUNBUFFERED")
                        .env("FOX_BIN_DIR", bundle.root())
                        .env("FOXBUILD_RUST_STAMP", rust_stamp.as_deref().unwrap())
                        .env(
                            "FOXBUILD_BUNDLE_MANIFEST",
                            o.bundled_tools.as_ref().unwrap(),
                        );
                } else {
                    let site = root.join("tools/build/trace_site");
                    let mut paths = vec![site];
                    if let Some(old) = std::env::var_os("PYTHONPATH") {
                        paths.extend(std::env::split_paths(&old));
                    }
                    c.env("PYTHONPATH", std::env::join_paths(paths)?)
                        .env("PYTHONUNBUFFERED", "1");
                }
                if let Some(prepared) = &prepared {
                    c.envs(&prepared.child_env);
                }
                // Invalidate before spawn, including crashes, I/O errors and
                // forced termination; retained output hashes remain historical.
                let mut record = st.stages.get(&s.name).cloned().unwrap_or_default();
                record.ok = false;
                st.stages.insert(s.name.clone(), record);
                st.save(&state_path)?;
                if cancel.requested()? {
                    break;
                }
                let priority = if game.is_some() {
                    Priority::Idle
                } else {
                    Priority::BelowNormal
                };
                let child = ManagedChild::spawn(&mut c, priority)
                    .with_context(|| format!("starting {}", s.name))?;
                events.emit(
                    "stage_started",
                    serde_json::json!({
                        "name": s.name, "pid": child.id(), "reason": reason[i],
                        "log_path": log_dir.join("logs").join(format!("{}.log", s.name)),
                    }),
                )?;
                for l in &s.locks {
                    locks_held.insert(l.clone());
                }
                log.line(&format!(
                    "start  {:<30} (est {:.0} MB; {})",
                    s.name, est, reason[i]
                ));
                status[i] = St::Running;
                running.push(Running {
                    i,
                    child,
                    start: Instant::now(),
                    peak: 0,
                    trace_dir,
                    est_mb: est,
                });
            }
        }

        report_progress(events, &selected, &status)?;
        let pending = status.contains(&St::Waiting);
        if running.is_empty() && (!pending || stop) {
            break;
        }
        if paused && pending {
            cancel.sleep(Duration::from_millis(500)); // waiting for the pause to end, not stuck
            continue;
        }
        if running.is_empty() && pending {
            // nothing running, something waiting, nothing startable: dependencies failed or a deadlock
            let stuck: Vec<&str> = (0..n)
                .filter(|&i| status[i] == St::Waiting && !reason[i].is_empty())
                .map(|i| g.stage[i].name.as_str())
                .collect();
            let unsettled: Vec<&str> = (0..n)
                .filter(|&i| status[i] == St::Waiting && reason[i].is_empty())
                .map(|i| g.stage[i].name.as_str())
                .collect();
            if !stuck.is_empty()
                && game.is_some()
                && stuck.iter().all(|nm| g.stage[g.index(nm).unwrap()].gpu)
            {
                cancel.sleep(Duration::from_secs(5)); // GPU stages wait for the game to close
                continue;
            }
            if stuck.is_empty()
                && unsettled.iter().all(|nm| {
                    deps[g.index(nm).unwrap()]
                        .iter()
                        .any(|&d| matches!(status[d], St::Failed | St::Blocked))
                })
            {
                for nm in unsettled {
                    let i = g.index(nm).unwrap();
                    status[i] = St::Blocked;
                }
                break;
            }
            if !stuck.is_empty() {
                // single stage over budget with nothing running: run it anyway (handled by `!running.is_empty()` above)
            }
        }
        cancel.sleep(Duration::from_millis(200));
    }

    for (i, state) in status.iter_mut().enumerate() {
        if !selected[i] {
            continue;
        }
        if *state == St::Waiting {
            *state = St::Blocked;
        }
        if matches!(*state, St::Blocked | St::Cancelled)
            && !timings.iter().any(|&(stage, _, _, _)| stage == i)
        {
            events.emit("stage_finished", serde_json::json!({
                "name": g.stage[i].name, "result": state.result(),
                "reason": if cancelled { "cancel requested before start" } else { "upstream failure or stop-on-error" },
                "seconds": 0.0, "peak_mb": 0.0,
            }))?;
        }
    }
    report_progress(events, &selected, &status)?;

    // ---- summary
    let total = log.t0.elapsed().as_secs_f64();
    log.line("--- timing ---------------------------------------------------------------");
    log.line(&format!(
        "{:<30} {:>9} {:>9}  {}",
        "stage", "seconds", "peak MB", "result"
    ));
    let mut sum = 0.0;
    for (i, secs, peak, what) in &timings {
        sum += secs;
        log.line(&format!(
            "{:<30} {:>9.1} {:>9.0}  {}",
            g.stage[*i].name, secs, peak, what
        ));
    }
    for (i, state) in status.iter().enumerate() {
        if *state == St::Blocked {
            log.line(&format!(
                "{:<30} {:>9} {:>9}  blocked (an upstream stage failed)",
                g.stage[i].name, "-", "-"
            ));
        }
    }
    log.line(&format!(
        "wall {:.0} s, stage time {:.0} s (parallel x{:.1}){}",
        total,
        sum,
        if total > 0.0 { sum / total } else { 0.0 },
        if failed_any { ", FAILURES" } else { "" }
    ));
    let summary: Vec<serde_json::Value> = timings
        .iter()
        .map(|(i, s, p, w)| serde_json::json!({"stage": g.stage[*i].name, "seconds": s, "peak_mb": p, "result": w}))
        .collect();
    std::fs::write(
        log_dir.join("last_run.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
        "finished": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        "wall_seconds": total, "stage_seconds": sum, "failed": failed_any, "cancelled": cancelled, "stages": summary}))?,
    )?;
    cache.save(&cache_path)?;
    let results: Vec<_> = status
        .iter()
        .enumerate()
        .filter(|&(i, _)| selected[i])
        .map(|(i, state)| {
            let timing = timings.iter().find(|&&(stage, _, _, _)| stage == i);
            serde_json::json!({
                "stage": g.stage[i].name, "result": state.result(),
                "seconds": timing.map(|(_, seconds, _, _)| *seconds).unwrap_or(0.0),
                "peak_mb": timing.map(|(_, _, peak, _)| *peak).unwrap_or(0.0),
            })
        })
        .collect();
    Ok(RunOutcome {
        code: if cancelled {
            130
        } else if failed_any {
            1
        } else {
            0
        },
        summary: serde_json::json!({"wall_seconds": total, "stage_seconds": sum, "stages": results}),
    })
}

fn report_progress(events: &mut Events, selected: &[bool], status: &[St]) -> Result<()> {
    events.emit("progress", serde_json::json!({
        "selected": selected.iter().filter(|&&value| value).count(),
        "completed": status.iter().filter(|state| matches!(state, St::Ran | St::Skipped | St::Failed | St::Blocked | St::Cancelled)).count(),
        "running": status.iter().filter(|&&state| state == St::Running).count(),
    }))
}

fn cancel_before_stages(
    events: &mut Events,
    graph: &Graph,
    selected: &[bool],
    log_dir: &Path,
    start: Instant,
) -> Result<RunOutcome> {
    let mut stages = Vec::new();
    for (stage, &selected) in graph.stage.iter().zip(selected) {
        if selected {
            let result = serde_json::json!({"name": stage.name, "result": "cancelled", "seconds": 0.0, "peak_mb": 0.0});
            events.emit("stage_finished", result.clone())?;
            stages.push(result);
        }
    }
    let summary = serde_json::json!({
        "finished": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        "wall_seconds": start.elapsed().as_secs_f64(), "stage_seconds": 0.0,
        "failed": false, "cancelled": true, "stages": stages,
    });
    std::fs::write(
        log_dir.join("last_run.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    Ok(RunOutcome { code: 130, summary })
}

// This diagnostic marker is owned only while our build lease is held.
struct PauseAwareGuard(PathBuf);

impl Drop for PauseAwareGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
