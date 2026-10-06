//! foxinstall: install .mgsv mods into MGSV:TPP's master/0/00.dat (files) and 01.dat (textures). Fast, exact,
//! reversible. Design: docs/release/REPLACE_THIRD_PARTY.md section 1.
//!
//! Model: the installed archives are a pure function of (base archives, installed mods in order). Every change
//! recomputes the target archives as raw-block copies (QAR entries move between archives byte for byte, no
//! re-compression), writes them to temp files and commits by rename under a journal. With no mods left the base
//! archives are restored byte for byte. Content of earlier mods that a later mod overrides is kept in a
//! content-addressed shadow store, so any uninstall order restores exact bytes.
//!
//! State (inside the game folder): foxinstall/manifest.json, foxinstall/shadow/<md5>, foxinstall/backup/<path>
//! (loose game files a mod replaced), master/0/00.dat.foxbase / 01.dat.foxbase (the base archives).
pub mod compat;
pub mod mgsv;
pub mod mgsvpack;
pub mod snakebite;
pub mod xmltree;

use foxcore::{fpk, qar};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const ARCHIVES: [&str; 2] = ["00", "01"];

/// the mod name of a package (.mgsv or staging folder)
pub fn package_name(p: &Path) -> Result<String, String> {
    let meta = if p.is_dir() {
        fs::read_to_string(p.join("metadata.xml")).map_err(|e| e.to_string())?
    } else {
        let f = File::open(p).map_err(|e| e.to_string())?;
        let mut z = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
        let mut m = String::new();
        z.by_name("metadata.xml")
            .map_err(|e| e.to_string())?
            .read_to_string(&mut m)
            .map_err(|e| e.to_string())?;
        m
    };
    let d = xmltree::parse(meta.trim_start_matches('\u{feff}'))?;
    d.root
        .attr("Name")
        .map(|s| s.to_string())
        .ok_or_else(|| "metadata.xml has no Name".into())
}

pub fn md5_hex(b: &[u8]) -> String {
    let d: [u8; 16] = Md5::digest(b).into();
    d.iter().map(|x| format!("{x:02X}")).collect()
}

/// MD5 of a file, uppercase hex (streamed)
pub fn md5_file_pub(p: &Path) -> Result<String, String> {
    md5_file(p)
}

fn md5_file(p: &Path) -> Result<String, String> {
    md5_file_progress(p, |_| {})
}

/// md5 of a file in 4 MB chunks; `on(n)` after each chunk with the bytes just read
fn md5_file_progress(p: &Path, mut on: impl FnMut(u64)) -> Result<String, String> {
    let mut f = File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut h = Md5::new();
    let mut buf = vec![0u8; 1 << 22];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        on(n as u64);
    }
    let d: [u8; 16] = h.finalize().into();
    Ok(d.iter().map(|x| format!("{x:02X}")).collect())
}

/// byte copy in 4 MB chunks (a large archive copy can then report progress); `on(n)` after each chunk
fn copy_progress(src: &Path, dst: &Path, mut on: impl FnMut(u64)) -> Result<(), String> {
    let mut r = File::open(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let mut w = File::create(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    let mut buf = vec![0u8; 1 << 22];
    loop {
        let n = r.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        w.write_all(&buf[..n])
            .map_err(|e| format!("{}: {e}", dst.display()))?;
        on(n as u64);
    }
    w.flush().map_err(|e| e.to_string())
}

/// a running byte count turned into phase fractions (at most ~200 reports per phase)
struct ByteMeter {
    total: u64,
    done: u64,
    next: u64,
}

impl ByteMeter {
    fn new(total: u64) -> Self {
        ByteMeter {
            total: total.max(1),
            done: 0,
            next: 0,
        }
    }
    fn frac(&self) -> f32 {
        (self.done as f64 / self.total as f64).min(1.0) as f32
    }
    /// Some(fraction) when worth reporting
    fn add(&mut self, n: u64) -> Option<f32> {
        self.done += n;
        if self.done >= self.next || self.done >= self.total {
            self.next = self.done + self.total / 200;
            return Some((self.done as f64 / self.total as f64).min(1.0) as f32);
        }
        None
    }
}

/// which archive a game file goes to (textures -> 01.dat, everything else -> 00.dat)
/// the file an inner pack path lands in when GzsTool unpacks on Windows: case-insensitive, and every path component
/// loses its trailing dots / spaces ("/x/blood./a.vfx" and "/x/blood/a.vfx" are the same file)
pub fn windows_key(path: &str) -> String {
    path.replace('\\', "/")
        .split('/')
        .map(|c| c.trim_end_matches(['.', ' ']))
        .collect::<Vec<_>>()
        .join("/")
        .to_lowercase()
}

/// SnakeBite merges a pack by GzsTool-unpacking the vanilla pack to a folder (entries written in order: a later
/// entry that maps to the same Windows file overwrites an earlier one), copying the mods' files over it, and
/// repacking every listed entry from that folder. So entries that collide on Windows all end up with the content of
/// the LAST writer: a mod file if any mod wrote one, else the last vanilla entry (vanilla f30150.fpkd lists both
/// ".../blood/fx_tpp_splbrdwng01_s1.vfx" and ".../blood./fx_tpp_splbrdwng01_s1.vfx").
fn gzs_folder_collisions(entries: &mut [(String, Vec<u8>)], mod_written: &HashMap<String, usize>) {
    let mut winner: HashMap<String, usize> = HashMap::new();
    for (i, (p, _)) in entries.iter().enumerate() {
        winner.insert(windows_key(p), i);
    }
    for (k, &i) in mod_written {
        winner.insert(k.clone(), i);
    }
    let fixes: Vec<(usize, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, (p, _))| {
            winner
                .get(&windows_key(p))
                .filter(|&&w| w != i)
                .map(|&w| (i, w))
        })
        .collect();
    for (i, w) in fixes {
        entries[i].1 = entries[w].1.clone();
    }
}

pub fn archive_for(path: &str) -> &'static str {
    let l = path.to_lowercase();
    if l.ends_with(".ftex") || l.ends_with(".ftexs") {
        "01"
    } else {
        "00"
    }
}

fn is_pack(path: &str) -> Option<fpk::Kind> {
    let l = path.to_lowercase();
    if l.ends_with(".fpkd") {
        Some(fpk::Kind::Fpkd)
    } else if l.ends_with(".fpk") {
        Some(fpk::Kind::Fpk)
    } else {
        None
    }
}

// ------------------------------------------------------------------------------------------------ manifest

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct FileSig {
    pub size: u64,
    pub md5: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileItem {
    pub path: String,
    pub hash: String,
    pub archive: String,
    pub md5: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PackItem {
    pub pack: String,
    pub archive: String,
    /// (inner path, content md5) in the mod's pack order
    pub entries: Vec<(String, String)>,
    pub references: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LooseItem {
    pub path: String,
    pub md5: String,
    /// game file that existed before (kept in foxinstall/backup/<path>) and is restored on uninstall
    pub replaced_original: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct InstalledMod {
    pub name: String,
    pub version: String,
    pub author: String,
    pub website: String,
    pub description: String,
    pub package_md5: String,
    pub installed: String,
    pub files: Vec<FileItem>,
    pub packs: Vec<PackItem>,
    pub loose: Vec<LooseItem>,
    /// SnakeBite-compatible mode: this mod's ModEntry element as snakebite.xml stores it
    #[serde(default)]
    pub sb_entry: Option<String>,
}

fn d_layout() -> String {
    "gzstool".into()
}

fn d_mode() -> String {
    "native".into()
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Manifest {
    pub format: u32,
    pub tool: String,
    /// "gzstool": full rewrite each time, GzsTool's exact layout (proven in game through SnakeBite);
    /// "inplace": reserved section-table capacity, appends / holes, table rewritten in place (needs its sandbox test)
    #[serde(default = "d_layout")]
    pub layout: String,
    /// "native" (our own setup) or "snakebite" (a SnakeBite-managed game: snakebite.xml kept in sync)
    #[serde(default = "d_mode")]
    pub mode: String,
    pub base: BTreeMap<String, FileSig>,
    pub mods: Vec<InstalledMod>,
}

// ------------------------------------------------------------------------------------------------ game

/// Progress of an installer operation, for a front end (Fox Studio). Reports come on the installing thread: at each
/// phase start, while archives are written / copied / hashed (by bytes), and once at the end.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    /// "install" | "uninstall" | "setup" | "layout"
    pub op: &'static str,
    /// what runs now ("vanilla tables", "write 00.dat", ...); "done" / "failed" at the end
    pub phase: String,
    /// this phase's own fraction 0..1 (None = not measurable)
    pub fraction: Option<f32>,
    /// whole-operation estimate 0..1: never goes back within an operation, 1.0 exactly once it succeeded
    pub overall: f32,
    /// the operation finished (`ok` says how); the last report of every operation has done = true
    pub done: bool,
    pub ok: bool,
}

pub type ProgressFn = Box<dyn FnMut(&Progress) + Send>;

/// Phase plans: each phase's share of the operation's time (measured on FLYK installs into the fake game: the
/// vanilla tables and the archive writes dominate). A phase not in the plan keeps the overall value.
const PLAN_INSTALL: &[(&str, f32)] = &[
    ("open package", 0.04),
    ("vanilla tables", 0.42),
    ("current archive indexes", 0.09),
    ("classify package files", 0.04),
    ("write archives", 0.38),
    ("commit", 0.03),
];
const PLAN_UNINSTALL: &[(&str, f32)] = &[
    ("loose files", 0.02),
    ("vanilla tables", 0.44),
    ("current archive indexes", 0.09),
    ("write archives", 0.42),
    ("commit", 0.03),
];
/// the last mod goes: the base archives are copied back
const PLAN_UNINSTALL_BASE: &[(&str, f32)] = &[
    ("loose files", 0.02),
    ("restore base archives", 0.93),
    ("commit", 0.05),
];
const PLAN_SETUP: &[(&str, f32)] = &[("copy base archives", 0.45), ("check signatures", 0.55)];
const PLAN_LAYOUT: &[(&str, f32)] = &[
    ("vanilla tables", 0.40),
    ("current archive indexes", 0.08),
    ("write archives", 0.49),
    ("commit", 0.03),
];
/// share of one archive's "write archives" span spent merging packs and planning (the rest is the byte writes)
const MERGE_SHARE: f32 = 0.3;

#[derive(Default)]
struct Prog {
    f: Option<ProgressFn>,
    op: Option<(&'static str, &'static [(&'static str, f32)])>,
    phase: String,
    /// text reported instead of the plan phase (e.g. "write 01.dat" inside "write archives"); cleared by phase()
    label: Option<String>,
    /// highest overall value sent in this operation
    last: f32,
    /// sub-span of the current phase that local fractions map into (start, width), for multi-archive writes
    span: (f32, f32),
}

impl Prog {
    fn overall(&self, frac: Option<f32>) -> f32 {
        let Some((_, plan)) = self.op else {
            return self.last;
        };
        let Some(i) = plan.iter().position(|(p, _)| *p == self.phase) else {
            return self.last;
        };
        let total: f32 = plan.iter().map(|(_, w)| w).sum();
        let before: f32 = plan[..i].iter().map(|(_, w)| w).sum();
        let f = frac.unwrap_or(0.0).clamp(0.0, 1.0);
        let local = self.span.0 + self.span.1 * f;
        // 1.0 belongs to the final "done" report only
        ((before + plan[i].1 * local) / total)
            .clamp(0.0, 0.99)
            .max(self.last)
    }
}

pub struct Game {
    context: Option<qar::Context>,
    order: Option<foxcore::runtime_data::PackOrder>,
    profile: Option<foxcore::runtime_data::RuntimeProfile>,
    pub root: PathBuf,
    /// where vanilla packs are looked up for merges (default: the game folder; read only)
    pub source_root: PathBuf,
    /// development: extracted vanilla folders (each holds Assets/...) used instead of the chunk archives
    pub vanilla_dirs: Vec<PathBuf>,
    pub log: Vec<String>,
    /// rewrite every archive even when no mod touches it (layout changes)
    force_rewrite: bool,
    inplace_journals: Vec<PathBuf>,
    /// merged pack -> the archive its vanilla pack came from (for snakebite.xml)
    merge_sources: HashMap<String, String>,
    /// merged pack -> its vanilla inner file paths (for snakebite.xml)
    merge_vanilla_entries: HashMap<String, Vec<String>>,
    /// temp files this operation created (*.foxtmp, *.foxbase.tmp): removed when the operation fails before its commit
    tmps: std::cell::RefCell<Vec<PathBuf>>,
    /// front-end progress callback (set_progress) and the running operation's plan
    prog: std::cell::RefCell<Prog>,
}

enum Src {
    /// entry i of the base archive
    Base(usize),
    /// entry i of the current archive (unchanged block)
    Cur(usize),
    /// a new raw block
    Bytes(Vec<u8>),
}

struct Archive {
    context: qar::Context,
    path: PathBuf,
    file: BufReader<File>,
    index: qar::Index,
    by_hash: HashMap<u64, usize>,
}

impl Archive {
    fn open(context: &qar::Context, p: &Path) -> Result<Archive, String> {
        let f = File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
        let mut file = BufReader::with_capacity(1 << 20, f);
        let index = context
            .read_index(&mut file)
            .map_err(|e| format!("{}: {e}", p.display()))?;
        let by_hash = index
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.hash, i))
            .collect();
        Ok(Archive {
            context: context.clone(),
            path: p.to_path_buf(),
            file,
            index,
            by_hash,
        })
    }
    fn raw(&mut self, i: usize) -> Result<Vec<u8>, String> {
        let e = &self.index.entries[i];
        let mut b = vec![0u8; e.raw_len() as usize];
        self.file
            .seek(SeekFrom::Start(e.offset))
            .map_err(|x| x.to_string())?;
        self.file
            .read_exact(&mut b)
            .map_err(|x| format!("{}: {x}", self.path.display()))?;
        Ok(b)
    }
    /// content md5 of entry i: the header's md5 field when it is the content md5 (plain and zlib entries, i.e. no
    /// second layer), else decode
    fn content_md5(&mut self, i: usize) -> Result<String, String> {
        let e = self.index.entries[i].clone();
        let mut head = [0u8; 8];
        if e.stored >= 8 {
            self.file
                .seek(SeekFrom::Start(e.offset + 32))
                .map_err(|x| x.to_string())?;
            self.file.read_exact(&mut head).map_err(|x| x.to_string())?;
        }
        if self.context.layer2_header_len(&e, &head) == 0 {
            return Ok(e.md5.iter().map(|x| format!("{x:02X}")).collect());
        }
        Ok(md5_hex(&self.content(i)?))
    }
    fn content(&mut self, i: usize) -> Result<Vec<u8>, String> {
        let raw = self.raw(i)?;
        let e = self.index.entries[i].clone();
        self.context.decode(&e, &raw[32..])
    }
}

/// a vanilla archive known only by its section table: entries found by hash on demand (one header read each)
struct Lazy {
    context: qar::Context,
    path: PathBuf,
    file: Option<BufReader<File>>,
    keys: HashMap<u64, Vec<u64>>,
}

impl Lazy {
    fn open(context: &qar::Context, p: &Path) -> Result<Lazy, String> {
        let mut f = BufReader::with_capacity(
            1 << 16,
            File::open(p).map_err(|e| format!("{}: {e}", p.display()))?,
        );
        let (_h, t) = context
            .read_table(&mut f)
            .map_err(|e| format!("{}: {e}", p.display()))?;
        let mut keys: HashMap<u64, Vec<u64>> = HashMap::with_capacity(t.len());
        for (k, off) in t {
            keys.entry(k).or_default().push(off);
        }
        Ok(Lazy {
            context: context.clone(),
            path: p.to_path_buf(),
            file: Some(f),
            keys,
        })
    }
    /// content of the entry with this hash, if present
    fn content(&mut self, hash: u64) -> Result<Option<Vec<u8>>, String> {
        let Some(offs) = self.keys.get(&qar::section_key(hash)).cloned() else {
            return Ok(None);
        };
        let f = self.file.as_mut().unwrap();
        for off in offs {
            let mut hb = [0u8; 32];
            f.seek(SeekFrom::Start(off)).map_err(|x| x.to_string())?;
            f.read_exact(&mut hb).map_err(|x| x.to_string())?;
            let e = self.context.read_entry_header(&hb, off)?;
            if e.hash == hash {
                let mut b = vec![0u8; e.stored as usize];
                f.read_exact(&mut b)
                    .map_err(|x| format!("{}: {x}", self.path.display()))?;
                return self.context.decode(&e, &b).map(Some);
            }
        }
        Ok(None)
    }
    fn has(&self, hash: u64) -> bool {
        self.keys.contains_key(&qar::section_key(hash))
    }
}

/// vanilla packs from an extracted folder tree (development: unpacked game data), keyed by entry hash
struct DirSource {
    files: HashMap<u64, PathBuf>,
}

impl DirSource {
    fn open(roots: &[PathBuf]) -> DirSource {
        let mut files = HashMap::new();
        for r in roots {
            let mut stack = vec![r.clone()];
            while let Some(d) = stack.pop() {
                let Ok(rd) = fs::read_dir(&d) else { continue };
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if is_pack(&p.to_string_lossy()).is_some()
                        && let Ok(rel) = p.strip_prefix(r)
                    {
                        let gp = format!(
                            "/{}",
                            rel.to_string_lossy()
                                .replace(std::path::MAIN_SEPARATOR, "/")
                        );
                        files.entry(qar::file_hash(&gp)).or_insert(p);
                    }
                }
            }
        }
        DirSource { files }
    }
}

enum Vanilla {
    Qar(Lazy),
    Dir(DirSource),
}

impl Vanilla {
    /// archive file name (SnakeBite's SourceName), or the folder for extracted sources
    fn name(&self) -> String {
        match self {
            Vanilla::Qar(l) => l
                .path
                .file_name()
                .map(|x| x.to_string_lossy().to_string())
                .unwrap_or_default(),
            Vanilla::Dir(_) => "extracted".into(),
        }
    }
    fn content(&mut self, hash: u64) -> Result<Option<Vec<u8>>, String> {
        match self {
            Vanilla::Qar(l) => {
                if !l.has(hash) {
                    return Ok(None);
                }
                l.content(hash)
            }
            Vanilla::Dir(d) => match d.files.get(&hash) {
                Some(p) => fs::read(p)
                    .map(Some)
                    .map_err(|e| format!("{}: {e}", p.display())),
                None => Ok(None),
            },
        }
    }
}

/// An mgsvtpp.exe running from THIS game folder (it holds the archives open), or one whose path cannot be read
/// (refuse to be safe). A game running from another install (the sandbox while a fake game is tested) is no reason.
fn game_running_in(root: &Path) -> Option<String> {
    let mut s = sysinfo::System::new();
    s.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::Always),
    );
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let root = canon(root);
    s.processes()
        .values()
        .filter(|p| {
            p.name()
                .to_string_lossy()
                .eq_ignore_ascii_case("mgsvtpp.exe")
        })
        .find(|p| match p.exe() {
            Some(exe) if !exe.as_os_str().is_empty() => canon(exe).starts_with(&root),
            _ => true,
        })
        .map(|p| match p.exe() {
            Some(e) => format!("mgsvtpp.exe ({})", e.display()),
            None => "mgsvtpp.exe".to_string(),
        })
}

impl Game {
    pub fn new(root: &Path, source_root: Option<&Path>) -> Game {
        Game {
            context: initial_context(),
            order: initial_order(),
            profile: None,
            root: root.to_path_buf(),
            source_root: source_root.unwrap_or(root).to_path_buf(),
            vanilla_dirs: vec![],
            log: vec![],
            force_rewrite: false,
            inplace_journals: vec![],
            merge_sources: HashMap::new(),
            merge_vanilla_entries: HashMap::new(),
            tmps: std::cell::RefCell::new(vec![]),
            prog: std::cell::RefCell::new(Prog::default()),
        }
    }
    /// Open an installer with the profile for its explicit source install.
    pub fn with_profile(
        root: &Path,
        source_root: Option<&Path>,
        profile: &foxcore::runtime_data::RuntimeProfile,
    ) -> Result<Self, String> {
        let mut game = Self::new(root, source_root);
        game.set_profile(profile)?;
        Ok(game)
    }

    pub fn set_profile(
        &mut self,
        profile: &foxcore::runtime_data::RuntimeProfile,
    ) -> Result<(), String> {
        profile
            .validate_for_game(&self.source_root)
            .map_err(|e| e.to_string())?;
        self.context = Some(profile.qar_context());
        self.order = Some(profile.order.clone());
        self.profile = Some(profile.clone());
        Ok(())
    }

    pub fn load_profile(&mut self, path: &Path) -> Result<(), String> {
        let profile =
            foxcore::runtime_data::RuntimeProfile::load(path).map_err(|e| e.to_string())?;
        self.set_profile(&profile)
    }

    pub(crate) fn qar_context(&self) -> Result<&qar::Context, String> {
        self.context.as_ref().ok_or_else(|| {
            "complete game setup and select its runtime profile before archive operations".into()
        })
    }

    fn pack_order(&self) -> Result<&foxcore::runtime_data::PackOrder, String> {
        self.order.as_ref().ok_or_else(|| {
            "complete game setup to learn package ordering before archive operations".into()
        })
    }

    fn require_profile(&self) -> Result<(), String> {
        self.qar_context()?;
        self.pack_order()?;
        if let Some(profile) = &self.profile {
            profile
                .validate_for_game(&self.source_root)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// register a temp file path the current operation is about to create
    pub(crate) fn tmp(&self, p: PathBuf) -> PathBuf {
        self.tmps.borrow_mut().push(p.clone());
        p
    }

    /// after an operation: on error, delete every temp file it created, unless a commit journal exists (then the
    /// temps are the committed data and the next run's recovery renames them into place); always forget them
    fn finish_op<T>(&mut self, r: Result<T, String>) -> Result<T, String> {
        self.end_op(r.is_ok());
        let tmps: Vec<PathBuf> = self.tmps.borrow_mut().drain(..).collect();
        if r.is_err() && !self.journal().exists() {
            for t in tmps {
                if t.is_file() {
                    let _ = fs::remove_file(&t);
                }
            }
        }
        r
    }

    fn dat(&self, a: &str) -> PathBuf {
        self.root.join("master").join("0").join(format!("{a}.dat"))
    }
    fn base(&self, a: &str) -> PathBuf {
        self.root
            .join("master")
            .join("0")
            .join(format!("{a}.dat.foxbase"))
    }
    fn state(&self) -> PathBuf {
        self.root.join("foxinstall")
    }
    fn manifest_path(&self) -> PathBuf {
        self.state().join("manifest.json")
    }
    /// Report progress to `f` during install / uninstall / setup / layout switches (see Progress). Runs on the
    /// installing thread between file writes: keep it cheap (send it to a channel, throttle in the front end).
    pub fn set_progress(&mut self, f: impl FnMut(&Progress) + Send + 'static) {
        self.prog.borrow_mut().f = Some(Box::new(f));
    }

    fn send(&self, p: &mut Prog, fraction: Option<f32>, done: bool, ok: bool) {
        let Some((op, _)) = p.op else { return };
        let overall = if done && ok { 1.0 } else { p.overall(fraction) };
        p.last = overall;
        let phase = p.label.clone().unwrap_or_else(|| p.phase.clone());
        let r = Progress {
            op,
            phase,
            fraction,
            overall,
            done,
            ok,
        };
        if let Some(f) = p.f.as_mut() {
            f(&r);
        }
    }

    /// an operation starts (its plan sets the overall estimate)
    fn begin_op(&self, op: &'static str, plan: &'static [(&'static str, f32)]) {
        let mut p = self.prog.borrow_mut();
        p.op = Some((op, plan));
        p.last = 0.0;
        p.span = (0.0, 1.0);
        p.label = None;
        p.phase = plan.first().map(|(n, _)| n.to_string()).unwrap_or_default();
        self.send(&mut p, Some(0.0), false, false);
    }

    /// a phase starts
    fn phase(&self, name: &str) {
        let mut p = self.prog.borrow_mut();
        if p.op.is_none() {
            return;
        }
        p.phase = name.to_string();
        p.span = (0.0, 1.0);
        p.label = None;
        self.send(&mut p, Some(0.0), false, false);
    }

    /// the current phase's own fraction; `label` renames what runs (e.g. "write 01.dat") without changing the plan
    fn phase_frac(&self, fraction: f32, label: Option<&str>) {
        let mut p = self.prog.borrow_mut();
        if p.op.is_none() {
            return;
        }
        if let Some(l) = label {
            p.label = Some(l.to_string());
        }
        self.send(&mut p, Some(fraction), false, false);
    }

    /// map the current phase's local fractions into [start, start + width) of it (one archive of several)
    fn phase_span(&self, start: f32, width: f32) {
        self.prog.borrow_mut().span = (start, width);
    }

    fn end_op(&self, ok: bool) {
        let mut p = self.prog.borrow_mut();
        if p.op.is_none() {
            return;
        }
        p.phase = if ok { "done".into() } else { "failed".into() };
        p.label = None;
        let frac = if ok { Some(1.0) } else { None };
        // "done" / "failed" are not plan phases: overall stays at the last value unless ok (then 1.0)
        self.send(&mut p, frac, true, ok);
        p.op = None;
        p.span = (0.0, 1.0);
        p.label = None;
    }

    fn tick(&mut self, what: &str, t: &mut std::time::Instant) {
        if std::env::var_os("FOX_TIMING").is_some() {
            self.say(format!("  [{:6.2} s] {what}", t.elapsed().as_secs_f64()));
        }
        *t = std::time::Instant::now();
    }
    fn say(&mut self, s: impl Into<String>) {
        let s = s.into();
        println!("{s}");
        self.log.push(s);
    }

    pub fn load_manifest(&self) -> Result<Manifest, String> {
        let p = self.manifest_path();
        let b = fs::read(&p)
            .map_err(|_| format!("not set up: no {} (run `fox mod setup`)", p.display()))?;
        serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", p.display()))
    }

    fn save_manifest(&self, m: &Manifest) -> Result<(), String> {
        let p = self.manifest_path();
        let tmp = p.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(m).unwrap()).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &p).map_err(|e| e.to_string())
    }

    fn preflight(&self) -> Result<(), String> {
        self.require_profile()?;
        if let Some(n) = game_running_in(&self.root) {
            return Err(format!("{n} is running: close the game first"));
        }
        if self.root.join("snakebite.xml").exists() {
            let snake = self
                .load_manifest()
                .map(|m| m.mode == "snakebite")
                .unwrap_or(false);
            if !snake {
                return Err("this game is managed by SnakeBite (snakebite.xml): adopt it first with \
                            `fox mod setup --snakebite` (imports SnakeBite's installed mods, writes nothing to the game)"
                    .into());
            }
        }
        self.recover()?;
        Ok(())
    }

    /// First use: keep the current 00.dat / 01.dat as the base (copies .foxbase) and record their signatures.
    pub fn setup(&mut self) -> Result<(), String> {
        let r = self.setup_op();
        self.finish_op(r)
    }

    fn setup_op(&mut self) -> Result<(), String> {
        self.require_profile()?;
        self.begin_op("setup", PLAN_SETUP);
        if let Some(n) = game_running_in(&self.root) {
            return Err(format!(
                "{n} is running: the game is running from this folder, close it first"
            ));
        }
        if self.manifest_path().exists() {
            self.say("already set up");
            return Ok(());
        }
        if self.root.join("snakebite.xml").exists() {
            return Err(
                "SnakeBite manages this game (snakebite.xml); compatible mode is not built yet"
                    .into(),
            );
        }
        fs::create_dir_all(self.state().join("shadow")).map_err(|e| e.to_string())?;
        let mut m = Manifest {
            format: 1,
            tool: format!("foxinstall {}", env!("CARGO_PKG_VERSION")),
            layout: d_layout(),
            ..Default::default()
        };
        let size = |p: &Path| {
            fs::metadata(p)
                .map(|m| m.len())
                .map_err(|e| format!("{}: {e}", p.display()))
        };
        let mut to_copy = 0;
        for a in ARCHIVES {
            Archive::open(self.qar_context()?, &self.dat(a))?; // must be a valid QAR
            if !self.base(a).exists() {
                to_copy += size(&self.dat(a))?;
            }
        }
        let mut meter = ByteMeter::new(to_copy);
        for a in ARCHIVES {
            let (d, b) = (self.dat(a), self.base(a));
            if !b.exists() {
                let t = std::time::Instant::now();
                self.phase_frac(meter.frac(), Some(&format!("copy {a}.dat")));
                copy_progress(&d, &self.tmp(b.with_extension("foxbase.tmp")), |n| {
                    if let Some(f) = meter.add(n) {
                        self.phase_frac(f, None);
                    }
                })?;
                fs::rename(b.with_extension("foxbase.tmp"), &b).map_err(|e| e.to_string())?;
                self.say(format!(
                    "base {a}.dat -> {} ({:.1} s)",
                    b.display(),
                    t.elapsed().as_secs_f64()
                ));
            }
        }
        self.phase("check signatures");
        let mut to_hash = 0;
        for a in ARCHIVES {
            to_hash += size(&self.dat(a))? + size(&self.base(a))?;
        }
        let mut meter = ByteMeter::new(to_hash);
        for a in ARCHIVES {
            let (d, b) = (self.dat(a), self.base(a));
            let mut on = |n| {
                if let Some(f) = meter.add(n) {
                    self.phase_frac(f, None);
                }
            };
            let sig = FileSig {
                size: size(&b)?,
                md5: md5_file_progress(&b, &mut on)?,
            };
            if md5_file_progress(&d, &mut on)? != sig.md5 {
                return Err(format!(
                    "{} differs from its .foxbase: the game was modified outside foxinstall",
                    d.display()
                ));
            }
            m.base.insert(a.to_string(), sig);
        }
        self.save_manifest(&m)?;
        self.say("set up: base archives recorded");
        Ok(())
    }

    // -------------------------------------------------------------------------------------------- journal

    fn journal(&self) -> PathBuf {
        self.state().join("journal.json")
    }

    /// finish an interrupted commit (roll forward: every temp file listed is renamed into place)
    fn recover(&self) -> Result<(), String> {
        for a in ARCHIVES {
            let jp = self.state().join(format!("inplace_{a}.journal"));
            if let Ok(b) = fs::read(&jp) {
                // an in-place update was interrupted before its manifest: restore the old table region and length
                let len = u64::from_le_bytes(b[..8].try_into().unwrap());
                let mut f = fs::OpenOptions::new()
                    .write(true)
                    .open(self.dat(a))
                    .map_err(|e| e.to_string())?;
                f.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
                f.write_all(&b[8..]).map_err(|e| e.to_string())?;
                f.set_len(len).map_err(|e| e.to_string())?;
                f.sync_all().map_err(|e| e.to_string())?;
                fs::remove_file(&jp).map_err(|e| e.to_string())?;
            }
        }
        let j = self.journal();
        if !j.exists() {
            return Ok(());
        }
        let pairs: Vec<(String, String)> =
            serde_json::from_slice(&fs::read(&j).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        for (tmp, dst) in &pairs {
            let (tmp, dst) = (PathBuf::from(tmp), PathBuf::from(dst));
            if tmp.exists() {
                fs::rename(&tmp, &dst).map_err(|e| format!("recover {}: {e}", dst.display()))?;
            }
        }
        fs::remove_file(&j).map_err(|e| e.to_string())
    }

    /// commit: journal, then every rename, then the manifest, then drop the journal
    fn commit(
        &mut self,
        mut renames: Vec<(PathBuf, PathBuf)>,
        deletes: Vec<PathBuf>,
        m: &Manifest,
    ) -> Result<(), String> {
        if m.mode == "snakebite" {
            let x = self.stage_snakebite_xml(m, &renames)?;
            renames.push(x);
        }
        let pairs: Vec<(String, String)> = renames
            .iter()
            .map(|(a, b)| (a.display().to_string(), b.display().to_string()))
            .collect();
        fs::write(self.journal(), serde_json::to_vec(&pairs).unwrap())
            .map_err(|e| e.to_string())?;
        for (tmp, dst) in &renames {
            fs::rename(tmp, dst).map_err(|e| format!("{}: {e}", dst.display()))?;
        }
        for d in deletes {
            let _ = fs::remove_file(&d);
            // folders a mod created and that are now empty go too (never the game root)
            let mut p = d.parent().map(|x| x.to_path_buf());
            while let Some(dir) = p {
                if dir == self.root || !dir.starts_with(&self.root) || fs::remove_dir(&dir).is_err()
                {
                    break;
                }
                p = dir.parent().map(|x| x.to_path_buf());
            }
        }
        self.save_manifest(m)?;
        for j in self.inplace_journals.drain(..) {
            let _ = fs::remove_file(j);
        }
        fs::remove_file(self.journal()).map_err(|e| e.to_string())
    }

    // -------------------------------------------------------------------------------------------- vanilla packs

    /// vanilla lookup order for merges: the base archives, then the chunk archives of the source root
    fn vanilla_sources(&self) -> Result<Vec<Vanilla>, String> {
        let mut v = vec![
            Vanilla::Qar(Lazy::open(self.qar_context()?, &self.base("00"))?),
            Vanilla::Qar(Lazy::open(self.qar_context()?, &self.base("01"))?),
        ];
        if !self.vanilla_dirs.is_empty() {
            v.push(Vanilla::Dir(DirSource::open(&self.vanilla_dirs)));
            return Ok(v);
        }
        let master = self.source_root.join("master");
        for n in [
            "a_chunk7.dat",
            "a_texture7.dat",
            "chunk0.dat",
            "chunk1.dat",
            "chunk2.dat",
            "chunk3.dat",
            "chunk4.dat",
        ] {
            let p = master.join(n);
            if p.exists() {
                v.push(Vanilla::Qar(Lazy::open(self.qar_context()?, &p)?));
            }
        }
        Ok(v)
    }

    fn shadow_put(&self, b: &[u8]) -> Result<String, String> {
        let m = md5_hex(b);
        let p = self.state().join("shadow").join(&m);
        if !p.exists() {
            fs::write(&p, b).map_err(|e| e.to_string())?;
        }
        Ok(m)
    }

    fn shadow_get(&self, m: &str) -> Option<Vec<u8>> {
        fs::read(self.state().join("shadow").join(m)).ok()
    }

    // -------------------------------------------------------------------------------------------- install

    pub fn install(&mut self, pkg_path: &Path, force: bool) -> Result<(), String> {
        self.install_opts(pkg_path, force, false)
    }

    /// replace = an installed mod of the same name is swapped for this package in ONE rewrite, at its position in the
    /// install order (its layering against the other mods is kept); its loose files the package no longer has are
    /// restored / removed exactly as an uninstall would.
    pub fn install_opts(
        &mut self,
        pkg_path: &Path,
        force: bool,
        replace: bool,
    ) -> Result<(), String> {
        self.begin_op("install", PLAN_INSTALL);
        let mut tt = std::time::Instant::now();
        let opened = if pkg_path.is_dir() {
            mgsv::ModPackage::open_dir(pkg_path).map(|p| (p, "staging folder".to_string()))
        } else {
            mgsv::ModPackage::open(pkg_path).and_then(|p| Ok((p, md5_file(pkg_path)?)))
        };
        let (pkg, md5) = match opened {
            Ok(x) => x,
            Err(e) => return self.finish_op(Err(e)),
        };
        self.tick("open package", &mut tt);
        self.install_package(pkg, md5, force, replace)
    }

    /// Install a package held in memory (mgsv::ModPackage::from_memory): what a builder (fox m3 packs --install)
    /// hands over without writing a mod tree or a .mgsv first. Same semantics as install_opts.
    pub fn install_files(
        &mut self,
        metadata_xml: &str,
        files: Vec<(String, Vec<u8>)>,
        force: bool,
        replace: bool,
    ) -> Result<(), String> {
        let pkg = mgsv::ModPackage::from_memory(metadata_xml, files)?;
        self.install_package(pkg, "in memory".into(), force, replace)
    }

    /// install an opened package (zip, staging folder or in memory); package_md5 is recorded in the manifest
    pub fn install_package(
        &mut self,
        pkg: mgsv::ModPackage,
        package_md5: String,
        force: bool,
        replace: bool,
    ) -> Result<(), String> {
        let r = self.install_package_op(pkg, package_md5, force, replace);
        self.finish_op(r)
    }

    fn install_package_op(
        &mut self,
        mut pkg: mgsv::ModPackage,
        package_md5: String,
        force: bool,
        replace: bool,
    ) -> Result<(), String> {
        if self.prog.borrow().op.map(|(o, _)| o) != Some("install") {
            self.begin_op("install", PLAN_INSTALL); // in memory / opened by the caller: no "open package" phase
        }
        self.preflight()?;
        let full = self.load_manifest()?;
        let t0 = std::time::Instant::now();
        let mut tt = std::time::Instant::now();
        let old_pos = full.mods.iter().position(|m| m.name == pkg.name);
        if old_pos.is_some() && !replace {
            return Err(format!(
                "'{}' is installed: uninstall it first, or install with --replace",
                pkg.name
            ));
        }
        let mut man = full.clone();
        let old_mod = old_pos.map(|i| man.mods.remove(i));
        for s in &pkg.skipped {
            self.say(format!(
                "note: skipped {s} (WMV movies are not supported yet)"
            ));
        }
        self.phase("vanilla tables");
        let mut vanilla = self.vanilla_sources()?;
        self.tick("vanilla tables", &mut tt);
        self.phase("current archive indexes");
        let mut current: Vec<Archive> = ARCHIVES
            .iter()
            .map(|a| Archive::open(self.qar_context()?, &self.dat(a)))
            .collect::<Result<_, _>>()?;
        self.tick("current archive indexes", &mut tt);
        self.phase("classify package files");
        let mut new_mod = InstalledMod {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            author: pkg.author.clone(),
            website: pkg.website.clone(),
            description: pkg.description.clone(),
            package_md5,
            installed: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            files: vec![],
            packs: vec![],
            loose: vec![],
            sb_entry: pkg.mod_entry_xml.clone(),
        };
        // classify the package's archive files; collect contents
        let mut new_content: HashMap<String, Vec<u8>> = HashMap::new(); // game path -> bytes (whole files)
        let mut new_inner: HashMap<(String, String), Vec<u8>> = HashMap::new(); // (pack, inner) -> bytes
        let mut conflicts = vec![];
        for (zn, gp) in pkg.archive_files.clone() {
            let data = pkg.read(&zn)?;
            let hash = qar::file_hash(&gp);
            let arch = archive_for(&gp);
            let vanilla_hit = is_pack(&gp).is_some() && {
                let mut hit = false;
                for v in vanilla.iter_mut() {
                    if v.content(hash)?.is_some() {
                        hit = true;
                        break;
                    }
                }
                hit
            };
            if let (Some(_), true) = (is_pack(&gp), vanilla_hit) {
                let p = fpk::read(&data).map_err(|e| format!("{gp}: {e}"))?;
                let mut item = PackItem {
                    pack: gp.clone(),
                    archive: arch.into(),
                    entries: vec![],
                    references: p.references.clone(),
                };
                for e in &p.entries {
                    let raw = &data[e.offset as usize..(e.offset + e.size) as usize];
                    // SnakeBite mode: merged packs carry entries as GzsTool unpacks them (obfuscated scripts
                    // decrypted), as SnakeBite's own merge does and as its ContentHash records them
                    let b = if man.mode == "snakebite" {
                        fpk::gzs_view(&e.path, raw).into_owned()
                    } else {
                        raw.to_vec()
                    };
                    item.entries.push((e.path.clone(), md5_hex(&b)));
                    for m in &man.mods {
                        if m.packs
                            .iter()
                            .any(|q| q.pack == gp && q.entries.iter().any(|(ip, _)| ip == &e.path))
                        {
                            conflicts.push(format!("{gp} :: {} (also in '{}')", e.path, m.name));
                        }
                    }
                    new_inner.insert((gp.clone(), e.path.clone()), b);
                }
                new_mod.packs.push(item);
            } else {
                for m in &man.mods {
                    if m.files.iter().any(|f| f.path.eq_ignore_ascii_case(&gp)) {
                        conflicts.push(format!("{gp} (also in '{}')", m.name));
                    }
                }
                new_mod.files.push(FileItem {
                    path: gp.clone(),
                    hash: format!("{hash:016x}"),
                    archive: arch.into(),
                    md5: md5_hex(&data),
                });
                new_content.insert(gp, data);
            }
        }
        for (_zn, rel) in &pkg.loose_files {
            for m in &man.mods {
                if m.loose.iter().any(|l| l.path.eq_ignore_ascii_case(rel)) {
                    conflicts.push(format!("GameDir/{rel} (also in '{}')", m.name));
                }
            }
        }
        if !conflicts.is_empty() {
            for c in &conflicts {
                self.say(format!("conflict: {c}"));
            }
            if !force {
                return Err(format!(
                    "{} conflict(s) with installed mods: nothing written (--force: this mod wins; \
                                    the shadowed content is kept so any uninstall order stays exact)",
                    conflicts.len()
                ));
            }
            // keep the shadowed content of earlier mods (exact restore whatever the uninstall order)
            self.shadow_earlier(&man, &mut current, &new_mod)?;
        }
        // loose files: stage into temps, back up replaced originals
        let mut renames = vec![];
        for (zn, rel) in pkg.loose_files.clone() {
            let data = pkg.read(&zn)?;
            let dst = self.root.join(&rel);
            let owned_by_mod = man
                .mods
                .iter()
                .any(|m| m.loose.iter().any(|l| l.path.eq_ignore_ascii_case(&rel)));
            let was_ours = old_mod
                .as_ref()
                .and_then(|o| o.loose.iter().find(|l| l.path.eq_ignore_ascii_case(&rel)));
            let replaced_original = match was_ours {
                Some(l) => l.replaced_original,
                None => dst.exists() && !owned_by_mod,
            };
            if replaced_original {
                let b = self.state().join("backup").join(&rel);
                if !b.exists() {
                    fs::create_dir_all(b.parent().unwrap()).map_err(|e| e.to_string())?;
                    fs::copy(&dst, &b).map_err(|e| e.to_string())?;
                }
            }
            if owned_by_mod {
                self.shadow_put(&fs::read(&dst).map_err(|e| e.to_string())?)?;
            }
            fs::create_dir_all(dst.parent().unwrap()).map_err(|e| e.to_string())?;
            let tmp = self.tmp(dst.with_extension(format!(
                "{}.foxtmp",
                dst.extension().and_then(|x| x.to_str()).unwrap_or("")
            )));
            fs::write(&tmp, &data).map_err(|e| e.to_string())?;
            new_mod.loose.push(LooseItem {
                path: rel.clone(),
                md5: md5_hex(&data),
                replaced_original,
            });
            renames.push((tmp, dst));
        }
        // replace: the old mod's loose files the new package no longer has
        let mut deletes = vec![];
        if let Some(old) = &old_mod {
            for l in &old.loose {
                if new_mod
                    .loose
                    .iter()
                    .any(|n| n.path.eq_ignore_ascii_case(&l.path))
                {
                    continue;
                }
                let dst = self.root.join(&l.path);
                let other = man.mods.iter().rev().find_map(|m| {
                    m.loose
                        .iter()
                        .find(|x| x.path.eq_ignore_ascii_case(&l.path))
                });
                let tmp = self.tmp(dst.with_extension("foxtmp"));
                if let Some(o) = other {
                    let b = self
                        .shadow_get(&o.md5)
                        .ok_or_else(|| format!("missing content for {} of another mod", l.path))?;
                    fs::write(&tmp, b).map_err(|e| e.to_string())?;
                    renames.push((tmp, dst));
                } else if l.replaced_original {
                    fs::copy(self.state().join("backup").join(&l.path), &tmp)
                        .map_err(|e| e.to_string())?;
                    renames.push((tmp, dst));
                } else {
                    deletes.push(dst);
                }
            }
        }
        self.tick("classify package files", &mut tt);
        self.phase("write archives");
        let mut target = man.clone();
        match old_pos {
            Some(i) => target.mods.insert(i, new_mod),
            None => target.mods.push(new_mod),
        }
        let stats = self.write_targets(
            &target,
            &mut current,
            &mut vanilla,
            &new_content,
            &new_inner,
            &mut renames,
        )?;
        self.tick("write archives", &mut tt);
        drop(current);
        drop(vanilla);
        self.phase("commit");
        self.commit(renames, deletes, &target)?;
        self.tick("commit", &mut tt);
        self.say(format!(
            "{} '{}' {}: {} in {:.1} s",
            if old_pos.is_some() {
                "replaced"
            } else {
                "installed"
            },
            pkg.name,
            pkg.version,
            stats,
            t0.elapsed().as_secs_f64()
        ));
        Ok(())
    }

    /// before a forced install: store every earlier mod's file / inner-file content the new mod overrides
    fn shadow_earlier(
        &self,
        man: &Manifest,
        current: &mut [Archive],
        new_mod: &InstalledMod,
    ) -> Result<(), String> {
        for f in &new_mod.files {
            let h = qar::file_hash(&f.path);
            let ai = ARCHIVES.iter().position(|a| *a == f.archive).unwrap();
            if let Some(&i) = current[ai].by_hash.get(&h) {
                let c = current[ai].content(i)?;
                self.shadow_put(&c)?;
            }
        }
        for p in &new_mod.packs {
            let h = qar::file_hash(&p.pack);
            let ai = ARCHIVES.iter().position(|a| *a == p.archive).unwrap();
            if man
                .mods
                .iter()
                .any(|m| m.packs.iter().any(|q| q.pack == p.pack))
                && let Some(&i) = current[ai].by_hash.get(&h)
            {
                let pb = current[ai].content(i)?;
                let pk = fpk::read(&pb)?;
                for e in &pk.entries {
                    if p.entries.iter().any(|(ip, _)| ip == &e.path) {
                        self.shadow_put(&pb[e.offset as usize..(e.offset + e.size) as usize])?;
                    }
                }
            }
        }
        Ok(())
    }

    /// switch the archive layout ("gzstool" | "inplace"): one full rewrite of both archives
    pub fn set_layout(&mut self, layout: &str) -> Result<(), String> {
        let r = self.set_layout_op(layout);
        self.finish_op(r)
    }

    fn set_layout_op(&mut self, layout: &str) -> Result<(), String> {
        if layout != "gzstool" && layout != "inplace" {
            return Err("layout: gzstool | inplace".into());
        }
        self.begin_op("layout", PLAN_LAYOUT);
        self.preflight()?;
        let mut target = self.load_manifest()?;
        target.layout = layout.into();
        let t0 = std::time::Instant::now();
        let mut renames = vec![];
        if !target.mods.is_empty() {
            self.phase("vanilla tables");
            let mut vanilla = self.vanilla_sources()?;
            self.phase("current archive indexes");
            let mut current: Vec<Archive> = ARCHIVES
                .iter()
                .map(|a| Archive::open(self.qar_context()?, &self.dat(a)))
                .collect::<Result<_, _>>()?;
            self.phase("write archives");
            self.force_rewrite = true;
            // a layout switch never edits in place: rewrite with the new layout's table
            let saved = target.layout.clone();
            target.layout = if layout == "inplace" {
                "inplace-rewrite".into()
            } else {
                layout.into()
            };
            let stats = self.write_targets(
                &target,
                &mut current,
                &mut vanilla,
                &HashMap::new(),
                &HashMap::new(),
                &mut renames,
            );
            self.force_rewrite = false;
            target.layout = saved;
            let stats = stats?;
            drop(current);
            drop(vanilla);
            self.phase("commit");
            self.commit(renames, vec![], &target)?;
            self.say(format!(
                "layout {layout}: {stats} in {:.1} s",
                t0.elapsed().as_secs_f64()
            ));
        } else {
            self.commit(renames, vec![], &target)?;
            self.say(format!(
                "layout {layout} (no mods installed: applies from the next install)"
            ));
        }
        Ok(())
    }

    // -------------------------------------------------------------------------------------------- uninstall

    pub fn uninstall(&mut self, name: &str) -> Result<(), String> {
        let r = self.uninstall_op(name);
        self.finish_op(r)
    }

    fn uninstall_op(&mut self, name: &str) -> Result<(), String> {
        self.begin_op("uninstall", PLAN_UNINSTALL);
        self.preflight()?;
        let man = self.load_manifest()?;
        let t0 = std::time::Instant::now();
        let pos = man
            .mods
            .iter()
            .position(|m| m.name == name)
            .ok_or_else(|| format!("'{name}' is not installed"))?;
        let gone = man.mods[pos].clone();
        let mut target = man.clone();
        target.mods.remove(pos);
        if target.mods.is_empty() {
            self.begin_op("uninstall", PLAN_UNINSTALL_BASE);
        }
        let mut renames = vec![];
        let mut deletes = vec![];
        // loose files: the next owner's content (a later/earlier mod), else the backup, else delete
        for l in &gone.loose {
            let dst = self.root.join(&l.path);
            let other = target.mods.iter().rev().find_map(|m| {
                m.loose
                    .iter()
                    .find(|x| x.path.eq_ignore_ascii_case(&l.path))
            });
            let tmp = self.tmp(dst.with_extension("foxtmp"));
            if let Some(o) = other {
                let b = self
                    .shadow_get(&o.md5)
                    .or_else(|| fs::read(&dst).ok().filter(|b| md5_hex(b) == o.md5))
                    .ok_or_else(|| format!("missing content for {} of another mod", l.path))?;
                fs::write(&tmp, b).map_err(|e| e.to_string())?;
                renames.push((tmp, dst));
            } else if l.replaced_original {
                fs::copy(self.state().join("backup").join(&l.path), &tmp)
                    .map_err(|e| e.to_string())?;
                renames.push((tmp, dst));
            } else {
                deletes.push(dst);
            }
        }
        if target.mods.is_empty() {
            // no mods left: the base archives, byte for byte
            self.phase("restore base archives");
            let mut total = 0;
            for a in ARCHIVES {
                total += fs::metadata(self.base(a))
                    .map_err(|e| format!("{}: {e}", self.base(a).display()))?
                    .len();
            }
            let mut meter = ByteMeter::new(total);
            for a in ARCHIVES {
                let tmp = self.tmp(self.dat(a).with_extension("dat.foxtmp"));
                self.phase_frac(meter.frac(), Some(&format!("restore {a}.dat")));
                copy_progress(&self.base(a), &tmp, |n| {
                    if let Some(f) = meter.add(n) {
                        self.phase_frac(f, None);
                    }
                })?;
                renames.push((tmp, self.dat(a)));
            }
            self.phase("commit");
            self.commit(renames, deletes, &target)?;
            self.say(format!(
                "uninstalled '{name}': base archives restored in {:.1} s",
                t0.elapsed().as_secs_f64()
            ));
            return Ok(());
        }
        self.phase("vanilla tables");
        let mut vanilla = self.vanilla_sources()?;
        self.phase("current archive indexes");
        let mut current: Vec<Archive> = ARCHIVES
            .iter()
            .map(|a| Archive::open(self.qar_context()?, &self.dat(a)))
            .collect::<Result<_, _>>()?;
        let mut tt = std::time::Instant::now();
        self.phase("write archives");
        let stats = self.write_targets(
            &target,
            &mut current,
            &mut vanilla,
            &HashMap::new(),
            &HashMap::new(),
            &mut renames,
        )?;
        self.tick("write archives", &mut tt);
        drop(current);
        drop(vanilla);
        self.phase("commit");
        self.commit(renames, deletes, &target)?;
        self.say(format!(
            "uninstalled '{name}': {} in {:.1} s",
            stats,
            t0.elapsed().as_secs_f64()
        ));
        Ok(())
    }

    // -------------------------------------------------------------------------------------------- target archives

    #[allow(clippy::too_many_arguments)]
    fn write_targets(
        &mut self,
        target: &Manifest,
        current: &mut [Archive],
        vanilla: &mut [Vanilla],
        new_content: &HashMap<String, Vec<u8>>,
        new_inner: &HashMap<(String, String), Vec<u8>>,
        renames: &mut Vec<(PathBuf, PathBuf)>,
    ) -> Result<String, String> {
        let mut summary = vec![];
        let before = self.load_manifest().unwrap_or_default(); // none yet: adoption (setup --snakebite)
        let touches = |m: &Manifest, a: &str| {
            m.mods.iter().any(|x| {
                x.files.iter().any(|f| f.archive == a) || x.packs.iter().any(|p| p.archive == a)
            })
        };
        let todo: Vec<bool> = ARCHIVES
            .iter()
            .map(|a| self.force_rewrite || touches(target, a) || touches(&before, a))
            .collect();
        // progress: each archive written gets a share of "write archives" by its base size
        let weights: Vec<f32> = ARCHIVES
            .iter()
            .zip(&todo)
            .map(|(a, t)| {
                if *t {
                    fs::metadata(self.base(a))
                        .map(|m| m.len() as f32)
                        .unwrap_or(1.0)
                        .max(1.0)
                } else {
                    0.0
                }
            })
            .collect();
        let wsum: f32 = weights.iter().sum::<f32>().max(1.0);
        let mut wdone = 0.0f32;
        for (ai, a) in ARCHIVES.iter().enumerate() {
            if !todo[ai] {
                summary.push(format!("{a}.dat unchanged"));
                continue;
            }
            self.phase_span(wdone / wsum, weights[ai] / wsum);
            wdone += weights[ai];
            self.phase_frac(0.0, Some(&format!("merge packs {a}.dat")));
            let mut base = Archive::open(self.qar_context()?, &self.base(a))?;
            // last writer wins per hash: whole files and merged packs of every mod for this archive
            let mut files: BTreeMap<u64, (usize, FileItem)> = BTreeMap::new();
            let mut packs: BTreeMap<String, Vec<(usize, PackItem)>> = BTreeMap::new();
            for (mi, m) in target.mods.iter().enumerate() {
                for f in m.files.iter().filter(|f| f.archive == *a) {
                    files.insert(qar::file_hash(&f.path), (mi, f.clone()));
                }
                for p in m.packs.iter().filter(|p| p.archive == *a) {
                    packs
                        .entry(p.pack.clone())
                        .or_default()
                        .push((mi, p.clone()));
                }
            }
            let pack_hashes: BTreeSet<u64> = packs.keys().map(|p| qar::file_hash(p)).collect();
            let mut out: Vec<(u64, Src)> = Vec::new();
            let mut n_base = 0;
            for i in 0..base.index.entries.len() {
                let h = base.index.entries[i].hash;
                if files.contains_key(&h) || pack_hashes.contains(&h) {
                    continue;
                }
                out.push((h, Src::Base(i)));
                n_base += 1;
            }
            // whole files, in mod order then package order
            let mut ordered: Vec<&(usize, FileItem)> = files.values().collect();
            ordered.sort_by_key(|(mi, f)| {
                let pos = target.mods[*mi]
                    .files
                    .iter()
                    .position(|x| x.path == f.path)
                    .unwrap_or(0);
                (*mi, pos)
            });
            for (_mi, f) in ordered {
                let h = qar::file_hash(&f.path);
                let src = match new_content.get(&f.path).filter(|c| md5_hex(c) == f.md5) {
                    Some(c) => Src::Bytes(self.qar_context()?.encode_plain(h, c)?.1),
                    None => {
                        // an unchanged raw block is copied as it is (keeps its original encoding)
                        let cur = current[ai].by_hash.get(&h).copied();
                        match cur {
                            Some(i) if current[ai].content_md5(i)? == f.md5 => Src::Cur(i),
                            _ => {
                                let c = self.shadow_get(&f.md5).ok_or_else(|| {
                                    format!("content of {} (md5 {}) not found", f.path, f.md5)
                                })?;
                                Src::Bytes(self.qar_context()?.encode_plain(h, &c)?.1)
                            }
                        }
                    }
                };
                out.push((h, src));
            }
            // merged packs: vanilla pack + each mod's inner files in order
            let n_packs = packs.len().max(1) as f32;
            for (pi, (pack, contribs)) in packs.iter().enumerate() {
                self.phase_frac(MERGE_SHARE * pi as f32 / n_packs, None);
                let h = qar::file_hash(pack);
                let kind = is_pack(pack).unwrap();
                let mut vb: Option<Vec<u8>> = None;
                for v in vanilla.iter_mut() {
                    if let Some(c) = v.content(h)? {
                        self.merge_sources.insert(pack.clone(), v.name());
                        vb = Some(c);
                        break;
                    }
                }
                let vb = vb.ok_or_else(|| format!("vanilla pack {pack} not found"))?;
                let vp = fpk::read(&vb)?;
                self.merge_vanilla_entries.insert(
                    pack.clone(),
                    vp.entries.iter().map(|e| e.path.clone()).collect(),
                );
                // SnakeBite mode: the vanilla entries as GzsTool unpacks them (SnakeBite merges through GzsTool, so its
                // merged packs hold the vanilla scripts decrypted); native mode keeps the vanilla bytes
                let gzs = target.mode == "snakebite";
                let mut entries: Vec<(String, Vec<u8>)> = vp
                    .entries
                    .iter()
                    .map(|e| {
                        let raw = &vb[e.offset as usize..(e.offset + e.size) as usize];
                        (
                            e.path.clone(),
                            if gzs {
                                fpk::gzs_view(&e.path, raw).into_owned()
                            } else {
                                raw.to_vec()
                            },
                        )
                    })
                    .collect();
                let mut refs = vp.references.clone();
                // the current merged pack (for earlier mods' inner contents)
                let cur_pack = match current[ai].by_hash.get(&h).copied() {
                    Some(i) => Some(current[ai].content(i)?),
                    None => None,
                };
                let cur_parsed = cur_pack.as_ref().map(|b| fpk::read(b)).transpose()?;
                let mut mod_written: HashMap<String, usize> = HashMap::new(); // gzs mode: windows key -> entry
                for (_mi, item) in contribs {
                    for (ip, m5) in &item.entries {
                        let b = if let Some(b) = new_inner
                            .get(&(pack.clone(), ip.clone()))
                            .filter(|b| &md5_hex(b) == m5)
                        {
                            b.clone()
                        } else {
                            let from_cur = cur_parsed.as_ref().and_then(|cp| {
                                cp.entries.iter().find(|e| &e.path == ip).map(|e| {
                                    cur_pack.as_ref().unwrap()
                                        [e.offset as usize..(e.offset + e.size) as usize]
                                        .to_vec()
                                })
                            });
                            match from_cur {
                                Some(b) if &md5_hex(&b) == m5 => b,
                                _ => self.shadow_get(m5).ok_or_else(|| {
                                    format!("content of {pack} :: {ip} not found")
                                })?,
                            }
                        };
                        let at = match entries.iter().position(|(p, _)| p == ip) {
                            Some(i) => {
                                entries[i].1 = b;
                                i
                            }
                            None => {
                                entries.push((ip.clone(), b));
                                entries.len() - 1
                            }
                        };
                        mod_written.insert(windows_key(ip), at);
                    }
                    for r in &item.references {
                        if !refs.contains(r) {
                            refs.push(r.clone());
                        }
                    }
                }
                if gzs {
                    gzs_folder_collisions(&mut entries, &mod_written);
                }
                let e: Vec<(&str, &[u8])> = entries
                    .iter()
                    .map(|(p, b)| (p.as_str(), b.as_slice()))
                    .collect();
                let r: Vec<&str> = refs.iter().map(|s| s.as_str()).collect();
                let merged = self.pack_order()?.write(kind, &e, &r)?;
                // unchanged merged pack: keep the current block
                let src = match current[ai].by_hash.get(&h).copied() {
                    Some(i) if current[ai].content_md5(i)? == md5_hex(&merged) => Src::Cur(i),
                    _ => Src::Bytes(self.qar_context()?.encode_plain(h, &merged)?.1),
                };
                out.push((h, src));
            }
            self.phase_frac(MERGE_SHARE, Some(&format!("write {a}.dat")));
            let how = if target.layout == "inplace-rewrite" {
                self.emit_full(a, &out, &mut base, &mut current[ai], true, renames)?
            } else if target.layout == "inplace" {
                match self.emit_inplace(a, &out, &mut base, &mut current[ai])? {
                    Some(how) => how,
                    None => self.emit_full(a, &out, &mut base, &mut current[ai], true, renames)?,
                }
            } else {
                self.emit_full(a, &out, &mut base, &mut current[ai], false, renames)?
            };
            summary.push(format!(
                "{a}.dat {} entries ({} base, {} mod files, {} merged packs; {how})",
                out.len(),
                n_base,
                files.len(),
                packs.len()
            ));
            self.phase_frac(1.0, None);
        }
        self.phase_span(0.0, 1.0);
        Ok(summary.join("; "))
    }

    fn src_len(src: &Src, base: &Archive, cur: &Archive) -> u64 {
        match src {
            Src::Base(i) => base.index.entries[*i].raw_len(),
            Src::Cur(i) => cur.index.entries[*i].raw_len(),
            Src::Bytes(b) => b.len() as u64,
        }
    }

    fn src_raw(src: &Src, base: &mut Archive, cur: &mut Archive) -> Result<Vec<u8>, String> {
        match src {
            Src::Base(i) => base.raw(*i),
            Src::Cur(i) => cur.raw(*i),
            Src::Bytes(b) => Ok(b.clone()),
        }
    }

    /// full rewrite into a temp file (committed by rename). reserve = in-place layout (section-table slack)
    fn emit_full(
        &mut self,
        a: &str,
        plan: &[(u64, Src)],
        base: &mut Archive,
        cur: &mut Archive,
        reserve: bool,
        renames: &mut Vec<(PathBuf, PathBuf)>,
    ) -> Result<String, String> {
        let hdr = base.index.header.clone();
        let lens: Vec<u64> = plan
            .iter()
            .map(|(_, s)| Self::src_len(s, base, cur))
            .collect();
        let cap = if reserve {
            plan.len() + (plan.len() / 4).max(4096)
        } else {
            plan.len()
        };
        let (dof, offs, end) = qar::layout(&lens, cap, hdr.block_shift);
        let dst = self.dat(a);
        let tmp = self.tmp(dst.with_extension("dat.foxtmp"));
        let mut w =
            BufWriter::with_capacity(1 << 22, File::create(&tmp).map_err(|e| e.to_string())?);
        let ents: Vec<(u64, u64)> = plan.iter().zip(&offs).map(|((h, _), o)| (*h, *o)).collect();
        w.write_all(&self.qar_context()?.table_region(
            hdr.flags,
            hdr.version,
            &ents,
            dof,
            end,
            hdr.block_shift,
        ))
        .map_err(|e| e.to_string())?;
        let mut at = dof;
        let mut meter = ByteMeter::new(end.saturating_sub(dof));
        for (k, (_, s)) in plan.iter().enumerate() {
            let raw = Self::src_raw(s, base, cur)?;
            if let Some(f) = meter.add(offs.get(k + 1).copied().unwrap_or(end) - offs[k]) {
                self.phase_frac(MERGE_SHARE + (1.0 - MERGE_SHARE) * f, None);
            }
            w.write_all(&raw).map_err(|e| e.to_string())?;
            at += raw.len() as u64;
            let next = offs.get(k + 1).copied().unwrap_or(end);
            w.write_all(&vec![0u8; (next - at) as usize])
                .map_err(|e| e.to_string())?;
            at = next;
        }
        w.flush().map_err(|e| e.to_string())?;
        renames.push((tmp, dst));
        Ok(if reserve {
            format!("rewritten, table capacity {cap}")
        } else {
            "rewritten".into()
        })
    }

    /// in place: keep every block the target shares with the current file, write the rest into holes or at the end,
    /// then rewrite the table region. Ok(None) = the table capacity is too small (the caller rewrites in full).
    fn emit_inplace(
        &mut self,
        a: &str,
        plan: &[(u64, Src)],
        base: &mut Archive,
        cur: &mut Archive,
    ) -> Result<Option<String>, String> {
        let context = self.qar_context()?.clone();
        let hdr = cur.index.header.clone();
        let shift = hdr.block_shift;
        let align = 1u64 << shift;
        let dof = hdr.data_offset as u64;
        let cap = ((dof - 32) / 8) as usize;
        if plan.len() > cap {
            return Ok(None);
        }
        let base_hdr: HashMap<u64, [u8; 32]> = base
            .index
            .entries
            .iter()
            .map(|e| (e.hash, context.entry_header_bytes(e)))
            .collect();
        let mut keep: Vec<Option<u64>> = Vec::with_capacity(plan.len());
        for (h, s) in plan {
            let k = match s {
                Src::Cur(i) => Some(cur.index.entries[*i].offset),
                Src::Base(_) => cur.by_hash.get(h).and_then(|&i| {
                    let e = &cur.index.entries[i];
                    (base_hdr.get(h) == Some(&context.entry_header_bytes(e))).then_some(e.offset)
                }),
                Src::Bytes(_) => None,
            };
            keep.push(k);
        }
        let file_len = fs::metadata(self.dat(a)).map_err(|e| e.to_string())?.len();
        let mut spans: Vec<(u64, u64)> = plan
            .iter()
            .zip(&keep)
            .filter_map(|((_, s), k)| {
                k.map(|o| (o, (o + Self::src_len(s, base, cur)).div_ceil(align) * align))
            })
            .collect();
        spans.sort();
        let mut holes: Vec<(u64, u64)> = vec![];
        let mut at = dof;
        for (s0, s1) in &spans {
            if *s0 > at {
                holes.push((at, *s0));
            }
            at = at.max(*s1);
        }
        let mut end = at;
        // compaction: when holes would exceed a quarter of the file, rewrite in full instead
        let live: u64 = plan
            .iter()
            .map(|(_, s)| Self::src_len(s, base, cur).div_ceil(align) * align)
            .sum();
        if (live as f64) < 0.75 * (file_len.max(end) - dof) as f64 {
            return Ok(None);
        }
        // journal: old length + old table region (restored and truncated by recover())
        let jp = self.state().join(format!("inplace_{a}.journal"));
        let mut old_region = vec![0u8; dof as usize];
        {
            let mut f = File::open(self.dat(a)).map_err(|e| e.to_string())?;
            f.read_exact(&mut old_region).map_err(|e| e.to_string())?;
        }
        let mut j = file_len.to_le_bytes().to_vec();
        j.extend_from_slice(&old_region);
        fs::write(&jp, j).map_err(|e| e.to_string())?;
        let mut f = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.dat(a))
            .map_err(|e| e.to_string())?;
        let mut offs = vec![0u64; plan.len()];
        let mut written = 0u64;
        let to_write: u64 = plan
            .iter()
            .zip(&keep)
            .filter(|(_, k)| k.is_none())
            .map(|((_, s), _)| Self::src_len(s, base, cur).div_ceil(align) * align)
            .sum();
        let mut meter = ByteMeter::new(to_write);
        for (k, (_, s)) in plan.iter().enumerate() {
            if let Some(o) = keep[k] {
                offs[k] = o;
                continue;
            }
            let raw = Self::src_raw(s, base, cur)?;
            let need = (raw.len() as u64).div_ceil(align) * align;
            let o = match holes.iter().position(|(h0, h1)| h1 - h0 >= need) {
                Some(i) => {
                    let o = holes[i].0;
                    holes[i].0 += need;
                    o
                }
                None => {
                    // never inside the old file's live area: append after both the live end and the old length
                    let o = end.max(file_len);
                    end = o + need;
                    o
                }
            };
            f.seek(SeekFrom::Start(o)).map_err(|e| e.to_string())?;
            f.write_all(&raw).map_err(|e| e.to_string())?;
            f.write_all(&vec![0u8; (need - raw.len() as u64) as usize])
                .map_err(|e| e.to_string())?;
            offs[k] = o;
            written += need;
            if let Some(fr) = meter.add(need) {
                self.phase_frac(MERGE_SHARE + (1.0 - MERGE_SHARE) * fr, None);
            }
        }
        f.sync_data().map_err(|e| e.to_string())?;
        let last = offs
            .iter()
            .zip(plan)
            .map(|(o, (_, s))| (o + Self::src_len(s, base, cur)).div_ceil(align) * align)
            .max()
            .unwrap_or(dof);
        let ents: Vec<(u64, u64)> = plan.iter().zip(&offs).map(|((h, _), o)| (*h, *o)).collect();
        let region =
            self.qar_context()?
                .table_region(hdr.flags, hdr.version, &ents, dof, last, shift);
        f.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        f.write_all(&region).map_err(|e| e.to_string())?;
        let new_len = f.metadata().map_err(|e| e.to_string())?.len();
        if new_len > last {
            f.set_len(last).map_err(|e| e.to_string())?;
        }
        f.sync_all().map_err(|e| e.to_string())?;
        self.inplace_journals.push(jp);
        Ok(Some(format!(
            "in place, {:.1} MB written",
            written as f64 / 1e6
        )))
    }

    // -------------------------------------------------------------------------------------------- verify / list

    pub fn verify(&mut self) -> Result<bool, String> {
        self.require_profile()?;
        let context = self.qar_context()?.clone();
        let man = self.load_manifest()?;
        let mut ok = true;
        for a in ARCHIVES {
            let sig = &man.base[a];
            let b = self.base(a);
            if md5_file(&b)? != sig.md5 {
                self.say(format!("FAIL {} changed (base signature)", b.display()));
                ok = false;
            }
            if man.mods.is_empty() && md5_file(&self.dat(a))? != sig.md5 {
                self.say(format!("FAIL {a}.dat != base with no mods installed"));
                ok = false;
            }
        }
        if !man.mods.is_empty() {
            let mut current: Vec<Archive> = ARCHIVES
                .iter()
                .map(|a| Archive::open(self.qar_context()?, &self.dat(a)))
                .collect::<Result<_, _>>()?;
            // expected owner per hash: last mod wins
            let mut want: HashMap<(usize, u64), String> = HashMap::new();
            for m in &man.mods {
                for f in &m.files {
                    let ai = ARCHIVES.iter().position(|x| *x == f.archive).unwrap();
                    want.insert((ai, qar::file_hash(&f.path)), f.md5.clone());
                }
            }
            // every base entry no mod replaces is present with its exact header; nothing unexpected is present
            for (ai, a) in ARCHIVES.iter().enumerate() {
                let base = Archive::open(self.qar_context()?, &self.base(a))?;
                let mut owned: BTreeSet<u64> = BTreeSet::new();
                for m in &man.mods {
                    owned.extend(
                        m.files
                            .iter()
                            .filter(|f| f.archive == *a)
                            .map(|f| qar::file_hash(&f.path)),
                    );
                    owned.extend(
                        m.packs
                            .iter()
                            .filter(|p| p.archive == *a)
                            .map(|p| qar::file_hash(&p.pack)),
                    );
                }
                let mut missing = 0;
                for e in &base.index.entries {
                    if owned.contains(&e.hash) {
                        continue;
                    }
                    let same = current[ai].by_hash.get(&e.hash).map(|&i| {
                        context.entry_header_bytes(&current[ai].index.entries[i])
                            == context.entry_header_bytes(e)
                    });
                    if same != Some(true) {
                        missing += 1;
                    }
                }
                let known: BTreeSet<u64> = base
                    .index
                    .entries
                    .iter()
                    .map(|e| e.hash)
                    .chain(owned.iter().copied())
                    .collect();
                let extra = current[ai]
                    .index
                    .entries
                    .iter()
                    .filter(|e| !known.contains(&e.hash))
                    .count();
                let dup = current[ai].index.entries.len() - current[ai].by_hash.len();
                if missing + extra + dup > 0 {
                    self.say(format!("FAIL {a}.dat: {missing} base entries missing/changed, {extra} unexpected, {dup} duplicate"));
                    ok = false;
                }
            }
            for ((ai, h), m5) in &want {
                match current[*ai].by_hash.get(h).copied() {
                    Some(i) => {
                        if current[*ai].content_md5(i)? != *m5 {
                            self.say(format!("FAIL {h:016x}: content differs from the manifest"));
                            ok = false;
                        }
                    }
                    None => {
                        self.say(format!("FAIL {h:016x}: missing"));
                        ok = false;
                    }
                }
            }
        }
        for m in &man.mods {
            for l in &m.loose {
                let owner = man
                    .mods
                    .iter()
                    .rev()
                    .find(|x| x.loose.iter().any(|y| y.path.eq_ignore_ascii_case(&l.path)))
                    .unwrap();
                if owner.name != m.name {
                    continue;
                }
                match fs::read(self.root.join(&l.path)) {
                    Ok(b) if md5_hex(&b) == l.md5 => {}
                    _ => {
                        self.say(format!("FAIL loose file {} differs", l.path));
                        ok = false;
                    }
                }
            }
        }
        self.say(if ok { "verify: OK" } else { "verify: PROBLEMS" });
        Ok(ok)
    }
}

fn initial_context() -> Option<qar::Context> {
    #[cfg(feature = "internal-game-data")]
    {
        Some(qar::Context::internal())
    }
    #[cfg(not(feature = "internal-game-data"))]
    {
        None
    }
}

fn initial_order() -> Option<foxcore::runtime_data::PackOrder> {
    #[cfg(feature = "internal-game-data")]
    {
        Some(foxcore::runtime_data::PackOrder::internal())
    }
    #[cfg(not(feature = "internal-game-data"))]
    {
        None
    }
}
