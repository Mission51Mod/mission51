//! Actual scheduler process tests with an authored Rust-only executable and
//! isolated project/package roots. No Python, game files, or private graph data.
use foxbuild::{State, bundle::BuildIdentity};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

fn artifacts() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::env::var_os("FOXBUILD_TEST_ARTIFACTS")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::temp_dir().join(format!("foxbuild_cli_{}", std::process::id()))
            });
        fs::create_dir_all(&root).unwrap();
        root.canonicalize().unwrap()
    })
}

fn native_tool() -> &'static Path {
    static TOOL: OnceLock<PathBuf> = OnceLock::new();
    TOOL.get_or_init(|| {
        let directory = artifacts().join("fixture_tool");
        fs::create_dir_all(&directory).unwrap();
        let executable = directory.join(if cfg!(windows) { "fox.exe" } else { "fox" });
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/native_tool.rs");
        let result = Command::new("rustc")
            .args(["--edition=2024", "-C", "opt-level=0"])
            .arg(source)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        executable
    })
}

struct Fixture {
    directory: PathBuf,
    project: PathBuf,
    manifest: PathBuf,
    tool: PathBuf,
    cancel: PathBuf,
    run: AtomicU64,
}

impl Fixture {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = artifacts().join(format!(
            "{label}_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let project = directory.join("project");
        let package = directory.join("package");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&package).unwrap();
        let tool = package.join(if cfg!(windows) { "fox.exe" } else { "fox" });
        fs::copy(native_tool(), &tool).unwrap();
        let manifest = package.join("fox-tools.json");
        let build_id = option_env!("FOX_BUNDLE_BUILD_ID").unwrap_or("unpackaged-fixture");
        fs::write(&manifest, serde_json::to_vec_pretty(&json!({
            "schema_version": 1, "package_version": env!("CARGO_PKG_VERSION"), "build_id": build_id,
            "tools": {"fox": {"path": tool.file_name().unwrap().to_str().unwrap(), "blake3": blake3::hash(&fs::read(&tool).unwrap()).to_hex().to_string()}},
        })).unwrap()).unwrap();
        let cancel = directory.join("cancel");
        println!("CLI fixture: {}", directory.display());
        Self {
            directory,
            project,
            manifest,
            tool,
            cancel,
            run: AtomicU64::new(0),
        }
    }

    fn write(&self, relative: &str, bytes: impl AsRef<[u8]>) {
        let path = self.project.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn graph(&self, stages: &str) {
        self.write(
            "graph.toml",
            format!("[settings]\nmax_jobs = 1\nmem_reserve_gb = 0.0\n{stages}"),
        );
    }

    fn command(&self, bundled: bool, extra: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_foxbuild"));
        command
            .arg("--root")
            .arg(&self.project)
            .args(["--config", "graph.toml", "--python"])
            .arg(&self.tool)
            .arg("--cancel-file")
            .arg(&self.cancel)
            .arg("--json");
        if bundled {
            command.arg("--bundled-tools").arg(&self.manifest);
        }
        command
            .args(extra)
            .env("FIXTURE_PROBE_SIGNAL", self.directory.join("probe"))
            .env("FIXTURE_SNAPSHOT_SIGNAL", self.directory.join("snapshot"))
            .env("FIXTURE_HEARTBEAT", self.directory.join("heartbeat"))
            .env("FIXTURE_CHILD_PID", self.directory.join("child_pid"))
            .env("FIXTURE_SCHEDULER", env!("CARGO_BIN_EXE_foxbuild"));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        command
    }

    fn run(&self, bundled: bool, extra: &[&str]) -> Output {
        let output = self.command(bundled, extra).output().unwrap();
        let run = self.run.fetch_add(1, Ordering::Relaxed);
        fs::write(
            self.directory.join(format!("run{run}.jsonl")),
            &output.stdout,
        )
        .unwrap();
        fs::write(
            self.directory.join(format!("run{run}.stderr")),
            &output.stderr,
        )
        .unwrap();
        fs::write(
            self.directory.join(format!("run{run}.exit")),
            output.status.code().unwrap_or(-1).to_string(),
        )
        .unwrap();
        output
    }

    fn launch(&self, bundled: bool, extra: &[&str]) -> Invocation {
        let mut command = self.command(bundled, extra);
        let run = self.run.fetch_add(1, Ordering::Relaxed);
        let stdout = self.directory.join(format!("live{run}.jsonl"));
        let stderr = self.directory.join(format!("live{run}.stderr"));
        command
            .stdout(fs::File::create(&stdout).unwrap())
            .stderr(fs::File::create(&stderr).unwrap())
            .stdin(Stdio::null());
        Invocation {
            child: command.spawn().unwrap(),
            cancel: self.cancel.clone(),
            stdout,
            stderr,
            finished: false,
        }
    }

    fn state(&self) -> State {
        serde_json::from_slice(&fs::read(self.project.join("work/build/state.json")).unwrap())
            .unwrap()
    }
}

struct Invocation {
    child: Child,
    cancel: PathBuf,
    stdout: PathBuf,
    stderr: PathBuf,
    finished: bool,
}

impl Invocation {
    fn finish(mut self) -> Output {
        wait_for(
            || self.child.try_wait().unwrap().is_some(),
            "scheduler exit",
        );
        let status = self.child.wait().unwrap();
        self.finished = true;
        Output {
            status,
            stdout: fs::read(&self.stdout).unwrap(),
            stderr: fs::read(&self.stderr).unwrap(),
        }
    }
}

impl Drop for Invocation {
    fn drop(&mut self) {
        if !self.finished {
            let _ = fs::write(&self.cancel, "cancel failed test");
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if self.child.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn wait_for(mut predicate: impl FnMut() -> bool, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {label}");
}

fn events(output: &Output, code: i32) -> Vec<Value> {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = std::str::from_utf8(&output.stdout).unwrap();
    let records: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(!records.is_empty());
    assert_eq!(records[0]["event"], "run_started");
    assert_eq!(records.last().unwrap()["event"], "run_finished");
    assert_eq!(records.last().unwrap()["data"]["exit_code"], code);
    assert_eq!(
        records
            .iter()
            .filter(|record| record["event"] == "run_finished")
            .count(),
        1
    );
    let mut previous_elapsed = 0;
    for (index, record) in records.iter().enumerate() {
        assert_eq!(record["schema_version"], 1);
        assert_eq!(record["sequence"], index + 1);
        assert_eq!(record["run_id"], records[0]["run_id"]);
        let elapsed = record["elapsed_ms"].as_u64().unwrap();
        assert!(elapsed >= previous_elapsed);
        previous_elapsed = elapsed;
    }
    records
}

fn finished(records: &[Value], result: &str) -> usize {
    records
        .iter()
        .filter(|record| record["event"] == "stage_finished" && record["data"]["result"] == result)
        .count()
}

fn copy_graph() -> &'static str {
    "[[stage]]\nname = 'first'\ncmd = ['fox', 'copy', 'input.txt', 'out/first.txt']\noutputs = ['out/first.txt']\nrust_tools = true\n[[stage]]\nname = 'second'\ncmd = ['work/rust/target/release/fox.exe', 'copy', 'out/first.txt', 'out/second.txt']\noutputs = ['out/second.txt']\nafter = ['first']\nrust_tools = true\n"
}

#[test]
#[ignore = "packaged CLI: run with FOX_BUNDLE_BUILD_ID and --include-ignored"]
fn native_build_status_and_incremental_changes_use_no_python_or_source_tree() {
    BuildIdentity::compiled().expect("set FOX_BUNDLE_BUILD_ID for packaged CLI tests");
    let fixture = Fixture::new("incremental");
    fixture.graph(copy_graph());
    fixture.write("input.txt", "original bytes");
    let mut command = fixture.command(true, &[]);
    command.env("PATH", "").env("PYTHONPATH", "stale-drop-in");
    let output = command.output().unwrap();
    fs::write(fixture.directory.join("empty_path.jsonl"), &output.stdout).unwrap();
    assert_eq!(finished(&events(&output, 0), "succeeded"), 2);
    assert!(!fixture.directory.join("probe").exists());
    assert!(!fixture.directory.join("snapshot").exists());
    assert!(!fixture.project.join("tools").exists());
    assert!(!fixture.project.join("work/rust").exists());
    assert!(fixture.state().stages["first"].trace_complete);
    assert_eq!(
        fs::read(fixture.project.join("out/second.txt")).unwrap(),
        b"original bytes"
    );
    assert!(
        fs::read_to_string(fixture.project.join("work/build/logs/first.log"))
            .unwrap()
            .contains("native stage:")
    );
    let before = fs::read(fixture.project.join("work/build/state.json")).unwrap();
    assert_eq!(finished(&events(&fixture.run(true, &[]), 0), "skipped"), 2);
    assert_eq!(
        fs::read(fixture.project.join("work/build/state.json")).unwrap(),
        before
    );
    let status = events(&fixture.run(true, &["--status"]), 0);
    let report = &status
        .iter()
        .find(|record| record["event"] == "status")
        .unwrap()["data"];
    assert!(
        report["stages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|stage| stage["dirty"].is_null())
    );
    assert_eq!(
        fs::read(fixture.project.join("work/build/state.json")).unwrap(),
        before
    );
    fixture.write("input.txt", "changed input with a different length");
    assert_eq!(
        finished(&events(&fixture.run(true, &[]), 0), "succeeded"),
        2
    );
    fs::remove_file(fixture.project.join("out/second.txt")).unwrap();
    let rebuilt = events(&fixture.run(true, &[]), 0);
    assert_eq!(finished(&rebuilt, "skipped"), 1);
    assert_eq!(finished(&rebuilt, "succeeded"), 1);
}

#[test]
fn parser_errors_and_help_always_have_one_terminal_json_record() {
    for args in [
        vec!["--json", "--unknown"],
        vec!["--json", "--jobs", "0"],
        vec!["--json", "--mem-gb", "NaN"],
        vec!["--json", "--config"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_foxbuild"))
            .args(args)
            .output()
            .unwrap();
        let records = events(&output, 2);
        assert!(records.iter().any(|record| record["event"] == "error"));
    }
    let output = Command::new(env!("CARGO_BIN_EXE_foxbuild"))
        .args(["--json", "--help"])
        .output()
        .unwrap();
    let records = events(&output, 0);
    assert!(
        records.last().unwrap()["data"]["help"]
            .as_str()
            .unwrap()
            .contains("--bundled-tools")
    );
    let plain = Command::new(env!("CARGO_BIN_EXE_foxbuild"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(
        std::str::from_utf8(&plain.stdout)
            .unwrap()
            .contains("--cancel-file")
    );
    assert!(
        !std::str::from_utf8(&plain.stdout)
            .unwrap()
            .contains("\"run_started\"")
    );
}

#[test]
fn unvalidated_or_unpackaged_bundles_fail_before_any_child() {
    let fixture = Fixture::new("unvalidated");
    fixture.graph(copy_graph());
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(&fixture.manifest).unwrap()).unwrap();
    manifest["build_id"] = json!("different-package-build");
    fs::write(&fixture.manifest, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let records = events(&fixture.run(true, &[]), 2);
    assert!(
        !records
            .iter()
            .any(|record| record["event"] == "stage_started")
    );
    assert!(!fixture.directory.join("probe").exists());
    assert!(!fixture.directory.join("snapshot").exists());
    assert!(!fixture.project.join("work/build/foxbuild.lock").exists());
}

#[test]
#[ignore = "packaged CLI: run with FOX_BUNDLE_BUILD_ID and --include-ignored"]
fn bundled_mode_rejects_python_commands_arguments_and_modified_executables() {
    let fixture = Fixture::new("native_only");
    for command in [
        "['python', 'reference.py']",
        "['fox', 'copy', 'reference.py', 'out.txt']",
    ] {
        fixture.graph(&format!("[[stage]]\nname = 'bad'\ncmd = {command}\n"));
        let records = events(&fixture.run(true, &[]), 2);
        assert!(
            !records
                .iter()
                .any(|record| record["event"] == "stage_started")
        );
    }
    fixture.graph(copy_graph());
    fs::write(&fixture.tool, "replaced executable").unwrap();
    assert!(String::from_utf8_lossy(&fixture.run(true, &[]).stderr).contains("BLAKE3 mismatch"));
}

#[test]
#[ignore = "packaged CLI: run with FOX_BUNDLE_BUILD_ID and --include-ignored"]
fn bound_native_tool_path_matches_alias_and_rejects_an_unlisted_path() {
    let fixture = Fixture::new("bound_path");
    fixture.write("input.txt", "bound path bytes");
    let program = serde_json::to_string(&fixture.tool.to_string_lossy()).unwrap();
    fixture.graph(&format!("[[stage]]\nname = 'bound'\ncmd = [{program}, 'copy', 'input.txt', 'output.txt']\noutputs = ['output.txt']\nrust_tools = true\n"));
    assert_eq!(
        finished(&events(&fixture.run(true, &[]), 0), "succeeded"),
        1
    );
    let other = fixture
        .directory
        .join(if cfg!(windows) { "other.exe" } else { "other" });
    fs::copy(native_tool(), &other).unwrap();
    let program = serde_json::to_string(&other.to_string_lossy()).unwrap();
    fixture.graph(&format!(
        "[[stage]]\nname = 'bound'\ncmd = [{program}, 'copy', 'input.txt', 'output.txt']\n"
    ));
    assert!(String::from_utf8_lossy(&fixture.run(true, &[]).stderr).contains("not in this bundle"));
}

#[test]
#[ignore = "packaged CLI: run with FOX_BUNDLE_BUILD_ID and --include-ignored"]
fn failed_stage_blocks_downstream_and_log_requirements_still_apply() {
    let fixture = Fixture::new("failures");
    fixture.graph("[[stage]]\nname = 'bad'\ncmd = ['fox', 'fail']\n[[stage]]\nname = 'dependent'\ncmd = ['fox', 'copy', 'input.txt', 'output.txt']\nafter = ['bad']\n");
    let records = events(&fixture.run(true, &["--stop-on-error"]), 1);
    assert_eq!(finished(&records, "failed"), 1);
    assert_eq!(finished(&records, "blocked"), 1);
    assert!(!fixture.state().stages["bad"].ok);
    fixture.write("input.txt", "verification bytes");
    fixture.graph("[[stage]]\nname = 'verify'\ncmd = ['fox', 'copy', 'input.txt', 'output.txt']\noutputs = ['output.txt']\nrequire_output = 'ACCEPTED_SENTINEL'\n");
    let records = events(&fixture.run(true, &[]), 1);
    let failure = records
        .iter()
        .find(|record| record["event"] == "stage_finished")
        .unwrap();
    assert_eq!(failure["data"]["exit_code"], 0);
    assert!(
        failure["data"]["error"]
            .as_str()
            .unwrap()
            .contains("log lacks")
    );
}

#[test]
fn cancellation_before_spawn_and_while_paused_preserves_marker_and_releases_lock() {
    let fixture = Fixture::new("pre_cancel");
    fixture.graph("[[stage]]\nname = 'copy'\ncmd = ['unavailable']\n");
    fs::write(&fixture.cancel, "cancel").unwrap();
    let records = events(&fixture.run(false, &[]), 130);
    assert!(
        !records
            .iter()
            .any(|record| record["event"] == "stage_started")
    );
    assert!(fixture.cancel.exists());
    assert!(!fixture.directory.join("probe").exists());
    assert!(!fixture.project.join("work/build/foxbuild.lock").exists());
    fs::remove_file(&fixture.cancel).unwrap();
    fixture.write("work/build/PAUSE", "paused fixture");
    let process = fixture.launch(false, &[]);
    wait_for(
        || {
            fixture
                .project
                .join("work/build/foxbuild.pause-aware")
                .exists()
        },
        "paused scheduler",
    );
    fs::write(&fixture.cancel, "cancel paused scheduler").unwrap();
    let records = events(&process.finish(), 130);
    assert_eq!(finished(&records, "cancelled"), 1);
    assert!(
        !records
            .iter()
            .any(|record| record["event"] == "stage_started")
    );
    assert!(!fixture.project.join("work/build/foxbuild.lock").exists());
    assert!(
        !fixture
            .project
            .join("work/build/foxbuild.pause-aware")
            .exists()
    );
}

fn assert_heartbeat_stopped(path: &Path) {
    std::thread::sleep(Duration::from_millis(150));
    let before = fs::metadata(path).unwrap().len();
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(
        fs::metadata(path).unwrap().len(),
        before,
        "descendant survived cancellation"
    );
}

#[test]
#[ignore = "packaged CLI: run with FOX_BUNDLE_BUILD_ID and --include-ignored"]
fn cancellation_kills_native_stage_descendants_and_invalidates_persisted_state() {
    let fixture = Fixture::new("stage_cancel");
    let heartbeat =
        serde_json::to_string(&fixture.directory.join("heartbeat").to_string_lossy()).unwrap();
    let pids =
        serde_json::to_string(&fixture.directory.join("stage_pids").to_string_lossy()).unwrap();
    fixture.graph(&format!("[[stage]]\nname = 'parent'\ncmd = ['fox', 'spawn', {heartbeat}, {pids}]\n[[stage]]\nname = 'later'\ncmd = ['fox', 'fail']\nafter = ['parent']\n"));
    let process = fixture.launch(true, &[]);
    wait_for(
        || fixture.directory.join("heartbeat").exists(),
        "native descendant heartbeat",
    );
    assert!(!fixture.state().stages["parent"].ok);
    fs::write(&fixture.cancel, "cancel native tree").unwrap();
    let records = events(&process.finish(), 130);
    assert_eq!(finished(&records, "cancelled"), 2);
    assert!(!fixture.state().stages["parent"].ok);
    assert!(
        !records
            .iter()
            .any(|record| record["event"] == "stage_started" && record["data"]["name"] == "later")
    );
    assert_heartbeat_stopped(&fixture.directory.join("heartbeat"));
    assert!(!fixture.project.join("work/build/foxbuild.lock").exists());
    let summary: Value = serde_json::from_slice(
        &fs::read(fixture.project.join("work/build/last_run.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(summary["cancelled"], true);
}

#[test]
fn cancellation_reaches_internal_snapshot_and_interpreter_probe_trees() {
    for probe in [false, true] {
        let fixture = Fixture::new(if probe {
            "probe_cancel"
        } else {
            "snapshot_cancel"
        });
        let program = serde_json::to_string(&fixture.tool.to_string_lossy()).unwrap();
        fixture.graph(&format!(
            "[[stage]]\nname = 'internal'\ncmd = [{program}, 'fail']\nrust_tools = true\n"
        ));
        fixture.write(
            "tools/rust/build.py",
            "authored path marker; Rust fixture handles this argv",
        );
        let mut command = fixture.command(false, &[]);
        if probe {
            command.env("FIXTURE_BLOCK_PROBE", "1");
        }
        let stdout = fixture.directory.join("live.jsonl");
        let stderr = fixture.directory.join("live.stderr");
        command
            .stdout(fs::File::create(&stdout).unwrap())
            .stderr(fs::File::create(&stderr).unwrap());
        let process = Invocation {
            child: command.spawn().unwrap(),
            cancel: fixture.cancel.clone(),
            stdout,
            stderr,
            finished: false,
        };
        wait_for(
            || fixture.directory.join("heartbeat").exists(),
            "managed probe/snapshot descendant",
        );
        fs::write(&fixture.cancel, "cancel prerequisite").unwrap();
        let records = events(&process.finish(), 130);
        assert_eq!(finished(&records, "cancelled"), 1);
        assert!(
            !records
                .iter()
                .any(|record| record["event"] == "stage_started")
        );
        assert_heartbeat_stopped(&fixture.directory.join("heartbeat"));
        assert!(!fixture.project.join("work/build/foxbuild.lock").exists());
    }
}

#[test]
#[ignore = "packaged CLI: run with FOX_BUNDLE_BUILD_ID and --include-ignored"]
fn list_graph_and_dry_run_emit_structure_without_starting_children() {
    let fixture = Fixture::new("inspection");
    fixture.graph(copy_graph());
    fixture.write("input.txt", "planned bytes");
    for flag in ["--list", "--graph", "--dry-run"] {
        let records = events(&fixture.run(true, &[flag]), 0);
        assert_eq!(
            records
                .iter()
                .filter(|record| record["event"] == "stage_declared")
                .count(),
            2
        );
        assert!(
            !records
                .iter()
                .any(|record| record["event"] == "stage_started")
        );
        if flag == "--dry-run" {
            assert_eq!(
                records
                    .iter()
                    .filter(|record| record["event"] == "stage_planned")
                    .count(),
                2
            );
        }
    }
    assert!(!fixture.directory.join("probe").exists());
    assert!(!fixture.directory.join("snapshot").exists());
    assert!(!fixture.project.join("work/build/state.json").exists());
}

fn toml_path(path: &Path) -> String {
    serde_json::to_string(&path.to_string_lossy()).unwrap()
}

fn gated_graph(fixture: &Fixture) -> String {
    format!(
        "[[stage]]\nname = 'writer'\ncmd = ['fox', 'gated-copy', 'input.txt', 'output.txt', {}, {}]\noutputs = ['output.txt']\n",
        toml_path(&fixture.directory.join("writer_ready")),
        toml_path(&fixture.directory.join("release_writer"))
    )
}

#[test]
#[ignore = "packaged CLI: run with FOX_BUNDLE_BUILD_ID and --include-ignored"]
fn concurrent_empty_and_stale_lock_recovery_admits_one_writer_and_preserves_owner() {
    for previous in ["", "4294967295"] {
        let fixture = Fixture::new(if previous.is_empty() {
            "empty_lock"
        } else {
            "stale_lock"
        });
        fixture.graph(&gated_graph(&fixture));
        fixture.write("input.txt", "one exclusive writer");
        fixture.write("work/build/foxbuild.lock", previous);
        let mut first = fixture.launch(true, &[]);
        let mut second = fixture.launch(true, &[]);
        wait_for(
            || {
                first.child.try_wait().unwrap().is_some()
                    || second.child.try_wait().unwrap().is_some()
            },
            "losing lock contender",
        );
        let (winner, loser) = if first.child.try_wait().unwrap().is_some() {
            (second, first)
        } else {
            (first, second)
        };
        let loser_events = events(&loser.finish(), 2);
        assert!(
            !loser_events
                .iter()
                .any(|record| record["event"] == "stage_declared")
        );
        assert!(
            !loser_events
                .iter()
                .any(|record| record["event"] == "stage_started")
        );
        wait_for(
            || fixture.directory.join("writer_ready").exists(),
            "exclusive writer",
        );
        let lock = fixture.project.join("work/build/foxbuild.lock");
        let owner = fs::read(&lock).unwrap();
        assert_eq!(
            String::from_utf8(owner.clone()).unwrap(),
            winner.child.id().to_string()
        );
        assert_eq!(
            foxbuild::lock_holder(&fixture.project.join("work/build")),
            Some(winner.child.id())
        );
        // Another failed guard must neither rewrite nor unlink the current PID.
        events(&fixture.run(true, &[]), 2);
        assert_eq!(fs::read(&lock).unwrap(), owner);
        fs::write(fixture.directory.join("release_writer"), "release").unwrap();
        assert_eq!(finished(&events(&winner.finish(), 0), "succeeded"), 1);
        assert!(!lock.exists());
        assert!(fixture.project.join("work/build/foxbuild.lease").exists());
        assert_eq!(finished(&events(&fixture.run(true, &[]), 0), "skipped"), 1);
    }
}

#[test]
#[ignore = "packaged CLI: run with FOX_BUNDLE_BUILD_ID and --include-ignored"]
fn a_new_owner_loads_completed_records_and_learned_dependencies_after_the_lease() {
    let fixture = Fixture::new("owner_state");
    fixture.write("input.txt", "next producer bytes");
    fixture.write("out/producer.txt", "completed owner bytes");
    fixture.graph(&format!(
        "[[stage]]\nname = 'producer'\ncmd = ['fox', 'copy', 'input.txt', 'out/producer.txt']\noutputs = ['out/producer.txt']\ndefault = false\n\
         [[stage]]\nname = 'consumer'\ncmd = ['fox', 'gated-copy', 'out/producer.txt', 'out/consumer.txt', {}, {}]\noutputs = ['out/consumer.txt']\n",
        toml_path(&fixture.directory.join("writer_ready")),
        toml_path(&fixture.directory.join("release_writer"))
    ));
    let owner = fixture.launch(true, &["--only", "consumer"]);
    wait_for(
        || fixture.directory.join("writer_ready").exists(),
        "completing owner",
    );
    let rejected = events(&fixture.run(true, &["--only", "producer"]), 2);
    // No state-derived event may escape before exclusive ownership. This is
    // the point at which the reviewed scheduler had already used stale state.
    assert!(
        !rejected
            .iter()
            .any(|record| record["event"] == "stage_declared")
    );
    fs::write(fixture.directory.join("release_writer"), "finish consumer").unwrap();
    events(&owner.finish(), 0);
    let completed = serde_json::to_value(&fixture.state().stages["consumer"]).unwrap();
    assert_eq!(completed["ok"], true);
    let next = events(&fixture.run(true, &["--only", "producer"]), 0);
    assert_eq!(
        serde_json::to_value(&fixture.state().stages["consumer"]).unwrap(),
        completed
    );
    let declaration = next
        .iter()
        .find(|record| record["event"] == "stage_declared" && record["data"]["name"] == "consumer")
        .unwrap();
    assert_eq!(declaration["data"]["dependencies"], json!(["producer"]));
    assert!(next.iter().any(|record| {
        record["event"] == "warning"
            && record["data"]["message"]
                .as_str()
                .unwrap()
                .contains("learned edge")
    }));
}

#[test]
#[ignore = "packaged CLI: run with FOX_BUNDLE_BUILD_ID and --include-ignored"]
fn review_recursive_native_builds_fail_before_inner_interpreter_or_program_execution() {
    let fixture = Fixture::new("recursive_bundle");
    let copied =
        fixture
            .manifest
            .parent()
            .unwrap()
            .join(if cfg!(windows) { "relay.exe" } else { "relay" });
    fs::copy(&fixture.tool, &copied).unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(&fixture.manifest).unwrap()).unwrap();
    manifest["tools"]["copied"] = manifest["tools"]["fox"].clone();
    manifest["tools"]["copied"]["path"] = copied.file_name().unwrap().to_str().unwrap().into();
    fs::write(&fixture.manifest, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let nested = fixture.project.join("nested");
    fs::create_dir_all(&nested).unwrap();
    let marker = fixture.directory.join("inner_program_ran");
    fs::write(nested.join("inner.toml"), format!(
        "[settings]\nmem_reserve_gb = 0.0\n[[stage]]\nname = 'inner'\ncmd = [{}, 'touch', {}]\n",
        toml_path(&fixture.tool), toml_path(&marker)
    )).unwrap();
    let suffix = format!(
        "'build', '--root', {}, '--config', 'inner.toml', '--python', {}",
        toml_path(&nested),
        toml_path(&fixture.tool)
    );
    for command in [
        format!("['fox', {suffix}]"),
        format!("['copied', {suffix}]"),
        format!("[{}, {suffix}]", toml_path(&copied)),
        format!("['work/rust/target/release/fox.exe', {suffix}]"),
        format!("[{}, {suffix}]", toml_path(&fixture.tool)),
        format!(
            "['fox', '--runtime-profile', 'unused.json', {suffix}, '--bundled-tools', 'alternate.json']"
        ),
    ] {
        fixture.graph(&format!("[[stage]]\nname = 'outer'\ncmd = {command}\n"));
        let records = events(&fixture.run(true, &[]), 2);
        assert!(records.iter().any(|record| {
            record["event"] == "error"
                && record["data"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("recursive build")
        }));
        assert!(
            !records
                .iter()
                .any(|record| record["event"] == "stage_started")
        );
        assert!(!fixture.directory.join("probe").exists());
        assert!(!fixture.directory.join("snapshot").exists());
        assert!(!marker.exists());
        assert!(!nested.join("work/build").exists());
    }
}

#[test]
#[ignore = "packaged CLI: run with FOX_BUNDLE_BUILD_ID and --include-ignored"]
fn review_dry_run_and_status_cache_reads_cannot_publish_over_a_real_owner() {
    use foxbuild::{StageRecord, bundle::BundledTools, config::Graph};
    let fixture = Fixture::new("cache_ownership");
    fixture.write("input.txt", "current input creates a fresh cached hash");
    fixture.graph(&format!(
        "{}\n[[stage]]\nname = 'consumer'\ncmd = ['fox', 'copy', 'input.txt', 'consumer.txt']\ndefault = false\n",
        gated_graph(&fixture)
    ));
    let graph = Graph::load(&fixture.project.join("graph.toml")).unwrap();
    let tools = BundledTools::load_compiled(&fixture.manifest).unwrap();
    let consumer = graph
        .stage
        .iter()
        .find(|stage| stage.name == "consumer")
        .unwrap();
    let mut state = State::default();
    state.stages.insert(
        "consumer".into(),
        StageRecord {
            ok: true,
            trace_complete: true,
            cmd_fp: tools.command_fingerprint(consumer).unwrap(),
            inputs: [("input.txt".into(), "previous input hash".into())].into(),
            ..StageRecord::default()
        },
    );
    fixture.write("work/build/state.json", serde_json::to_vec(&state).unwrap());
    fixture.write(
        "work/build/hashcache.json",
        b"{\"entries\":{\"history\":[1,1,\"owner history\"]}}\n",
    );
    let owner = fixture.launch(true, &["--only", "writer"]);
    wait_for(
        || fixture.directory.join("writer_ready").exists(),
        "cache owner",
    );
    let directory = fixture.project.join("work/build");
    let cache = directory.join("hashcache.json");
    let temporary = directory.join("hashcache.json.tmp");
    // Emulate the owner's in-progress atomic publication. Readers must not
    // replace, rename or consume this fixed temporary while its lease is held.
    fs::write(&temporary, b"owner's staged cache bytes").unwrap();
    let prior_cache = fs::read(&cache).unwrap();
    let prior_temporary = fs::read(&temporary).unwrap();
    let prior_state = fs::read(directory.join("state.json")).unwrap();
    let dry = events(&fixture.run(true, &["--dry-run", "--only", "consumer"]), 0);
    assert_eq!(
        dry.iter()
            .filter(|record| record["event"] == "stage_planned")
            .count(),
        1
    );
    assert_eq!(fs::read(&cache).unwrap(), prior_cache);
    assert_eq!(fs::read(&temporary).unwrap(), prior_temporary);
    assert_eq!(fs::read(directory.join("state.json")).unwrap(), prior_state);
    let records = events(&fixture.run(true, &["--status"]), 0);
    let report = &records
        .iter()
        .find(|record| record["event"] == "status")
        .unwrap()["data"];
    assert_eq!(report["running_pid"], owner.child.id());
    assert_eq!(fs::read(&cache).unwrap(), prior_cache);
    assert_eq!(fs::read(&temporary).unwrap(), prior_temporary);
    fs::write(fixture.directory.join("release_writer"), "complete owner").unwrap();
    events(&owner.finish(), 0);

    let empty = Fixture::new("read_only_plan");
    empty.graph(copy_graph());
    empty.write("input.txt", "unbuilt plan");
    events(&empty.run(true, &["--dry-run"]), 0);
    assert!(
        !empty.project.join("work").exists(),
        "dry run created build state/cache/log/temp files"
    );
}
