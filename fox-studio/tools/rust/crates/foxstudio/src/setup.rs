//! First-run setup (PLAN.md §4.4), the logic behind the Setup tab: check a game folder, survey its archives
//! (tables only), unpack / index them into a cache folder the user picked, build a local name dictionary from the
//! user's own archives (foxcore::dict, the library behind `fox dict build`), and check the prerequisites.
//!
//! The game folder is only ever READ here. Unpacking refuses a cache inside the game folder (or the other way
//! round); the only writes into a game folder anywhere in Fox Studio are foxinstall's mod operations.
use crate::steam;
use foxcore::{qar, runtime_data::RuntimeProfile};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// What a game folder holds (sizes only; nothing is opened for writing).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GameCheck {
    pub dir: PathBuf,
    pub exe: bool,
    pub master00: bool,
    /// every .dat under master/ (and master/0/): (path, bytes)
    pub archives: Vec<(PathBuf, u64)>,
    pub foxinstall_manifest: bool,
    pub snakebite: bool,
    pub problems: Vec<String>,
}

impl GameCheck {
    pub fn ok(&self) -> bool {
        self.exe && self.master00
    }
    pub fn total_bytes(&self) -> u64 {
        self.archives.iter().map(|a| a.1).sum()
    }
}

pub fn check_game(dir: &Path) -> GameCheck {
    let mut g = GameCheck {
        dir: dir.to_path_buf(),
        ..Default::default()
    };
    if !dir.is_dir() {
        g.problems.push("the folder does not exist".into());
        return g;
    }
    g.exe = dir.join(steam::GAME_EXE).is_file();
    g.master00 = dir.join("master").join("0").join("00.dat").is_file();
    if !g.exe {
        g.problems.push(format!("no {} here", steam::GAME_EXE));
    }
    if !g.master00 {
        g.problems.push("no master/0/00.dat here".into());
    }
    for sub in [dir.join("master"), dir.join("master").join("0")] {
        if let Ok(rd) = std::fs::read_dir(&sub) {
            let mut v: Vec<(PathBuf, u64)> = rd
                .flatten()
                .filter(|e| {
                    e.path()
                        .extension()
                        .is_some_and(|x| x.eq_ignore_ascii_case("dat"))
                })
                .filter_map(|e| {
                    e.metadata()
                        .ok()
                        .filter(|m| m.is_file())
                        .map(|m| (e.path(), m.len()))
                })
                .collect();
            v.sort();
            g.archives.extend(v);
        }
    }
    g.foxinstall_manifest = dir.join("foxinstall").join("manifest.json").is_file();
    g.snakebite = dir.join("snakebite.xml").is_file();
    g
}

/// short archive id under the cache: master/chunk0.dat -> "chunk0", master/0/00.dat -> "0_00"
pub fn archive_id(game: &Path, dat: &Path) -> String {
    let master = game.join("master");
    let rel = dat.strip_prefix(&master).unwrap_or(dat);
    let s = rel
        .with_extension("")
        .to_string_lossy()
        .replace(['\\', '/'], "_");
    s.trim_start_matches('_').to_string()
}

/// The table of one archive: entry count and content bytes (read from the table; no data read).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ArchiveSurvey {
    pub path: PathBuf,
    pub id: String,
    pub bytes: u64,
    pub entries: usize,
    pub content_bytes: u64,
    pub error: Option<String>,
}

pub fn survey_with_context(context: &qar::Context, game: &Path, dat: &Path) -> ArchiveSurvey {
    let mut s = ArchiveSurvey {
        path: dat.to_path_buf(),
        id: archive_id(game, dat),
        ..Default::default()
    };
    s.bytes = std::fs::metadata(dat).map(|m| m.len()).unwrap_or(0);
    match File::open(dat)
        .map_err(|e| e.to_string())
        .and_then(|f| context.read_index(&mut BufReader::with_capacity(1 << 16, f)))
    {
        Ok(idx) => {
            s.entries = idx.entries.len();
            s.content_bytes = idx.entries.iter().map(|e| e.uncompressed as u64).sum();
        }
        Err(e) => s.error = Some(e),
    }
    s
}

pub use foxcore::dict::PATH_MASK;

/// A name dictionary (one game path per line, ours or a community one), keyed by the path part of the hash.
pub fn load_dict(text: &str) -> HashMap<u64, String> {
    foxcore::dict::load(text)
}

/// (relative output path, named by the dictionary)
pub fn entry_name(hash: u64, dict: &HashMap<u64, String>) -> (String, bool) {
    let ph = hash & PATH_MASK;
    let ext = qar::ext_of(hash).unwrap_or("_unknown");
    match foxcore::dict::name_of(hash, dict) {
        // the dictionary may carry an extension: the entry's own type wins
        Some(s) => (
            format!("{}.{ext}", safe_rel(s.split('.').next().unwrap_or(s))),
            true,
        ),
        None => (format!("_unnamed/{ph:013x}.{ext}"), false),
    }
}

/// a dictionary path made safe to join under the cache: no root, no "..", no characters Windows refuses
pub fn safe_rel(p: &str) -> String {
    p.split(['/', '\\'])
        .filter(|c| !c.is_empty() && *c != "." && *c != "..")
        .map(|c| {
            c.chars()
                .map(|ch| {
                    if matches!(ch, ':' | '*' | '?' | '"' | '<' | '>' | '|') || ch.is_control() {
                        '_'
                    } else {
                        ch
                    }
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// The cache must be a separate place: never the game folder, inside it, or holding it.
pub fn check_cache(cache: &Path, game: Option<&Path>) -> Result<(), String> {
    if cache.as_os_str().is_empty() {
        return Err("choose a cache folder".into());
    }
    if !cache.is_absolute() {
        return Err("the cache folder must be a full path".into());
    }
    let cache = resolved_path(cache)?;
    if let Some(g) = game {
        let g = resolved_path(g)?;
        if steam::is_within(&cache, &g) {
            return Err("the cache folder is inside the game folder: choose a folder elsewhere (the game folder is never written)".into());
        }
        if steam::is_within(&g, &cache) {
            return Err(
                "the cache folder contains the game folder: choose a separate folder".into(),
            );
        }
    }
    Ok(())
}

/// Resolve existing filesystem links and normalize the missing suffix without creating anything.
fn resolved_path(path: &Path) -> Result<PathBuf, String> {
    use std::path::Component;
    let mut existing = std::path::absolute(path).map_err(|error| error.to_string())?;
    let mut suffix = Vec::new();
    loop {
        match std::fs::canonicalize(&existing) {
            Ok(mut resolved) => {
                for part in suffix.iter().rev() {
                    if part == std::ffi::OsStr::new("..") {
                        resolved.pop();
                    } else if part != std::ffi::OsStr::new(".") {
                        resolved.push(part);
                    }
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let part = match existing.components().next_back() {
                    Some(Component::Normal(part)) => part.to_os_string(),
                    Some(Component::ParentDir) => "..".into(),
                    Some(Component::CurDir) => ".".into(),
                    _ => return Err(format!("{}: {error}", path.display())),
                };
                suffix.push(part);
                if !existing.pop() {
                    return Err(format!("{}: {error}", path.display()));
                }
            }
            Err(error) => return Err(format!("{}: {error}", path.display())),
        }
    }
}

/// A profile always belongs to the explicitly selected installation.
pub fn load_profile(game: &Path, path: &Path) -> Result<RuntimeProfile, String> {
    check_cache(path, Some(game))?;
    let profile = RuntimeProfile::load(path).map_err(|error| error.to_string())?;
    profile
        .validate_for_game(game)
        .map_err(|error| error.to_string())?;
    Ok(profile)
}

/// Learn local archive facts in Rust and atomically save them outside the game folder.
pub fn prepare_profile(
    game: &Path,
    path: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32, &str),
) -> Result<RuntimeProfile, String> {
    check_cache(path, Some(game))?;
    let profile = RuntimeProfile::learn(game, &mut |fraction, phase| {
        progress(fraction, phase);
        !cancel.load(Ordering::Relaxed)
    })
    .map_err(|error| error.to_string())?;
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    profile.save(path).map_err(|error| error.to_string())?;
    Ok(profile)
}

fn cache_output(cache: &Path, output: &Path) -> Result<(), String> {
    if !steam::is_within(&resolved_path(output)?, &resolved_path(cache)?) {
        return Err(format!(
            "{} leaves the selected cache folder",
            output.display()
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UnpackReport {
    pub archives: usize,
    pub written: usize,
    pub skipped: usize,
    pub bytes_written: u64,
    pub named: usize,
    pub unnamed: usize,
    pub errors: Vec<String>,
    pub cancelled: bool,
}

#[derive(Serialize)]
struct IndexEntry<'a> {
    hash: String,
    name: &'a str,
    size: u64,
}

/// Unpack `archives` into `cache/<archive id>/...` and write `cache/<archive id>.index.json`. Files already there
/// with the right size are kept (a cancelled unpack resumes). `progress(fraction, text)`.
pub fn unpack_with_context(
    context: &qar::Context,
    game: &Path,
    archives: &[PathBuf],
    cache: &Path,
    dict: &HashMap<u64, String>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32, String),
) -> Result<UnpackReport, String> {
    check_cache(cache, Some(game))?;
    for a in archives {
        if !steam::is_within(a, game) {
            return Err(format!("{} is not in the game folder", a.display()));
        }
    }
    std::fs::create_dir_all(cache).map_err(|e| format!("{}: {e}", cache.display()))?;
    let total: u64 = archives
        .iter()
        .map(|a| std::fs::metadata(a).map(|m| m.len()).unwrap_or(0))
        .sum::<u64>()
        .max(1);
    let mut done_bytes = 0u64;
    let mut rep = UnpackReport::default();
    for a in archives {
        let id = archive_id(game, a);
        let out = cache.join(&id);
        let f = File::open(a).map_err(|e| format!("{}: {e}", a.display()))?;
        let mut r = BufReader::with_capacity(1 << 20, f);
        let idx = match context.read_index(&mut r) {
            Ok(i) => i,
            Err(e) => {
                rep.errors.push(format!("{}: {e}", a.display()));
                continue;
            }
        };
        let mut order: Vec<&qar::Entry> = idx.entries.iter().collect();
        order.sort_by_key(|e| e.offset);
        let mut index = Vec::with_capacity(order.len());
        let mut names: Vec<String> = Vec::with_capacity(order.len());
        let mut seen = std::collections::BTreeSet::new();
        let destinations: Vec<_> = order
            .iter()
            .map(|entry| {
                let (name, named) = entry_name(entry.hash, dict);
                let path = out.join(&name);
                cache_output(cache, &path)?;
                if !seen.insert(steam::norm(&path)) {
                    return Err(format!(
                        "archive entries resolve to the same cache file: {name}"
                    ));
                }
                Ok((name, named, path))
            })
            .collect::<Result<_, String>>()?;
        for (k, (e, (name, named, p))) in order.iter().zip(destinations).enumerate() {
            if cancel.load(Ordering::Relaxed) {
                rep.cancelled = true;
                return Ok(rep);
            }
            if named {
                rep.named += 1;
            } else {
                rep.unnamed += 1;
            }
            let want = e.uncompressed as u64;
            let have = std::fs::metadata(&p)
                .ok()
                .filter(|m| m.is_file())
                .map(|m| m.len());
            let fits = have.is_some_and(|h| h == want || h + 8 == want || h + 16 == want);
            if fits {
                rep.skipped += 1;
            } else {
                let mut b = vec![0u8; e.stored as usize];
                let res = r
                    .seek(SeekFrom::Start(e.offset + 32))
                    .and_then(|_| r.read_exact(&mut b))
                    .map_err(|x| x.to_string())
                    .and_then(|_| context.decode(e, &b));
                match res {
                    Ok(c) => {
                        if let Some(d) = p.parent() {
                            std::fs::create_dir_all(d)
                                .map_err(|x| format!("{}: {x}", d.display()))?;
                        }
                        let part = p.with_extension(format!(
                            "{}.part",
                            p.extension()
                                .map(|x| x.to_string_lossy().into_owned())
                                .unwrap_or_default()
                        ));
                        cache_output(cache, &part)?;
                        std::fs::write(&part, &c)
                            .map_err(|x| format!("{}: {x}", part.display()))?;
                        std::fs::rename(&part, &p).map_err(|x| format!("{}: {x}", p.display()))?;
                        rep.written += 1;
                        rep.bytes_written += c.len() as u64;
                    }
                    Err(x) => rep.errors.push(format!("{id} {:016x}: {x}", e.hash)),
                }
            }
            names.push(name);
            done_bytes += e.raw_len();
            if k % 64 == 0 {
                progress(
                    (done_bytes as f32 / total as f32).min(1.0),
                    format!("{id}: {} of {} files", k + 1, order.len()),
                );
            }
        }
        for (e, n) in order.iter().zip(&names) {
            index.push(IndexEntry {
                hash: format!("{:016x}", e.hash),
                name: n,
                size: e.uncompressed as u64,
            });
        }
        let modified = std::fs::metadata(a)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let j = serde_json::json!({
            "archive": a.display().to_string(), "id": id, "bytes": std::fs::metadata(a).map(|m| m.len()).unwrap_or(0),
            "modified": modified, "entries": index,
        });
        let ip = cache.join(format!("{id}.index.json"));
        cache_output(cache, &ip)?;
        std::fs::write(&ip, serde_json::to_vec(&j).map_err(|e| e.to_string())?)
            .map_err(|e| format!("{}: {e}", ip.display()))?;
        rep.archives += 1;
    }
    progress(1.0, "done".into());
    Ok(rep)
}

/// free bytes on the drive holding `p` (the nearest existing parent)
pub fn free_space(p: &Path) -> Option<u64> {
    let mut d = p.to_path_buf();
    while !d.exists() {
        if !d.pop() {
            return None;
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let w: Vec<u16> = d
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut free = 0u64;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                w.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok != 0 { Some(free) } else { None }
    }
    #[cfg(not(windows))]
    {
        let _ = d;
        None
    }
}

/// one prerequisite line
#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub name: String,
    pub level: Level,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Ok,
    Info,
    Warn,
    Fail,
}

/// What the prerequisites check looks at.
#[derive(Clone, Debug, Default)]
pub struct PrereqInputs {
    pub game: Option<PathBuf>,
    pub buildid: Option<u64>,
    pub test_install: Option<PathBuf>,
    pub cache: Option<PathBuf>,
    pub needed_bytes: u64,
    pub fox_exe: PathBuf,
    pub python: String,
    pub dict: Option<PathBuf>,
    pub gpu: Option<String>,
    pub runtime_profile: Option<PathBuf>,
    /// Only legacy stage workflows need an interpreter. Native editor actions do not.
    pub needs_python: bool,
}

/// Run the checks on a serial worker. Python is probed only for legacy stage workflows.
pub fn prereqs(i: &PrereqInputs) -> Vec<Check> {
    let mut v = vec![];
    let mut add = |name: &str, level: Level, detail: String| {
        v.push(Check {
            name: name.into(),
            level,
            detail,
        })
    };
    match &i.game {
        Some(g) => {
            let c = check_game(g);
            if c.ok() {
                add(
                    "Game",
                    Level::Ok,
                    format!(
                        "{} ({} archives, {:.1} GB)",
                        g.display(),
                        c.archives.len(),
                        c.total_bytes() as f64 / 1e9
                    ),
                );
            } else {
                add(
                    "Game",
                    Level::Fail,
                    format!("{}: {}", g.display(), c.problems.join("; ")),
                );
            }
        }
        None => add(
            "Game",
            Level::Fail,
            "not found yet: scan, or pick the folder".into(),
        ),
    }
    match i.buildid {
        Some(b) => add("Game version", Level::Info, format!("Steam build {b}")),
        None => add(
            "Game version",
            Level::Info,
            "unknown (no Steam manifest)".into(),
        ),
    }
    match (&i.test_install, &i.game) {
        (None, _) => add(
            "Mod test install",
            Level::Warn,
            "not chosen: the Mods tab needs a game folder to install into".into(),
        ),
        (Some(t), Some(g)) if steam::same_path(t, g) => add(
            "Mod test install",
            Level::Warn,
            "is the Steam install itself: test on a copy first".into(),
        ),
        (Some(t), _) => {
            let c = check_game(t);
            if c.master00 {
                add("Mod test install", Level::Ok, t.display().to_string());
            } else {
                add(
                    "Mod test install",
                    Level::Fail,
                    format!("{}: {}", t.display(), c.problems.join("; ")),
                );
            }
        }
    }
    match &i.cache {
        None => add("Cache folder", Level::Fail, "not chosen".into()),
        Some(c) => match check_cache(c, i.game.as_deref()) {
            Err(e) => add("Cache folder", Level::Fail, e),
            Ok(()) => {
                let free = free_space(c);
                let need = i.needed_bytes;
                match free {
                    Some(f) if need > 0 && f < need => add(
                        "Cache folder",
                        Level::Fail,
                        format!(
                            "{}: {:.1} GB free, {:.1} GB needed",
                            c.display(),
                            f as f64 / 1e9,
                            need as f64 / 1e9
                        ),
                    ),
                    Some(f) => add(
                        "Cache folder",
                        Level::Ok,
                        format!("{} ({:.1} GB free)", c.display(), f as f64 / 1e9),
                    ),
                    None => add(
                        "Cache folder",
                        Level::Info,
                        format!("{} (free space unknown)", c.display()),
                    ),
                }
            }
        },
    }
    match &i.dict {
        Some(d) if d.is_file() => add("Name dictionary", Level::Ok, d.display().to_string()),
        Some(d) => add(
            "Name dictionary",
            Level::Warn,
            format!("{} missing: files are named by hash", d.display()),
        ),
        None => add(
            "Name dictionary",
            Level::Warn,
            "none: unpacked files are named by hash (build one from your game below)".into(),
        ),
    }
    if i.fox_exe.is_file() {
        add("fox tools", Level::Ok, i.fox_exe.display().to_string());
    } else {
        add(
            "fox tools",
            Level::Fail,
            format!(
                "{} missing: place the companion fox executable beside Fox Studio or select it in Settings",
                i.fox_exe.display()
            ),
        );
    }
    match (&i.game, &i.runtime_profile) {
        (Some(game), Some(path)) => match load_profile(game, path) {
            Ok(_) => add("Local game metadata", Level::Ok, path.display().to_string()),
            Err(error) => add("Local game metadata", Level::Fail, error),
        },
        _ => add(
            "Local game metadata",
            Level::Fail,
            "prepare game data in step 3 before archive operations".into(),
        ),
    }
    if i.needs_python {
        match python_version(&i.python) {
            Ok(v) => add("Python (stages)", Level::Ok, format!("{} ({v})", i.python)),
            Err(e) => add(
                "Python (stages)",
                Level::Warn,
                format!(
                    "{}: {e} (needed by the selected legacy stage workflow)",
                    i.python
                ),
            ),
        }
    }
    match &i.gpu {
        Some(g) => add("GPU (previews)", Level::Ok, g.clone()),
        None => add(
            "GPU (previews)",
            Level::Info,
            "no wgpu device in this session: previews use the CPU views".into(),
        ),
    }
    v
}

fn python_version(py: &str) -> Result<String, String> {
    let mut c = std::process::Command::new(py);
    c.arg("--version").stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let o = c.output().map_err(|e| e.to_string())?;
    let s = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
    .trim()
    .to_string();
    if o.status.success() { Ok(s) } else { Err(s) }
}

/// Build a name dictionary from the game's archives (and its exe) into `out` (blocking; call on a worker).
/// `progress(fraction, text)`; stops when `cancel` is set. Returns a one-line summary.
pub fn build_dictionary_with_context(
    context: &qar::Context,
    game: &Path,
    archives: &[PathBuf],
    out: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32, &str),
) -> Result<String, String> {
    check_cache(out, Some(game))?;
    let exe = game.join(steam::GAME_EXE);
    let (names, st) = foxcore::dict::build_with_context(
        context,
        archives,
        exe.is_file().then_some(exe.as_path()),
        &mut |f, t| {
            progress(f, t);
            !cancel.load(Ordering::Relaxed)
        },
    )?;
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    if let Some(d) = out.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let tmp = out.with_extension("txt.part");
    check_cache(&tmp, Some(game))?;
    std::fs::write(&tmp, foxcore::dict::to_text(&names))
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, out).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(format!(
        "{} names resolve {} of {} entry hashes ({:.1} %) in {:.0} s{}",
        names.len(),
        st.resolved,
        st.wanted,
        100.0 * st.resolved as f64 / st.wanted.max(1) as f64,
        st.seconds,
        if st.skipped.is_empty() {
            String::new()
        } else {
            format!("; {} archive(s) skipped", st.skipped.len())
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_names_and_safety() {
        let g = Path::new(r"D:\Steam\MGS_TPP");
        assert_eq!(
            archive_id(g, &g.join("master").join("chunk0.dat")),
            "chunk0"
        );
        assert_eq!(
            archive_id(g, &g.join("master").join("0").join("00.dat")),
            "0_00"
        );
        assert_eq!(
            safe_rel("/Assets/tpp/../x:y/a?.lua"),
            "Assets/tpp/x_y/a_.lua"
        );
        let d = load_dict("/Assets/tpp/pack/a.fpk\n\n/Assets/tpp/b\n");
        let h = qar::path_hash("/Assets/tpp/b") & PATH_MASK;
        assert_eq!(d.get(&h).map(|s| s.as_str()), Some("/Assets/tpp/b"));
        let full = h | (qar::ext_id("lua") << 51);
        assert_eq!(entry_name(full, &d), ("Assets/tpp/b.lua".into(), true));
        let (n, named) = entry_name(0x1234 | (qar::ext_id("fpk") << 51), &d);
        assert!(
            !named && n.starts_with("_unnamed/") && n.ends_with(".fpk"),
            "{n}"
        );
    }

    #[test]
    fn cache_must_be_separate_from_the_game() {
        // absolute paths on this platform (no file-system access)
        let lib = std::env::temp_dir().join("SteamLibrary");
        let g = lib.join("steamapps").join("common").join("MGS_TPP");
        assert!(check_cache(&g.join("cache"), Some(&g)).is_err());
        assert!(check_cache(&lib, Some(&g)).is_err());
        assert!(check_cache(Path::new("relative"), Some(&g)).is_err());
        assert!(check_cache(&std::env::temp_dir().join("fox-cache"), Some(&g)).is_ok());
    }

    #[test]
    fn game_check_of_a_non_game() {
        let d = std::env::temp_dir().join(format!("foxstudio_game_{}", std::process::id()));
        std::fs::create_dir_all(d.join("master").join("0")).unwrap();
        let c = check_game(&d);
        assert!(!c.ok());
        assert_eq!(c.problems.len(), 2);
        std::fs::write(d.join("master").join("0").join("00.dat"), b"x").unwrap();
        std::fs::write(d.join(steam::GAME_EXE), b"x").unwrap();
        let c = check_game(&d);
        assert!(c.ok());
        assert_eq!(c.archives.len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// a synthetic archive (foxcore's writer) unpacks, names from the dictionary, resumes, and refuses the game folder
    #[test]
    fn unpack_a_synthetic_archive() {
        let d = std::env::temp_dir().join(format!("foxstudio_unpack_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let game = d.join("game");
        std::fs::create_dir_all(game.join("master")).unwrap();
        let context = qar::Context::new(foxcore::runtime_data::QarKeys {
            header_masks: [11, 22, 33, 44],
            layer1: [1, 2, 3, 4, 5, 6, 7, 8],
        });
        let names = ["/Assets/tpp/script/a", "/Assets/tpp/level/b"];
        let exts = ["lua", "fox2"];
        let mut raws = vec![];
        for (n, e) in names.iter().zip(exts) {
            let h = (qar::path_hash(n) & PATH_MASK) | (qar::ext_id(e) << 51);
            let (_, raw) = context
                .encode_plain(h, format!("content of {n}").as_bytes())
                .unwrap();
            raws.push(qar::RawEntry {
                hash: h,
                raw,
                pad: None,
            });
        }
        let dat = game.join("master").join("data9.dat");
        let mut f = std::fs::File::create(&dat).unwrap();
        context.write_archive(&mut f, 0x800, 1, &raws).unwrap();
        drop(f);
        let dict = load_dict(&names[..1].join("\n"));
        let cache = d.join("cache");
        let cancel = AtomicBool::new(false);
        let mut last = 0.0;
        let r = unpack_with_context(
            &context,
            &game,
            std::slice::from_ref(&dat),
            &cache,
            &dict,
            &cancel,
            &mut |f, _| last = f,
        )
        .unwrap();
        assert_eq!(
            (r.archives, r.written, r.named, r.unnamed),
            (1, 2, 1, 1),
            "{r:?}"
        );
        assert_eq!(last, 1.0);
        assert_eq!(
            std::fs::read(cache.join("data9/Assets/tpp/script/a.lua")).unwrap(),
            b"content of /Assets/tpp/script/a"
        );
        assert!(cache.join("data9.index.json").is_file());
        let r2 = unpack_with_context(
            &context,
            &game,
            std::slice::from_ref(&dat),
            &cache,
            &dict,
            &cancel,
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!((r2.written, r2.skipped), (0, 2));
        assert!(
            unpack_with_context(
                &context,
                &game,
                std::slice::from_ref(&dat),
                &game.join("x"),
                &dict,
                &cancel,
                &mut |_, _| {}
            )
            .is_err()
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
