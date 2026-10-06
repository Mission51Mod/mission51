//! Per-user settings: %APPDATA%\fox-studio\settings.json (FOX_STUDIO_HOME overrides the folder).
//! Unknown or missing fields fall back to defaults, so old files keep loading.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ThemeChoice {
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tab {
    #[default]
    Project,
    Setup,
    Build,
    Previews,
    Mods,
    Settings,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeChoice,
    /// Use the enhanced text/status contrast palette in every theme.
    pub high_contrast: bool,
    /// interface zoom (1.0 = 100 %)
    pub ui_scale: f32,
    /// use the Windows UI fonts (Segoe UI / Consolas) when installed
    pub system_fonts: bool,
    pub last_tab: Tab,
    /// repository root; empty = detect (FOX_REPO_ROOT, then the folders above the working directory and the exe)
    pub repo_root: String,
    /// stage graph, relative to the repo root
    pub graph_file: String,
    /// the `fox` executable that runs builds; empty = <repo>/work/rust/target/release/fox.exe
    pub fox_exe: String,
    /// Validated native tool manifest; empty discovers executable-sibling fox-tools.json.
    pub bundle_manifest: String,
    /// interpreter for "python" in stage commands; empty = FOX_PYTHON, else "python" (what `fox build` uses)
    pub python: String,
    /// confirm before a real (non dry-run) build
    pub confirm_builds: bool,
    /// game folder for the Mods view
    pub game_dir: String,
    /// last package picked for install (.mgsv or staging folder)
    pub install_source: String,
    pub install_replace: bool,
    /// development: extracted vanilla folders (each holding Assets/...), ';'-separated, used for pack merges instead
    /// of the game's chunk archives (`fox mod --vanilla-dirs`); empty = the game folder
    pub vanilla_dirs: String,
    /// .mgsv packing: staging folder, output file, deflate level
    pub pack_stage: String,
    pub pack_out: String,
    pub pack_level: u32,
    /// the open project (a foxproject.toml); empty = the repository's graph file above (no project)
    pub project_file: String,
    /// recently opened projects, newest first
    pub recent_projects: Vec<String>,
    /// setup: the Steam install of the game (read only, ever)
    pub game_install: String,
    pub game_buildid: Option<u64>,
    /// setup: where vanilla archives are unpacked / indexed
    pub cache_dir: String,
    /// Locally learned archive context; empty uses the selected cache's runtime_profile.json.
    pub runtime_profile: String,
    /// name dictionary used when unpacking (empty = <cache>/dictionary.txt when it exists)
    pub dict_file: String,
    /// archive ids chosen for unpacking (e.g. "chunk0", "0_00")
    pub unpack_select: Vec<String>,
    /// the user finished (or skipped) the setup wizard
    pub setup_done: bool,
    /// Previews: the last texture file opened
    pub preview_texture: String,
    /// Previews: the last model opened
    pub preview_model: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: ThemeChoice::System,
            high_contrast: false,
            ui_scale: 1.0,
            system_fonts: true,
            last_tab: Tab::Project,
            repo_root: String::new(),
            graph_file: "tools/build/flyk_stages.toml".into(),
            fox_exe: String::new(),
            bundle_manifest: String::new(),
            python: String::new(),
            confirm_builds: true,
            game_dir: String::new(),
            install_source: String::new(),
            install_replace: false,
            vanilla_dirs: String::new(),
            pack_stage: String::new(),
            pack_out: String::new(),
            pack_level: 6,
            project_file: String::new(),
            recent_projects: vec![],
            game_install: String::new(),
            game_buildid: None,
            cache_dir: String::new(),
            runtime_profile: String::new(),
            dict_file: String::new(),
            unpack_select: vec![],
            setup_done: false,
            preview_texture: String::new(),
            preview_model: String::new(),
        }
    }
}

/// %APPDATA%\fox-studio (FOX_STUDIO_HOME overrides; ~/.config/fox-studio elsewhere)
pub fn config_dir() -> PathBuf {
    if let Some(h) = std::env::var_os("FOX_STUDIO_HOME") {
        return PathBuf::from(h);
    }
    if let Some(a) = std::env::var_os("APPDATA") {
        return PathBuf::from(a).join("fox-studio");
    }
    if let Some(h) = std::env::var_os("HOME") {
        return PathBuf::from(h).join(".config").join("fox-studio");
    }
    PathBuf::from("fox-studio")
}

pub const MAX_RECENT: usize = 8;

pub fn default_path() -> PathBuf {
    config_dir().join("settings.json")
}

impl Settings {
    pub fn resolved_bundle_manifest(&self) -> Option<PathBuf> {
        let configured = self.bundle_manifest.trim();
        if !configured.is_empty() {
            let path = PathBuf::from(configured);
            return Some(if path.is_absolute() {
                path
            } else {
                self.resolved_root().join(path)
            });
        }
        let executable = std::env::current_exe().ok()?;
        let manifest = executable.parent()?.join("fox-tools.json");
        manifest.is_file().then_some(manifest)
    }

    /// Load from `path`; a missing file gives the defaults, a broken one the defaults plus a message.
    pub fn load(path: &Path) -> (Settings, Option<String>) {
        match std::fs::read(path) {
            Err(_) => (Settings::default(), None),
            Ok(b) => match serde_json::from_slice::<Settings>(&b) {
                Ok(mut s) => {
                    s.sanitize();
                    (s, None)
                }
                Err(e) => (
                    Settings::default(),
                    Some(format!(
                        "settings file {} unreadable ({e}); using defaults",
                        path.display()
                    )),
                ),
            },
        }
    }

    /// Atomic save (temp file + rename).
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn sanitize(&mut self) {
        if !self.ui_scale.is_finite() {
            self.ui_scale = 1.0;
        }
        self.ui_scale = self.ui_scale.clamp(0.6, 2.5);
        self.pack_level = self.pack_level.min(9);
        self.recent_projects.truncate(MAX_RECENT);
        if self.graph_file.trim().is_empty() {
            self.graph_file = Settings::default().graph_file;
        }
    }

    /// the repo root in use: the setting, else foxpipe's detection, else the folders above this executable
    pub fn resolved_root(&self) -> PathBuf {
        if !self.repo_root.trim().is_empty() {
            return PathBuf::from(self.repo_root.trim());
        }
        let r = foxpipe::paths::repo_root();
        if is_repo(&r) {
            return r;
        }
        if let Ok(exe) = std::env::current_exe() {
            let mut d = exe;
            while d.pop() {
                if is_repo(&d) {
                    return d;
                }
            }
        }
        r
    }

    pub fn resolved_fox_exe(&self, root: &Path) -> PathBuf {
        let executable = std::env::current_exe().ok();
        self.resolved_fox_exe_in(root, executable.as_deref().and_then(Path::parent))
    }

    /// Explicit settings win; a portable sibling wins over the internal development tree.
    /// Supplying the editor directory also makes packaging checks independent of cwd/environment.
    pub fn resolved_fox_exe_in(&self, root: &Path, executable_dir: Option<&Path>) -> PathBuf {
        if !self.fox_exe.trim().is_empty() {
            let path = PathBuf::from(self.fox_exe.trim());
            return if path.is_absolute() { path } else { root.join(path) };
        }
        let name = if cfg!(windows) { "fox.exe" } else { "fox" };
        if let Some(dir) = executable_dir {
            let sibling = dir.join(name);
            if sibling.is_file() {
                return sibling;
            }
        }
        root.join("work").join("rust").join("target").join("release").join(name)
    }

    /// the dictionary to use for unpacking: the setting, else <cache>/dictionary.txt
    pub fn resolved_dict(&self) -> Option<PathBuf> {
        if !self.dict_file.trim().is_empty() {
            return Some(PathBuf::from(self.dict_file.trim()));
        }
        (!self.cache_dir.trim().is_empty()).then(|| PathBuf::from(self.cache_dir.trim()).join("dictionary.txt"))
    }

    /// The explicit/cache-local path only. Callers load and validate it against their selected install.
    pub fn resolved_runtime_profile(&self) -> Option<PathBuf> {
        if !self.runtime_profile.trim().is_empty() {
            return Some(PathBuf::from(self.runtime_profile.trim()));
        }
        (!self.cache_dir.trim().is_empty())
            .then(|| PathBuf::from(self.cache_dir.trim()).join(foxcore::runtime_data::PROFILE_FILE))
    }

    pub fn resolved_python(&self) -> String {
        if !self.python.trim().is_empty() {
            return self.python.trim().to_string();
        }
        foxbuild::default_python()
    }
}

/// a folder holding tools/rust/Cargo.toml (our repository layout)
pub fn is_repo(p: &Path) -> bool {
    p.join("tools").join("rust").join("Cargo.toml").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_defaults() {
        let dir = std::env::temp_dir().join(format!("foxstudio_settings_{}", std::process::id()));
        let p = dir.join("settings.json");
        let (d, note) = Settings::load(&p);
        assert!(note.is_none());
        assert_eq!(d, Settings::default());
        let s = Settings {
            theme: ThemeChoice::Light,
            ui_scale: 1.25,
            game_dir: "X:/game".into(),
            last_tab: Tab::Mods,
            ..Default::default()
        };
        s.save(&p).unwrap();
        let (r, note) = Settings::load(&p);
        assert!(note.is_none());
        assert_eq!(r, s);
        // unknown and missing fields are fine
        std::fs::write(&p, r#"{"theme":"Dark","future_field":1}"#).unwrap();
        let (r, note) = Settings::load(&p);
        assert!(note.is_none());
        assert_eq!(r.theme, ThemeChoice::Dark);
        assert_eq!(r.pack_level, 6);
        // a broken file: defaults plus a message
        std::fs::write(&p, "{not json").unwrap();
        let (r, note) = Settings::load(&p);
        assert!(note.is_some());
        assert_eq!(r, Settings::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sanitize_clamps() {
        let mut s = Settings {
            ui_scale: 9.0,
            pack_level: 42,
            graph_file: " ".into(),
            ..Default::default()
        };
        s.sanitize();
        assert_eq!(s.ui_scale, 2.5);
        assert_eq!(s.pack_level, 9);
        assert_eq!(s.graph_file, "tools/build/flyk_stages.toml");
    }
    #[test]
    fn portable_sibling_precedes_development_tree_but_explicit_choice_wins() {
        let root = std::env::temp_dir().join(format!(
            "foxstudio_sibling_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bin = root.join("portable");
        std::fs::create_dir_all(&bin).unwrap();
        let name = if cfg!(windows) { "fox.exe" } else { "fox" };
        let sibling = bin.join(name);
        std::fs::write(&sibling, b"fixture tool").unwrap();
        let dev = root.join("work/rust/target/release").join(name);
        std::fs::create_dir_all(dev.parent().unwrap()).unwrap();
        std::fs::write(&dev, b"dev tool").unwrap();
        assert_eq!(Settings::default().resolved_fox_exe_in(&root, Some(&bin)), sibling);
        let explicit = Settings {
            fox_exe: "chosen/native_fox".into(),
            ..Default::default()
        };
        assert_eq!(
            explicit.resolved_fox_exe_in(&root, Some(&bin)),
            root.join("chosen/native_fox")
        );
        std::fs::remove_file(&sibling).unwrap();
        std::fs::create_dir(&sibling).unwrap();
        assert_eq!(Settings::default().resolved_fox_exe_in(&root, Some(&bin)), dev);
        std::fs::remove_dir_all(root).unwrap();
    }
}
