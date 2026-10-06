//! Write guard for Rust stage processes (the Rust twin of the editor's safety.py audit-hook guard).
//!
//! `FOX_WRITE_ROOTS` = ';'-separated folders (absolute, or relative to the repo root). When set, the recording write
//! wrappers `trace::fs_write` / `trace::create` refuse any path outside those roots (io::ErrorKind::PermissionDenied,
//! "outside FOX_WRITE_ROOTS"), and the record-only `trace::write` / `trace::rename` log the violation. Every violation
//! is appended to `<first root>/logs/guard.log` and counted (`violations()`), so a caller can fail the process.
//! The editor runs Rust steps of a workspace build with `FOX_WRITE_ROOTS=<workspace>;<temp>` (M3_PLAN.md §6).
//!
//! Unset: every check is one atomic load (OnceLock) and returns Ok.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

struct Guard {
    /// lower-case, '/'-separated absolute roots, with a trailing '/'
    keys: Vec<String>,
    log: PathBuf,
}

static GUARD: OnceLock<Option<Guard>> = OnceLock::new();
static VIOLATIONS: AtomicUsize = AtomicUsize::new(0);

fn norm_abs(p: &Path) -> String {
    let abs = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(p) };
    let s = abs.to_string_lossy().replace('\\', "/");
    let mut parts: Vec<&str> = vec![];
    for c in s.split('/') {
        match c {
            "" | "." if !parts.is_empty() => {}
            ".." => {
                if parts.len() > 1 {
                    parts.pop();
                }
            }
            x => parts.push(x),
        }
    }
    parts.join("/").to_lowercase()
}

fn guard() -> Option<&'static Guard> {
    GUARD
        .get_or_init(|| {
            let v = std::env::var_os("FOX_WRITE_ROOTS")?;
            let v = v.to_string_lossy().into_owned();
            let roots: Vec<PathBuf> = v
                .split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| {
                    let p = PathBuf::from(s);
                    if p.is_absolute() { p } else { crate::paths::repo_root().join(p) }
                })
                .collect();
            if roots.is_empty() {
                return None;
            }
            let keys = roots.iter().map(|r| format!("{}/", norm_abs(r).trim_end_matches('/'))).collect();
            Some(Guard { keys, log: roots[0].join("logs").join("guard.log") })
        })
        .as_ref()
}

/// true when FOX_WRITE_ROOTS is in effect
pub fn active() -> bool {
    guard().is_some()
}

fn allowed(g: &Guard, p: &Path) -> bool {
    let k = format!("{}/", norm_abs(p).trim_end_matches('/'));
    g.keys.iter().any(|r| k.starts_with(r.as_str()))
}

fn violation(g: &Guard, p: &Path, what: &str) {
    VIOLATIONS.fetch_add(1, Ordering::SeqCst);
    let line = format!("{} pid {} {what} outside FOX_WRITE_ROOTS: {}\n",
                       std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
                       std::process::id(), p.display());
    eprintln!("[foxpipe::guard] {}", line.trim_end());
    if let Some(d) = g.log.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&g.log) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Before a write: Err(PermissionDenied) outside the roots (and logged). Ok when the guard is off.
pub fn check(p: &Path) -> std::io::Result<()> {
    let Some(g) = guard() else { return Ok(()) };
    if allowed(g, p) {
        return Ok(());
    }
    violation(g, p, "write refused");
    Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("{}: outside FOX_WRITE_ROOTS", p.display())))
}

/// After the fact (a write recorded by trace::write): log and count a violation, never fail.
pub fn note(p: &Path) {
    let Some(g) = guard() else { return };
    if !allowed(g, p) {
        violation(g, p, "write recorded");
    }
}

/// violations so far in this process
pub fn violations() -> usize {
    VIOLATIONS.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_and_normalisation() {
        let d = std::env::temp_dir().join(format!("foxguard_{}", std::process::id()));
        let g = Guard { keys: vec![format!("{}/", norm_abs(&d.join("ws")))], log: d.join("ws").join("logs").join("guard.log") };
        assert!(allowed(&g, &d.join("ws").join("a").join("b.npy")));
        assert!(allowed(&g, &d.join("WS").join("x")));
        assert!(!allowed(&g, &d.join("ws2").join("x")));
        assert!(!allowed(&g, &d.join("ws").join("..").join("other")));
        violation(&g, &d.join("other"), "test");
        assert!(violations() >= 1);
        assert!(std::fs::read_to_string(&g.log).unwrap().contains("outside FOX_WRITE_ROOTS"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
