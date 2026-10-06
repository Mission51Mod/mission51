//! Finding MGSV:TPP (Steam app 287700) without touching it: the Steam folder from the registry, its library list
//! (`steamapps/libraryfolders.vdf`), the app manifest (`appmanifest_287700.acf`: install folder, build id), plus a
//! cheap scan of the usual library locations on every drive. Everything here only reads.
use std::path::{Path, PathBuf};

pub const APP_ID: u32 = 287_700;
/// the game's folder name under steamapps/common when no manifest says otherwise
pub const DEFAULT_DIR: &str = "MGS_TPP";
pub const GAME_EXE: &str = "mgsvtpp.exe";

/// A KeyValues (VDF / ACF) node: a quoted string or a block of key / node pairs, in file order.
#[derive(Clone, Debug, PartialEq)]
pub enum Vdf {
    Str(String),
    Block(Vec<(String, Vdf)>),
}

impl Vdf {
    /// the first child with this key (case-insensitive, as Steam treats keys)
    pub fn get(&self, key: &str) -> Option<&Vdf> {
        match self {
            Vdf::Block(kv) => kv.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v),
            Vdf::Str(_) => None,
        }
    }
    pub fn str(&self) -> Option<&str> {
        match self {
            Vdf::Str(s) => Some(s),
            Vdf::Block(_) => None,
        }
    }
    pub fn children(&self) -> &[(String, Vdf)] {
        match self {
            Vdf::Block(kv) => kv,
            Vdf::Str(_) => &[],
        }
    }
}

/// Parse KeyValues text. Unquoted tokens, `//` comments and `[$WIN32]`-style conditions are tolerated; escapes `\\`,
/// `\"`, `\n`, `\t` are decoded. Unbalanced input gives what was read so far.
pub fn parse_vdf(text: &str) -> Vdf {
    let toks = tokenize(text);
    let mut i = 0;
    Vdf::Block(parse_block(&toks, &mut i))
}

#[derive(Debug, PartialEq)]
enum Tok {
    S(String),
    Open,
    Close,
}

fn tokenize(text: &str) -> Vec<Tok> {
    let mut out = vec![];
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '{' => out.push(Tok::Open),
            '}' => out.push(Tok::Close),
            '"' => {
                let mut s = String::new();
                while let Some(c) = it.next() {
                    match c {
                        '"' => break,
                        '\\' => match it.next() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(o) => s.push(o),
                            None => {}
                        },
                        o => s.push(o),
                    }
                }
                out.push(Tok::S(s));
            }
            '/' if it.peek() == Some(&'/') => {
                for c in it.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '[' => {
                // conditional ("[$WIN32]"): skip
                for c in it.by_ref() {
                    if c == ']' {
                        break;
                    }
                }
            }
            c if c.is_whitespace() => {}
            c => {
                let mut s = String::from(c);
                while let Some(&n) = it.peek() {
                    if n.is_whitespace() || n == '{' || n == '}' || n == '"' {
                        break;
                    }
                    s.push(n);
                    it.next();
                }
                out.push(Tok::S(s));
            }
        }
    }
    out
}

fn parse_block(t: &[Tok], i: &mut usize) -> Vec<(String, Vdf)> {
    let mut kv = vec![];
    while *i < t.len() {
        match &t[*i] {
            Tok::Close => {
                *i += 1;
                return kv;
            }
            Tok::Open => {
                // a block without a key: keep its content under ""
                *i += 1;
                kv.push((String::new(), Vdf::Block(parse_block(t, i))));
            }
            Tok::S(k) => {
                *i += 1;
                match t.get(*i) {
                    Some(Tok::S(v)) => {
                        kv.push((k.clone(), Vdf::Str(v.clone())));
                        *i += 1;
                    }
                    Some(Tok::Open) => {
                        *i += 1;
                        kv.push((k.clone(), Vdf::Block(parse_block(t, i))));
                    }
                    _ => kv.push((k.clone(), Vdf::Str(String::new()))),
                }
            }
        }
    }
    kv
}

/// Library folders listed in libraryfolders.vdf (new format: `"0" { "path" "D:\\SteamLibrary" ... }`; old format:
/// `"1" "D:\\SteamLibrary"`). In file order, without duplicates.
pub fn library_folders(vdf_text: &str) -> Vec<PathBuf> {
    let v = parse_vdf(vdf_text);
    let root = v.get("libraryfolders").or_else(|| v.get("LibraryFolders"));
    let mut out: Vec<PathBuf> = vec![];
    if let Some(r) = root {
        for (k, node) in r.children() {
            if !k.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let p = match node {
                Vdf::Str(s) => Some(s.clone()),
                Vdf::Block(_) => node.get("path").and_then(|p| p.str()).map(|s| s.to_string()),
            };
            if let Some(p) = p.filter(|p| !p.is_empty()) {
                let pb = PathBuf::from(p);
                if !out.iter().any(|x| same_path(x, &pb)) {
                    out.push(pb);
                }
            }
        }
    }
    out
}

/// The parts of an app manifest the setup needs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AppManifest {
    pub appid: u32,
    pub name: String,
    pub installdir: String,
    pub buildid: Option<u64>,
    /// StateFlags 4 = fully installed
    pub state_flags: Option<u32>,
    pub size_on_disk: Option<u64>,
}

pub fn parse_acf(text: &str) -> Option<AppManifest> {
    let v = parse_vdf(text);
    let s = v.get("AppState")?;
    let g = |k: &str| s.get(k).and_then(|x| x.str()).map(|x| x.to_string());
    Some(AppManifest {
        appid: g("appid")?.parse().ok()?,
        name: g("name").unwrap_or_default(),
        installdir: g("installdir").unwrap_or_default(),
        buildid: g("buildid").and_then(|x| x.parse().ok()),
        state_flags: g("StateFlags").and_then(|x| x.parse().ok()),
        size_on_disk: g("SizeOnDisk").and_then(|x| x.parse().ok()),
    })
}

/// where a candidate was found
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// a Steam library's app manifest
    Manifest,
    /// the usual folder name on a drive, without a manifest
    DriveScan,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub dir: PathBuf,
    pub library: PathBuf,
    pub source: Source,
    pub manifest: Option<AppManifest>,
    /// mgsvtpp.exe is there
    pub has_exe: bool,
}

/// Steam install folders: the registry (current user, then machine), then the default location.
pub fn steam_roots() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = vec![];
    let mut add = |p: PathBuf| {
        if p.join("steamapps").is_dir() && !v.iter().any(|x| same_path(x, &p)) {
            v.push(p);
        }
    };
    #[cfg(windows)]
    {
        if let Some(p) = reg::read_string(reg::HKCU, r"Software\Valve\Steam", "SteamPath") {
            add(PathBuf::from(p.replace('/', "\\")));
        }
        for key in [r"SOFTWARE\WOW6432Node\Valve\Steam", r"SOFTWARE\Valve\Steam"] {
            if let Some(p) = reg::read_string(reg::HKLM, key, "InstallPath") {
                add(PathBuf::from(p));
            }
        }
        add(PathBuf::from(r"C:\Program Files (x86)\Steam"));
        add(PathBuf::from(r"C:\Program Files\Steam"));
    }
    #[cfg(not(windows))]
    if let Some(h) = std::env::var_os("HOME") {
        add(PathBuf::from(&h).join(".steam/steam"));
        add(PathBuf::from(&h).join(".local/share/Steam"));
    }
    v
}

/// Every library of the given Steam roots (each root is a library too).
pub fn libraries(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut libs: Vec<PathBuf> = vec![];
    let mut push = |p: PathBuf| {
        if !libs.iter().any(|x| same_path(x, &p)) {
            libs.push(p);
        }
    };
    for r in roots {
        push(r.clone());
        for f in ["steamapps/libraryfolders.vdf", "config/libraryfolders.vdf"] {
            if let Ok(t) = std::fs::read_to_string(r.join(f)) {
                for l in library_folders(&t) {
                    push(l);
                }
            }
        }
    }
    libs
}

/// The game in these libraries (by manifest), then the usual folders on every drive (no manifest needed).
pub fn find_game(libs: &[PathBuf], scan_drives: bool) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = vec![];
    for lib in libs {
        let acf = lib.join("steamapps").join(format!("appmanifest_{APP_ID}.acf"));
        let Ok(t) = std::fs::read_to_string(&acf) else { continue };
        let Some(m) = parse_acf(&t) else { continue };
        let dir = lib.join("steamapps").join("common").join(if m.installdir.is_empty() { DEFAULT_DIR } else { &m.installdir });
        if out.iter().any(|c| same_path(&c.dir, &dir)) {
            continue;
        }
        out.push(Candidate { has_exe: dir.join(GAME_EXE).is_file(), dir, library: lib.clone(), source: Source::Manifest, manifest: Some(m) });
    }
    if scan_drives {
        for lib in drive_libraries() {
            let dir = lib.join("steamapps").join("common").join(DEFAULT_DIR);
            if dir.join(GAME_EXE).is_file() && !out.iter().any(|c| same_path(&c.dir, &dir)) {
                out.push(Candidate { dir, library: lib, source: Source::DriveScan, manifest: None, has_exe: true });
            }
        }
    }
    out
}

/// the usual library folders on each fixed drive letter (existence checks only)
fn drive_libraries() -> Vec<PathBuf> {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut v = vec![];
    #[cfg(windows)]
    for d in b'C'..=b'Z' {
        let root = PathBuf::from(format!("{}:\\", d as char));
        if !root.is_dir() {
            continue;
        }
        for sub in ["SteamLibrary", "Steam", r"Program Files (x86)\Steam", r"Program Files\Steam", r"Games\Steam", r"Games\SteamLibrary"] {
            let p = root.join(sub);
            if p.join("steamapps").is_dir() {
                v.push(p);
            }
        }
    }
    v
}

/// case-insensitive, separator-agnostic path equality (Windows semantics)
pub fn same_path(a: &Path, b: &Path) -> bool {
    norm(a) == norm(b)
}

pub fn norm(p: &Path) -> String {
    let s = p.to_string_lossy().replace('/', "\\");
    let s = s.trim_start_matches(r"\\?\");
    s.trim_end_matches('\\').to_lowercase()
}

/// `inner` is `outer` or below it (after normalising; no file-system access)
pub fn is_within(inner: &Path, outer: &Path) -> bool {
    let (i, o) = (norm(inner), norm(outer));
    !o.is_empty() && (i == o || i.starts_with(&format!("{o}\\")))
}

#[cfg(windows)]
mod reg {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    pub const HKCU: HKEY = HKEY_CURRENT_USER;
    pub const HKLM: HKEY = HKEY_LOCAL_MACHINE;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// a REG_SZ value (read only)
    pub fn read_string(root: HKEY, key: &str, value: &str) -> Option<String> {
        let (k, v) = (wide(key), wide(value));
        let mut buf = vec![0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        let rc = unsafe {
            RegGetValueW(root, k.as_ptr(), v.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len)
        };
        if rc != 0 {
            return None;
        }
        let n = (len as usize / 2).min(buf.len());
        let s = String::from_utf16_lossy(&buf[..n]);
        let s = s.trim_end_matches('\0').to_string();
        (!s.is_empty()).then_some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIBS_NEW: &str = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"C:\\Program Files (x86)\\Steam"
		"label"		""
		"apps"
		{
			"228980"		"0"
		}
	}
	"1"
	{
		"path"		"D:\\SteamLibrary"
		"apps"
		{
			"287700"		"25153478104"
		}
	}
}
"#;

    const LIBS_OLD: &str = "\"LibraryFolders\"\n{\n\t\"TimeNextStatsReport\"\t\"1600000000\"\n\t\"ContentStatsID\"\t\"-1\"\n\t\"1\"\t\"E:\\\\Games\\\\Steam\"\n}\n";

    const ACF: &str = r#"
"AppState"
{
	"appid"		"287700"
	"universe"		"1"
	"name"		"METAL GEAR SOLID V: THE PHANTOM PAIN"
	"StateFlags"		"4"
	"installdir"		"MGS_TPP"
	"SizeOnDisk"		"25153478104"
	"buildid"		"6436034"
	"UserConfig"
	{
		"language"		"english"
	}
}
"#;

    #[test]
    fn library_folders_new_and_old() {
        assert_eq!(library_folders(LIBS_NEW), vec![PathBuf::from(r"C:\Program Files (x86)\Steam"), PathBuf::from(r"D:\SteamLibrary")]);
        assert_eq!(library_folders(LIBS_OLD), vec![PathBuf::from(r"E:\Games\Steam")]);
        assert!(library_folders("garbage {{{").is_empty());
    }

    #[test]
    fn acf_fields() {
        let m = parse_acf(ACF).unwrap();
        assert_eq!(m.appid, APP_ID);
        assert_eq!(m.installdir, "MGS_TPP");
        assert_eq!(m.buildid, Some(6436034));
        assert_eq!(m.state_flags, Some(4));
        assert_eq!(m.size_on_disk, Some(25153478104));
        assert!(m.name.contains("PHANTOM PAIN"));
        assert!(parse_acf("\"Other\" { }").is_none());
    }

    #[test]
    fn vdf_comments_conditions_and_unquoted() {
        let v = parse_vdf("// c\nroot { a 1 \"b\" \"x\\\"y\" [$WIN32] c { d \"e\" } }");
        let r = v.get("root").unwrap();
        assert_eq!(r.get("a").and_then(|x| x.str()), Some("1"));
        assert_eq!(r.get("B").and_then(|x| x.str()), Some("x\"y"));
        assert_eq!(r.get("c").and_then(|c| c.get("d")).and_then(|x| x.str()), Some("e"));
    }

    #[test]
    fn find_game_in_a_synthetic_library() {
        let d = std::env::temp_dir().join(format!("foxstudio_steam_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let lib = d.join("lib");
        let game = lib.join("steamapps/common/MGS_TPP");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join(GAME_EXE), b"x").unwrap();
        std::fs::write(lib.join("steamapps/appmanifest_287700.acf"), ACF).unwrap();
        let root = d.join("steam");
        std::fs::create_dir_all(root.join("steamapps")).unwrap();
        let vdf = format!("\"libraryfolders\" {{ \"0\" {{ \"path\" \"{}\" }} \"1\" {{ \"path\" \"{}\" }} }}",
                          root.display().to_string().replace('\\', "\\\\"), lib.display().to_string().replace('\\', "\\\\"));
        std::fs::write(root.join("steamapps/libraryfolders.vdf"), vdf).unwrap();
        let libs = libraries(std::slice::from_ref(&root));
        assert_eq!(libs.len(), 2);
        let c = find_game(&libs, false);
        assert_eq!(c.len(), 1);
        assert!(same_path(&c[0].dir, &game));
        assert!(c[0].has_exe);
        assert_eq!(c[0].manifest.as_ref().unwrap().buildid, Some(6436034));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn path_helpers() {
        assert!(same_path(Path::new(r"D:\SteamLibrary\"), Path::new("d:/steamlibrary")));
        assert!(is_within(Path::new(r"D:\SteamLibrary\steamapps\common\MGS_TPP\master"), Path::new(r"d:\steamlibrary")));
        assert!(!is_within(Path::new(r"D:\SteamLibrary2"), Path::new(r"D:\SteamLibrary")));
        assert!(is_within(Path::new(r"D:\x"), Path::new(r"D:\x\")));
    }
}
