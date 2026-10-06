//! Small, isolated UI fixtures. No installer setup, installation or real build is called.
#![allow(dead_code)]

pub mod runtime_fixture;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

pub struct ModsFixture {
    pub root: PathBuf,
    pub game: PathBuf,
    pub other_game: PathBuf,
    pub profile: PathBuf,
    pub other_profile: PathBuf,
    pub package: PathBuf,
    pub bad_package: PathBuf,
    before: Vec<(PathBuf, Vec<u8>)>,
}

impl ModsFixture {
    pub fn new() -> Self {
        let unique = format!("foxstudio_mods_ui_{}_{}_{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed));
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir(&root).unwrap();
        let game = root.join("test_install");
        let other_game = root.join("other_install");
        let profile = root.join("cache/runtime_profile.json");
        let other_profile = root.join("other_cache/runtime_profile.json");
        for (dir, path, seed) in [(&game, &profile, 0), (&other_game, &other_profile, 100)] {
            let runtime = runtime_fixture::write_game(dir, seed);
            runtime.save(path).unwrap();
            std::fs::create_dir_all(dir.join("foxinstall")).unwrap();
            let mut manifest = foxinstall::Manifest { format: 1, tool: "UI fixture".into(),
                mode: "native".into(), layout: "gzstool".into(), ..Default::default() };
            for archive in ["00", "01"] {
                let current = dir.join(format!("master/0/{archive}.dat"));
                std::fs::copy(&current, current.with_extension("dat.foxbase")).unwrap();
                manifest.base.insert(archive.into(), foxinstall::FileSig {
                    size: current.metadata().unwrap().len(),
                    md5: foxinstall::md5_file_pub(&current).unwrap(),
                });
            }
            std::fs::write(dir.join("foxinstall/manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
        }
        // App tests see only this fixture graph. No stage is ever executed.
        std::fs::write(root.join("stages.toml"), "[settings]\nlog_dir = \"logs\"\n[[stage]]\nname = \"fixture\"\ncmd = [\"unused\"]\noutputs = [\"unused\"]\n").unwrap();
        let stage = root.join("stage");
        std::fs::create_dir_all(stage.join("Assets/tpp/script/fixture")).unwrap();
        std::fs::write(stage.join("metadata.xml"), r#"<?xml version="1.0" encoding="utf-8"?>
<ModEntry Name="Fixture island" Version="1.0" Author="Fox Studio tests" Website=""><Description>Synthetic review fixture</Description></ModEntry>"#).unwrap();
        std::fs::write(stage.join("Assets/tpp/script/fixture/main.lua"), b"-- synthetic fixture\n").unwrap();
        let package = root.join("fixture.MGSV");
        foxinstall::mgsvpack::write_mgsv(&stage, &package, 0).unwrap();
        let bad_package = root.join("broken.mgsv");
        std::fs::write(&bad_package, b"not a zip").unwrap();
        let mut before = snapshot(&game);
        before.extend(snapshot(&other_game));
        Self { root, game, other_game, profile, other_profile, package, bad_package, before }
    }

    pub fn with_profile_inside_game() -> Self {
        let mut fixture = Self::new();
        let path = fixture.game.join("mod-fixture/runtime_profile.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::copy(&fixture.profile, &path).unwrap();
        fixture.profile = path;
        fixture.before = snapshot(&fixture.game);
        fixture.before.extend(snapshot(&fixture.other_game));
        fixture
    }

    pub fn assert_games_unchanged(&self) {
        let mut after = snapshot(&self.game);
        after.extend(snapshot(&self.other_game));
        assert_eq!(self.before, after, "UI review wrote into a fixture game");
    }
}

fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut children = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect::<Vec<_>>();
    children.sort();
    let mut files = Vec::new();
    for path in children {
        if path.is_dir() {
            files.extend(snapshot(&path));
        } else {
            files.push((path.clone(), std::fs::read(path).unwrap()));
        }
    }
    files
}

impl Drop for ModsFixture {
    fn drop(&mut self) {
        // This path was created exclusively by this fixture, outside all game/repository folders.
        let _ = std::fs::remove_dir_all(&self.root);
    }
}


#[derive(Debug)]
struct LocalDrop(PathBuf);

impl eframe::egui::DroppedFile for LocalDrop {
    fn path(&self) -> &Path { &self.0 }
    fn bytes(&self) -> Result<Vec<u8>, String> { std::fs::read(&self.0).map_err(|e| e.to_string()) }
}

pub fn dropped(path: &Path) -> eframe::egui::DroppedFileHandle {
    std::sync::Arc::new(LocalDrop(path.to_path_buf()))
}
