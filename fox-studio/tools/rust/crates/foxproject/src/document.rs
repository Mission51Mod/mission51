//! Validated location documents for native editors. The repository and file origin are explicit.
use foxpipe::project::{LoadOptions, Spec};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The original bytes and their validated spec. Draft text stays in the caller's editor.
#[derive(Debug)]
pub struct ProjectDocument {
    spec: Spec,
    text: String,
}

impl ProjectDocument {
    pub fn open_in(root: &Path, path: &Path) -> Result<Self, String> {
        if root.as_os_str().is_empty() {
            return Err("repository root: expected a nonempty directory path".into());
        }
        let root =
            std::path::absolute(root).map_err(|error| format!("repository root: {error}"))?;
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        let path = if path.is_dir() {
            path.join("project.toml")
        } else {
            path
        };
        let text =
            fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let spec = validate(&root, &path, &text)?;
        Ok(Self { spec, text })
    }

    /// Create a validated document, refusing any existing file. The selected root must already exist.
    pub fn create_in(root: &Path, path: &Path, text: &str) -> Result<Self, String> {
        let spec = validate(root, path, text)?;
        let path = spec.path();
        let parent = path
            .parent()
            .ok_or("project file has no parent directory")?;
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| format!("{}: cannot create project: {error}", path.display()))?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("{}: cannot write project: {error}", path.display()))?;
        Ok(Self {
            spec,
            text: text.to_owned(),
        })
    }

    pub fn spec(&self) -> &Spec {
        &self.spec
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn path(&self) -> &Path {
        self.spec.path()
    }
    pub fn repo_root(&self) -> &Path {
        self.spec.repo_root()
    }

    /// Validate a draft without publishing graph/facet files or changing the editor's saved state.
    pub fn validate(&self, draft: &str) -> Result<Spec, String> {
        validate(self.repo_root(), self.path(), draft)
    }

    /// Check current disk bytes before replacement. Editors sharing one file still need coordination.
    /// Validation and stale-document errors leave the file and this document unchanged.
    pub fn save(&mut self, draft: &str) -> Result<bool, String> {
        let spec = self.validate(draft)?;
        let disk =
            fs::read(self.path()).map_err(|error| format!("{}: {error}", self.path().display()))?;
        if disk != self.text.as_bytes() {
            return Err(format!(
                "{} changed outside this editor; reopen it before saving",
                self.path().display()
            ));
        }
        if draft == self.text {
            return Ok(false);
        }
        replace(self.path(), draft.as_bytes())?;
        self.spec = spec;
        self.text = draft.to_owned();
        Ok(true)
    }
}

fn validate(root: &Path, path: &Path, text: &str) -> Result<Spec, String> {
    let spec = Spec::from_toml_str_in(root, text, path, &LoadOptions::default())
        .map_err(|error| error.0)?;
    crate::generate(&spec).map_err(|error| error.0)?;
    Ok(spec)
}

fn replace(path: &Path, bytes: &[u8]) -> Result<(), String> {
    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
    let serial = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let mut name = path
        .file_name()
        .ok_or("project file has no name")?
        .to_owned();
    name.push(format!(".{}.{}.tmp", std::process::id(), serial));
    let temporary: PathBuf = path.with_file_name(name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| format!("{}: cannot save project: {error}", path.display()))?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(format!("{}: cannot save project: {error}", path.display()));
    }
    Ok(())
}
