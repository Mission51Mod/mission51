//! Native document and tool integrity proofs. The ignored bundle test runs actual packaged CLI actions.
use foxproject::{ProjectDocument, plan_build_in};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const SPEC: &str = r#"format = 1
[project]
name = "qnative"
[location]
code = "qnative"
id = 99
[build]
pinned = [{ path = "project.toml", owner = "project" }]
[[stage]]
name = "project.validate"
cmd = ["fox", "project", "check", "project.toml", "--json"]
mem_gb = 0.1
"#;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("q_native_project_{}_{nonce}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn root(&self, name: &str) -> PathBuf {
        let root = self.0.join(name);
        std::fs::create_dir(&root).unwrap();
        ProjectDocument::create_in(&root, Path::new("project.toml"), SPEC).unwrap();
        root
    }
    fn tool(root: &Path) -> PathBuf {
        let tool = root.join("bin/fox");
        std::fs::create_dir(tool.parent().unwrap()).unwrap();
        std::fs::copy(std::env::current_exe().unwrap(), &tool).unwrap();
        tool
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = std::fs::canonicalize(&self.0).unwrap();
        assert!(root.starts_with(&temp) && root != temp);
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn documents_create_open_and_save_at_selected_roots_without_publication() {
    let fixture = Fixture::new();
    let roots = [
        fixture.root("first relocated"),
        fixture.root("second relocated"),
    ];
    let environment: BTreeMap<_, _> = std::env::vars_os().collect();
    let cwd = std::env::current_dir().unwrap();
    for (index, root) in roots.iter().enumerate() {
        assert!(!root.join("tools/rust/Cargo.toml").exists());
        let mut document = ProjectDocument::open_in(root, Path::new("project.toml")).unwrap();
        let draft = SPEC.replace("id = 99", &format!("id = {}", 100 + index));
        assert!(document.save(&draft).unwrap());
        assert!(!document.save(&draft).unwrap());
        assert_eq!(document.spec().repo_root(), root);
        assert_eq!(document.spec().file().location.id, 100 + index as i64);
        assert!(!root.join("work").exists());
    }
    assert_eq!(std::env::vars_os().collect::<BTreeMap<_, _>>(), environment);
    assert_eq!(std::env::current_dir().unwrap(), cwd);
}

#[test]
fn invalid_or_stale_documents_do_not_overwrite_files() {
    let fixture = Fixture::new();
    let root = fixture.root("document");
    let mut document = ProjectDocument::open_in(&root, Path::new("project.toml")).unwrap();
    assert!(ProjectDocument::create_in(&root, Path::new("project.toml"), SPEC).is_err());
    assert!(
        ProjectDocument::create_in(
            &root,
            Path::new("invalid.toml"),
            &SPEC.replace("id = 99", "id = 0")
        )
        .is_err()
    );
    assert!(!root.join("invalid.toml").exists());
    assert!(document.save(&SPEC.replace("id = 99", "id = 0")).is_err());
    assert_eq!(std::fs::read_to_string(document.path()).unwrap(), SPEC);
    std::fs::write(document.path(), "external edit").unwrap();
    assert!(
        document
            .save(&SPEC.replace("id = 99", "id = 100"))
            .unwrap_err()
            .contains("outside this editor")
    );
    assert_eq!(
        std::fs::read_to_string(document.path()).unwrap(),
        "external edit"
    );
    assert_eq!(document.text(), SPEC);
}

#[test]
fn portable_tool_identity_and_alias_are_captured_without_changing_the_plan() {
    let fixture = Fixture::new();
    let root = fixture.root("native");
    let tool = Fixture::tool(&root);
    let original = plan_build_in(&root, Path::new("project.toml")).unwrap();
    let bound = original.with_tool(Path::new("bin/fox")).unwrap();
    let identity = bound.native_tool().unwrap();
    assert_eq!(identity.path, std::fs::canonicalize(&tool).unwrap());
    assert_eq!(
        identity.blake3,
        blake3::hash(&std::fs::read(&tool).unwrap())
            .to_hex()
            .to_string()
    );
    assert!(original.native_tool().is_none());
    assert_eq!(original.graph().stage[0].cmd, bound.graph().stage[0].cmd);
    assert_eq!(bound.graph().stage[0].cmd[0], "fox");
    assert!(
        bound
            .graph()
            .pinned
            .iter()
            .any(|pin| pin.owner == "native tool" && pin.path == identity.path.to_str().unwrap())
    );
    assert!(!root.join("work").exists());
}

#[test]
fn native_library_commands_keep_developer_flags_and_legacy_commands_stay_unchanged() {
    let fixture = Fixture::new();
    let root = fixture.root("library");
    let tool = Fixture::tool(&root);
    std::fs::write(root.join("project.toml"), SPEC.replace("name = \"project.validate\"\ncmd = [\"fox\", \"project\", \"check\", \"project.toml\", \"--json\"]", "use = \"nav.ground\"")).unwrap();
    let original = plan_build_in(&root, Path::new("project.toml")).unwrap();
    let bound = original.with_tool(&tool).unwrap();
    assert_eq!(original.graph().stage[0].cmd[0], foxproject::FOX_EXE);
    assert_eq!(bound.graph().stage[0].cmd[0], "fox");
    assert_eq!(
        bound.graph().stage[0].rust_tools,
        original.graph().stage[0].rust_tools
    );
    assert!(bound.graph().stage[0].rust_tools);
    let explicit_alias =
        SPEC.replace("cmd = [\"fox\",", "cmd = [\"fox.exe\",") + "\nrust_tools = true\n";
    std::fs::write(root.join("project.toml"), explicit_alias).unwrap();
    let bound = plan_build_in(&root, Path::new("project.toml"))
        .unwrap()
        .with_tool(&tool)
        .unwrap();
    assert_eq!(bound.graph().stage[0].cmd[0], "fox");
    assert!(bound.graph().stage[0].rust_tools);
}

#[test]
fn changed_tool_fails_before_publication_or_child_environment_changes() {
    let fixture = Fixture::new();
    let root = fixture.root("changed tool");
    let tool = Fixture::tool(&root);
    let plan = plan_build_in(&root, Path::new("project.toml"))
        .unwrap()
        .with_tool(&tool)
        .unwrap();
    std::fs::write(&tool, "replaced tool bytes").unwrap();
    assert!(
        plan.materialize()
            .unwrap_err()
            .contains("changed after planning")
    );
    assert!(plan.child_environment(&BTreeMap::new()).is_err());
    let mut child = Command::new("never_execute");
    child.env("KEEP", "original");
    let before: Vec<_> = child
        .get_envs()
        .map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned)))
        .collect();
    assert!(plan.apply_child_environment(&mut child).is_err());
    assert_eq!(
        before,
        child
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned)))
            .collect::<Vec<_>>()
    );
    assert!(!root.join("work").exists());
}

#[test]
fn portable_binding_rejects_missing_tools_python_and_transitional_variants() {
    let fixture = Fixture::new();
    let root = fixture.root("bad native");
    let tool = Fixture::tool(&root);
    assert!(
        plan_build_in(&root, Path::new("project.toml"))
            .unwrap()
            .with_tool(&root.join("missing"))
            .is_err()
    );
    for text in [
        SPEC.replace("cmd = [\"fox\",", "cmd = [\"python3.11\","),
        format!("{SPEC}\ncmd_rs = [\"fox\", \"project\"]\n"),
    ] {
        std::fs::write(root.join("project.toml"), text).unwrap();
        assert!(
            plan_build_in(&root, Path::new("project.toml"))
                .unwrap()
                .with_tool(&tool)
                .is_err()
        );
    }
}


#[test]
fn temporary_file_collisions_preserve_existing_bytes() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "temporary_file_collision_probe", "--nocapture"])
        .env("Q_TEMP_COLLISION_PROBE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn temporary_file_collision_probe() {
    if std::env::var_os("Q_TEMP_COLLISION_PROBE").is_none() {
        return;
    }
    // The fresh child runs only this probe, so each publication helper starts at serial zero.
    let fixture = Fixture::new();
    let root = fixture.root("temporary file collision");
    let temporary_name = |name: &str| format!("{name}.{}.0.tmp", std::process::id());
    let document_temp = root.join(temporary_name("project.toml"));
    std::fs::write(&document_temp, "existing document temporary bytes").unwrap();
    let mut document = ProjectDocument::open_in(&root, Path::new("project.toml")).unwrap();
    assert!(document.save(&SPEC.replace("id = 99", "id = 100")).is_err());
    assert_eq!(std::fs::read_to_string(document.path()).unwrap(), SPEC);
    assert_eq!(document.spec().file().location.id, 99);
    assert_eq!(
        std::fs::read_to_string(&document_temp).unwrap(),
        "existing document temporary bytes"
    );

    let plan = plan_build_in(&root, Path::new("project.toml")).unwrap();
    let graph_path = plan.graph_path();
    std::fs::create_dir_all(graph_path.parent().unwrap()).unwrap();
    let graph_temp = graph_path.with_file_name(temporary_name("graph.generated.toml"));
    std::fs::write(graph_path, "existing saved graph bytes").unwrap();
    std::fs::write(&graph_temp, "existing graph temporary bytes").unwrap();
    assert!(plan.materialize().is_err());
    assert_eq!(
        std::fs::read_to_string(graph_path).unwrap(),
        "existing saved graph bytes"
    );
    assert_eq!(
        std::fs::read_to_string(&graph_temp).unwrap(),
        "existing graph temporary bytes"
    );
    assert!(!root.join("work/projects/qnative/spec").exists());
}

#[test]
fn cli_creation_rejects_unsafe_codes_and_incomplete_options_before_writing() {
    for args in [
        vec!["new", "../../escape"],
        vec!["new", "bad\"code"],
        vec!["new", "qnative", "--id"],
        vec!["new", "qnative", "--unknown"],
        vec!["new", "qnative", "extra"],
    ] {
        let args: Vec<_> = args.into_iter().map(str::to_owned).collect();
        assert_ne!(foxproject::cli::cli(&args), 0);
    }
}

#[test]
fn cli_creates_checked_and_placeholder_projects_in_an_isolated_child_root() {
    let fixture = Fixture::new();
    let environment: BTreeMap<_, _> = std::env::vars_os().collect();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "cli_creation_probe", "--nocapture"])
        .env("FOX_REPO_ROOT", &fixture.0)
        .env("Q_CLI_CREATION_PROBE", "1")
        .current_dir(&fixture.0)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let checked = fixture.0.join("projects/qchecked/project.toml");
    let spec = foxpipe::project::Spec::load_in(&fixture.0, checked).unwrap();
    assert_eq!(spec.file().location.id, 99);
    let placeholder =
        std::fs::read_to_string(fixture.0.join("projects/qdraft/project.toml")).unwrap();
    let table: toml::Table = toml::from_str(&placeholder).unwrap();
    assert_eq!(table["location"]["id"].as_integer(), Some(0));
    assert!(!fixture.0.join("projects/badid").exists());
    assert_eq!(std::env::vars_os().collect::<BTreeMap<_, _>>(), environment);
}

#[test]
fn cli_creation_probe() {
    if std::env::var_os("Q_CLI_CREATION_PROBE").is_none() {
        return;
    }
    let invoke = |args: &[&str]| {
        foxproject::cli::cli(
            &args
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(invoke(&["new", "qchecked", "--id", "99"]), 0);
    assert_ne!(invoke(&["new", "qchecked", "--id", "100"]), 0);
    assert_eq!(invoke(&["new", "qdraft"]), 0);
    assert_ne!(invoke(&["new", "badid", "--id", "70"]), 0);
}

/// Set Q_RELEASE_BUNDLE to X/root's validated manifest after C's bundle contract is READY.
/// Uses two copied tool bundles and real native project-check/mod-pack stages, without Python or a workspace.
#[test]
#[ignore = "requires a built release bundle with compiled FOX_BUNDLE_BUILD_ID and C's ready bundled scheduler"]
fn actual_bundle_builds_from_two_relocated_roots() {
    let source_manifest = PathBuf::from(
        std::env::var_os("Q_RELEASE_BUNDLE")
            .expect("set Q_RELEASE_BUNDLE to the release fox-tools.json"),
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&source_manifest).unwrap()).unwrap();
    let source_folder = source_manifest.parent().unwrap();
    let fixture = Fixture::new();
    let mut packages = Vec::new();
    for label in ["first relocated root", "second relocated root"] {
        let root = fixture.root(label);
        let bundle = root.join("tools bundle");
        std::fs::create_dir(&bundle).unwrap();
        for tool in manifest["tools"].as_object().unwrap().values() {
            let relative = Path::new(tool["path"].as_str().unwrap());
            assert!(
                !relative.is_absolute()
                    && !relative
                        .components()
                        .any(|part| part == std::path::Component::ParentDir)
            );
            let destination = bundle.join(relative);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::copy(source_folder.join(relative), destination).unwrap();
        }
        let manifest_path = bundle.join("fox-tools.json");
        std::fs::copy(&source_manifest, &manifest_path).unwrap();
        let tool = bundle.join(manifest["tools"]["fox"]["path"].as_str().unwrap());
        let input = root.join("input/mod/GameDir");
        std::fs::create_dir_all(&input).unwrap();
        std::fs::write(input.join("fixture.txt"), "Q native synthetic fixture\n").unwrap();
        std::fs::write(root.join("input/mod/metadata.xml"), "<ModEntry Name=\"Q native fixture\" Version=\"1.0\" Author=\"Q\"><Description>Synthetic only</Description></ModEntry>").unwrap();
        let text = format!(
            "{SPEC}\n[[stage]]\nname = \"package.fixture\"\nafter = [\"project.validate\"]\ncmd = [\"fox\", \"mod\", \"pack\", \"input/mod\", \"work/native.mgsv\"]\noutputs = [\"work/native.mgsv\"]\nmem_gb = 0.1\n"
        );
        let mut document = ProjectDocument::open_in(&root, Path::new("project.toml")).unwrap();
        document.save(&text).unwrap();
        let plan = plan_build_in(&root, Path::new("project.toml"))
            .unwrap()
            .with_tool(&tool)
            .unwrap();
        assert_eq!(
            plan.native_tool().unwrap().blake3,
            manifest["tools"]["fox"]["blake3"].as_str().unwrap()
        );
        let published = plan.materialize().unwrap();
        let mut command = Command::new(&tool);
        command
            .args(["build", "--root"])
            .arg(&root)
            .arg("--config")
            .arg(&published.graph_path)
            .arg("--bundled-tools")
            .arg(&manifest_path)
            .args(["--jobs", "1", "--stop-on-error"])
            .current_dir(&root)
            .env_remove("PYTHONPATH")
            .env("FOX_PYTHON", root.join("no-python"));
        plan.apply_child_environment(&mut command).unwrap();
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let package = std::fs::read(root.join("work/native.mgsv")).unwrap();
        assert!(package.starts_with(b"PK\x03\x04") && package.len() > 100);
        assert!(root.join("work/build/state.json").is_file());
        assert!(!root.join("tools/rust").exists());
        packages.push(package);
    }
    assert_eq!(
        packages[0], packages[1],
        "relocated native package bytes changed"
    );
    println!(
        "actual native bundle: two relocated roots, project validation plus .mgsv fixture packaging, no Cargo workspace or Python stage, identical package bytes"
    );
}

#[test]
fn prepared_graph_preserves_captured_hashes_and_child_identity_without_publication() {
    let fixture = Fixture::new();
    let root = fixture.root("prepared graph");
    let tool = Fixture::tool(&root);
    let environment: BTreeMap<_, _> = std::env::vars_os().collect();
    let cwd = std::env::current_dir().unwrap();
    let plan = plan_build_in(&root, Path::new("project.toml"))
        .unwrap()
        .with_tool(&tool)
        .unwrap();
    let prepared = plan.prepared_graph().unwrap();
    assert!(!root.join("work").exists());
    assert_eq!(prepared.input_hashes.len(), foxpipe::project::FACETS.len());
    assert_eq!(prepared.child_env.len(), 2);
    assert_eq!(
        prepared.child_env[std::ffi::OsStr::new("FOX_PROJECT")],
        plan.spec_path().as_os_str()
    );
    assert_eq!(
        prepared.child_env[std::ffi::OsStr::new("FOX_REPO_ROOT")],
        plan.repo_root().as_os_str()
    );
    assert_eq!(
        serde_json::to_value(&prepared.graph).unwrap(),
        serde_json::to_value(plan.graph()).unwrap()
    );
    for (planned, inspected) in plan.graph().stage.iter().zip(&prepared.graph.stage) {
        assert_eq!(
            foxbuild::command_fingerprint(planned, ""),
            foxbuild::command_fingerprint(inspected, "")
        );
    }
    // Later source edits do not alter this plan's captured publication bytes.
    std::fs::write(
        root.join("project.toml"),
        SPEC.replace("id = 99", "id = 100"),
    )
    .unwrap();
    plan.materialize().unwrap();
    for (relative, expected) in &prepared.input_hashes {
        let bytes = std::fs::read(root.join(relative)).unwrap();
        assert_eq!(&blake3::hash(&bytes).to_hex()[..32], expected);
    }
    assert_eq!(std::env::vars_os().collect::<BTreeMap<_, _>>(), environment);
    assert_eq!(std::env::current_dir().unwrap(), cwd);
}

#[test]
fn prepared_status_sees_unpublished_spec_changes_under_a_held_lease_without_writes() {
    use foxbuild::bundle::{BuildIdentity, BundledTools};
    use foxbuild::{StageRecord, State, StatusOptions};

    let fixture = Fixture::new();
    let root = fixture.root("prepared status");
    let tool = Fixture::tool(&root);
    let environment: BTreeMap<_, _> = std::env::vars_os().collect();
    let cwd = std::env::current_dir().unwrap();
    let old_plan = plan_build_in(&root, Path::new("project.toml"))
        .unwrap()
        .with_tool(&tool)
        .unwrap();
    let old_prepared = old_plan.prepared_graph().unwrap();
    // This authored trust context exercises native status; no fixture executable or stage is run.
    let identity = BuildIdentity {
        package_version: env!("CARGO_PKG_VERSION").into(),
        build_id: "q-prepared-status-fixture".into(),
    };
    let manifest = root.join("fox-tools.json");
    std::fs::write(
        &manifest,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "package_version": identity.package_version,
            "build_id": identity.build_id,
            "tools": {"fox": {"path": "bin/fox", "blake3": old_plan.native_tool().unwrap().blake3}},
        }))
        .unwrap(),
    )
    .unwrap();
    let bundle = BundledTools::load(&manifest, &identity).unwrap();
    old_plan.materialize().unwrap();
    let facet = old_prepared
        .input_hashes
        .iter()
        .find(|(path, _)| path.ends_with("/location.json"))
        .unwrap();
    let record = StageRecord {
        ok: true,
        trace_complete: true,
        cmd_fp: bundle
            .command_fingerprint(&old_prepared.graph.stage[0])
            .unwrap(),
        inputs: BTreeMap::from([(facet.0.clone(), facet.1.clone())]),
        ..StageRecord::default()
    };
    let state = State {
        stages: BTreeMap::from([("project.validate".into(), record)]),
    };
    let log_dir = root.join(&old_plan.graph().settings.log_dir);
    state.save(&log_dir.join("state.json")).unwrap();
    let options = StatusOptions {
        python: "this-inspection-must-not-start-python".into(),
        check_dirty: true,
        save_cache: false,
    };
    let before = foxbuild::status_prepared(
        &root,
        old_plan.graph_path(),
        &options,
        Some(&bundle),
        &old_prepared,
    )
    .unwrap();
    assert!(before.stages[0].dirty.is_none());
    std::fs::write(
        root.join("project.toml"),
        SPEC.replace("id = 99", "id = 100"),
    )
    .unwrap();
    let new_plan = plan_build_in(&root, Path::new("project.toml"))
        .unwrap()
        .with_tool(&tool)
        .unwrap();
    let new_prepared = new_plan.prepared_graph().unwrap();
    assert_ne!(new_prepared.input_hashes[facet.0], *facet.1);
    // PublicationLease uses the real scheduler's OS lease and excludes competing shared writes.
    let _owner = foxbuild::PublicationLease::try_acquire(&log_dir)
        .unwrap()
        .unwrap();
    let snapshot = prepared_publication_snapshot(&root);
    let after = foxbuild::status_prepared(
        &root,
        new_plan.graph_path(),
        &options,
        Some(&bundle),
        &new_prepared,
    )
    .unwrap();
    assert_eq!(after.config, new_plan.graph_path());
    assert!(
        after.stages[0]
            .dirty
            .as_ref()
            .unwrap()
            .contains(facet.0.as_str())
    );
    assert_eq!(prepared_publication_snapshot(&root), snapshot);
    assert_eq!(std::env::vars_os().collect::<BTreeMap<_, _>>(), environment);
    assert_eq!(std::env::current_dir().unwrap(), cwd);
}

fn prepared_publication_snapshot(
    root: &Path,
) -> BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
    let mut files = BTreeMap::new();
    let mut directories = vec![root.join("work")];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                directories.push(path);
            } else if path.file_name() != Some(std::ffi::OsStr::new("foxbuild.lease")) {
                // Windows forbids reading a held OS lease; it is control state, not a publication.
                let bytes = std::fs::read(&path).unwrap();
                let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
                files.insert(path, (bytes, modified));
            }
        }
    }
    files
}

#[test]
fn prepared_graph_rejects_replaced_tool_without_publishing() {
    let fixture = Fixture::new();
    let root = fixture.root("prepared stale tool");
    let tool = Fixture::tool(&root);
    let plan = plan_build_in(&root, Path::new("project.toml"))
        .unwrap()
        .with_tool(&tool)
        .unwrap();
    assert!(plan.prepared_graph().is_ok());
    std::fs::write(&tool, "replacement tool bytes").unwrap();
    assert!(plan.prepared_graph().err().unwrap().contains("changed"));
    assert!(!root.join("work").exists());
}
