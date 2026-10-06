//! A frontend's selected root is independent of inherited process root, project and cwd.
use foxpipe::project::{LoadOptions, Spec};
use foxproject::plan_build_in;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier};

const REL_SPEC: &str = "projects/qre/project.toml";

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
        let root = temp_root.join(format!("q_root_{}_{nonce}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        Self { root, temp_root }
    }
    fn repo(&self, name: &str, grid: usize) -> PathBuf {
        let root = self.root.join(name);
        for d in ["config", "projects/qre/stages", "tools/rust/biomes"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(
            root.join("config/base.toml"),
            r#"format = 1
[project]
name = "qre"
[location]
code = "qre"
id = 99
[paths]
work = "work/data"
[build]
log_dir = "work/logs"
temp_dir = "work/tmp"
ignore = []
[package.templates]
seq_lua = "seq.lua"
"#,
        )
        .unwrap();
        std::fs::write(root.join("config/seq.lua"), name).unwrap();
        std::fs::write(
            root.join(REL_SPEC),
            "extends = \"../../config/base.toml\"\n[project]\ninclude = [\"stages/*.toml\"]\n",
        )
        .unwrap();
        std::fs::write(
            root.join("projects/qre/stages/01_nav.toml"),
            "[[stage]]\nuse = \"nav.ground\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("tools/rust/biomes/mafr.toml"),
            format!(
                r#"name = "mafr"
grid = {grid}
logical = []
mat_id = []
heli_space = 0
weather = []
[map]
lang_id = "{name}"
height = "height"
photo = "photo"
"#
            ),
        )
        .unwrap();
        root
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
fn probe(mode: &str, root: &Path) -> Command {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args(["--exact", "root_probe", "--nocapture", "--test-threads=1"])
        .env("Q_ROOT_PROBE", mode)
        .env("Q_ROOT_SELECTED", root);
    c
}
fn run(c: &mut Command) -> String {
    let out = c.output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{text}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    text.into_owned()
}

#[test]
fn root_probe() {
    let Ok(mode) = std::env::var("Q_ROOT_PROBE") else {
        return;
    };
    let selected = PathBuf::from(std::env::var_os("Q_ROOT_SELECTED").unwrap());
    let env = env_snapshot();
    let cwd = std::env::current_dir().unwrap();
    if mode == "parent" {
        assert_ne!(
            std::env::var_os("FOX_REPO_ROOT").unwrap(),
            selected.as_os_str()
        );
        let plan = plan_build_in(&selected, Path::new(REL_SPEC)).unwrap();
        assert_eq!(plan.repo_root(), selected);
        let mut child = probe("current", &selected);
        plan.apply_child_environment(&mut child).unwrap();
        assert!(run(&mut child).contains("explicit-root child loaded selected repository"));
    } else {
        assert_eq!(mode, "current");
        let s = Spec::current();
        assert_eq!(s.repo_root(), selected);
        assert_eq!(s.rel(), REL_SPEC);
        assert_eq!(
            s.datasets().path("heights"),
            selected.join(s.dataset_rel("heights"))
        );
        assert_eq!(
            std::fs::read_to_string(
                selected.join(s.file().package.templates.seq_lua.as_ref().unwrap())
            )
            .unwrap(),
            "second"
        );
        assert_eq!(s.biome().unwrap().grid, 4096);
        println!("explicit-root child loaded selected repository");
    }
    assert_eq!(env_snapshot(), env);
    assert_eq!(std::env::current_dir().unwrap(), cwd);
}

#[test]
fn concurrent_roots_resolve_extends_includes_templates_datasets_and_publications() {
    let f = Fixture::new();
    let a = f.repo("first", 2048);
    let b = f.repo("second", 4096);
    let env = env_snapshot();
    let cwd = std::env::current_dir().unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let jobs: Vec<_> = [(a.clone(), "first", 2048), (b.clone(), "second", 4096)]
        .into_iter()
        .map(|(root, marker, grid)| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let plan = plan_build_in(&root, Path::new("projects/qre")).unwrap();
                let spec = Spec::load_in(&root, REL_SPEC).unwrap();
                assert_eq!(
                    spec.file().project.include,
                    ["projects/qre/stages/01_nav.toml"]
                );
                assert_eq!(
                    spec.file().package.templates.seq_lua.as_deref(),
                    Some("config/seq.lua")
                );
                assert_eq!(
                    std::fs::read_to_string(root.join("config/seq.lua")).unwrap(),
                    marker
                );
                assert_eq!(spec.biome().unwrap().grid, grid);
                assert_eq!(spec.dataset("spec_cache"), root.join("work/data/spec"));
                assert_eq!(
                    spec.datasets().get("heights").unwrap().path_in(&root),
                    spec.dataset("heights")
                );
                assert!(!root.join("work/logs").exists() && !root.join("work/data/spec").exists());
                let published = plan.materialize().unwrap();
                assert_eq!(
                    published.graph_path,
                    root.join("work/logs/graph.generated.toml")
                );
                assert!(
                    spec.write_facets().unwrap().is_empty(),
                    "explicit-root facet bytes changed"
                );
                let text =
                    std::fs::read_to_string(root.join("work/data/spec/package.json")).unwrap();
                assert!(text.contains("config/seq.lua"));
                assert_eq!(
                    plan.child_environment(&BTreeMap::new())
                        .unwrap()
                        .get(OsStr::new("FOX_REPO_ROOT")),
                    Some(&root.as_os_str().to_owned())
                );
                plan
            })
        })
        .collect();
    let plans: Vec<_> = jobs.into_iter().map(|j| j.join().unwrap()).collect();
    assert_eq!(plans[0].graph_text(), plans[1].graph_text());
    assert_ne!(plans[0].graph_path(), plans[1].graph_path());
    assert_eq!(env_snapshot(), env);
    assert_eq!(std::env::current_dir().unwrap(), cwd);
    println!(
        "explicit roots: concurrent same relative spec, selected-root includes/templates/datasets/biomes/facets, parent env and cwd unchanged"
    );
}

#[test]
fn inherited_different_root_and_cwd_are_overridden_only_in_child() {
    let f = Fixture::new();
    let a = f.repo("first", 2048);
    let b = f.repo("second", 4096);
    let env = env_snapshot();
    let mut child = probe("parent", &b);
    child
        .env("FOX_REPO_ROOT", &a)
        .env("FOX_PROJECT", a.join(REL_SPEC))
        .current_dir(&a);
    run(&mut child);
    assert_eq!(env_snapshot(), env);
}

#[test]
fn invalid_roots_and_explicit_identity_conflicts_fail_without_publication() {
    let f = Fixture::new();
    let a = f.repo("first", 2048);
    let b = f.repo("second", 4096);
    for root in [PathBuf::new(), f.root.join("missing"), a.join(REL_SPEC)] {
        let err = plan_build_in(&root, Path::new(REL_SPEC)).unwrap_err();
        assert!(err.contains("repository root"), "{err}");
    }
    let plan = plan_build_in(&b, Path::new(REL_SPEC)).unwrap();
    let env: BTreeMap<_, _> = [("FOX_REPO_ROOT".into(), a.to_string_lossy().into_owned())].into();
    assert!(
        plan.child_environment(&env)
            .unwrap_err()
            .contains("conflicts")
    );
    let mut child = probe("current", &b);
    child.env("FOX_REPO_ROOT", &a);
    let before: Vec<_> = child
        .get_envs()
        .map(|(k, v)| (k.to_owned(), v.map(OsStr::to_owned)))
        .collect();
    assert!(
        plan.apply_child_environment(&mut child)
            .unwrap_err()
            .contains("conflicts")
    );
    assert_eq!(
        child
            .get_envs()
            .map(|(k, v)| (k.to_owned(), v.map(OsStr::to_owned)))
            .collect::<Vec<_>>(),
        before
    );
    assert!(!b.join("work/logs").exists());
    let text = std::fs::read_to_string(b.join(REL_SPEC)).unwrap();
    let from_text =
        Spec::from_toml_str_in(&b, &text, Path::new(REL_SPEC), &LoadOptions::default()).unwrap();
    assert_eq!(from_text.repo_root(), b);
    std::fs::remove_file(b.join("config/seq.lua")).unwrap();
    assert!(
        plan_build_in(&b, Path::new(REL_SPEC))
            .unwrap_err()
            .contains("package.templates.seq_lua")
    );
}

