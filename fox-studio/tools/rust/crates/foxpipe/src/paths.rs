//! Repository-relative paths. The repo root is FOX_REPO_ROOT if set, else found by walking up from the current
//! directory to the folder holding tools/rust/Cargo.toml. Ports use repo-relative paths exactly as their Python
//! reference (ROOT / "work" / ...), never absolute drive paths.
use std::path::{Path, PathBuf};

pub fn repo_root() -> PathBuf {
    if let Some(r) = std::env::var_os("FOX_REPO_ROOT") {
        return PathBuf::from(r);
    }
    let mut d = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    loop {
        if d.join("tools").join("rust").join("Cargo.toml").exists() {
            return d;
        }
        if !d.pop() {
            return std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        }
    }
}

pub fn repo(rel: &str) -> PathBuf {
    repo_root().join(rel.replace('/', std::path::MAIN_SEPARATOR_STR))
}

/// repo-relative, forward-slash form of a path (as the Python manifests store them)
pub fn rel(p: &Path) -> String {
    p.strip_prefix(repo_root()).unwrap_or(p).to_string_lossy().replace('\\', "/")
}
