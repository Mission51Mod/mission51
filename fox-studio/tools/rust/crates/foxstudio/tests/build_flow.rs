//! Build-target/confirmation regressions: synthetic graphs and a harmless mock child, no build stages or GPU.
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use foxstudio::build_view::{BuildView, Env, Live};
use foxstudio::runner::{self, BuildCmd};
use foxstudio::settings::{Settings, Tab, ThemeChoice};
use foxstudio::{AppOptions, FoxStudio};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::time::{Duration, Instant};

struct Fixture {
    root: PathBuf,
    first: PathBuf,
    second: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("foxstudio_build_ui_{}_{nonce}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let first = root.join("first");
        let second = root.join("second");
        for (dir, name) in [(&first, "first"), (&second, "second")] {
            std::fs::create_dir(dir).unwrap();
            std::fs::write(dir.join("stages.toml"), format!("[settings]\nlog_dir = \"logs\"\n[[stage]]\nname = \"fixture.{name}\"\ncmd = [\"never_execute\"]\noutputs = [\"missing\"]\n")).unwrap();
            std::fs::write(dir.join("foxproject.toml"), format!("format = 1\n[project]\nname = \"{name}\"\nroot = \".\"\ngraph = \"stages.toml\"\n")).unwrap();
        }
        Self {
            root,
            first,
            second,
        }
    }

    fn env(&self) -> Env {
        Env {
            root: self.first.clone(),
            graph: self.first.join("stages.toml"),
            project_spec: None,
            fox_exe: self.first.join("no_fox"),
            python: self.first.join("no_python").display().to_string(),
            game_running: None,
            lock_pid: None,
            confirm_builds: true,
            allow_real_builds: false,
            real_build_block: None,
            paused: false,
        }
    }

    fn settings(&self) -> Settings {
        Settings {
            repo_root: self.first.display().to_string(),
            graph_file: "stages.toml".into(),
            fox_exe: self.first.join("no_fox").display().to_string(),
            python: self.env().python,
            system_fonts: false,
            setup_done: true,
            last_tab: Tab::Build,
            ..Default::default()
        }
    }

    fn assert_no_stage_outputs(&self) {
        for dir in [&self.first, &self.second] {
            for path in ["missing", "logs/state.json", "logs/hashcache.json"] {
                assert!(
                    !dir.join(path).exists(),
                    "UI wrote {}",
                    dir.join(path).display()
                );
            }
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        if let Ok(target) = std::fs::canonicalize(&self.root) {
            assert!(target.starts_with(&temp) && target != temp);
            let _ = std::fs::remove_dir_all(target);
        }
    }
}

struct View {
    build: BuildView,
    env: Env,
}

fn view(f: &Fixture) -> Harness<'static, View> {
    Harness::builder()
        .with_size([1440.0, 1050.0])
        .build_ui_state(
            |ui, v: &mut View| {
                foxstudio::theme::apply(ui.ctx(), ThemeChoice::Dark, 1.0);
                v.build.poll(ui.ctx(), &v.env);
                v.build.show(ui, &v.env);
            },
            View {
                build: BuildView::default(),
                env: f.env(),
            },
        )
}

fn app(f: &Fixture) -> Harness<'static, FoxStudio> {
    let settings = f.settings();
    Harness::builder()
        .with_size([1440.0, 1050.0])
        .build_eframe(move |cc| {
            FoxStudio::new(
                &cc.egui_ctx,
                settings,
                None,
                AppOptions {
                    settings_path: None,
                    allow_real_builds: false,
                    probe_every: Duration::from_millis(200),
                },
            )
        })
}

fn until<T>(h: &mut Harness<'_, T>, done: impl Fn(&T) -> bool) {
    let started = Instant::now();
    loop {
        h.step();
        if done(h.state()) {
            h.run_steps(2);
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "fixture worker did not finish"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn review(h: &mut Harness<'_, View>) {
    let ctx = h.ctx.clone();
    let v = h.state_mut();
    v.build.request_run(
        &ctx,
        &v.env,
        BuildCmd::Build {
            only: vec!["fixture.first".into()],
            from: vec![],
        },
    );
    h.run_steps(2);
    assert!(h.state().build.pending_confirm.is_some());
}

#[test]
fn settings_target_change_refreshes_status_while_build_tab_is_hidden() {
    let f = Fixture::new();
    let mut h = app(&f);
    until(&mut h, |a| a.build.checked);
    assert_eq!(
        h.state().build.report.as_ref().unwrap().stages[0].name,
        "fixture.first"
    );
    h.state_mut().build.selected = Some("fixture.first".into());
    h.state_mut()
        .build
        .live
        .insert("fixture.first".into(), Live::Done);
    h.state_mut().settings.last_tab = Tab::Settings;
    h.state_mut().settings.repo_root = f.second.display().to_string();
    h.step();
    assert!(h.state().build.selected.is_none() && h.state().build.live.is_empty());
    until(&mut h, |a| a.build.checked && !a.build.status_busy());
    let report = h.state().build.report.as_ref().unwrap();
    assert_eq!(report.config, f.second.join("stages.toml"));
    assert_eq!(report.stages[0].name, "fixture.second");
    f.assert_no_stage_outputs();
}

#[test]
fn target_retry_failed_refresh_clears_previous_success_and_can_be_retried() {
    let f = Fixture::new();
    let mut h = view(&f);
    until(&mut h, |v| v.build.checked);
    std::fs::write(&h.state().env.graph, b"invalid [graph").unwrap();
    let ctx = h.ctx.clone();
    let v = h.state_mut();
    v.build.refresh_status(&ctx, &v.env);
    assert!(!v.build.checked);
    until(&mut h, |v| {
        v.build.status_error.is_some() && !v.build.status_busy()
    });
    assert!(h.state().build.report.is_none() && !h.state().build.checked);
    std::fs::write(&h.state().env.graph, "[settings]\nlog_dir = \"logs\"\n[[stage]]\nname = \"repaired\"\ncmd = [\"never_execute\"]\noutputs = [\"missing\"]\n").unwrap();
    assert!(!h.get_by_label("Refresh").accesskit_node().is_disabled());
    h.get_by_label("Refresh").click();
    h.run_steps(2);
    assert!(
        h.state().build.status_error.is_none(),
        "Refresh did not clear the previous error: {:?}",
        h.state().build.status_error
    );
    until(&mut h, |v| {
        v.build.checked || v.build.status_error.is_some()
    });
    assert!(
        h.state().build.checked,
        "Retry failed: {:?}",
        h.state().build.status_error
    );
    assert_eq!(
        h.state().build.report.as_ref().unwrap().stages[0].name,
        "repaired"
    );
    assert!(h.state().build.status_error.is_none());
    f.assert_no_stage_outputs();
}

#[test]
fn confirmation_displays_the_reviewed_target_and_cancel_starts_nothing() {
    let f = Fixture::new();
    let mut h = view(&f);
    review(&mut h);
    h.get_by_label(&format!("Root: {}", f.first.display()));
    h.get_by_label(&format!("Graph: {}", f.first.join("stages.toml").display()));
    h.get_by_label(&format!("Interpreter: {}", f.env().python));
    assert!(h.get_by_label("Start build").accesskit_node().is_disabled());
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(h.state().build.pending_confirm.is_none() && h.state().build.run.is_none());
    f.assert_no_stage_outputs();
}

#[test]
fn changed_root_graph_python_executable_or_project_identity_cancels_confirmation() {
    let f = Fixture::new();
    for field in ["root", "graph", "python", "executable", "project"] {
        let mut h = view(&f);
        review(&mut h);
        let env = &mut h.state_mut().env;
        match field {
            "root" => env.root = f.second.clone(),
            "graph" => env.graph = f.second.join("stages.toml"),
            "python" => env.python = "different-unavailable-python".into(),
            "executable" => env.fox_exe = f.second.join("no_fox"),
            "project" => env.project_spec = Some(f.second.join("project.toml")),
            _ => unreachable!(),
        }
        h.run_steps(2);
        assert!(
            h.state().build.pending_confirm.is_none(),
            "{field} did not cancel review"
        );
        assert!(h.state().build.run.is_none(), "{field} launched a child");
        assert!(h.query_all_by_label("Start build").next().is_none());
    }
    f.assert_no_stage_outputs();
}

#[test]
fn newly_locked_or_disallowed_destination_disables_confirmation_action() {
    let f = Fixture::new();
    let mut h = view(&f);
    h.state_mut().env.allow_real_builds = true;
    review(&mut h);
    assert!(!h.get_by_label("Start build").accesskit_node().is_disabled());
    h.state_mut().env.lock_pid = Some(123);
    h.run_steps(2);
    assert!(h.get_by_label("Start build").accesskit_node().is_disabled());
    h.state_mut().env.lock_pid = None;
    h.state_mut().env.real_build_block = Some("project build is unavailable".into());
    h.run_steps(2);
    assert!(h.get_by_label("Start build").accesskit_node().is_disabled());
    h.state_mut().env.real_build_block = None;
    h.state_mut().env.allow_real_builds = false;
    h.run_steps(2);
    assert!(h.get_by_label("Start build").accesskit_node().is_disabled());
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(h.state().build.run.is_none());
    f.assert_no_stage_outputs();
}

#[test]
fn child_command_uses_selected_graph_root_and_python_without_parent_environment_changes() {
    let f = Fixture::new();
    let env = f.env();
    let before = std::env::vars_os().collect::<BTreeMap<OsString, OsString>>();
    let cmd = BuildCmd::DryRun {
        only: vec!["fixture.first".into()],
        from: vec![],
    };
    let child = runner::build_command(&env.fox_exe, &env.root, &env.graph, &env.python, &cmd, None)
        .unwrap();
    let args: Vec<OsString> = child.get_args().map(|a| a.to_owned()).collect();
    assert_eq!(
        args,
        [
            "build",
            "--dry-run",
            "--only",
            "fixture.first",
            "--config",
            &env.graph.display().to_string(),
            "--root",
            &env.root.display().to_string(),
            "--python",
            &env.python
        ]
        .into_iter()
        .map(OsString::from)
        .collect::<Vec<_>>()
    );
    assert_eq!(child.get_current_dir(), Some(env.root.as_path()));
    assert!(
        child
            .get_envs()
            .any(|(k, v)| k == "FOX_PROJECT" && v.is_none())
    );
    assert_eq!(
        std::env::vars_os().collect::<BTreeMap<OsString, OsString>>(),
        before
    );
    f.assert_no_stage_outputs();
}

#[test]
fn build_target_m3_child_environment_uses_q_plan_without_publishing_files() {
    let f = Fixture::new();
    let nonce = f.root.file_name().unwrap().to_string_lossy();
    let spec = f.root.join("project.toml");
    std::fs::write(&spec, format!("format = 1\n[project]\nname = \"pua\"\n[location]\ncode = \"pua\"\nid = 99\n[paths]\nwork = \"work/tmp/{nonce}/data\"\n[build]\nlog_dir = \"work/tmp/{nonce}/logs\"\ntemp_dir = \"work/tmp/{nonce}/tmp\"\nignore = []\n[[stage]]\nuse = \"nav.ground\"\n")).unwrap();
    let before = std::env::vars_os().collect::<BTreeMap<OsString, OsString>>();
    let plan = foxproject::plan_build_in(&f.root, &spec).unwrap();
    let child = runner::build_command(
        &f.env().fox_exe,
        plan.repo_root(),
        &f.first.join("stages.toml"),
        &f.env().python,
        &BuildCmd::Status,
        Some(&spec),
    )
    .unwrap();
    let overlays = child
        .get_envs()
        .map(|(k, v)| (k.to_owned(), v.map(|v| v.to_owned())))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        overlays.get(OsStr::new("FOX_PROJECT")),
        Some(&Some(plan.spec_path().as_os_str().to_owned()))
    );
    assert_eq!(
        overlays.get(OsStr::new("FOX_REPO_ROOT")),
        Some(&Some(plan.repo_root().as_os_str().to_owned()))
    );
    assert!(!plan.graph_path().exists());
    assert!(
        !plan
            .repo_root()
            .join(format!("work/tmp/{nonce}/data"))
            .exists()
    );
    assert_eq!(
        std::env::vars_os().collect::<BTreeMap<OsString, OsString>>(),
        before
    );
    f.assert_no_stage_outputs();
}

#[cfg(unix)]
#[test]
fn build_target_project_switch_keeps_mock_child_running_and_isolates_its_live_state() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let mock = f.first.join("mock_fox");
    std::fs::write(
        &mock,
        r#"#!/bin/sh
printf '%s\n' "$@" > "$PWD/mock_args.txt"
printf '%s\n' '{"schema_version":1,"sequence":1,"run_id":"mock-first","elapsed_ms":0,"event":"run_started","data":{"operation":"status"}}'
printf '%s\n' '{"schema_version":1,"sequence":2,"run_id":"mock-first","elapsed_ms":0,"event":"stage_planned","data":{"name":"fixture.first","result":"run","reason":"mock-only"}}'
while test ! -e "$PWD/release"; do sleep 0.01; done
printf '%s\n' '{"schema_version":1,"sequence":3,"run_id":"mock-first","elapsed_ms":0,"event":"run_finished","data":{"result":"succeeded","exit_code":0,"elapsed_seconds":0.01}}'
"#,
    )
    .unwrap();
    std::fs::set_permissions(&mock, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut h = app(&f);
    until(&mut h, |a| a.build.checked);
    let mut env = f.env();
    env.fox_exe = mock;
    h.state_mut().settings.fox_exe = env.fox_exe.display().to_string();
    let ctx = h.ctx.clone();
    h.state_mut().build.start_run(&ctx, &env, BuildCmd::Status);
    assert!(
        h.state().build.run_busy(),
        "{:?}",
        h.state().build.run_error
    );
    let pid = h.state().build.run.as_ref().unwrap().pid();
    until(&mut h, |a| a.build.live.contains_key("fixture.first"));
    h.state_mut()
        .open_project_path(&f.second.join("foxproject.toml"));
    h.run_steps(2);
    assert_eq!(
        h.state().build.run.as_ref().unwrap().pid(),
        pid,
        "project switch discarded the child"
    );
    assert!(h.state().build.run_busy());
    assert!(
        h.state().build.live.is_empty(),
        "previous project live states leaked into the new graph"
    );
    until(&mut h, |a| a.build.checked && !a.build.status_busy());
    assert_eq!(
        h.state().build.report.as_ref().unwrap().stages[0].name,
        "fixture.second"
    );
    h.get_by_label(&format!(
        "Run output belongs to {} · {}",
        f.first.display(),
        f.first.join("stages.toml").display()
    ));
    let args = std::fs::read_to_string(f.first.join("mock_args.txt")).unwrap();
    assert!(args.contains(&format!(
        "--config\n{}\n",
        f.first.join("stages.toml").display()
    )));
    std::fs::write(f.first.join("release"), b"finish mock").unwrap();
    until(&mut h, |a| !a.build.run_busy());
    assert!(h.state().build.run.as_ref().unwrap().succeeded());
    f.assert_no_stage_outputs();
}

#[test]
fn build_target_selected_stage_dry_runs_remain_available_when_real_project_builds_are_held() {
    let f = Fixture::new();
    let mut h = view(&f);
    until(&mut h, |v| v.build.checked);
    h.state_mut().build.selected = Some("fixture.first".into());
    h.state_mut().env.real_build_block = Some("real project builds held".into());
    h.run_steps(2);
    h.get_by_label("Selected stage").click();
    h.run_steps(2);
    assert!(
        !h.get_by_label("Dry run only fixture.first")
            .accesskit_node()
            .is_disabled()
    );
    assert!(
        h.get_by_label("Build only fixture.first")
            .accesskit_node()
            .is_disabled()
    );
    assert!(h.state().build.run.is_none());
    f.assert_no_stage_outputs();
}

#[test]
fn build_review_rejects_changed_graph_before_attempting_any_child() {
    let f = Fixture::new();
    let mut h = view(&f);
    until(&mut h, |v| v.build.checked);
    h.state_mut().env.allow_real_builds = true;
    review(&mut h);
    std::fs::write(f.first.join("stages.toml"), "[settings]\nlog_dir = \"logs\"\n[[stage]]\nname = \"changed.after.review\"\ncmd = [\"never_execute\"]\noutputs = [\"missing\"]\n").unwrap();
    h.get_by_label("Start build").click();
    h.run_steps(2);
    assert!(h.state().build.run.is_none());
    assert!(h.state().build.pending_confirm.is_none());
    assert!(
        h.state()
            .build
            .run_error
            .as_ref()
            .unwrap()
            .contains("source changed after review")
    );
    f.assert_no_stage_outputs();
}

#[test]
fn context_change_and_save_reload_invalidation_discard_pending_review() {
    let f = Fixture::new();
    let mut h = view(&f);
    until(&mut h, |v| v.build.checked);
    review(&mut h);
    h.state_mut()
        .build
        .set_context(Some(foxstudio::runner::BuildContext::json_v1()));
    h.run_steps(2);
    assert!(h.state().build.pending_confirm.is_none());
    until(&mut h, |v| v.build.checked);
    review(&mut h);
    let ctx = h.ctx.clone();
    let env = h.state().env.clone();
    h.state_mut().build.project_changed(&ctx, &env);
    h.run_steps(2);
    assert!(h.state().build.pending_confirm.is_none());
    until(&mut h, |v| v.build.checked);
    assert!(h.state().build.run.is_none());
    f.assert_no_stage_outputs();
}
