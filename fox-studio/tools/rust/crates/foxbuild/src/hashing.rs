//! Content hashes with a (size, mtime) cache, path normalisation and the ignore / static matchers.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub const MISSING: &str = "missing";

/// repo-relative, forward slashes, as written in traces and the graph file
pub fn norm_rel(p: &str) -> String {
    let mut s = p.replace('\\', "/");
    while s.starts_with("./") {
        s = s[2..].to_string();
    }
    s.trim_end_matches('/').to_string()
}

/// case-insensitive key (Windows)
pub fn key(rel: &str) -> String {
    norm_rel(rel).to_lowercase()
}

/// ignore / static patterns: "dir/" (prefix), "**/name/" (any path segment), "**/*.ext" (suffix), "a/b" (prefix)
pub fn matches(rel_key: &str, pat: &str) -> bool {
    let pat = pat.to_lowercase();
    if let Some(rest) = pat.strip_prefix("**/") {
        if let Some(ext) = rest.strip_prefix('*') {
            return rel_key.ends_with(ext);
        }
        if let Some(seg) = rest.strip_suffix('/') {
            return rel_key.starts_with(&format!("{seg}/"))
                || rel_key.contains(&format!("/{seg}/"));
        }
        return rel_key == rest || rel_key.ends_with(&format!("/{rest}"));
    }
    if let Some(dir) = pat.strip_suffix('/') {
        return rel_key == dir || rel_key.starts_with(&format!("{dir}/"));
    }
    rel_key == pat || rel_key.starts_with(&format!("{pat}/"))
}

pub fn any_match(rel_key: &str, pats: &[String]) -> bool {
    pats.iter().any(|p| matches(rel_key, p))
}

#[derive(Serialize, Deserialize, Default)]
pub struct HashCache {
    /// key -> (size, mtime_ns, hash)
    entries: HashMap<String, (u64, u128, String)>,
    #[serde(skip)]
    dirty: bool,
    /// Inspection-only captured inputs; never serialized as filesystem metadata.
    #[serde(skip)]
    planned_inputs: HashMap<String, String>,
}

pub fn stat(path: &Path) -> Option<(u64, u128)> {
    let md = std::fs::metadata(path).ok()?;
    if !md.is_file() {
        return None;
    }
    let mt = md
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((md.len(), mt))
}

impl HashCache {
    pub fn load(path: &Path) -> HashCache {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if !self.dirty && path.exists() {
            return Ok(());
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// blake3 (first 128 bits, hex) of a repo file; MISSING when absent. Cached on (size, mtime).
    pub fn file(&mut self, root: &Path, rel: &str) -> String {
        if let Some(hash) = self.planned_file(rel) {
            return hash.to_owned();
        }
        self.file_current(root, rel)
    }

    pub(crate) fn set_planned_inputs(&mut self, inputs: &BTreeMap<String, String>) {
        self.planned_inputs = inputs
            .iter()
            .map(|(path, hash)| (key(path), hash.clone()))
            .collect();
    }

    pub(crate) fn planned_file(&self, rel: &str) -> Option<&str> {
        self.planned_inputs.get(&key(rel)).map(String::as_str)
    }

    /// Current filesystem bytes, regardless of an inspection's planned inputs.
    pub(crate) fn file_current(&mut self, root: &Path, rel: &str) -> String {
        let p = root.join(norm_rel(rel));
        let Some((size, mt)) = stat(&p) else {
            return MISSING.into();
        };
        let k = key(rel);
        if let Some((s, m, h)) = self.entries.get(&k)
            && *s == size
            && *m == mt
        {
            return h.clone();
        }
        let h = match hash_path(&p) {
            Some(h) => h,
            None => return MISSING.into(),
        };
        self.entries.insert(k, (size, mt, h.clone()));
        self.dirty = true;
        h
    }
}

pub fn hash_path(p: &Path) -> Option<String> {
    let mut f = std::fs::File::open(p).ok()?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Some(h.finalize().to_hex()[..32].to_string())
}

/// hash of a directory listing (sorted entry names, '/' marks folders); MISSING when absent
pub fn list_hash(root: &Path, rel: &str) -> String {
    let p = root.join(norm_rel(rel));
    let Ok(rd) = std::fs::read_dir(&p) else {
        return MISSING.into();
    };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .map(|e| {
            let mut n = e.file_name().to_string_lossy().to_lowercase();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                n.push('/');
            }
            n
        })
        .collect();
    names.sort();
    let mut h = blake3::Hasher::new();
    for n in names {
        h.update(n.as_bytes());
        h.update(b"\0");
    }
    h.finalize().to_hex()[..32].to_string()
}

/// every file under a repo path (file or folder), repo-relative
pub fn walk(root: &Path, rel: &str) -> Vec<String> {
    let base = root.join(norm_rel(rel));
    let mut out = Vec::new();
    if base.is_file() {
        out.push(norm_rel(rel));
        return out;
    }
    let mut stack: Vec<PathBuf> = vec![base];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(p),
                Ok(t) if t.is_file() => {
                    if let Ok(r) = p.strip_prefix(root) {
                        out.push(norm_rel(&r.to_string_lossy()));
                    }
                }
                _ => {}
            }
        }
    }
    out.sort();
    out
}
