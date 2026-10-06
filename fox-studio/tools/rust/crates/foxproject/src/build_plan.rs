//! Immutable project preparation for frontends that launch more than one project.
//!
//! Planning reads an explicit spec and captures the graph and facet bytes. Publication is a separate step;
//! neither step changes the parent environment or uses `Spec::current`. Apply the child environment immediately
//! before launching a separate foxbuild process. In-process foxbuild still reads process-wide options.
use crate::{generate, graph_toml};
use foxbuild::config::Graph;
use foxpipe::project::{FACETS, Spec};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// A read-only snapshot of one project's graph, identity and facet publications.
/// Re-plan after editing the spec. Serialize publication/builds that share writable storage directories.
#[derive(Debug, Clone)]
pub struct ProjectBuildPlan {
    repo_root: PathBuf,
    repo_identity: PathBuf,
    spec_path: PathBuf,
    spec_identity: PathBuf,
    spec_origin_identity: PathBuf,
    spec_rel: String,
    compat: Option<String>,
    graph: Graph,
    graph_path: PathBuf,
    graph_text: String,
    facets: Vec<(PathBuf, Vec<u8>)>,
    native_tool: Option<NativeToolIdentity>,
}

/// Captured bytes of the explicitly selected tool. C additionally validates its release bundle/build identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeToolIdentity {
    pub path: PathBuf,
    pub blake3: String,
}

/// Files changed by an explicit publication; no build stage has been executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedBuild {
    pub graph_path: PathBuf,
    pub graph_changed: bool,
    pub facets_changed: Vec<String>,
}

/// Load an explicit spec (or a directory containing project.toml) without writing files or changing environment.
pub fn plan_build(spec_path: &Path) -> Result<ProjectBuildPlan, String> {
    plan_build_in(&foxpipe::paths::repo_root(), spec_path)
}

/// Plan using a frontend's chosen repository root, independently of inherited FOX_REPO_ROOT.
/// Relative specs resolve against this root; include/template origins and all publications retain it.
pub fn plan_build_in(repo_root: &Path, spec_path: &Path) -> Result<ProjectBuildPlan, String> {
    if repo_root.as_os_str().is_empty() {
        return Err("repository root: expected a nonempty directory path".into());
    }
    let root = std::path::absolute(repo_root).map_err(|e| format!("repository root: {e}"))?;
    let path = if spec_path.is_absolute() {
        spec_path.to_path_buf()
    } else {
        root.join(spec_path)
    };
    let path = if path.is_dir() {
        path.join("project.toml")
    } else {
        path
    };
    let spec = Spec::load_in(&root, &path).map_err(|e| e.0)?;
    let graph = generate(&spec).map_err(|e| e.0)?.graph;
    let graph_text = graph_toml(&spec, &graph).map_err(|e| e.0)?;
    let repo_identity = canonical(&root, "repository root")?;
    // Retain the selected origin: includes/templates are relative to it, including a spec file symlink.
    // Canonical identity is for validation, not a rewrite of the path that stage processes load.
    let repo_root = root;
    let spec_path = spec.path().to_path_buf();
    let spec_identity = canonical(&spec_path, "project spec")?;
    let spec_origin_identity = canonical(
        spec_path.parent().ok_or("project spec has no parent")?,
        "project spec origin",
    )?;
    let graph_path = repo_root
        .join(&graph.settings.log_dir)
        .join("graph.generated.toml");
    let cache = repo_root.join(spec.datasets_untraced().rel("spec_cache"));
    let mut facets = Vec::new();
    for name in FACETS {
        let mut bytes =
            serde_json::to_vec_pretty(&spec.resolved_json()[*name]).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        facets.push((cache.join(format!("{name}.json")), bytes));
    }
    let spec_rel = spec.rel().to_owned();
    let compat = spec.compat().map(str::to_owned);
    let plan = ProjectBuildPlan {
        repo_root,
        repo_identity,
        spec_path,
        spec_identity,
        spec_origin_identity,
        spec_rel,
        compat,
        graph,
        graph_path,
        graph_text,
        facets,
        native_tool: None,
    };
    plan.validate_graph_environment()?;
    Ok(plan)
}

impl ProjectBuildPlan {
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }
    pub fn spec_path(&self) -> &Path {
        &self.spec_path
    }
    pub fn graph(&self) -> &Graph {
        &self.graph
    }
    pub fn graph_path(&self) -> &Path {
        &self.graph_path
    }
    pub fn graph_text(&self) -> &str {
        &self.graph_text
    }
    pub fn native_tool(&self) -> Option<&NativeToolIdentity> {
        self.native_tool.as_ref()
    }

    /// Inspect the captured graph and facet hashes without publishing shared project files.
    /// The scheduler runs `materialize` only in its real-build callback, under its retained build lease.
    pub fn prepared_graph(&self) -> Result<foxbuild::PreparedGraph, String> {
        let child_env = self.child_environment(&BTreeMap::new())?;
        let mut input_hashes = BTreeMap::new();
        for (path, bytes) in &self.facets {
            let relative = path
                .strip_prefix(&self.repo_root)
                .map_err(|_| {
                    format!(
                        "planned facet {} is outside the selected repository",
                        path.display()
                    )
                })?
                .to_str()
                .ok_or("planned facet path is not UTF-8")?
                .replace('\\', "/");
            // foxbuild uses the first 128 bits of BLAKE3 for traced file inputs.
            let hash = blake3::hash(bytes).to_hex()[..32].to_owned();
            input_hashes.insert(relative, hash);
        }
        Ok(foxbuild::PreparedGraph {
            graph: self.graph.clone(),
            input_hashes,
            child_env,
        })
    }

    /// Capture a prebuilt tool and emit its `fox` bundle alias in a separate graph snapshot.
    /// Launch with C's validated `--bundled-tools` manifest; this never bypasses developer freshness flags.
    pub fn with_tool(&self, executable: &Path) -> Result<Self, String> {
        if self.compat.is_some() {
            return Err("portable tool binding requires a native project without a legacy compatibility recipe".into());
        }
        let executable = if executable.is_absolute() {
            executable.to_path_buf()
        } else {
            self.repo_root.join(executable)
        };
        if !executable.is_file() {
            return Err(format!(
                "native tool: {} is not an executable file",
                executable.display()
            ));
        }
        let executable = canonical(&executable, "native tool")?;
        let command = executable.to_str().ok_or("native tool path is not UTF-8")?;
        let identity = NativeToolIdentity {
            path: executable.clone(),
            blake3: tool_hash(&executable)?,
        };
        let mut plan = self.clone();
        if let Some(previous) = &plan.native_tool {
            let path = previous.path.to_string_lossy();
            plan.graph
                .pinned
                .retain(|pin| pin.owner != "native tool" || pin.path != path);
        }
        for stage in &mut plan.graph.stage {
            if stage.cmd_rs.is_some() {
                return Err(format!(
                    "stage {}: portable projects use a native cmd directly, without transitional cmd_rs variants",
                    stage.name
                ));
            }
            let first = stage
                .cmd
                .first_mut()
                .ok_or_else(|| format!("stage {}: empty command", stage.name))?;
            if first == crate::FOX_EXE || first == "fox" || first == "fox.exe" {
                *first = "fox".to_owned();
            } else if stage.rust_tools {
                return Err(format!(
                    "stage {}: a portable Rust stage must invoke the selected fox tool",
                    stage.name
                ));
            }
            let program = Path::new(first)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(first);
            let program = program.to_ascii_lowercase();
            if program.starts_with("python")
                || matches!(program.as_str(), "py" | "py.exe")
                || program.ends_with(".py")
            {
                return Err(format!(
                    "stage {}: portable project actions must be native Rust commands",
                    stage.name
                ));
            }
        }
        plan.graph.pinned.push(foxbuild::config::Pinned {
            path: command.to_owned(),
            owner: "native tool".into(),
        });
        plan.native_tool = Some(identity);
        plan.graph_text =
            crate::graph_toml_from_origin(&plan.spec_rel, &plan.graph).map_err(|error| error.0)?;
        plan.validate_graph_environment()?;
        Ok(plan)
    }

    /// Publish only changed snapshot bytes. Does not mutate this plan, the parent environment or build state.
    /// Concurrent plans must have separate log/spec-cache/data roots; this is not a build scheduler or lock.
    pub fn materialize(&self) -> Result<MaterializedBuild, String> {
        self.validate_native_tool()?;
        let graph_changed = write_changed(&self.graph_path, self.graph_text.as_bytes())?;
        let mut facets_changed = Vec::new();
        for (path, bytes) in &self.facets {
            if write_changed(path, bytes)? {
                facets_changed.push(
                    path.file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        Ok(MaterializedBuild {
            graph_path: self.graph_path.clone(),
            graph_changed,
            facets_changed,
        })
    }

    /// Child-only overlays. The selected project replaces any inherited parent identity; explicit overrides must
    /// agree with it. Identity is not inserted into graph settings or stage env, preserving command fingerprints.
    pub fn child_environment(
        &self,
        overrides: &BTreeMap<String, String>,
    ) -> Result<BTreeMap<OsString, OsString>, String> {
        self.validate_native_tool()?;
        let mut env = BTreeMap::new();
        for (key, value) in overrides {
            self.validate_env(key.as_ref(), Some(value.as_ref()), "child environment")?;
            if !identity_key(key.as_ref()) {
                env.insert(OsString::from(key), OsString::from(value));
            }
        }
        env.insert("FOX_PROJECT".into(), self.spec_path.as_os_str().to_owned());
        env.insert(
            "FOX_REPO_ROOT".into(),
            self.repo_root.as_os_str().to_owned(),
        );
        Ok(env)
    }

    /// Validate existing explicit command overlays/removals before applying child identity. An error leaves the
    /// Command unchanged. Call this last, before spawn/status/output; later env changes require revalidation.
    pub fn apply_child_environment(&self, command: &mut Command) -> Result<(), String> {
        self.validate_native_tool()?;
        let mut aliases = Vec::new();
        for (key, value) in command.get_envs() {
            self.validate_env(key, value, "child command environment")?;
            if identity_key(key) && key != "FOX_PROJECT" && key != "FOX_REPO_ROOT" {
                aliases.push(key.to_owned());
            }
        }
        for key in aliases {
            command.env_remove(key);
        }
        command
            .env("FOX_PROJECT", &self.spec_path)
            .env("FOX_REPO_ROOT", &self.repo_root);
        Ok(())
    }

    fn validate_graph_environment(&self) -> Result<(), String> {
        for key in &self.graph.settings.unset_env {
            self.validate_env(key.as_ref(), None, "build.unset_env")?;
        }
        for (key, value) in &self.graph.settings.env {
            self.validate_env(key.as_ref(), Some(value.as_ref()), "build.env")?;
        }
        for stage in &self.graph.stage {
            for (key, value) in &stage.env {
                self.validate_env(
                    key.as_ref(),
                    Some(value.as_ref()),
                    &format!("stage {} env", stage.name),
                )?;
            }
        }
        Ok(())
    }

    fn validate_native_tool(&self) -> Result<(), String> {
        if let Some(tool) = &self.native_tool
            && tool_hash(&tool.path)? != tool.blake3
        {
            return Err(format!(
                "native tool {} changed after planning; select the validated bundle and re-plan",
                tool.path.display()
            ));
        }
        Ok(())
    }

    fn validate_env(
        &self,
        key: &OsStr,
        value: Option<&OsStr>,
        context: &str,
    ) -> Result<(), String> {
        let name = key.to_string_lossy();
        let bytes = key.as_encoded_bytes();
        if bytes.is_empty() || bytes.contains(&b'=') || bytes.contains(&0) {
            return Err(format!(
                "{context}: invalid environment key {name:?} (empty, '=' or NUL)"
            ));
        }
        if value.is_some_and(|v| v.as_encoded_bytes().contains(&0)) {
            return Err(format!("{context}.{name}: environment value contains NUL"));
        }
        if !identity_key(key) {
            return Ok(());
        }
        let project = name.eq_ignore_ascii_case("FOX_PROJECT");
        let expected = if project {
            &self.spec_identity
        } else {
            &self.repo_identity
        };
        let value = value
            .ok_or_else(|| format!("{context}.{name}: cannot remove selected project identity"))?;
        if value.is_empty()
            || value
                .to_str()
                .is_some_and(|v| v.trim().is_empty() || v.chars().any(char::is_control))
        {
            return Err(format!(
                "{context}.{name}: project identity must be a nonempty path without control characters"
            ));
        }
        let path = Path::new(value);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.repo_root.join(path)
        };
        let actual = canonical(&path, &format!("{context}.{name}"))?;
        if &actual != expected {
            return Err(format!(
                "{context}.{name}: {} conflicts with selected project identity {}",
                path.display(),
                expected.display()
            ));
        }
        // The same root file in another directory can resolve different include/template files. A hard link or
        // file symlink alone does not establish an equivalent project origin.
        if project
            && canonical(
                path.parent().ok_or("project identity has no parent")?,
                context,
            )? != self.spec_origin_identity
        {
            return Err(format!(
                "{context}.{name}: {} conflicts with selected project include/template origin {}",
                path.display(),
                self.spec_path.display()
            ));
        }
        Ok(())
    }
}

fn identity_key(key: &OsStr) -> bool {
    key.to_str().is_some_and(|k| {
        k.eq_ignore_ascii_case("FOX_PROJECT") || k.eq_ignore_ascii_case("FOX_REPO_ROOT")
    })
}

fn canonical(path: &Path, context: &str) -> Result<PathBuf, String> {
    std::fs::canonicalize(path)
        .map_err(|e| format!("{context}: cannot resolve {}: {e}", path.display()))
}

fn tool_hash(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("native tool {}: {error}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("native tool {}: {error}", path.display()))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn write_changed(path: &Path, bytes: &[u8]) -> Result<bool, String> {
    if std::fs::read(path).ok().as_deref() == Some(bytes) {
        return Ok(false);
    }
    let dir = path
        .parent()
        .ok_or_else(|| format!("{}: no parent directory", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(format!(".{}.{}.tmp", std::process::id(), serial));
    let temp = dir.join(name);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let result = (|| {
        file.write_all(bytes)?;
        drop(file);
        std::fs::rename(&temp, path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("{}: {e}", path.display()));
    }
    Ok(true)
}
