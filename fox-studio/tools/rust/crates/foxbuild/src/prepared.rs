//! Validated, caller-owned project inputs. Publication belongs to the scheduler's
//! normal build lease; inspections use these inputs without publishing them.
use crate::{config::Graph, hashing};
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path};

#[derive(Debug, Clone)]
pub struct PreparedGraph {
    pub graph: Graph,
    /// Repository-relative, slash-normalized paths and first-128-bit BLAKE3 hashes.
    pub input_hashes: BTreeMap<String, String>,
    /// Stage-only overrides, applied last; never part of a command fingerprint.
    pub child_env: BTreeMap<OsString, OsString>,
}

impl PreparedGraph {
    pub(crate) fn validate(&self, root: &Path, config: &Path) -> Result<()> {
        self.graph.validate()?;
        let canonical_root = root
            .canonicalize()
            .with_context(|| format!("resolving prepared root {}", root.display()))?;
        if !canonical_root.is_dir() {
            bail!("prepared root must be a directory");
        }
        validate_path(root, config, &canonical_root)?;
        for path in [&self.graph.settings.log_dir, &self.graph.settings.temp_dir] {
            validate_relative(path)?;
            validate_path(root, &root.join(path), &canonical_root)?;
        }
        let mut keys = BTreeSet::new();
        for (path, hash) in &self.input_hashes {
            validate_relative(path)?;
            validate_path(root, &root.join(path), &canonical_root)?;
            if !keys.insert(hashing::key(path)) {
                bail!("duplicate prepared input path alias: {path}");
            }
            if hash.len() != 32
                || !hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                bail!("prepared input {path} needs a lowercase first-128-bit BLAKE3 hash");
            }
        }
        for key in &self.graph.settings.unset_env {
            validate_env(OsStr::new(key), OsStr::new(""))?;
        }
        for (key, value) in &self.graph.settings.env {
            validate_env(OsStr::new(key), OsStr::new(value))?;
        }
        for stage in &self.graph.stage {
            if stage.name.is_empty()
                || stage.name == "."
                || stage.name == ".."
                || stage
                    .name
                    .chars()
                    .any(|character| character.is_control() || "/\\:<>\"|?*".contains(character))
            {
                bail!(
                    "prepared stage name must be a plain filename: {:?}",
                    stage.name
                );
            }
            for command in std::iter::once(&stage.cmd).chain(stage.cmd_rs.iter()) {
                if command[0].is_empty() || command.iter().any(|argument| argument.contains('\0')) {
                    bail!(
                        "prepared stage {} has an invalid command argument",
                        stage.name
                    );
                }
            }
            for (key, value) in &stage.env {
                validate_env(OsStr::new(key), OsStr::new(value))?;
            }
        }
        for (key, value) in &self.child_env {
            validate_env(key, value)?;
            if (key == "FOX_PROJECT" || key == "FOX_REPO_ROOT") && !Path::new(value).is_absolute() {
                bail!(
                    "prepared {} must be an absolute path",
                    key.to_string_lossy()
                );
            }
        }
        Ok(())
    }
}

fn validate_relative(path: &str) -> Result<()> {
    if path.is_empty()
        || path.contains(['\\', ':'])
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        bail!("prepared path must be repository-relative and slash-normalized: {path:?}");
    }
    Ok(())
}

fn validate_path(root: &Path, path: &Path, canonical_root: &Path) -> Result<()> {
    if path.components().any(|part| part == Component::ParentDir)
        || !path.starts_with(root)
        || path.as_os_str().to_string_lossy().contains('\0')
    {
        bail!(
            "prepared path escapes the selected root: {}",
            path.display()
        );
    }
    // Future facet/graph files need not exist, but an existing ancestor must
    // resolve inside the selected root, including through directory aliases.
    let mut ancestor = path;
    loop {
        match ancestor.canonicalize() {
            Ok(resolved) if resolved.starts_with(canonical_root) => return Ok(()),
            Ok(_) => bail!(
                "prepared path alias escapes the selected root: {}",
                path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ancestor = ancestor
                    .parent()
                    .context("prepared path has no existing ancestor")?;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("resolving prepared path {}", path.display()));
            }
        }
    }
}

fn validate_env(key: &OsStr, value: &OsStr) -> Result<()> {
    let text = key.to_string_lossy();
    if text.is_empty() || text.contains(['=', '\0']) || value.to_string_lossy().contains('\0') {
        bail!("invalid prepared environment entry: {text:?}");
    }
    Ok(())
}
