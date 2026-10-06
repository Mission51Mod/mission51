//! What the Project panel shows: repo root, graph file, Rust tool stamp, the fox executable, whether a game or a
//! build is running. Probed on a background thread every few seconds (process lists are not free).
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// game executables that make tools polite (and that the installer refuses to work under)
pub const GAME_EXES: [&str; 1] = ["mgsvtpp.exe"];

#[derive(Clone, Debug, PartialEq, Default)]
pub struct ProjectInfo {
    pub root: PathBuf,
    /// tools/rust/Cargo.toml present
    pub root_ok: bool,
    pub graph: PathBuf,
    pub graph_exists: bool,
    /// work/rust/py/current.txt (the source stamp of the installed foxrs module)
    pub stamp: Option<String>,
    pub fox_exe: PathBuf,
    pub fox_exe_modified: Option<SystemTime>,
    pub fox_exe_size: Option<u64>,
    /// a listed game process that is running now
    pub game_running: Option<String>,
    /// pid of a foxbuild holding the repo's build lock
    pub build_running: Option<u32>,
    /// <log_dir>/PAUSE exists: foxbuild starts no new stage (tools/build/pause.py)
    pub build_paused: bool,
    pub probed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProbeConfig {
    pub root: PathBuf,
    pub graph: PathBuf,
    pub fox_exe: PathBuf,
    pub games: Vec<String>,
    pub log_dir: PathBuf,
}

/// one probe (filesystem + process list)
pub fn probe(cfg: &ProbeConfig, sys: &mut sysinfo::System) -> ProjectInfo {
    let md = std::fs::metadata(&cfg.fox_exe).ok();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let names: Vec<String> = cfg.games.iter().map(|g| g.to_lowercase()).collect();
    let game_running = sys
        .processes()
        .values()
        .map(|p| p.name().to_string_lossy().to_lowercase())
        .find(|n| names.iter().any(|g| g == n));
    ProjectInfo {
        root: cfg.root.clone(),
        root_ok: crate::settings::is_repo(&cfg.root),
        graph: cfg.graph.clone(),
        graph_exists: cfg.graph.is_file(),
        stamp: read_stamp(&cfg.root),
        fox_exe: cfg.fox_exe.clone(),
        fox_exe_modified: md.as_ref().and_then(|m| m.modified().ok()),
        fox_exe_size: md.as_ref().map(|m| m.len()),
        game_running,
        build_running: foxbuild::lock_holder(&cfg.log_dir),
        build_paused: cfg.log_dir.join("PAUSE").exists(),
        probed: true,
    }
}

pub fn read_stamp(root: &Path) -> Option<String> {
    std::fs::read_to_string(
        root.join("work")
            .join("rust")
            .join("py")
            .join("current.txt"),
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

/// Background poller: probes every `every`, repaints the UI only when something changed.
pub struct Poller {
    shared: Arc<Mutex<(ProbeConfig, ProjectInfo, bool)>>,
}

impl Poller {
    pub fn start(cfg: ProbeConfig, ctx: &egui::Context, every: Duration) -> Poller {
        let shared = Arc::new(Mutex::new((cfg, ProjectInfo::default(), false)));
        let s2 = shared.clone();
        let ctx = ctx.clone();
        let _ = std::thread::Builder::new()
            .name("fox-studio: project probe".into())
            .spawn(move || {
                crate::tasks::lower_thread_priority();
                let mut sys = sysinfo::System::new();
                loop {
                    let cfg = match s2.lock() {
                        Ok(g) if g.2 => return, // stopped
                        Ok(g) => g.0.clone(),
                        Err(_) => return,
                    };
                    let info = probe(&cfg, &mut sys);
                    if let Ok(mut g) = s2.lock()
                        && g.0 == cfg
                        && g.1 != info
                    {
                        g.1 = info;
                        ctx.request_repaint();
                    }
                    std::thread::sleep(every);
                }
            });
        Poller { shared }
    }

    pub fn info(&self) -> ProjectInfo {
        self.shared.lock().map(|g| g.1.clone()).unwrap_or_default()
    }

    /// new paths (settings changed): the next probe uses them
    pub fn reconfigure(&self, cfg: ProbeConfig) {
        if let Ok(mut g) = self.shared.lock()
            && g.0 != cfg
        {
            g.0 = cfg;
            g.1.probed = false;
        }
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        if let Ok(mut g) = self.shared.lock() {
            g.2 = true;
        }
    }
}
