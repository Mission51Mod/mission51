//! Build state (work/build/state.json): what every stage was last built from, and the traces it is made of.
use crate::hashing::{self, HashCache, MISSING};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct StageRecord {
    pub ok: bool,
    pub trace_complete: bool,
    pub cmd_fp: String,
    /// repo-relative path -> content hash (files read; not written by the stage itself)
    pub inputs: BTreeMap<String, String>,
    /// folders listed -> listing hash
    pub lists: BTreeMap<String, String>,
    /// repo source modules imported -> content hash
    pub code: BTreeMap<String, String>,
    /// files written / owned -> content hash
    pub outputs: BTreeMap<String, String>,
    /// static (vanilla / third-party) files read: count only
    pub static_reads: usize,
    pub procs: Vec<String>,
    pub seconds: f64,
    pub peak_mb: f64,
    pub finished: String,
    /// inputs whose size/mtime changed while the stage ran (another process wrote them): forces a re-run
    pub changed_during_run: Vec<String>,
    /// soft inputs read (recorded, never triggering)
    #[serde(default)]
    pub soft: Vec<String>,
    /// rerun_with stage -> the run of it this record was built after ("finished" stamp)
    #[serde(default)]
    pub with_runs: BTreeMap<String, String>,
    /// Rust-routed steps that ran their Python reference instead (foxrs.report_fallback): "what: reason"
    #[serde(default)]
    pub fallbacks: Vec<String>,
    /// the Rust tools build (fox-tools.stamp) this run had (FOXBUILD_RUST_STAMP); None = no stamp known
    #[serde(default)]
    pub rust_stamp: Option<String>,
}

/// the stage's Python-fallback records (traces/<stage>/*.fallback, JSON lines from foxrs.report_fallback)
pub fn fallbacks(trace_dir: &Path) -> Vec<String> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(trace_dir) else {
        return out;
    };
    let mut files: Vec<_> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("fallback"))
        .collect();
    files.sort();
    for p in files {
        for l in std::fs::read_to_string(&p).unwrap_or_default().lines() {
            match serde_json::from_str::<serde_json::Value>(l) {
                Ok(v) => out.push(format!(
                    "{}: {}",
                    v["what"].as_str().unwrap_or("?"),
                    v["reason"].as_str().unwrap_or("?")
                )),
                Err(_) if !l.trim().is_empty() => out.push(l.trim().to_string()),
                Err(_) => {}
            }
        }
    }
    out
}

#[derive(Serialize, Deserialize, Default)]
pub struct State {
    pub stages: BTreeMap<String, StageRecord>,
}

impl State {
    pub fn load(path: &Path) -> State {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// One process's trace (tools/build/trace_site/sitecustomize.py)
#[derive(Deserialize, Default)]
struct Trace {
    pid: u32,
    #[serde(default)]
    exe: String,
    #[serde(default)]
    reads: BTreeMap<String, Option<(u64, u128)>>,
    #[serde(default)]
    writes: Vec<String>,
    #[serde(default)]
    deleted: Vec<String>,
    #[serde(default)]
    lists: Vec<String>,
    #[serde(default)]
    procs: Vec<String>,
    #[serde(default)]
    modules: Vec<String>,
}

pub struct Collected {
    pub record: StageRecord,
    pub notes: Vec<String>,
}

/// Merge the traces of a finished stage into a record (hashes taken now, after the run).
#[allow(clippy::too_many_arguments)]
pub fn collect(
    root: &Path,
    trace_dir: &Path,
    root_pid: u32,
    declared_outputs: &[String],
    ignore: &[String],
    static_roots: &[String],
    soft: &[String],
    cache: &mut HashCache,
) -> Collected {
    let mut notes = Vec::new();
    let mut reads: BTreeMap<String, (String, Option<(u64, u128)>)> = BTreeMap::new(); // key -> (rel, stat)
    let mut writes: BTreeMap<String, String> = BTreeMap::new();
    let mut deleted: BTreeSet<String> = BTreeSet::new();
    let mut lists: BTreeMap<String, String> = BTreeMap::new();
    let mut modules: BTreeMap<String, String> = BTreeMap::new();
    let mut procs: BTreeSet<String> = BTreeSet::new();
    let mut have_root = false;
    let mut n = 0;
    let mut fox_traces = 0;
    if let Ok(rd) = std::fs::read_dir(trace_dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Ok(b) = std::fs::read(&p) else { continue };
            let Ok(t) = serde_json::from_slice::<Trace>(&b) else {
                notes.push(format!("unreadable trace {}", p.display()));
                continue;
            };
            n += 1;
            have_root |= t.pid == root_pid;
            if is_fox_exe(&t.exe) {
                fox_traces += 1;
            }
            for (r, st) in t.reads {
                reads
                    .entry(hashing::key(&r))
                    .or_insert((hashing::norm_rel(&r), st));
            }
            for w in t.writes {
                writes.insert(hashing::key(&w), hashing::norm_rel(&w));
            }
            for d in t.deleted {
                deleted.insert(hashing::key(&d));
            }
            for l in t.lists {
                lists.insert(hashing::key(&l), hashing::norm_rel(&l));
            }
            for m in t.modules {
                modules.insert(hashing::key(&m), hashing::norm_rel(&m));
            }
            for p in t.procs {
                procs.insert(p);
            }
        }
    }
    // Rust pipeline ports (fox.exe nav|place|..., fox-place.exe) report their own I/O (foxpipe::trace). A run of
    // one that left no trace read files nobody recorded: the stage cannot be trusted as up to date.
    let fox_runs = procs.iter().filter(|p| is_fox_exe(p)).count();
    let mut rec = StageRecord {
        trace_complete: have_root && fox_traces >= fox_runs,
        ..Default::default()
    };
    if !have_root {
        notes.push(format!("no trace from the stage's own process ({n} child traces): it will not be skipped next time"));
    } else if fox_traces < fox_runs {
        notes.push(format!("{fox_runs} Rust pipeline process(es) ran but only {fox_traces} left an I/O trace                             (foxpipe::trace): it will not be skipped next time"));
    }
    // outputs: traced writes (still present) + every file under the declared outputs
    let mut outs: BTreeMap<String, String> = BTreeMap::new();
    for (k, r) in &writes {
        if !deleted.contains(k) && !hashing::any_match(k, ignore) {
            outs.insert(k.clone(), r.clone());
        }
    }
    for d in declared_outputs {
        for f in hashing::walk(root, d) {
            let k = hashing::key(&f);
            if !hashing::any_match(&k, ignore) {
                outs.insert(k, f);
            }
        }
    }
    for r in outs.values() {
        let h = cache.file(root, r);
        if h != MISSING {
            rec.outputs.insert(r.clone(), h);
        }
    }
    // inputs: reads that the stage did not write itself
    for (k, (r, st)) in &reads {
        if outs.contains_key(k) || writes.contains_key(k) || hashing::any_match(k, ignore) {
            continue;
        }
        if hashing::any_match(k, static_roots) {
            rec.static_reads += 1;
            continue;
        }
        if hashing::any_match(k, soft) {
            rec.soft.push(r.clone());
            continue;
        }
        let now = hashing::stat(&root.join(r));
        let h = cache.file(root, r);
        if st.is_some() && now.is_some() && *st != now {
            rec.changed_during_run.push(r.clone());
        }
        rec.inputs.insert(r.clone(), h);
    }
    for (k, r) in &lists {
        if hashing::any_match(k, ignore)
            || hashing::any_match(k, static_roots)
            || hashing::any_match(k, soft)
        {
            continue;
        }
        rec.lists.insert(r.clone(), hashing::list_hash(root, r));
    }
    for (k, r) in &modules {
        if hashing::any_match(k, ignore) {
            continue;
        }
        rec.code.insert(r.clone(), cache.file(root, r));
    }
    rec.procs = procs.into_iter().collect();
    if !rec.changed_during_run.is_empty() {
        notes.push(format!(
            "{} input(s) changed while the stage ran (another process wrote them), e.g. {}: it re-runs next time",
            rec.changed_during_run.len(),
            rec.changed_during_run[0]
        ));
    }
    Collected { record: rec, notes }
}

/// fox.exe / fox-place.exe / fox-*.exe: a Rust pipeline process expected to write its own trace
fn is_fox_exe(p: &str) -> bool {
    let b = p.rsplit(['/', '\\']).next().unwrap_or(p).to_lowercase();
    b == "fox.exe" || b == "fox" || (b.starts_with("fox-") && b.ends_with(".exe"))
}

/// Why a stage must run, or None when it is up to date.
pub fn dirty_reason(
    rec: Option<&StageRecord>,
    cmd_fp: &str,
    root: &Path,
    cache: &mut HashCache,
) -> Option<String> {
    let Some(r) = rec else {
        return Some("never built".into());
    };
    if !r.ok {
        return Some("last run failed".into());
    }
    if !r.trace_complete {
        return Some("no complete I/O trace from the last run".into());
    }
    if r.cmd_fp != cmd_fp {
        return Some("command changed".into());
    }
    if !r.changed_during_run.is_empty() {
        return Some(format!(
            "input changed during the last run: {}",
            r.changed_during_run[0]
        ));
    }
    for (p, h) in &r.code {
        if &cache.file_current(root, p) != h {
            return Some(format!("code changed: {p}"));
        }
    }
    let mut changed: Vec<&String> = Vec::new();
    for (p, h) in &r.inputs {
        if &cache.file(root, p) != h {
            changed.push(p);
        }
    }
    if !changed.is_empty() {
        return Some(if changed.len() == 1 {
            format!("input changed: {}", changed[0])
        } else {
            format!("{} inputs changed: {}, ...", changed.len(), changed[0])
        });
    }
    for (p, h) in &r.lists {
        if &hashing::list_hash(root, p) != h {
            return Some(format!("folder contents changed: {p}"));
        }
    }
    for p in r.outputs.keys() {
        if hashing::stat(&root.join(p)).is_none() {
            return Some(format!("output missing: {p}"));
        }
    }
    None
}

#[cfg(test)]
mod fallback_tests {
    #[test]
    fn fallback_records_are_read() {
        let d = std::env::temp_dir().join(format!("foxbuild_fb_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("fallback-12.fallback"),
            "{\"pid\": 12, \"what\": \"place_rs walkcheck\", \"reason\": \"stale\"}

plain line
",
        )
        .unwrap();
        std::fs::write(d.join("12.json"), "{}").unwrap(); // a trace: not a fallback
        assert_eq!(
            super::fallbacks(&d),
            vec![
                "place_rs walkcheck: stale".to_string(),
                "plain line".to_string()
            ]
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
