//! Library API for front ends (Fox Studio): the data behind `--status` and `--graph`, as values.
//!
//! Read-only build state: no stage starts and no state.json writes. The API
//! shares the CLI's graph, learned-edge, fingerprint and dirty-check logic.
//! Optional hash-cache saving acquires the OS lease before loading the cache
//! and retains it through publication. A contended reader computes status
//! without publishing its snapshot.
use crate::PreparedGraph;
use crate::bundle::BundledTools;
use crate::cancel::Cancellation;
use crate::config::Graph;
use crate::hashing::{self, HashCache};
use crate::state::State;
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// One stage as `--status` and `--list` describe it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct StageStatus {
    pub name: String,
    pub owner: String,
    pub cmd: Vec<String>,
    /// declared `after` + `rerun_with`, in graph-file order
    pub declared: Vec<String>,
    /// every dependency the scheduler uses: declared + learned from traces
    pub deps: Vec<String>,
    /// on by default (false: on demand only)
    pub default: bool,
    pub gpu: bool,
    pub verify: bool,
    pub deterministic: bool,
    pub mem_gb: f64,
    pub locks: Vec<String>,
    /// the "finished" stamp of the last successful run (YYYY-MM-DD HH:MM:SS)
    pub last_run: Option<String>,
    /// whether the last run succeeded (None: never ran)
    pub last_ok: Option<bool>,
    pub seconds: f64,
    pub peak_mb: f64,
    pub inputs: usize,
    pub outputs: usize,
    /// None: up to date; Some(reason): it would run (the `--status` / `--dry-run` reason)
    pub dirty: Option<String>,
    /// its last run used a Python fallback for a Rust-routed step ("what: reason"); empty = none
    pub fallbacks: Vec<String>,
    /// it routes work into the Rust tools (rust_tools / a proven cmd_rs)
    pub rust: bool,
}

impl StageStatus {
    /// the state column of `--status` (and Fox Studio): "up to date" or the reason it would run, plus a loud
    /// marker when the last run fell back to Python
    pub fn state_text(&self) -> String {
        let mut s = self.dirty.clone().unwrap_or_else(|| "up to date".into());
        if !self.fallbacks.is_empty() {
            s.push_str(&format!(
                "  [last run: PYTHON FALLBACK x{}]",
                self.fallbacks.len()
            ));
        }
        s
    }
}

/// Are the Rust tools (module + release binaries) built from the current sources? `python tools/rust/build.py
/// --check` (one implementation of the source stamp). Ok(true) fresh, Ok(false) stale, Err: cannot tell.
pub fn rust_tools_fresh(root: &Path, python: &str) -> Result<bool, String> {
    let b = root.join("tools").join("rust").join("build.py");
    if !b.is_file() {
        return Err(format!("{} not found", b.display()));
    }
    let mut c = std::process::Command::new(python);
    c.arg(&b)
        .arg("--check")
        .current_dir(root)
        .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = c
        .output()
        .map_err(|e| format!("{python} {}: {e}", b.display()))?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        x => Err(format!(
            "build.py --check: rc {x:?}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

/// the source stamp the release binaries were last built from (work/rust/target/release/fox-tools.stamp)
pub fn rust_tools_stamp(root: &Path) -> Option<String> {
    let p = root
        .join("work")
        .join("rust")
        .join("target")
        .join("release")
        .join("fox-tools.stamp");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(p).ok()?).ok()?;
    v["stamp"].as_str().map(|s| s.to_string())
}

#[derive(Debug, Clone, Serialize)]
pub struct PinnedStatus {
    pub path: String,
    pub owner: String,
    /// as `--status` prints it: a file hash, "<hash> (<n> files)" for a folder, or "missing"
    pub hash: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    pub config: PathBuf,
    pub root: PathBuf,
    /// <root>/<settings.log_dir> (build.log, logs/<stage>.log, state.json)
    pub log_dir: PathBuf,
    /// stages in the scheduler's topological order (the order `--status` prints)
    pub stages: Vec<StageStatus>,
    /// learned-edge notes (as `--graph` prints them)
    pub notes: Vec<String>,
    pub pinned: Vec<PinnedStatus>,
    /// pid of a foxbuild holding the build lock right now
    pub running_pid: Option<u32>,
    /// process names that make the build polite (settings.games)
    pub games: Vec<String>,
    /// with check_dirty and a stage that routes into the Rust tools: Ok(true) built from the current sources,
    /// Ok(false) stale (a build rebuilds them first), Err(why) cannot tell. None: not checked.
    pub rust_tools: Option<Result<bool, String>>,
}

#[derive(Debug, Clone)]
pub struct StatusOptions {
    /// the interpreter that replaces "python" in stage commands. Fingerprints use its canonical identity
    /// (python_identity: real path + version), so any spec naming the same interpreter agrees with `fox build`.
    pub python: String,
    /// compute up-to-date / dirty (hashes inputs, cached); false = structure and last-run data only (instant)
    pub check_dirty: bool,
    /// save the hash cache afterwards, as `--status` does (skipped while a build holds the lock)
    pub save_cache: bool,
}

impl Default for StatusOptions {
    fn default() -> Self {
        StatusOptions {
            python: default_python(),
            check_dirty: true,
            save_cache: false,
        }
    }
}

/// The interpreter `fox build` passes to foxbuild: FOX_PYTHON, else "python".
pub fn default_python() -> String {
    std::env::var("FOX_PYTHON").unwrap_or_else(|_| "python".into())
}

/// The pid in <log_dir>/foxbuild.lock when that process is alive (a build is running on this repo).
pub fn lock_holder(log_dir: &Path) -> Option<u32> {
    crate::lock::holder(log_dir)
}

/// Load the graph and the build state and describe every stage, like `fox build --status` (plus `--graph`'s
/// dependency lists). `config` may be relative to `root`.
pub fn status(root: &Path, config: &Path, opts: &StatusOptions) -> Result<StatusReport> {
    status_context(root, config, opts, None, &Cancellation::default())?
        .context("status cancelled without a cancellation marker")
}

/// Native packaged counterpart to status: no interpreter or source-snapshot
/// freshness probe. The explicit context must already match the trusted build.
pub fn status_with_bundle(
    root: &Path,
    config: &Path,
    opts: &StatusOptions,
    bundle: &BundledTools,
) -> Result<StatusReport> {
    status_context(root, config, opts, Some(bundle), &Cancellation::default())?
        .context("status cancelled without a cancellation marker")
}

/// Inspect a captured project without publishing its graph, facets or hash cache.
/// `config` is the intended report/publication origin and need not exist yet.
pub fn status_prepared(
    root: &Path,
    config: &Path,
    opts: &StatusOptions,
    bundle: Option<&BundledTools>,
    prepared: &PreparedGraph,
) -> Result<StatusReport> {
    status_context_prepared(
        root,
        config,
        opts,
        bundle,
        &Cancellation::default(),
        Some(prepared),
    )?
    .context("status cancelled without a cancellation marker")
}

pub(crate) fn status_context(
    root: &Path,
    config: &Path,
    opts: &StatusOptions,
    bundle: Option<&BundledTools>,
    cancel: &Cancellation,
) -> Result<Option<StatusReport>> {
    status_context_prepared(root, config, opts, bundle, cancel, None)
}

pub(crate) fn status_context_prepared(
    root: &Path,
    config: &Path,
    opts: &StatusOptions,
    bundle: Option<&BundledTools>,
    cancel: &Cancellation,
    prepared: Option<&PreparedGraph>,
) -> Result<Option<StatusReport>> {
    if cancel.requested()? {
        return Ok(None);
    }
    let config = if config.is_relative() {
        root.join(config)
    } else {
        config.to_path_buf()
    };
    let g = match prepared {
        Some(prepared) => {
            prepared.validate(root, &config)?;
            prepared.graph.clone()
        }
        None => Graph::load(&config)?,
    };
    if let Some(bundle) = bundle {
        bundle.verify_current()?;
        for stage in &g.stage {
            bundle.command_fingerprint(stage)?;
        }
    }
    let log_dir = root.join(&g.settings.log_dir);
    let state_path = log_dir.join("state.json");
    let cache_path = log_dir.join("hashcache.json");
    let write_cache = prepared.is_none() && opts.save_cache && opts.check_dirty;
    // Acquire before loading: a later owner must not publish an earlier
    // owner's cache snapshot. Contended status remains entirely read-only.
    let cache_lease = if write_cache {
        std::fs::create_dir_all(&log_dir)?;
        crate::lock::Lease::try_acquire(&log_dir)?
    } else {
        None
    };
    let st = State::load(&state_path);
    let mut cache = HashCache::load(&cache_path);
    if let Some(prepared) = prepared {
        cache.set_planned_inputs(&prepared.input_hashes);
    }
    let mut notes = Vec::new();
    let deps = crate::build_deps(&g, &st, &mut notes);
    let order =
        crate::graph::topo_order(g.stage.len(), &deps).context("stage graph has a cycle")?;
    let mut stages = Vec::with_capacity(order.len());
    let python_identity = if opts.check_dirty && bundle.is_none() {
        let Some(identity) = crate::interpreter::identity_cancellable(&opts.python, cancel)? else {
            return Ok(None);
        };
        identity
    } else {
        String::new()
    };
    for &i in &order {
        if cancel.requested()? {
            return Ok(None);
        }
        let s = &g.stage[i];
        let rec = st.stages.get(&s.name);
        let dirty = if opts.check_dirty {
            let fingerprint = match bundle {
                Some(bundle) => bundle.command_fingerprint(s)?,
                None => crate::cmd_fp(s, &python_identity),
            };
            crate::stage_dirty(s, &fingerprint, &st, root, &mut cache)
        } else {
            None
        };
        stages.push(StageStatus {
            name: s.name.clone(),
            owner: s.owner.clone(),
            cmd: s.run_cmd().to_vec(),
            declared: s.after.iter().chain(&s.rerun_with).cloned().collect(),
            deps: deps[i].iter().map(|&j| g.stage[j].name.clone()).collect(),
            default: s.default,
            gpu: s.gpu,
            verify: s.verify,
            deterministic: s.deterministic,
            mem_gb: s.mem_gb,
            locks: s.locks.clone(),
            last_run: rec
                .filter(|r| !r.finished.is_empty())
                .map(|r| r.finished.clone()),
            last_ok: rec.map(|r| r.ok),
            seconds: rec.map(|r| r.seconds).unwrap_or(0.0),
            peak_mb: rec.map(|r| r.peak_mb).unwrap_or(0.0),
            inputs: rec.map(|r| r.inputs.len()).unwrap_or(0),
            outputs: rec.map(|r| r.outputs.len()).unwrap_or(0),
            dirty,
            fallbacks: rec.map(|r| r.fallbacks.clone()).unwrap_or_default(),
            rust: s.needs_rust(),
        });
    }
    let mut pinned = Vec::new();
    if opts.check_dirty {
        for p in &g.pinned {
            pinned.push(PinnedStatus {
                path: p.path.clone(),
                owner: p.owner.clone(),
                hash: pinned_hash(root, &p.path, &mut cache),
            });
        }
    } else {
        for p in &g.pinned {
            pinned.push(PinnedStatus {
                path: p.path.clone(),
                owner: p.owner.clone(),
                hash: String::new(),
            });
        }
    }
    let running_pid = lock_holder(&log_dir);
    if write_cache {
        if cache_lease.is_some() && running_pid.is_none() {
            cache.save(&cache_path)?;
        } else if cache_lease.is_none() {
            notes.push("build lease is held; the hash cache was not saved".into());
        }
    }
    let rust_tools = if opts.check_dirty && stages.iter().any(|s| s.rust) {
        if bundle.is_some() {
            Some(Ok(true))
        } else {
            let Some(fresh) = rust_tools_fresh_cancellable(root, &opts.python, cancel)? else {
                return Ok(None);
            };
            Some(fresh)
        }
    } else {
        None
    };
    Ok(Some(StatusReport {
        config,
        root: root.to_path_buf(),
        log_dir,
        stages,
        notes,
        pinned,
        running_pid,
        games: g.settings.games.clone(),
        rust_tools,
    }))
}

pub(crate) fn rust_tools_fresh_cancellable(
    root: &Path,
    python: &str,
    cancel: &Cancellation,
) -> Result<Option<Result<bool, String>>> {
    let script = root.join("tools/rust/build.py");
    if !script.is_file() {
        return Ok(Some(Err(format!("{} not found", script.display()))));
    }
    let mut command = std::process::Command::new(python);
    command.arg(&script).arg("--check").current_dir(root);
    let output = match crate::process::capture(&mut command, cancel) {
        Ok(Some(output)) => output,
        Ok(None) => return Ok(None),
        Err(error) => {
            return Ok(Some(Err(format!(
                "{python} {}: {error:#}",
                script.display()
            ))));
        }
    };
    let result = match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        code => Err(format!(
            "build.py --check: rc {code:?}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    };
    Ok(Some(result))
}

/// a pinned path's hash, exactly as `--status` prints it
fn pinned_hash(root: &Path, path: &str, cache: &mut HashCache) -> String {
    if let Some(hash) = cache.planned_file(path) {
        return hash.to_owned();
    }
    let files = hashing::walk(root, path);
    if files.len() == 1 && hashing::key(&files[0]) == hashing::key(path) {
        cache.file(root, path)
    } else if files.is_empty() {
        hashing::MISSING.to_string()
    } else {
        let mut b = blake3::Hasher::new();
        for f in &files {
            b.update(f.as_bytes());
            b.update(cache.file(root, f).as_bytes());
        }
        format!("{} ({} files)", &b.finalize().to_hex()[..16], files.len())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod cache_review_tests {
    use super::*;
    use crate::bundle::{BuildIdentity, BundledTools};
    use std::ffi::CString;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    unsafe extern "C" {
        fn mkfifo(path: *const std::ffi::c_char, mode: u32) -> i32;
    }

    struct Fixture(std::path::PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn review_status_cache_load_during_owner_handoff_never_publishes_the_old_map() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let fixture = Fixture(std::env::temp_dir().join(format!(
            "foxbuild_status_cache_{}_{unique}",
            std::process::id()
        )));
        let root = &fixture.0;
        let log_dir = root.join("work/build");
        fs::create_dir_all(&log_dir).unwrap();
        fs::write(root.join("graph.toml"),
            "[settings]\nmem_reserve_gb = 0.0\n[[stage]]\nname = 'asset'\ncmd = ['fox', 'pack']\n[[pinned]]\npath = 'input.txt'\nowner = 'fixture'\n"
        ).unwrap();
        fs::write(
            root.join("input.txt"),
            "hashing this makes the loaded cache dirty",
        )
        .unwrap();
        fs::write(root.join("fox"), "authored native identity bytes").unwrap();
        let expected = BuildIdentity {
            package_version: env!("CARGO_PKG_VERSION").into(),
            build_id: "cache-handoff-fixture".into(),
        };
        let manifest = root.join("fox-tools.json");
        fs::write(&manifest, serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "package_version": expected.package_version, "build_id": expected.build_id,
            "tools": {"fox": {"path": "fox", "blake3": blake3::hash(&fs::read(root.join("fox")).unwrap()).to_hex().to_string()}}
        })).unwrap()).unwrap();
        let bundle = BundledTools::load(&manifest, &expected).unwrap();

        let cache = log_dir.join("hashcache.json");
        let name = CString::new(cache.as_os_str().as_bytes()).unwrap();
        // A FIFO makes the ownership/read handoff deterministic: the status
        // reader opens the old cache, but receives bytes only after the owner
        // publishes a newer cache and releases its lease.
        assert_eq!(
            unsafe { mkfifo(name.as_ptr(), 0o600) },
            0,
            "{}",
            std::io::Error::last_os_error()
        );
        let owner = crate::lock::Lease::try_acquire(&log_dir).unwrap().unwrap();
        let reader_root = root.clone();
        let task = std::thread::spawn(move || {
            status_with_bundle(
                &reader_root,
                &reader_root.join("graph.toml"),
                &StatusOptions {
                    python: "unavailable".into(),
                    check_dirty: true,
                    save_cache: true,
                },
                &bundle,
            )
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut writer = loop {
            // Linux O_NONBLOCK bounds setup if the reader unexpectedly fails.
            match OpenOptions::new()
                .write(true)
                .custom_flags(0o4000)
                .open(&cache)
            {
                Ok(file) => break file,
                Err(error) if error.raw_os_error() == Some(6) => {
                    assert!(
                        !task.is_finished(),
                        "status exited before opening the cache"
                    );
                    assert!(Instant::now() < deadline, "status did not open the cache");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("opening cache FIFO writer: {error}"),
            }
        };
        let published = b"{\"entries\":{\"fresh-owner\":[7,11,\"new cache hash\"]}}\n";
        let replacement = log_dir.join("owner-published.json");
        fs::write(&replacement, published).unwrap();
        fs::rename(&replacement, &cache).unwrap();
        drop(owner);
        writer
            .write_all(b"{\"entries\":{\"old-reader\":[1,1,\"old cache hash\"]}}")
            .unwrap();
        drop(writer);
        let report = task.join().unwrap().unwrap();
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.contains("lease is held"))
        );
        assert_eq!(
            fs::read(&cache).unwrap(),
            published,
            "a status reader published stale cache after the completing owner"
        );
        assert_eq!(report.pinned.len(), 1);
        assert!(!report.pinned[0].hash.is_empty());
    }
}
