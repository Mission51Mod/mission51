//! Frontend planning/launch isolation. Child probes execute this test binary, never location stages or foxbuild.
use foxbuild::config::Graph;
use foxpipe::project::{FACETS, Spec};
use foxproject::{plan_build, plan_build_in};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier};

const STAGE: &str = "[[stage]]\nuse = \"nav.ground\"\n";

struct Fixture {
    root: PathBuf,
    temp_root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_root = std::env::temp_dir().canonicalize().unwrap();
        let root = temp_root.join(format!("q_build_plan_{}_{nonce}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        Self { root, temp_root }
    }

    fn project(&self, code: &str, sections: &str) -> PathBuf {
        let path = self.root.join("work").join(code).join("project.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text = format!(
            r#"format = 1
[project]
name = "{code}"
[location]
code = "{code}"
id = 99
[paths]
work = "work/{code}/data"
[build]
log_dir = "work/{code}/logs"
temp_dir = "work/{code}/tmp"
ignore = [] # synthetic facets must remain visible to invalidation
{sections}
"#,
        );
        std::fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        assert_eq!(self.root.parent(), Some(self.temp_root.as_path()));
        assert_eq!(self.root.canonicalize().unwrap(), self.root);
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn env_snapshot() -> BTreeMap<OsString, OsString> {
    std::env::vars_os().collect()
}

fn probe(mode: &str, spec: &Path, code: &str) -> Command {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "--exact",
        "child_environment_probe",
        "--nocapture",
        "--test-threads=1",
    ])
    .env("Q_PLAN_PROBE", mode)
    .env("Q_PLAN_SPEC", spec)
    .env("Q_PLAN_EXPECTED_CODE", code);
    cmd
}

fn run_probe(command: &mut Command) -> String {
    let out = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout.into_owned()
}

// Compiles the unchanged compatibility signature without mutating the test harness's process environment.
const _: fn(&Path) -> Result<PathBuf, String> = foxproject::prepare_build;

#[test]
fn child_environment_probe() {
    let Ok(mode) = std::env::var("Q_PLAN_PROBE") else {
        return;
    };
    let spec_path = PathBuf::from(std::env::var_os("Q_PLAN_SPEC").unwrap());
    let expected = std::env::var("Q_PLAN_EXPECTED_CODE").unwrap();
    match mode.as_str() {
        "current" => {
            let spec = Spec::current();
            assert_eq!(spec.code(), expected);
            assert_eq!(
                std::fs::canonicalize(spec.path()).unwrap(),
                std::fs::canonicalize(&spec_path).unwrap()
            );
            println!("child loaded selected project {expected}");
        }
        "traced-plan" => {
            assert!(std::env::var_os("FOXBUILD_TRACE_DIR").is_some());
            let plan = plan_build(&spec_path).unwrap();
            assert!(!plan.graph_path().exists());
            assert!(!spec_path.parent().unwrap().join("data/spec").exists());
            println!("traced planner wrote no files");
        }
        "parent" => {
            let before = env_snapshot();
            let inherited = std::env::var_os("FOX_PROJECT").unwrap();
            assert_ne!(
                std::fs::canonicalize(PathBuf::from(&inherited)).unwrap(),
                std::fs::canonicalize(&spec_path).unwrap()
            );
            let plan = plan_build(&spec_path).unwrap();
            let mut child = probe("current", &spec_path, &expected);
            plan.apply_child_environment(&mut child).unwrap();
            assert!(
                run_probe(&mut child)
                    .contains(&format!("child loaded selected project {expected}"))
            );
            assert_eq!(env_snapshot(), before);
            println!(
                "inherited other project overridden only in child; parent environment unchanged"
            );
        }
        other => panic!("unknown probe mode {other}"),
    }
}

#[test]
fn planning_is_read_only_and_publication_is_idempotent() {
    let f = Fixture::new();
    let path = f.project("qpa", STAGE);
    let before = env_snapshot();
    let plan = plan_build_in(&f.root, path.parent().unwrap()).unwrap();
    assert!(!plan.graph_path().exists());
    assert!(!path.parent().unwrap().join("data/spec").exists());
    let text = plan.graph_text().to_string();
    let first = plan.materialize().unwrap();
    assert!(first.graph_changed);
    assert_eq!(first.facets_changed.len(), FACETS.len());
    assert_eq!(std::fs::read_to_string(plan.graph_path()).unwrap(), text);
    assert!(
        Spec::load_in(&f.root, &path)
            .unwrap()
            .write_facets()
            .unwrap()
            .is_empty(),
        "plan facets differ from the existing stage helper"
    );
    assert!(
        foxproject::diff_graphs(&Graph::load(plan.graph_path()).unwrap(), plan.graph()).is_empty()
    );
    let second = plan.materialize().unwrap();
    assert!(!second.graph_changed && second.facets_changed.is_empty());
    assert_eq!(plan.graph_text(), text);
    assert!(!path.parent().unwrap().join("logs/state.json").exists());
    assert_eq!(env_snapshot(), before);
}

#[test]
fn simultaneous_projects_publish_and_launch_with_distinct_identity() {
    let f = Fixture::new();
    let a = f.project("qpa", STAGE);
    let b = f.project("qpb", STAGE);
    let before = env_snapshot();
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = [(a, "qpa"), (b, "qpb")]
        .into_iter()
        .map(|(path, code)| {
            let barrier = barrier.clone();
            let root = f.root.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let plan = plan_build_in(&root, &path).unwrap();
                let fp = foxbuild::command_fingerprint(&plan.graph().stage[0], "unresolved:python");
                barrier.wait();
                plan.materialize().unwrap();
                let mut child = probe("current", &path, code);
                plan.apply_child_environment(&mut child).unwrap();
                assert!(
                    run_probe(&mut child)
                        .contains(&format!("child loaded selected project {code}"))
                );
                assert!(
                    foxproject::diff_graphs(&Graph::load(plan.graph_path()).unwrap(), plan.graph())
                        .is_empty()
                );
                assert_eq!(
                    foxbuild::command_fingerprint(&plan.graph().stage[0], "unresolved:python"),
                    fp
                );
                (
                    plan.graph_path().to_path_buf(),
                    plan.spec_path().to_path_buf(),
                    plan.graph().stage[0].outputs.clone(),
                )
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_ne!(results[0].0, results[1].0);
    assert_ne!(results[0].1, results[1].1);
    assert_ne!(results[0].2, results[1].2);
    assert_eq!(env_snapshot(), before);
    eprintln!(
        "simultaneous projects: separate graphs/facets and real child Spec::current identities; parent unchanged"
    );
}

#[test]
fn inherited_other_project_is_overridden_without_parent_mutation() {
    let f = Fixture::new();
    let a = f.project("qpa", STAGE);
    let b = f.project("qpb", STAGE);
    let before = env_snapshot();
    let mut parent = probe("parent", &a, "qpa");
    parent
        .env("FOX_PROJECT", std::fs::canonicalize(b).unwrap())
        .env("FOX_REPO_ROOT", &f.root);
    assert!(run_probe(&mut parent).contains("parent environment unchanged"));
    assert_eq!(env_snapshot(), before);
}

#[test]
fn traced_planning_has_no_implicit_facet_publication() {
    let f = Fixture::new();
    let path = f.project("qpa", STAGE);
    let plan = plan_build_in(&f.root, &path).unwrap();
    let mut child = probe("traced-plan", &path, "qpa");
    child.env("FOXBUILD_TRACE_DIR", f.root.join("trace"));
    plan.apply_child_environment(&mut child).unwrap();
    assert!(run_probe(&mut child).contains("traced planner wrote no files"));
    assert!(!path.parent().unwrap().join("data/spec").exists());
}

#[test]
fn conflicting_graph_identity_and_identity_removal_are_rejected() {
    let f = Fixture::new();
    let other = f.project("qpb", STAGE);
    let other_rel = other
        .strip_prefix(&f.root)
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");
    for (sections, context) in [
        (
            format!("[build.env]\nFOX_PROJECT = {other_rel:?}\n{STAGE}"),
            "build.env.FOX_PROJECT",
        ),
        (
            format!("{STAGE}env = {{ fox_project = {other_rel:?} }}\n"),
            "stage nav.ground env.fox_project",
        ),
        (
            format!("unset_env = [\"FOX_PROJECT\"]\n{STAGE}"),
            "build.unset_env.FOX_PROJECT",
        ),
        (
            format!("unset_env = [\"fox_repo_root\"]\n{STAGE}"),
            "build.unset_env.fox_repo_root",
        ),
        (
            format!("[build.env]\nFOX_REPO_ROOT = \"work\"\n{STAGE}"),
            "build.env.FOX_REPO_ROOT",
        ),
    ] {
        let path = f.project("qpa", &sections);
        let error = plan_build_in(&f.root, &path).unwrap_err();
        assert!(error.contains(context), "{error}");
        assert!(!path.parent().unwrap().join("logs").exists());
        assert!(!path.parent().unwrap().join("data/spec").exists());
    }
}

#[test]
fn malformed_environment_fails_before_publication_or_command_mutation() {
    let f = Fixture::new();
    let path = f.project("qpa", STAGE);
    let plan = plan_build_in(&f.root, &path).unwrap();
    for (key, value) in [
        ("", "ok"),
        ("BAD=KEY", "ok"),
        ("BAD\0KEY", "ok"),
        ("OPTION", "bad\0value"),
        ("FOX_PROJECT", ""),
        ("FOX_PROJECT", " \n "),
        ("FOX_PROJECT", "missing/project.toml"),
    ] {
        let overrides = [(key.into(), value.into())].into();
        let error = plan.child_environment(&overrides).unwrap_err();
        assert!(
            error.contains("environment") || error.contains("FOX_PROJECT"),
            "{error}"
        );
        let mut cmd = Command::new("unused");
        cmd.env(key, value);
        let before: Vec<_> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_owned(), v.map(OsString::from)))
            .collect();
        assert!(plan.apply_child_environment(&mut cmd).is_err());
        assert_eq!(
            cmd.get_envs()
                .map(|(k, v)| (k.to_owned(), v.map(OsString::from)))
                .collect::<Vec<_>>(),
            before
        );
    }
    for sections in [
        format!("[build.env]\nFOX_PROJECT = \"\"\n{STAGE}"),
        format!("{STAGE}env = {{ FOX_PROJECT = \"\" }}\n"),
        format!("[build.env]\nFOX_PROJECT = 42\n{STAGE}"),
    ] {
        let path = f.project("qpa", &sections);
        assert!(plan_build_in(&f.root, &path).is_err());
    }
    assert!(!path.parent().unwrap().join("logs").exists());
}

#[test]
fn explicit_child_identity_conflicts_and_removals_leave_command_unchanged() {
    let f = Fixture::new();
    let path = f.project("qpa", STAGE);
    let other = f.project("qpb", STAGE);
    let plan = plan_build_in(&f.root, &path).unwrap();
    for remove in [false, true] {
        let mut cmd = Command::new("unused");
        if remove {
            cmd.env_remove("fox_project");
        } else {
            cmd.env("FOX_PROJECT", &other);
        }
        let before: Vec<_> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_owned(), v.map(OsString::from)))
            .collect();
        let error = plan.apply_child_environment(&mut cmd).unwrap_err();
        assert!(error.contains("project identity"), "{error}");
        assert_eq!(
            cmd.get_envs()
                .map(|(k, v)| (k.to_owned(), v.map(OsString::from)))
                .collect::<Vec<_>>(),
            before
        );
    }
}

#[cfg(unix)]
#[test]
fn same_file_with_different_include_origin_is_rejected() {
    let f = Fixture::new();
    let path = f.project("qpa", STAGE);
    let alias = f.root.join("other/project.toml");
    std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&path, &alias).unwrap();
    let plan = plan_build_in(&f.root, &path).unwrap();
    let error = plan
        .child_environment(&[("FOX_PROJECT".into(), alias.display().to_string())].into())
        .unwrap_err();
    assert!(error.contains("include/template origin"), "{error}");
    // Loading a selected symlink retains its chosen origin, rather than canonicalizing to another folder.
    let link = f.root.join("other/link.toml");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    let linked_plan = plan_build_in(&f.root, &link).unwrap();
    assert_eq!(linked_plan.spec_path(), link);
    assert_eq!(
        linked_plan
            .child_environment(&BTreeMap::new())
            .unwrap()
            .get(OsStr::new("FOX_PROJECT")),
        Some(&link.as_os_str().to_owned())
    );
}

#[test]
fn matching_identity_aliases_preserve_graph_and_fingerprints() {
    let f = Fixture::new();
    let path = f.root.join("work/qpa/project.toml");
    let relative = path
        .strip_prefix(&f.root)
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");
    f.project("qpa", &format!("[build.env]\nFOX_PROJECT = {relative:?}\n{STAGE}env = {{ fox_project = {relative:?} }}\n"));
    let direct = foxproject::generate(&Spec::load_in(&f.root, &path).unwrap())
        .unwrap()
        .graph;
    let plan = plan_build_in(&f.root, &path).unwrap();
    assert!(foxproject::diff_graphs(&direct, plan.graph()).is_empty());
    assert_eq!(
        foxbuild::command_fingerprint(&direct.stage[0], "python"),
        foxbuild::command_fingerprint(&plan.graph().stage[0], "python")
    );
    let overrides = [
        ("fox_project".into(), relative),
        ("OPTION".into(), "value".into()),
    ]
    .into();
    let child = plan.child_environment(&overrides).unwrap();
    assert_eq!(
        child.get(OsStr::new("FOX_PROJECT")),
        Some(&plan.spec_path().as_os_str().to_owned())
    );
    assert!(!child.contains_key(OsStr::new("fox_project")));
    assert_eq!(
        child.get(OsStr::new("OPTION")),
        Some(&OsString::from("value"))
    );
    let mut cmd = probe("current", &path, "qpa");
    cmd.envs(child);
    plan.apply_child_environment(&mut cmd).unwrap();
    assert!(run_probe(&mut cmd).contains("child loaded selected project qpa"));
}

#[test]
fn native_plan_preserves_generated_graph_and_command_fingerprints() {
    let fixture = Fixture::new();
    let path = fixture.project(
        "qnative",
        r#"[[stage]]
name = "project.validate"
cmd = ["fox", "project", "check", "project.toml", "--json"]
env = { OPTION = "first value" }
[[stage]]
name = "path.hash"
cmd = ["fox", "hash", "path", "example/input.bin"]
after = ["project.validate"]
env = { OPTION = "second value" }
"#,
    );
    let generated = foxproject::generate(&Spec::load_in(&fixture.root, &path).unwrap())
        .unwrap()
        .graph;
    let plan = plan_build_in(&fixture.root, &path).unwrap();
    let loaded = Graph::from_toml_str(plan.graph_text()).unwrap();
    assert!(foxproject::diff_graphs(&generated, plan.graph()).is_empty());
    assert!(foxproject::diff_graphs(&generated, &loaded).is_empty());
    assert_eq!(generated.stage.len(), 2);
    for interpreter in ["unresolved:python", "unused native interpreter identity"] {
        for (expected, actual) in generated.stage.iter().zip(&loaded.stage) {
            assert_eq!(
                foxbuild::command_fingerprint(expected, interpreter),
                foxbuild::command_fingerprint(actual, interpreter)
            );
        }
    }
}

