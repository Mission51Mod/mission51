//! foxbuild I/O trace for Rust stage code (the Rust twin of tools/build/trace_site/sitecustomize.py).
//!
//! foxbuild decides whether a stage is up to date from the files its processes read, wrote and listed. Python
//! processes are traced by an audit hook; a Rust process (fox.exe / fox-place.exe ...) reports its own I/O here:
//!
//!   foxpipe::trace::read(&path);      // a file the stage's result depends on
//!   foxpipe::trace::write(&path);     // a file it produced
//!   foxpipe::trace::list(&dir);       // a folder whose listing it depends on
//!   foxpipe::trace::delete(&path);
//!   ...
//!   foxpipe::trace::finish();         // once, before the process exits (foxcli's main does it for every command)
//!
//! Or the wrappers fs_read / fs_read_to_string / fs_write / open_read / create, which do the I/O and record it.
//! Without FOXBUILD_TRACE_DIR in the environment everything here is a no-op costing one atomic load.
//! The trace file is FOXBUILD_TRACE_DIR/<pid>.json in the Python tracer's format: pid, argv, exe, end, reads
//! {rel: [size, mtime_ns] | null}, writes [rel], deleted [rel], lists [rel], procs [exe], modules [rel]. `modules`
//! carries the running executable when it lies inside the repo, so rebuilding fox.exe marks the stage dirty like a
//! changed Python module does. Paths outside the repo root (FOXBUILD_ROOT, else the cwd) are not recorded.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

struct T {
    dir: PathBuf,
    root: PathBuf,
    root_key: String,
    reads: BTreeMap<String, Option<(u64, u128)>>,
    writes: Vec<String>,
    deleted: BTreeSet<String>,
    lists: BTreeSet<String>,
    procs: Vec<String>,
}

static STATE: Mutex<Option<Option<T>>> = Mutex::new(None);

fn with<F: FnOnce(&mut T)>(f: F) {
    let mut g = match STATE.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if g.is_none() {
        *g = Some(std::env::var_os("FOXBUILD_TRACE_DIR").map(|d| {
            let root = std::env::var_os("FOXBUILD_ROOT").map(PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let root = std::path::absolute(&root).unwrap_or(root);
            let root_key = key(&root.to_string_lossy());
            T { dir: PathBuf::from(d), root, root_key, reads: BTreeMap::new(), writes: vec![], deleted: BTreeSet::new(),
                lists: BTreeSet::new(), procs: vec![] }
        }));
    }
    if let Some(Some(t)) = g.as_mut() {
        f(t);
    }
}

fn key(p: &str) -> String {
    p.replace('\\', "/").trim_end_matches('/').to_lowercase()
}

impl T {
    /// repo-relative path with '/' (original case), or None outside the repo
    fn rel(&self, p: &Path) -> Option<String> {
        let abs = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().ok()?.join(p) };
        let abs = std::path::absolute(&abs).unwrap_or(abs);
        let s = abs.to_string_lossy().replace('\\', "/");
        let k = s.to_lowercase();
        let pre = format!("{}/", self.root_key);
        if !k.starts_with(&pre) {
            return None;
        }
        // normalise "." / ".." components
        let mut parts: Vec<&str> = vec![];
        for c in s[pre.len()..].split('/') {
            match c {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                x => parts.push(x),
            }
        }
        Some(parts.join("/"))
    }
}

fn stat(p: &Path) -> Option<(u64, u128)> {
    let m = std::fs::metadata(p).ok()?;
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    Some((m.len(), t))
}

/// A file the result depends on (recorded with its size / mtime at first read).
pub fn read(p: impl AsRef<Path>) {
    with(|t| {
        if let Some(r) = t.rel(p.as_ref())
            && !t.reads.contains_key(&r)
            && !t.writes.contains(&r)
        {
            let st = stat(&t.root.join(&r));
            t.reads.insert(r, st);
        }
    })
}

/// A file the stage produced.
pub fn write(p: impl AsRef<Path>) {
    crate::guard::note(p.as_ref());
    with(|t| {
        if let Some(r) = t.rel(p.as_ref()) {
            t.deleted.remove(&r);
            if !t.writes.contains(&r) {
                t.writes.push(r);
            }
        }
    })
}

/// A file the stage removed (or renamed away).
pub fn delete(p: impl AsRef<Path>) {
    with(|t| {
        if let Some(r) = t.rel(p.as_ref()) {
            t.writes.retain(|w| w != &r);
            t.deleted.insert(r);
        }
    })
}

/// A rename: src deleted, dst written.
pub fn rename(src: impl AsRef<Path>, dst: impl AsRef<Path>) {
    delete(src);
    write(dst);
}

/// A folder whose listing the result depends on.
pub fn list(p: impl AsRef<Path>) {
    with(|t| {
        if let Some(r) = t.rel(p.as_ref()) {
            t.lists.insert(r);
        }
    })
}

/// A child process started by the stage.
pub fn proc(exe: impl AsRef<Path>) {
    with(|t| t.procs.push(exe.as_ref().to_string_lossy().into_owned()))
}

/// Write the trace (once, at the end of the process; later calls rewrite it with everything so far). A process that
/// recorded no I/O at all writes NO trace: foxbuild then treats the stage's trace as incomplete (it always re-runs)
/// instead of believing an uninstrumented command read nothing.
pub fn finish() {
    with(|t| {
        if t.reads.is_empty() && t.writes.is_empty() && t.lists.is_empty() && t.deleted.is_empty() {
            return;
        }
        let mut modules = vec![];
        if let Ok(exe) = std::env::current_exe()
            && let Some(r) = t.rel(&exe)
        {
            modules.push(r);
        }
        let reads: serde_json::Map<String, serde_json::Value> = t
            .reads
            .iter()
            .map(|(k, v)| (k.clone(), v.map(|(s, m)| serde_json::json!([s, m as u64])).unwrap_or(serde_json::Value::Null)))
            .collect();
        let end = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let doc = serde_json::json!({
            "pid": std::process::id(),
            "argv": std::env::args().collect::<Vec<_>>(),
            "exe": std::env::current_exe().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default(),
            "end": end,
            "reads": reads,
            "writes": t.writes,
            "deleted": t.deleted,
            "lists": t.lists,
            "procs": t.procs,
            "modules": modules,
        });
        let _ = std::fs::create_dir_all(&t.dir);
        let tmp = t.dir.join(format!("{}.json.tmp", std::process::id()));
        let res = std::fs::write(&tmp, serde_json::to_vec(&doc).unwrap_or_default())
            .and_then(|_| std::fs::rename(&tmp, t.dir.join(format!("{}.json", std::process::id()))));
        if let Err(e) = res {
            eprintln!("[foxbuild trace] could not write trace: {e}");
        }
    })
}

/// True when this process is traced (FOXBUILD_TRACE_DIR set).
pub fn active() -> bool {
    let mut on = false;
    with(|_| on = true);
    on
}

// ---- recording wrappers

pub fn fs_read(p: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
    read(p.as_ref());
    std::fs::read(p)
}

pub fn fs_read_to_string(p: impl AsRef<Path>) -> std::io::Result<String> {
    read(p.as_ref());
    std::fs::read_to_string(p)
}

pub fn open_read(p: impl AsRef<Path>) -> std::io::Result<std::fs::File> {
    read(p.as_ref());
    std::fs::File::open(p)
}

pub fn fs_write(p: impl AsRef<Path>, data: impl AsRef<[u8]>) -> std::io::Result<()> {
    crate::guard::check(p.as_ref())?;
    std::fs::write(p.as_ref(), data)?;
    write(p);
    Ok(())
}

pub fn create(p: impl AsRef<Path>) -> std::io::Result<std::fs::File> {
    crate::guard::check(p.as_ref())?;
    let f = std::fs::File::create(p.as_ref())?;
    write(p);
    Ok(f)
}

pub fn read_dir(p: impl AsRef<Path>) -> std::io::Result<std::fs::ReadDir> {
    list(p.as_ref());
    std::fs::read_dir(p)
}

#[cfg(test)]
mod tests {
    #[test]
    fn writes_python_format() {
        let dir = std::env::temp_dir().join(format!("foxtrace_test_{}", std::process::id()));
        let root = std::env::current_dir().unwrap();
        // single-threaded test binary section; nothing else reads the environment meanwhile
        unsafe {
            std::env::set_var("FOXBUILD_TRACE_DIR", &dir);
            std::env::set_var("FOXBUILD_ROOT", &root);
        }
        super::read(root.join("Cargo.toml"));
        super::write(root.join("x").join("..").join("out.bin"));
        // an absolute path outside the repository on either platform
        super::read(if cfg!(windows) { "C:/definitely/outside/repo.txt" } else { "/definitely/outside/repo.txt" });
        super::list(root.join("src"));
        super::finish();
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join(format!("{}.json", std::process::id()))).unwrap()).unwrap();
        assert!(doc["reads"]["Cargo.toml"].is_array());
        assert_eq!(doc["reads"].as_object().unwrap().len(), 1);
        assert_eq!(doc["writes"][0], "out.bin");
        assert_eq!(doc["lists"][0], "src");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
