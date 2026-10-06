//! Authored prepared-project fixtures. No project library, private graph, game,
//! interpreter, global environment changes or public installation is required.
use foxbuild::{
    PreparedGraph, PublicationLease, StageRecord, State, StatusOptions,
    bundle::{BuildIdentity, BundledTools},
    config::Graph,
};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex()[..32].to_owned()
}

fn native_tool() -> &'static Path {
    static TOOL: OnceLock<PathBuf> = OnceLock::new();
    TOOL.get_or_init(|| {
        let root =
            std::env::temp_dir().join(format!("foxbuild_prepared_tool_{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let executable = root.join(if cfg!(windows) { "fox.exe" } else { "fox" });
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/native_tool.rs");
        let output = Command::new("rustc")
            .args(["--edition=2024", "-C", "opt-level=0"])
            .arg(source)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        executable
    })
}

struct Fixture {
    root: PathBuf,
    config: PathBuf,
    manifest: PathBuf,
    bundle: BundledTools,
    graph: Graph,
}

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("foxbuild_prepared_{}_{nonce}", std::process::id()));
        fs::create_dir_all(root.join("package")).unwrap();
        let root = root.canonicalize().unwrap();
        let config = root.join("unpublished/graph.toml");
        let tool = root
            .join("package")
            .join(if cfg!(windows) { "fox.exe" } else { "fox" });
        fs::copy(native_tool(), &tool).unwrap();
        let manifest = root.join("package/fox-tools.json");
        let identity = BuildIdentity::compiled().unwrap();
        fs::write(
            &manifest,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1, "package_version": identity.package_version,
                "build_id": identity.build_id, "tools": {"fox": {
                    "path": tool.file_name().unwrap().to_str().unwrap(),
                    "blake3": blake3::hash(&fs::read(&tool).unwrap()).to_hex().to_string()
                }}
            }))
            .unwrap(),
        )
        .unwrap();
        let bundle = BundledTools::load(&manifest, &identity).unwrap();
        let graph = Graph::from_toml_str(
            r#"
[settings]
log_dir = "build"
temp_dir = "temp"
games = []
mem_reserve_gb = 0.0
max_jobs = 1
unset_env = ["FIXTURE_BINDING"]
[settings.env]
FIXTURE_BINDING = "settings"
[[pinned]]
path = "facet.json"
owner = "authored"
[[stage]]
name = "asset"
cmd = ["fox", "prepared-copy", "facet.json", "output.bin", "build/foxbuild.lease", "child-env.txt"]
outputs = ["output.bin", "child-env.txt"]
[stage.env]
FIXTURE_BINDING = "stage"
FOX_PROJECT = "stage"
FOX_REPO_ROOT = "stage"
"#,
        )
        .unwrap();
        fs::write(root.join("project.toml"), "authored fixture identity").unwrap();
        fs::write(root.join("facet.json"), b"shared old facet").unwrap();
        Self {
            root,
            config,
            manifest,
            bundle,
            graph,
        }
    }

    fn prepared(&self, bytes: &[u8]) -> PreparedGraph {
        PreparedGraph {
            graph: self.graph.clone(),
            input_hashes: BTreeMap::from([("facet.json".into(), hash(bytes))]),
            child_env: BTreeMap::from([
                (
                    OsString::from("FIXTURE_BINDING"),
                    OsString::from("prepared"),
                ),
                (
                    OsString::from("FOX_PROJECT"),
                    self.root.join("project.toml").into_os_string(),
                ),
                (
                    OsString::from("FOX_REPO_ROOT"),
                    self.root.clone().into_os_string(),
                ),
            ]),
        }
    }

    fn args(&self, extra: &[&str]) -> Vec<String> {
        let mut args = vec![
            "--root".into(),
            self.root.display().to_string(),
            "--config".into(),
            self.config.display().to_string(),
            "--bundled-tools".into(),
            self.manifest.display().to_string(),
            "--python".into(),
            "no-interpreter-required".into(),
            "--jobs".into(),
            "1".into(),
            "--json".into(),
        ];
        args.extend(extra.iter().map(|value| (*value).into()));
        args
    }

    fn seed_completed(&self) {
        fs::create_dir_all(self.root.join("build")).unwrap();
        fs::write(self.root.join("output.bin"), "previous output").unwrap();
        let input = self.root.join("facet.json");
        let bytes = fs::read(&input).unwrap();
        let metadata = input.metadata().unwrap();
        let stamp = metadata
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let record = StageRecord {
            ok: true,
            trace_complete: true,
            cmd_fp: self
                .bundle
                .command_fingerprint(&self.graph.stage[0])
                .unwrap(),
            inputs: BTreeMap::from([("facet.json".into(), hash(&bytes))]),
            outputs: BTreeMap::from([("output.bin".into(), hash(b"previous output"))]),
            ..StageRecord::default()
        };
        State {
            stages: BTreeMap::from([("asset".into(), record)]),
        }
        .save(&self.root.join("build/state.json"))
        .unwrap();
        fs::write(
            self.root.join("build/hashcache.json"),
            serde_json::to_vec(&serde_json::json!({
                "entries": {"facet.json": [metadata.len(), stamp, hash(&bytes)]}
            }))
            .unwrap(),
        )
        .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

type Snapshot = BTreeMap<PathBuf, (Vec<u8>, SystemTime)>;

fn snapshot(root: &Path) -> Snapshot {
    let mut pending = vec![root.to_owned()];
    let mut files = BTreeMap::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().unwrap() != "foxbuild.lease" {
                // Windows excludes reads of a file held by the OS lease.
                files.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    (
                        fs::read(&path).unwrap(),
                        path.metadata().unwrap().modified().unwrap(),
                    ),
                );
            }
        }
    }
    files
}

#[test]
fn contended_build_never_invokes_publication() {
    let fixture = Fixture::new();
    fixture.seed_completed();
    let owner = PublicationLease::try_acquire(&fixture.root.join("build"))
        .unwrap()
        .unwrap();
    let before = snapshot(&fixture.root);
    let calls = AtomicUsize::new(0);
    assert_eq!(
        foxbuild::main_with_prepared(&fixture.args(&[]), fixture.prepared(b"new"), || {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }),
        2
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    // Real-build setup directories are allowed, but shared inputs/state/cache stay unchanged.
    assert_eq!(snapshot(&fixture.root), before);
    assert!(!fixture.config.exists());
    drop(owner);
    assert!(
        PublicationLease::try_acquire(&fixture.root.join("build"))
            .unwrap()
            .is_some()
    );
}

#[test]
fn one_lease_spans_callback_authoritative_load_and_actual_child() {
    let fixture = Fixture::new();
    let globals = ["FIXTURE_BINDING", "FOX_PROJECT", "FOX_REPO_ROOT"].map(std::env::var_os);
    let fingerprint = fixture
        .bundle
        .command_fingerprint(&fixture.graph.stage[0])
        .unwrap();
    let calls = AtomicUsize::new(0);
    let published = b"actual callback-published facet";
    assert_eq!(
        foxbuild::main_with_prepared(
            &fixture.args(&[]),
            fixture.prepared(b"planned different bytes"),
            || {
                calls.fetch_add(1, Ordering::Relaxed);
                assert!(
                    PublicationLease::try_acquire(&fixture.root.join("build"))
                        .unwrap()
                        .is_none()
                );
                assert_eq!(
                    foxbuild::lock_holder(&fixture.root.join("build")),
                    Some(std::process::id())
                );
                fs::write(fixture.root.join("facet.json"), published).unwrap();
                let state = State {
                    stages: BTreeMap::from([(
                        "callback-record".into(),
                        StageRecord {
                            finished: "published under lease".into(),
                            ..StageRecord::default()
                        },
                    )]),
                };
                state.save(&fixture.root.join("build/state.json")).unwrap();
                Ok(())
            }
        ),
        0
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        fs::read(fixture.root.join("output.bin")).unwrap(),
        published
    );
    let state = State::load(&fixture.root.join("build/state.json"));
    assert_eq!(
        state.stages["callback-record"].finished,
        "published under lease"
    );
    assert_eq!(state.stages["asset"].inputs["facet.json"], hash(published));
    assert_eq!(
        fs::read_to_string(fixture.root.join("child-env.txt")).unwrap(),
        format!(
            "prepared\n{}\n{}",
            fixture.root.join("project.toml").display(),
            fixture.root.display()
        )
    );
    assert_eq!(
        ["FIXTURE_BINDING", "FOX_PROJECT", "FOX_REPO_ROOT"].map(std::env::var_os),
        globals
    );
    assert_eq!(
        fixture
            .bundle
            .command_fingerprint(&fixture.graph.stage[0])
            .unwrap(),
        fingerprint
    );
    assert!(!fixture.root.join("build/foxbuild.lock").exists());
    assert!(
        PublicationLease::try_acquire(&fixture.root.join("build"))
            .unwrap()
            .is_some()
    );
}

#[test]
fn callback_failure_and_callback_cancellation_start_no_stage_and_release_lease() {
    for cancel in [false, true] {
        let fixture = Fixture::new();
        let marker = fixture.root.join("cancel");
        let mut args = fixture.args(&[]);
        args.extend(["--cancel-file".into(), marker.display().to_string()]);
        let calls = AtomicUsize::new(0);
        let code = foxbuild::main_with_prepared(&args, fixture.prepared(b"new"), || {
            calls.fetch_add(1, Ordering::Relaxed);
            assert!(
                PublicationLease::try_acquire(&fixture.root.join("build"))
                    .unwrap()
                    .is_none()
            );
            if cancel {
                fs::write(&marker, "cancel during publication").unwrap();
                Ok(())
            } else {
                Err("authored publication failure".into())
            }
        });
        assert_eq!(code, if cancel { 130 } else { 2 });
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(!fixture.root.join("output.bin").exists());
        assert!(!fixture.root.join("build/state.json").exists());
        assert!(!fixture.root.join("build/foxbuild.lock").exists());
        assert!(
            PublicationLease::try_acquire(&fixture.root.join("build"))
                .unwrap()
                .is_some()
        );
    }
}

#[test]
fn help_precancel_invalid_selection_and_failed_setup_skip_publication() {
    let fixture = Fixture::new();
    for (extra, expected) in [(vec!["--help"], 0), (vec!["--only", "absent"], 2)] {
        assert_eq!(
            foxbuild::main_with_prepared(
                &fixture.args(&extra),
                fixture.prepared(b"new"),
                || panic!("unexpected publisher")
            ),
            expected
        );
    }
    let cancel = fixture.root.join("cancel");
    fs::write(&cancel, "already cancelled").unwrap();
    let mut args = fixture.args(&[]);
    args.extend(["--cancel-file".into(), cancel.display().to_string()]);
    assert_eq!(
        foxbuild::main_with_prepared(&args, fixture.prepared(b"new"), || panic!(
            "unexpected publisher"
        )),
        130
    );
    fs::write(fixture.root.join("build"), "not a log directory").unwrap();
    assert_eq!(
        foxbuild::main_with_prepared(&fixture.args(&[]), fixture.prepared(b"new"), || panic!(
            "unexpected publisher"
        )),
        2
    );
    assert!(!fixture.root.join("output.bin").exists());
}

#[test]
fn every_inspection_is_readonly_under_a_held_lease_without_a_graph_file() {
    let fixture = Fixture::new();
    fixture.seed_completed();
    let owner = PublicationLease::try_acquire(&fixture.root.join("build"))
        .unwrap()
        .unwrap();
    let before = snapshot(&fixture.root);
    for operation in ["--list", "--graph", "--dry-run", "--status"] {
        assert_eq!(
            foxbuild::main_with_prepared(
                &fixture.args(&[operation]),
                fixture.prepared(b"unpublished edited facet"),
                || panic!("inspection published")
            ),
            0
        );
        assert_eq!(snapshot(&fixture.root), before);
    }
    let report = foxbuild::status_prepared(
        &fixture.root,
        &fixture.config,
        &StatusOptions {
            python: "never-run".into(),
            check_dirty: true,
            save_cache: true,
        },
        Some(&fixture.bundle),
        &fixture.prepared(b"unpublished edited facet"),
    )
    .unwrap();
    assert_eq!(
        report.stages[0].dirty.as_deref(),
        Some("input changed: facet.json")
    );
    assert_eq!(report.pinned[0].hash, hash(b"unpublished edited facet"));
    assert_eq!(report.config, fixture.config);
    assert_eq!(snapshot(&fixture.root), before);
    assert!(!fixture.config.exists());
    drop(owner);
}

#[test]
fn planned_hash_precedes_cached_metadata_and_missing_files_but_not_code_or_outputs() {
    let fixture = Fixture::new();
    fixture.seed_completed();
    let options = StatusOptions {
        python: "never-run".into(),
        check_dirty: true,
        save_cache: true,
    };
    let prepared = fixture.prepared(b"planned");
    for missing in [false, true] {
        if missing {
            fs::remove_file(fixture.root.join("facet.json")).unwrap();
        }
        let before = snapshot(&fixture.root);
        let report = foxbuild::status_prepared(
            &fixture.root,
            &fixture.config,
            &options,
            Some(&fixture.bundle),
            &prepared,
        )
        .unwrap();
        assert_eq!(
            report.stages[0].dirty.as_deref(),
            Some("input changed: facet.json")
        );
        assert_eq!(report.pinned[0].hash, hash(b"planned"));
        assert_eq!(snapshot(&fixture.root), before);
    }
    let mut state = State::load(&fixture.root.join("build/state.json"));
    state.stages.get_mut("asset").unwrap().inputs.clear();
    state
        .stages
        .get_mut("asset")
        .unwrap()
        .code
        .insert("facet.json".into(), hash(b"planned"));
    state.save(&fixture.root.join("build/state.json")).unwrap();
    let report = foxbuild::status_prepared(
        &fixture.root,
        &fixture.config,
        &options,
        Some(&fixture.bundle),
        &prepared,
    )
    .unwrap();
    assert_eq!(
        report.stages[0].dirty.as_deref(),
        Some("code changed: facet.json")
    );
    state.stages.get_mut("asset").unwrap().code.clear();
    state
        .stages
        .get_mut("asset")
        .unwrap()
        .outputs
        .insert("facet.json".into(), hash(b"planned"));
    state.save(&fixture.root.join("build/state.json")).unwrap();
    let report = foxbuild::status_prepared(
        &fixture.root,
        &fixture.config,
        &options,
        Some(&fixture.bundle),
        &prepared,
    )
    .unwrap();
    assert_eq!(
        report.stages[0].dirty.as_deref(),
        Some("output missing: facet.json")
    );
}

#[test]
fn invalid_prepared_paths_hashes_graphs_and_env_are_rejected_before_publication() {
    let fixture = Fixture::new();
    let mut cases = Vec::new();
    for path in [
        "../outside",
        "/absolute",
        "./facet.json",
        "facet\\file",
        "c:/file",
    ] {
        let mut prepared = fixture.prepared(b"new");
        prepared.input_hashes = BTreeMap::from([(path.into(), hash(b"new"))]);
        cases.push(prepared);
    }
    let mut malformed = fixture.prepared(b"new");
    malformed
        .input_hashes
        .insert("facet.json".into(), "wrong".into());
    cases.push(malformed);
    let mut alias = fixture.prepared(b"new");
    alias.input_hashes.insert("FACET.JSON".into(), hash(b"new"));
    cases.push(alias);
    let mut graph = fixture.prepared(b"new");
    graph.graph.stage[0].cmd.clear();
    cases.push(graph);
    let mut graph = fixture.prepared(b"new");
    graph.graph.settings.log_dir = "../outside".into();
    cases.push(graph);
    let mut env = fixture.prepared(b"new");
    env.child_env.insert("bad=key".into(), "value".into());
    cases.push(env);
    let mut env = fixture.prepared(b"new");
    env.child_env.insert("valid".into(), "bad\0value".into());
    cases.push(env);
    let mut env = fixture.prepared(b"new");
    env.child_env
        .insert("FOX_PROJECT".into(), "relative".into());
    cases.push(env);
    for prepared in cases {
        assert_eq!(
            foxbuild::main_with_prepared(&fixture.args(&[]), prepared, || panic!(
                "invalid plan published"
            )),
            2
        );
    }
    assert!(!fixture.root.join("build").exists());
    assert!(!fixture.root.join("temp").exists());
    assert!(!fixture.config.exists());
}
