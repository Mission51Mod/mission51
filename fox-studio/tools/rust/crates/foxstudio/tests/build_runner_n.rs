//! Synthetic process/plan tests. No real scheduler stage, installation or renderer.
#[cfg(unix)]
use foxstudio::build_events::{CompletionIssue, CompletionOutcome};
#[cfg(unix)]
use foxstudio::build_view::Live;
#[cfg(unix)]
use foxstudio::runner::BuildRun;
use foxstudio::runner::{self, BuildCmd, BuildContext, RunTarget};
#[cfg(unix)]
use serde_json::Value;
use serde_json::json;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(unix)]
use std::time::{Duration, Instant};

struct Fixture {
    root: PathBuf,
    graph: PathBuf,
    tool: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("foxstudio_runner_n_{}_{nonce}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let graph = root.join("stages.toml");
        std::fs::write(&graph, "[settings]\nlog_dir = \"logs\"\n[[stage]]\nname = \"fixture\"\ncmd = [\"never_execute\"]\noutputs = [\"missing\"]\n").unwrap();
        let tool = root.join("synthetic_fox");
        std::fs::write(&tool, b"synthetic native tool fixture\n").unwrap();
        Self { tool, root, graph }
    }
    fn target(&self) -> RunTarget<'_> {
        RunTarget {
            fox_exe: &self.tool,
            root: &self.root,
            graph: &self.graph,
            python: "no-python-needed-by-mock",
            project_spec: None,
        }
    }
    #[cfg(unix)]
    fn script(&self, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&self.tool, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&self.tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(unix)]
    fn start(&self, cmd: BuildCmd) -> BuildRun {
        BuildRun::start_with_context(
            self.target(),
            cmd,
            &eframe::egui::Context::default(),
            Some(&BuildContext::json_v1()),
        )
        .unwrap()
    }
    fn no_stage_output(&self) {
        assert!(!self.root.join("missing").exists());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let temp = std::env::temp_dir().canonicalize().unwrap();
        if let Ok(path) = self.root.canonicalize() {
            assert!(path.starts_with(&temp) && path != temp);
            std::fs::remove_dir_all(path).unwrap();
        }
    }
}

#[cfg(unix)]
fn event(sequence: u64, kind: &str, data: Value) -> Value {
    json!({"schema_version":1,"sequence":sequence,"run_id":"synthetic-run","elapsed_ms":0,"event":kind,"data":data})
}
#[cfg(unix)]
fn start_event() -> Value {
    event(1, "run_started", json!({"operation":"status"}))
}
#[cfg(unix)]
fn finish_event(sequence: u64, result: &str, exit: i32) -> Value {
    event(
        sequence,
        "run_finished",
        json!({"result":result,"exit_code":exit,"elapsed_seconds":0.01}),
    )
}
#[cfg(unix)]
fn print_json(value: Value) -> String {
    format!(
        "printf '%s\\n' '{}'\n",
        value.to_string().replace('\'', "'\\''")
    )
}

#[cfg(unix)]
fn until(run: &mut BuildRun, condition: impl Fn(&BuildRun) -> bool) {
    let started = Instant::now();
    loop {
        run.poll();
        if condition(run) {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "synthetic child timed out: {:?}",
            run.lines
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn json_context_adds_only_explicit_flags_and_preserves_parent_identity() {
    let fixture = Fixture::new();
    let before = std::env::vars_os().collect::<std::collections::BTreeMap<_, _>>();
    let cancel = fixture.root.join("explicit-cancel");
    let command = runner::build_command_with_context(
        fixture.target(),
        &BuildCmd::Status,
        Some(&BuildContext::json_v1()),
        Some(&cancel),
    )
    .unwrap();
    let args = command.get_args().collect::<Vec<_>>();
    assert!(args.iter().any(|arg| *arg == "--json"));
    assert!(
        args.windows(2)
            .any(|args| args[0] == "--cancel-file" && args[1] == cancel.as_os_str())
    );
    assert!(!cancel.exists());
    assert_eq!(command.get_current_dir(), Some(fixture.root.as_path()));
    assert_eq!(
        std::env::vars_os().collect::<std::collections::BTreeMap<_, _>>(),
        before
    );
    fixture.no_stage_output();
}

#[test]
fn project_preparation_uses_selected_root_actual_graph_and_remains_read_only() {
    let fixture = Fixture::new();
    let spec = fixture.root.join("project.toml");
    std::fs::write(&spec, "format = 1\n[project]\nname = \"native_fixture\"\n[location]\ncode = \"nfx\"\nid = 99\n[[stage]]\nname = \"check\"\ncmd = [\"fox\", \"project-check\"]\noutputs = []\n").unwrap();
    let target = RunTarget {
        project_spec: Some(&spec),
        ..fixture.target()
    };
    let cwd = std::env::current_dir().unwrap();
    let prepared = runner::prepare_command(target, &BuildCmd::Status, None, None).unwrap();
    let plan = prepared.plan.as_ref().unwrap();
    assert_eq!(plan.repo_root(), fixture.root.as_path());
    assert!(!plan.graph_path().exists());
    assert_eq!(prepared.command.get_current_dir(), Some(plan.repo_root()));
    assert!(!prepared.command.get_args().any(|arg| arg == "--config"));
    let args = prepared.command.get_args().collect::<Vec<_>>();
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "--project" && pair[1] == plan.spec_path().as_os_str())
    );
    assert!(prepared.command.get_envs().any(|(key,value)| key == "FOX_PROJECT" && value == Some(plan.spec_path().as_os_str())));
    assert_eq!(std::env::current_dir().unwrap(), cwd);
    assert!(
        runner::prepare_command(
            target,
            &BuildCmd::Status,
            Some(&BuildContext::json_v1()),
            None
        )
        .unwrap_err()
        .contains("validated native tool bundle")
    );
    fixture.no_stage_output();
}

#[test]
fn review_source_tracks_config_and_resolved_project_changes_without_publication() {
    let fixture = Fixture::new();
    let before = runner::review_source(&fixture.root, &fixture.graph, None).unwrap();
    std::fs::write(&fixture.graph, "[settings]\nlog_dir = \"different\"\n").unwrap();
    assert_ne!(
        runner::review_source(&fixture.root, &fixture.graph, None).unwrap(),
        before
    );
    let spec = fixture.root.join("project.toml");
    let text = "format = 1\n[project]\nname = \"native_fixture\"\n[location]\ncode = \"nfx\"\nid = 99\n[[stage]]\nname = \"check\"\ncmd = [\"fox\", \"project-check\"]\noutputs = []\n";
    std::fs::write(&spec, text).unwrap();
    let before = runner::review_source(&fixture.root, &fixture.graph, Some(&spec)).unwrap();
    std::fs::write(&spec, text.replace("native_fixture", "edited_fixture")).unwrap();
    assert_ne!(
        runner::review_source(&fixture.root, &fixture.graph, Some(&spec)).unwrap(),
        before
    );
    assert!(!fixture.root.join("work").exists());
}

#[test]
fn packaged_context_requires_compiled_identity_and_pins_actual_tool_bytes() {
    let fixture = Fixture::new();
    std::fs::write(&fixture.tool, b"synthetic native tool fixture\n").unwrap();
    let compiled = foxbuild::bundle::BuildIdentity::compiled();
    let (version, id) = compiled
        .as_ref()
        .map(|identity| {
            (
                identity.package_version.as_str(),
                identity.build_id.as_str(),
            )
        })
        .unwrap_or(("0.1.0", "self-claimed-fixture"));
    let manifest = fixture.root.join("tools.json");
    std::fs::write(&manifest, serde_json::to_vec(&json!({"schema_version":1,"package_version":version,"build_id":id,"tools":{"fox":{"path":"synthetic_fox","blake3":"60beee9aaaa4d4bb422bbfe3d0da7a772463b6230b50ed9e093ae1087bbbd92e"}}})).unwrap()).unwrap();
    if compiled.is_err() {
        assert!(
            BuildContext::packaged(&manifest)
                .unwrap_err()
                .contains("FOX_BUNDLE_BUILD_ID")
        );
        return;
    }
    let context = BuildContext::packaged(&manifest).unwrap();
    assert!(context.is_packaged());
    let spec = fixture.root.join("project.toml");
    std::fs::write(&spec, "format = 1\n[project]\nname = \"native_fixture\"\n[location]\ncode = \"nfx\"\nid = 99\n[[stage]]\nname = \"check\"\ncmd = [\"fox\", \"project-check\"]\noutputs = []\n").unwrap();
    let target = RunTarget {
        project_spec: Some(&spec),
        ..fixture.target()
    };
    let prepared =
        runner::prepare_command(target, &BuildCmd::Status, Some(&context), None).unwrap();
    let plan = prepared.plan.unwrap();
    assert_eq!(
        plan.native_tool().unwrap().path,
        fixture.tool.canonicalize().unwrap()
    );
    assert_eq!(
        plan.native_tool().unwrap().blake3,
        "60beee9aaaa4d4bb422bbfe3d0da7a772463b6230b50ed9e093ae1087bbbd92e"
    );
    assert!(!plan.graph_path().exists());
    std::fs::write(&fixture.tool, b"changed tool").unwrap();
    assert!(runner::prepare_command(target, &BuildCmd::Status, Some(&context), None).is_err());
    assert!(plan.materialize().is_err());
    assert!(!plan.graph_path().exists());
    fixture.no_stage_output();
}

#[cfg(unix)]
#[test]
fn terminal_success_waits_for_the_actual_child_exit_and_detects_mismatch() {
    let fixture = Fixture::new();
    fixture.script(
        &(print_json(start_event())
            + &print_json(finish_event(2, "succeeded", 0))
            + "while test ! -f release; do sleep 0.01; done\nexit 1"),
    );
    let mut run = fixture.start(BuildCmd::Status);
    until(&mut run, |run| run.event_count == 2);
    assert!(run.is_running());
    assert!(!run.succeeded());
    std::fs::write(fixture.root.join("release"), b"release mock").unwrap();
    until(&mut run, |run| !run.is_running());
    let completion = run.completion.as_ref().unwrap();
    assert_eq!(completion.outcome, CompletionOutcome::Incomplete);
    assert!(completion.issues.contains(&CompletionIssue::ExitMismatch {
        reported: 0,
        actual: 1
    }));
    assert!(!run.succeeded());
    fixture.no_stage_output();
}

#[cfg(unix)]
#[test]
fn stderr_stage_like_text_never_creates_session_state_or_timeline_spans() {
    let fixture = Fixture::new();
    fixture.script(&(print_json(start_event()) + "printf '[ 0:00] done invented.stage 1 s\\n' >&2\n"
        + &print_json(event(2,"stage_started",json!({"name":"actual.stage","pid":123,"reason":"fixture","log_path":"fixture.log"})))
        + &print_json(event(3,"stage_finished",json!({"name":"actual.stage","result":"succeeded","seconds":0.1,"peak_mb":1})))
        + &print_json(finish_event(4,"succeeded",0))));
    let mut run = fixture.start(BuildCmd::Status);
    until(&mut run, |run| !run.is_running());
    assert!(run.succeeded());
    assert_eq!(run.live.get("actual.stage"), Some(&Live::Done));
    assert!(!run.live.contains_key("invented.stage"));
    assert!(run.timeline.span("actual.stage").is_some());
    assert!(run.timeline.span("invented.stage").is_none());
    assert!(run.lines.iter().any(|line| line.contains("invented.stage")));
    fixture.no_stage_output();
}

#[cfg(unix)]
#[test]
fn malformed_or_missing_terminal_output_cannot_report_success_on_exit_zero() {
    for tail in ["printf 'not-json\\n'", "printf '{'", "true"] {
        let fixture = Fixture::new();
        fixture.script(&(print_json(start_event()) + tail));
        let mut run = fixture.start(BuildCmd::Status);
        until(&mut run, |run| !run.is_running());
        assert_eq!(run.exit, Some(Ok(0)));
        assert_eq!(
            run.completion.as_ref().unwrap().outcome,
            CompletionOutcome::Incomplete
        );
        assert!(!run.succeeded());
        fixture.no_stage_output();
    }
}

#[cfg(unix)]
#[test]
fn cooperative_cancellation_is_confirmed_only_after_terminal_and_exit_130() {
    let fixture = Fixture::new();
    let parse = "while test \"$#\" -gt 0; do if test \"$1\" = --cancel-file; then marker=\"$2\"; shift; fi; shift; done\nprintf '%s' \"$marker\" > marker-path\n";
    fixture.script(
        &(parse.to_owned()
            + &print_json(start_event())
            + "while test ! -f \"$marker\"; do sleep 0.01; done\n"
            + &print_json(finish_event(2, "cancelled", 130))
            + "exit 130"),
    );
    let mut run = fixture.start(BuildCmd::Status);
    until(&mut run, |run| run.event_count == 1);
    run.stop();
    assert!(run.cancel_requested);
    assert!(run.exit.is_none());
    assert!(run.completion.is_none());
    until(&mut run, |run| !run.is_running());
    let completion = run.completion.as_ref().unwrap();
    assert!(completion.confirmed);
    assert_eq!(completion.outcome, CompletionOutcome::Cancelled);
    assert_eq!(completion.child_exit.code, Some(130));
    let marker = std::fs::read_to_string(fixture.root.join("marker-path")).unwrap();
    assert!(!Path::new(&marker).exists());
    assert!(!run.succeeded());
    fixture.no_stage_output();
}

#[cfg(unix)]
#[test]
fn cancellation_request_that_loses_the_exit_race_preserves_actual_success() {
    let fixture = Fixture::new();
    fixture.script(
        &(print_json(start_event())
            + &print_json(finish_event(2, "succeeded", 0))
            + "while test ! -f release; do sleep 0.01; done\nexit 0"),
    );
    let mut run = fixture.start(BuildCmd::Status);
    until(&mut run, |run| run.event_count == 2);
    run.stop();
    assert!(run.cancel_requested);
    std::fs::write(fixture.root.join("release"), b"exit normally").unwrap();
    until(&mut run, |run| !run.is_running());
    assert_eq!(
        run.completion.as_ref().unwrap().outcome,
        CompletionOutcome::Succeeded
    );
    assert!(run.succeeded());
    fixture.no_stage_output();
}

#[cfg(unix)]
#[test]
fn forced_stop_with_no_terminal_is_incomplete_and_never_succeeded() {
    let fixture = Fixture::new();
    fixture.script(&(print_json(start_event()) + "while test ! -f release; do sleep 0.01; done"));
    let mut run = fixture.start(BuildCmd::Status);
    until(&mut run, |run| run.event_count == 1);
    run.force_stop();
    until(&mut run, |run| !run.is_running());
    assert!(run.stopped_by_user);
    assert_eq!(
        run.completion.as_ref().unwrap().outcome,
        CompletionOutcome::Incomplete
    );
    assert!(!run.succeeded());
    fixture.no_stage_output();
}

#[cfg(unix)]
#[test]
fn long_stderr_line_is_truncated_and_total_retained_logs_are_bounded() {
    let fixture = Fixture::new();
    fixture.script(
        &(print_json(start_event())
            + "head -c 100000 /dev/zero | tr '\\000' x >&2\nprintf '\\n' >&2\n"
            + &print_json(finish_event(2, "succeeded", 0))),
    );
    let mut run = fixture.start(BuildCmd::Status);
    until(&mut run, |run| !run.is_running());
    assert!(run.succeeded());
    assert!(run.lines.iter().any(|line| line.contains("line truncated")));
    assert!(run.lines.iter().all(|line| line.len() <= 8192));
    assert!(run.lines.iter().map(String::len).sum::<usize>() <= 4 * 1024 * 1024);
    fixture.no_stage_output();
}

#[cfg(unix)]
#[test]
fn output_baseline_is_captured_before_the_child_can_replace_actual_state() {
    use foxstudio::output_diff::{ChangeKind, OutputDiff, OutputSnapshot};
    let fixture = Fixture::new();
    std::fs::create_dir(fixture.root.join("logs")).unwrap();
    let state = |hash: &str, finished: &str| {
        let record = foxbuild::StageRecord {
            ok: true,
            finished: finished.into(),
            cmd_fp: "fixture-fingerprint".into(),
            trace_complete: true,
            outputs: [("payload.bin".into(), hash.into())].into(),
            ..Default::default()
        };
        foxbuild::State {
            stages: [("fixture".into(), record)].into(),
        }
    };
    let before = state("before", "fixture-before");
    let after = state("after", "fixture-after");
    std::fs::write(
        fixture.root.join("logs/state.json"),
        serde_json::to_vec(&before).unwrap(),
    )
    .unwrap();
    fixture.script(
        &(format!(
            "printf '%s' '{}' > logs/state.json\n",
            serde_json::to_string(&after).unwrap()
        ) + &print_json(start_event())
            + &print_json(finish_event(2, "succeeded", 0))),
    );
    let mut run = fixture.start(BuildCmd::Build {
        only: vec![],
        from: vec![],
    });
    until(&mut run, |run| !run.is_running());
    let snapshot = run.before_output.take().unwrap().unwrap().unwrap();
    let source = run.output_source.clone().unwrap();
    assert_eq!(source.graph, fixture.graph);
    let after = OutputSnapshot::read(source).unwrap().unwrap();
    let diff = OutputDiff::compare(Some(&snapshot), Some(&after));
    assert_eq!(diff.counts().get(&ChangeKind::Changed), Some(&1));
    assert_eq!(diff.rows[0].before_hash.as_deref(), Some("before"));
    assert_eq!(diff.rows[0].after_hash.as_deref(), Some("after"));
    assert!(run.succeeded());
    fixture.no_stage_output();
}

#[cfg(unix)]
fn relative_from_cwd(path: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap();
    let here = cwd.components().collect::<Vec<_>>();
    let there = path.components().collect::<Vec<_>>();
    let common = here.iter().zip(&there).take_while(|(a, b)| a == b).count();
    let mut relative = PathBuf::new();
    for _ in common..here.len() {
        relative.push("..");
    }
    for component in &there[common..] {
        relative.push(component.as_os_str());
    }
    relative
}

#[cfg(unix)]
#[test]
fn relative_root_uses_checked_executable_and_absolute_control_paths_in_actual_child() {
    let fixture = Fixture::new();
    fixture.script(
        &("while test \"$#\" -gt 0; do if test \"$1\" = --cancel-file; then marker=\"$2\"; shift; fi; shift; done\n".to_owned()
            + "printf '%s' \"$marker\" > marker-path\nprintf '%s' \"$TEMP\" > temp-path\nprintf '%s' \"$PWD\" > child-cwd\n"
            + &print_json(start_event())
            + "while test ! -f \"$marker\"; do sleep 0.01; done\n"
            + &print_json(finish_event(2, "cancelled", 130))
            + "exit 130"),
    );
    let spec = fixture.root.join("project.toml");
    std::fs::write(&spec, "format = 1\n[project]\nname = \"native_fixture\"\n[location]\ncode = \"nfx\"\nid = 99\n[[stage]]\nname = \"check\"\ncmd = [\"fox\", \"project-check\"]\noutputs = []\n").unwrap();
    let context = if let Ok(identity) = foxbuild::bundle::BuildIdentity::compiled() {
        let tool = foxproject::plan_build_in(&fixture.root, &spec)
            .unwrap()
            .with_tool(&fixture.tool)
            .unwrap();
        let manifest = fixture.root.join("tools.json");
        std::fs::write(
            &manifest,
            serde_json::to_vec(&json!({
                "schema_version":1, "package_version":identity.package_version,
                "build_id":identity.build_id,
                "tools":{"fox":{"path":"synthetic_fox","blake3":tool.native_tool().unwrap().blake3}}
            }))
            .unwrap(),
        )
        .unwrap();
        BuildContext::packaged(&manifest).unwrap()
    } else {
        BuildContext::json_v1()
    };
    let root = relative_from_cwd(&fixture.root);
    let exe = relative_from_cwd(&fixture.tool);
    let target = RunTarget {
        root: &root,
        fox_exe: &exe,
        graph: Path::new("stages.toml"),
        python: "unused",
        project_spec: None,
    };
    let cancel = Path::new("work/tmp/review-marker");
    let prepared =
        runner::prepare_command(target, &BuildCmd::Status, Some(&context), Some(cancel)).unwrap();
    assert_eq!(
        prepared.command.get_program(),
        fixture.tool.canonicalize().unwrap().as_os_str()
    );
    assert_eq!(
        prepared.command.get_current_dir(),
        Some(fixture.root.as_path())
    );
    let args = prepared.command.get_args().collect::<Vec<_>>();
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "--config" && pair[1] == fixture.graph.as_os_str())
    );
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "--cancel-file"
                && pair[1] == fixture.root.join(cancel).as_os_str())
    );
    let settings_graph = root.join("stages.toml");
    let settings_target = RunTarget {
        graph: &settings_graph,
        ..target
    };
    let settings_command =
        runner::prepare_command(settings_target, &BuildCmd::Status, Some(&context), None).unwrap();
    let settings_args = settings_command.command.get_args().collect::<Vec<_>>();
    assert!(
        settings_args
            .windows(2)
            .any(|pair| pair[0] == "--config" && pair[1] == fixture.graph.as_os_str())
    );
    assert_eq!(
        runner::review_source(&root, &settings_graph, None).unwrap(),
        std::fs::read(&fixture.graph).unwrap()
    );
    let parent = std::env::current_dir().unwrap();
    let mut run = BuildRun::start_with_context(
        settings_target,
        BuildCmd::Status,
        &eframe::egui::Context::default(),
        Some(&context),
    )
    .unwrap();
    until(&mut run, |run| run.event_count == 1);
    let marker = PathBuf::from(std::fs::read_to_string(fixture.root.join("marker-path")).unwrap());
    assert!(marker.is_absolute() && marker.starts_with(fixture.root.join("work/tmp")));
    let temp = PathBuf::from(std::fs::read_to_string(fixture.root.join("temp-path")).unwrap());
    assert_eq!(temp, fixture.root.join("work/tmp"));
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("child-cwd")).unwrap(),
        fixture.root.to_string_lossy()
    );
    run.stop();
    until(&mut run, |run| !run.is_running());
    assert_eq!(
        run.completion.as_ref().unwrap().outcome,
        CompletionOutcome::Cancelled
    );
    assert!(run.completion.as_ref().unwrap().confirmed);
    assert!(!marker.exists());
    assert_eq!(std::env::current_dir().unwrap(), parent);
    fixture.no_stage_output();

    // The same relative target must work without a packaged context too.
    let mut run = BuildRun::start_with_context(
        target,
        BuildCmd::Status,
        &eframe::egui::Context::default(),
        Some(&BuildContext::json_v1()),
    )
    .unwrap();
    until(&mut run, |run| run.event_count == 1);
    run.stop();
    until(&mut run, |run| !run.is_running());
    assert!(run.completion.as_ref().unwrap().confirmed);
    assert_eq!(
        run.completion.as_ref().unwrap().outcome,
        CompletionOutcome::Cancelled
    );
}

#[cfg(unix)]
fn snapshot_files(
    root: &Path,
) -> std::collections::BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
    fn visit(
        path: &Path,
        files: &mut std::collections::BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)>,
    ) {
        if !path.exists() {
            return;
        }
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            // Persistent OS lease is control metadata, not a published input/output.
            if path
                .file_name()
                .is_some_and(|name| name == "foxbuild.lease")
            {
                continue;
            }
            let metadata = path.symlink_metadata().unwrap();
            assert!(!metadata.file_type().is_symlink());
            if metadata.is_dir() {
                visit(&path, files);
            } else {
                files.insert(
                    path.clone(),
                    (std::fs::read(path).unwrap(), metadata.modified().unwrap()),
                );
            }
        }
    }
    let mut files = std::collections::BTreeMap::new();
    visit(root, &mut files);
    files
}

#[cfg(unix)]
#[test]
#[ignore = "requires captured public fox CLI with fixture compiled bundle identity; N guarded driver supplies FOX_BUILD_TEST_EXE"]
fn actual_project_dry_run_and_contended_build_never_publish_parent_inputs() {
    let fixture = Fixture::new();
    let actual = std::env::var_os("FOX_BUILD_TEST_EXE")
        .map(PathBuf::from)
        .expect("N driver must provide its actual compiled public fox CLI");
    assert!(actual.is_absolute() && actual.is_file());
    std::fs::copy(&actual, &fixture.tool).unwrap();
    let spec = fixture.root.join("project.toml");
    let text = "format = 1\n[project]\nname = \"native_fixture\"\n[location]\ncode = \"nfx\"\nid = 99\n[[stage]]\nname = \"fixture\"\ncmd = [\"fox\", \"project\", \"check\", \"project.toml\"]\noutputs = []\n";
    std::fs::write(&spec, text).unwrap();
    let plan = foxproject::plan_build_in(&fixture.root, &spec)
        .unwrap()
        .with_tool(&fixture.tool)
        .unwrap();
    let identity = foxbuild::bundle::BuildIdentity::compiled()
        .expect("actual CLI test requires explicit fixture identity");
    let manifest = fixture.root.join("tools.json");
    std::fs::write(&manifest, serde_json::to_vec(&json!({
        "schema_version":1, "package_version":identity.package_version, "build_id":identity.build_id,
        "tools":{"fox":{"path":"synthetic_fox","blake3":plan.native_tool().unwrap().blake3}}
    })).unwrap()).unwrap();
    let context = BuildContext::packaged(&manifest).unwrap();
    // Establish only tiny authored graph/facet bytes in this isolated fixture.
    plan.materialize().unwrap();
    let log_dir = fixture.root.join(&plan.graph().settings.log_dir);
    let lease = foxbuild::PublicationLease::try_acquire(&log_dir)
        .unwrap()
        .unwrap();
    let before = snapshot_files(&fixture.root.join("work"));
    assert!(!before.is_empty());
    let old_name = plan.graph().stage[0].name.clone();
    std::fs::write(
        &spec,
        text.replace("name = \"fixture\"", "name = \"edited\""),
    )
    .unwrap();
    let target = RunTarget {
        project_spec: Some(&spec),
        ..fixture.target()
    };

    let selected = runner::prepare_command(
        target,
        &BuildCmd::DryRun {
            only: vec![],
            from: vec![],
        },
        Some(&context),
        None,
    )
    .unwrap();
    assert!(!selected.command.get_args().any(|arg| arg == "--config"));
    let args = selected.command.get_args().collect::<Vec<_>>();
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "--project" && pair[1] == spec.as_os_str())
    );
    assert_eq!(snapshot_files(&fixture.root.join("work")), before);

    // No Python exists in this fixture; the validated prepared status is native.
    let report = runner::read_status(
        target,
        Some(&context),
        &foxbuild::StatusOptions {
            python: "no-python".into(),
            check_dirty: true,
            save_cache: true,
        },
    )
    .unwrap();
    assert_ne!(report.stages[0].name, old_name);
    assert_eq!(report.config, plan.graph_path());
    assert_eq!(snapshot_files(&fixture.root.join("work")), before);

    let ctx = eframe::egui::Context::default();
    let mut dry = BuildRun::start_with_context(
        target,
        BuildCmd::DryRun {
            only: vec![],
            from: vec![],
        },
        &ctx,
        Some(&context),
    )
    .unwrap();
    until(&mut dry, |run| !run.is_running());
    assert!(dry.succeeded(), "{:?} {:?}", dry.completion, dry.lines);
    assert!(dry.completion.as_ref().unwrap().confirmed);
    assert_eq!(
        snapshot_files(&fixture.root.join("work")),
        before,
        "dry run changed shared project publications/cache"
    );

    let mut blocked = BuildRun::start_with_context(
        target,
        BuildCmd::Build {
            only: vec![],
            from: vec![],
        },
        &ctx,
        Some(&context),
    )
    .unwrap();
    until(&mut blocked, |run| !run.is_running());
    assert!(!blocked.succeeded());
    let completion = blocked.completion.as_ref().unwrap();
    assert!(completion.confirmed, "{completion:?} {:?}", blocked.lines);
    assert_eq!(completion.outcome, CompletionOutcome::Failed);
    assert!(
        blocked.lines.iter().any(|line| line.contains("lease")),
        "{:?}",
        blocked.lines
    );
    assert!(
        !blocked
            .live
            .values()
            .any(|state| matches!(state, Live::Running | Live::Done))
    );
    assert_eq!(
        snapshot_files(&fixture.root.join("work")),
        before,
        "losing build published before acquiring ownership"
    );
    drop(lease);
    fixture.no_stage_output();
}
