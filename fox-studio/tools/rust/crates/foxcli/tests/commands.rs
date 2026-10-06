//! Public command discovery must work from an extracted package, without a
//! checkout, game installation, or Python interpreter.
use std::process::{Command, Output};

fn fox(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fox"))
        .args(arguments)
        .env_remove("FOX_PROJECT")
        .env_remove("FOX_REPO_ROOT")
        .env_remove("FOXBUILD_TRACE_DIR")
        .output()
        .expect("run the compiled fox executable")
}

#[test]
fn version_reports_the_built_package() {
    let output = fox(&["--version"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        concat!("fox ", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn help_lists_the_available_public_commands() {
    let output = fox(&["--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for command in ["project", "build", "qar", "pack", "dict", "mod"] {
        assert!(help.contains(command), "missing help for {command}");
    }
    assert!(!help.contains("C:\\Users\\"));
    assert!(!help.contains("H:\\"));
    assert!(output.stderr.is_empty());
}

#[test]
fn unknown_command_is_a_failed_invocation() {
    let output = fox(&["not-a-command"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("fox --help")
    );
}

#[test]
fn project_commands_are_reachable() {
    let output = fox(&["project", "--help"]);
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("project")
    );
}

#[test]
fn incomplete_profile_option_reports_a_usage_error() {
    let output = fox(&["qar", "info", "unused.dat", "--runtime-profile"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("needs a file path")
    );
}

#[cfg(not(feature = "internal-pipeline"))]
#[test]
fn unconfigured_archive_command_fails_before_reading_inputs() {
    let output = Command::new(env!("CARGO_BIN_EXE_fox"))
        .args(["qar", "info", "does-not-exist.dat"])
        .env_remove("FOX_RUNTIME_PROFILE")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("fox setup")
    );
}

#[test]
fn build_identity_is_machine_readable() {
    let output = fox(&["--build-info"]);
    assert!(output.status.success());
    let identity: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(identity["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        identity["internal_pipeline"],
        cfg!(feature = "internal-pipeline")
    );
}

#[cfg(not(feature = "internal-pipeline"))]
#[test]
fn excluded_pipeline_command_fails_clearly() {
    let output = fox(&["terrain"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("not included in the editor distribution")
    );
}

#[test]
fn project_build_rejects_ambiguous_or_incomplete_sources_before_loading_them() {
    let cases: &[(&[&str], &str)] = &[
        (&["build", "--project"], "--project needs a value"),
        (
            &["build", "--project", "a.toml", "--project", "b.toml"],
            "--project must be specified once",
        ),
        (
            &["build", "--project", "a.toml", "--config", "graph.toml"],
            "choose one",
        ),
        (
            &["build", "--project", "a.toml", "--root"],
            "--root needs a value",
        ),
        (
            &["build", "--project", "a.toml", "--root", "a", "--root", "b"],
            "--root must be specified once",
        ),
        (
            &["build", "--project", "a.toml", "--bundled-tools"],
            "--bundled-tools needs a value",
        ),
        (
            &["build", "--project", "a.toml"],
            "requires --bundled-tools",
        ),
    ];
    for (arguments, diagnostic) in cases {
        let output = fox(arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains(diagnostic), "{arguments:?}: {error}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
    }
}

#[test]
fn project_build_help_does_not_require_a_bundle_or_existing_project() {
    let output = fox(&["build", "--project", "missing/project.toml", "--help"]);
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("--dry-run")
    );
    assert!(output.stderr.is_empty());
}

#[test]
#[ignore = "requires a compiled FOX_BUNDLE_BUILD_ID; run during native package validation"]
fn packaged_project_inspection_and_contended_build_preserve_shared_inputs() {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut pending = vec![root.to_owned()];
        let mut result = BTreeMap::new();
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    pending.push(entry.path());
                } else if entry.file_name() != "foxbuild.lease" {
                    result.insert(
                        entry.path().strip_prefix(root).unwrap().to_owned(),
                        std::fs::read(entry.path()).unwrap(),
                    );
                }
            }
        }
        result
    }

    let identity =
        foxbuild::bundle::BuildIdentity::compiled().expect("compile with a native bundle ID");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture = Fixture(std::env::temp_dir().join(format!(
        "fox_project_boundary_{}_{nonce}",
        std::process::id()
    )));
    let root = fixture.0.join("project");
    let package = fixture.0.join("package");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&package).unwrap();
    let executable = package.join(if cfg!(windows) { "fox.exe" } else { "fox" });
    std::fs::copy(env!("CARGO_BIN_EXE_fox"), &executable).unwrap();
    let manifest = package.join("fox-tools.json");
    let definition = serde_json::json!({
        "schema_version": 1, "package_version": env!("CARGO_PKG_VERSION"), "build_id": identity.build_id,
        "tools": {"fox": {
            "path": executable.file_name().unwrap().to_str().unwrap(),
            "blake3": blake3::hash(&std::fs::read(&executable).unwrap()).to_hex().to_string()
        }}
    });
    std::fs::write(&manifest, serde_json::to_vec(&definition).unwrap()).unwrap();
    let spec = root.join("project.toml");
    let original = foxproject::cli::new_project_text("tstp", 99);
    std::fs::write(&spec, &original).unwrap();
    let plan = foxproject::plan_build_in(&root, &spec)
        .unwrap()
        .with_tool(&executable)
        .unwrap();
    plan.materialize().unwrap();
    let log_dir = root.join(&plan.graph().settings.log_dir);
    std::fs::create_dir_all(&log_dir).unwrap();
    let lease = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(log_dir.join("foxbuild.lease"))
        .unwrap();
    lease.try_lock().unwrap();

    // The new plan differs from published facets. A parent-side materialize
    // would overwrite those inputs even though another build owns the lease.
    let edited = original.replace("id = 99", "id = 100");
    assert_ne!(edited, original);
    std::fs::write(&spec, edited).unwrap();
    let before = files(&root);
    for inspection in [Some("--dry-run"), Some("--status"), None] {
        let mut command = Command::new(&executable);
        command
            .args(["build", "--project"])
            .arg(&spec)
            .arg("--root")
            .arg(&root)
            .arg("--bundled-tools")
            .arg(&manifest)
            .arg("--json")
            .env_remove("FOX_PROJECT")
            .env_remove("FOX_REPO_ROOT")
            .env_remove("FOXBUILD_TRACE_DIR");
        if let Some(flag) = inspection {
            command.arg(flag);
        }
        let output = command.output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        if inspection.is_some() {
            assert!(
                output.status.success(),
                "{inspection:?}: {stdout}\n{stderr}"
            );
        } else {
            assert_eq!(output.status.code(), Some(2), "{stdout}\n{stderr}");
            assert!(format!("{stdout}\n{stderr}").contains("lease"));
            assert!(!stdout.contains("stage_started"));
        }
        assert_eq!(
            files(&root),
            before,
            "{inspection:?} changed shared build inputs"
        );
    }
    drop(lease);
}
