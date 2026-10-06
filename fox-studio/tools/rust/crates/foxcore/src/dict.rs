//! A local file-name dictionary generated from the user's own game (replaces the downloaded community dictionaries).
//! Owner: tooling. Used by `fox dict build` and Fox Studio's setup wizard.
//!
//! 1. Read every QAR archive's entry hashes (the names we want).
//! 2. Harvest candidate strings from inside the game data:
//!    - fpk/fpkd entry paths and pack references;
//!    - printable runs in every decoded entry and in every pack's inner files (fox2 string tables, Lua, XML,
//!      foxfs.dat, binary formats that embed paths);
//!    - optionally the game executable.
//! 3. Derive variants (with / without "/Assets/", extension stripped at the first '.').
//! 4. Keep only candidates whose path hash matches an entry hash in the user's archives.
//!
//! Nothing harvested ships with the tool; the dictionary is built on the user's machine.
use crate::{fpk, qar};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// the path part of a QAR entry hash (the upper bits carry the extension)
pub const PATH_MASK: u64 = 0x3_FFFF_FFFF_FFFF;

fn is_path_char(c: u8) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            b'/' | b'_' | b'-' | b'.' | b' ' | b'(' | b')' | b'+' | b'#' | b'@' | b'&' | b'\''
        )
}

/// printable runs that look like paths (contain '/', length >= 4)
fn harvest(data: &[u8], out: &mut HashSet<String>) {
    let mut i = 0;
    while i < data.len() {
        if is_path_char(data[i]) {
            let s = i;
            while i < data.len() && is_path_char(data[i]) {
                i += 1;
            }
            let run = &data[s..i];
            if run.len() >= 4 && run.contains(&b'/')
                && let Ok(text) = std::str::from_utf8(run)
            {
                // Split spaces and quotes; retain tokens that contain a path separator.
                for token in text.split([' ', '(', ')', '\'']) {
                    if token.len() >= 4 && token.contains('/') {
                        out.insert(token.to_string());
                    }
                }
            }
        } else {
            i += 1;
        }
    }
}

/// candidate stems (no extension) for a harvested token
fn variants(tok: &str, out: &mut Vec<String>) {
    let t = tok.trim_matches(|c: char| c == '.' || c == '-');
    let stem = t.split('.').next().unwrap_or(t);
    if stem.len() < 2 {
        return;
    }
    let mut push = |s: String| {
        if !s.is_empty() {
            out.push(s)
        }
    };
    push(stem.to_string());
    if !stem.starts_with('/') {
        push(format!("/{stem}"));
    }
    if let Some(rest) = stem.strip_prefix("/Assets/") {
        push(format!("/{rest}"));
    } else if stem.starts_with("/tpp/") || stem.starts_with("tpp/") {
        push(format!("/Assets/{}", stem.trim_start_matches('/')));
    }
}

#[derive(Debug, Clone, Default)]
pub struct BuildStats {
    /// entry hashes (path part) in the archives
    pub wanted: usize,
    /// of those, resolved by a name
    pub resolved: usize,
    pub tokens: usize,
    pub scanned_bytes: u64,
    pub seconds: f64,
    /// archives that could not be read (path, why)
    pub skipped: Vec<(PathBuf, String)>,
    /// the entry hashes (path part) of the archives, for comparisons
    pub wanted_hashes: HashSet<u64>,
    pub resolved_hashes: HashSet<u64>,
}

/// Build the dictionary. `progress(fraction 0..1, text)` is called per archive and every few hundred entries;
/// returning false cancels (Err("cancelled")).
pub fn build_with_context(
    context: &qar::Context,
    archives: &[PathBuf],
    exe: Option<&Path>,
    progress: &mut dyn FnMut(f32, &str) -> bool,
) -> Result<(BTreeSet<String>, BuildStats), String> {
    let t0 = std::time::Instant::now();
    let mut st = BuildStats::default();
    let mut wanted: HashMap<u64, u64> = HashMap::new(); // path hash -> one full entry hash
    let mut tokens: HashSet<String> = HashSet::new();
    let total: u64 = archives
        .iter()
        .map(|a| std::fs::metadata(a).map(|m| m.len()).unwrap_or(0))
        .sum::<u64>()
        .max(1);
    let mut done_bytes = 0u64;
    for a in archives {
        let size = std::fs::metadata(a).map(|m| m.len()).unwrap_or(0);
        let f = match File::open(a) {
            Ok(f) => f,
            Err(e) => {
                st.skipped.push((a.clone(), e.to_string()));
                done_bytes += size;
                continue;
            }
        };
        let mut r = BufReader::with_capacity(1 << 22, f);
        let idx = match context.read_index(&mut r) {
            Ok(i) => i,
            Err(e) => {
                st.skipped.push((a.clone(), e.to_string()));
                done_bytes += size;
                continue;
            }
        };
        for e in &idx.entries {
            wanted.insert(e.hash & PATH_MASK, e.hash);
        }
        // harvest: entries in file order (sequential reads)
        let mut order: Vec<&qar::Entry> = idx.entries.iter().collect();
        order.sort_by_key(|e| e.offset);
        let name = a.display().to_string();
        for (k, e) in order.iter().enumerate() {
            if k % 256 == 0 {
                let f = (done_bytes as f64 + e.offset as f64).min(total as f64) / total as f64;
                if !progress(f as f32, &format!("{name}: {k}/{} entries", order.len())) {
                    return Err("cancelled".into());
                }
            }
            let mut b = vec![0u8; e.stored as usize];
            if r.seek(SeekFrom::Start(e.offset + 32)).is_err() || r.read_exact(&mut b).is_err() {
                continue;
            }
            let Ok(c) = context.decode(e, &b) else {
                continue;
            };
            st.scanned_bytes += c.len() as u64;
            if c.starts_with(b"foxfpk") && let Ok(package) = fpk::read(&c) {
                for entry in &package.entries {
                    tokens.insert(entry.path.clone());
                    let (offset, size) = (entry.offset as usize, entry.size as usize);
                    if offset + size <= c.len() {
                        harvest(&c[offset..offset + size], &mut tokens);
                    }
                }
                for reference in &package.references {
                    tokens.insert(reference.clone());
                }
                continue;
            }
            harvest(&c, &mut tokens);
        }
        done_bytes += size;
        if !progress(
            (done_bytes as f64 / total as f64) as f32,
            &format!(
                "{name}: {} entries; {} tokens so far ({:.0} s)",
                idx.entries.len(),
                tokens.len(),
                t0.elapsed().as_secs_f64()
            ),
        ) {
            return Err("cancelled".into());
        }
    }
    if let Some(path) = exe && let Ok(bytes) = std::fs::read(path) {
        harvest(&bytes, &mut tokens);
    }
    // variants, hash, keep matches
    let mut found: BTreeSet<String> = BTreeSet::new();
    let mut v = Vec::new();
    for t in &tokens {
        v.clear();
        variants(t, &mut v);
        for s in &v {
            let h = qar::path_hash(s) & PATH_MASK;
            if wanted.contains_key(&h) && st.resolved_hashes.insert(h) {
                found.insert(s.clone());
            }
        }
    }
    st.wanted = wanted.len();
    st.resolved = st.resolved_hashes.len();
    st.tokens = tokens.len();
    st.wanted_hashes = wanted.keys().copied().collect();
    st.seconds = t0.elapsed().as_secs_f64();
    Ok((found, st))
}

/// the dictionary file text: one name per line, sorted
pub fn to_text(names: &BTreeSet<String>) -> String {
    names.iter().map(|s| format!("{s}\n")).collect()
}

/// path hash (path part) -> name, from a dictionary file (ours or a community one: extensions are stripped)
pub fn load(text: &str) -> HashMap<u64, String> {
    let mut m = HashMap::new();
    for l in text.lines() {
        let name = l.trim();
        if name.is_empty() {
            continue;
        }
        let stem = name.split('.').next().unwrap_or("");
        m.entry(qar::path_hash(stem) & PATH_MASK)
            .or_insert_with(|| name.to_string());
    }
    m
}

/// the name of a QAR entry hash, if the dictionary has it
pub fn name_of(hash: u64, dict: &HashMap<u64, String>) -> Option<&str> {
    dict.get(&(hash & PATH_MASK)).map(|s| s.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_and_load() {
        let mut v = vec![];
        variants("/Assets/tpp/pack/x.fpk", &mut v);
        assert_eq!(v, vec!["/Assets/tpp/pack/x", "/tpp/pack/x"]);
        let d = load("/Assets/tpp/pack/x.fpk\n");
        let h = qar::path_hash("/Assets/tpp/pack/x");
        assert_eq!(
            name_of(h | (7u64 << 52), &d),
            Some("/Assets/tpp/pack/x.fpk")
        );
    }
}

#[cfg(feature = "internal-game-data")]
pub fn build(
    archives: &[PathBuf],
    exe: Option<&Path>,
    progress: &mut dyn FnMut(f32, &str) -> bool,
) -> Result<(BTreeSet<String>, BuildStats), String> {
    build_with_context(&qar::Context::internal(), archives, exe, progress)
}
