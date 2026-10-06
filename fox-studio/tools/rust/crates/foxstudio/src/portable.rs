//! Portable entry points that exit before settings persistence, process/game probes or graphics.
//! Packaging can check the actual sibling CLI and CPU initialization without opening a window.
use foxstudio::settings::Settings;
use serde::Serialize;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const HELP: &str = "Fox Studio desktop editor\n\n\
Usage: fox-studio [--config-dir DIR] [--fox-exe FILE]\n\
       fox-studio --smoke-test [--config-dir DIR] [--fox-exe FILE]\n\
       fox-studio --version | --help\n\n\
--config-dir DIR  Use an explicit settings directory. Smoke mode reads it only.\n\
--fox-exe FILE    Override the native CLI; the sibling fox executable is preferred by default.\n\
--smoke-test      Print a JSON check report and exit without a window or settings writes.\n\
--version, -V    Print the editor version.\n\
--help, -h       Show this help.\n";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub config_dir: Option<PathBuf>,
    pub fox_exe: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Launch {
    Gui(Options),
    Smoke(Options),
    Version,
    Help,
}

pub fn parse(arguments: &[OsString]) -> Result<Launch, String> {
    let mut options = Options::default();
    let mut mode = None;
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].to_str().ok_or("option name is not valid Unicode")?;
        match argument {
            "--version" | "-V" | "--help" | "-h" | "--smoke-test" => {
                let selected = match argument {
                    "--version" | "-V" => "version",
                    "--help" | "-h" => "help",
                    _ => "smoke",
                };
                if mode.replace(selected).is_some() {
                    return Err("choose one of --version, --help or --smoke-test".into());
                }
            }
            "--config-dir" | "--fox-exe" => {
                let path = arguments
                    .get(index + 1)
                    .ok_or_else(|| format!("{argument} needs a path"))?;
                if path.is_empty() || path.to_string_lossy().starts_with('-') {
                    return Err(format!("{argument} needs a path"));
                }
                if argument == "--config-dir" {
                    options.config_dir = Some(path.into());
                } else {
                    options.fox_exe = Some(path.into());
                }
                index += 1;
            }
            _ => return Err(format!("unknown option {argument:?}; use --help")),
        }
        index += 1;
    }
    if matches!(mode, Some("version" | "help")) && options != Options::default() {
        return Err("path options are only used for the editor or --smoke-test".into());
    }
    Ok(match mode {
        Some("version") => Launch::Version,
        Some("help") => Launch::Help,
        Some("smoke") => Launch::Smoke(options),
        _ => Launch::Gui(options),
    })
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct SmokeReport {
    pub schema_version: u32,
    pub ok: bool,
    pub editor_version: &'static str,
    pub build_id: Option<&'static str>,
    pub executable_dir: PathBuf,
    pub explicit_config_dir: Option<PathBuf>,
    pub fox_executable: PathBuf,
    pub checks: Vec<Check>,
    pub graphics_started: bool,
    pub game_opened: bool,
    pub user_settings_read: bool,
    pub settings_written: bool,
    pub python_required: bool,
}

pub fn smoke(options: &Options, executable_dir: &Path) -> SmokeReport {
    let mut checks = Vec::new();
    let mut settings = Settings::default();
    if let Some(dir) = &options.config_dir {
        let path = dir.join("settings.json");
        let result = if !path.exists() {
            Ok("explicit settings file absent; defaults used without writing it".into())
        } else {
            std::fs::metadata(&path)
                .map_err(|e| e.to_string())
                .and_then(|metadata| {
                    if metadata.len() > 1024 * 1024 {
                        return Err("settings file exceeds 1 MiB smoke limit".into());
                    }
                    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                    settings = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
                    settings.sanitize();
                    Ok(format!("explicit settings decoded: {}", path.display()))
                })
        };
        checks.push(check("explicit_settings", result));
    }
    if let Some(file) = &options.fox_exe {
        settings.fox_exe = file.display().to_string();
    }
    let root = if settings.repo_root.trim().is_empty() {
        executable_dir.to_path_buf()
    } else {
        PathBuf::from(settings.repo_root.trim())
    };
    let fox = settings.resolved_fox_exe_in(&root, Some(executable_dir));
    checks.push(check("native_cli", native_version(&fox)));
    checks.push(check("fmdl_core", check_container()));
    let context = eframe::egui::Context::default();
    let output = context.run_ui(eframe::egui::RawInput::default(), |ui| {
        ui.label("Fox Studio CPU initialization");
    });
    let shape_count = output.shapes.len();
    let font_texture_count = output.textures_delta.set.len();
    output.drop_without_applying_deltas();
    let icon = foxstudio::theme::icon();
    let icon_ok =
        icon.width > 0 && icon.height > 0 && icon.rgba.len() == icon.width as usize * icon.height as usize * 4;
    checks.push(Check {
        name: "ui_cpu_core",
        ok: icon_ok && shape_count > 0 && font_texture_count > 0,
        detail: if icon_ok && shape_count > 0 && font_texture_count > 0 {
            format!("egui produced {shape_count} shapes and {font_texture_count} font textures; application icon initialized without a renderer")
        } else {
            format!("CPU UI initialization failed: icon_valid={icon_ok}, shapes={shape_count}, font_textures={font_texture_count}")
        },
    });
    SmokeReport {
        schema_version: 1,
        ok: checks.iter().all(|check| check.ok),
        editor_version: foxstudio::VERSION,
        build_id: option_env!("FOX_BUNDLE_BUILD_ID"),
        executable_dir: executable_dir.to_path_buf(),
        explicit_config_dir: options.config_dir.clone(),
        fox_executable: fox,
        checks,
        graphics_started: false,
        game_opened: false,
        user_settings_read: false,
        settings_written: false,
        python_required: false,
    }
}

fn check(name: &'static str, result: Result<String, String>) -> Check {
    match result {
        Ok(detail) => Check { name, ok: true, detail },
        Err(detail) => Check {
            name,
            ok: false,
            detail,
        },
    }
}

fn check_container() -> Result<String, String> {
    use foxcore::fmdl::Fmdl;
    let mut model = Fmdl {
        head: [0; 32],
        s0off: 0x40,
        order: vec![],
        info_order: vec![],
        blocks: Default::default(),
        s1_infos: vec![],
        section1: vec![],
        tail: vec![],
    };
    model.head[..4].copy_from_slice(b"FMDL");
    model.head[4..8].copy_from_slice(&2.04f32.to_le_bytes());
    model.head[8..12].copy_from_slice(&0x40u32.to_le_bytes());
    let bytes = model.build();
    let decoded = foxcore::fmdl::read(&bytes)?;
    if decoded.build() != bytes {
        return Err("synthetic FMDL container round trip differs".into());
    }
    Ok("native FMDL container read/write round trip passed on synthetic data".into())
}

fn native_version(path: &Path) -> Result<String, String> {
    if !path.is_file() {
        return Err(format!("native fox executable missing: {}", path.display()));
    }
    let mut command = Command::new(path);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // No ambient project identity can make a packaging smoke check touch another project.
    command.env_remove("FOX_PROJECT").env_remove("FOX_REPO_ROOT");
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        command.current_dir(parent);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", path.display()))?;
    let started = Instant::now();
    let status = loop {
        match child
            .try_wait()
            .map_err(|e| format!("cannot wait for native fox: {e}"))?
        {
            Some(status) => break status,
            None if started.elapsed() > Duration::from_secs(5) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("native fox --version timed out after 5 seconds".into());
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    let mut output = String::new();
    if let Some(stdout) = child.stdout.take() {
        stdout
            .take(16 * 1024)
            .read_to_string(&mut output)
            .map_err(|e| e.to_string())?;
    }
    let expected = format!("fox {}", foxstudio::VERSION);
    if !status.success() || output.trim() != expected {
        return Err(format!(
            "native fox version check failed (exit {:?}, expected {expected:?}, got {:?})",
            status.code(),
            output.trim()
        ));
    }
    Ok(format!("{}: {}", path.display(), output.trim()))
}

pub fn print_report(report: &SmokeReport) -> Result<(), String> {
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, report).map_err(|e| e.to_string())?;
    writeln!(output).map_err(|e| e.to_string())
}

/// GUI-subsystem executables still provide useful CLI output in an existing Windows terminal.
/// Captured handles from a packaging subprocess remain valid; no console or graphics window is created.
pub fn attach_parent_console() {
    #[cfg(windows)]
    unsafe {
        unsafe extern "system" {
            fn AttachConsole(process_id: u32) -> i32;
        }
        AttachConsole(u32::MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn portable_options_choose_headless_before_gui() {
        assert_eq!(parse(&[]).unwrap(), Launch::Gui(Options::default()));
        assert_eq!(parse(&args(&["--version"])).unwrap(), Launch::Version);
        assert_eq!(parse(&args(&["-h"])).unwrap(), Launch::Help);
        assert_eq!(
            parse(&args(&["--smoke-test", "--config-dir", "fixture"])).unwrap(),
            Launch::Smoke(Options {
                config_dir: Some("fixture".into()),
                fox_exe: None
            })
        );
        assert!(parse(&args(&["--unknown"])).is_err());
        assert!(parse(&args(&["--config-dir", "--smoke-test"])).is_err());
        assert!(parse(&args(&["--help", "--version"])).is_err());
    }

    #[test]
    fn portable_smoke_reports_missing_tool_without_creating_settings() {
        let root = std::env::temp_dir().join(format!("foxstudio_smoke_missing_{}", std::process::id()));
        let options = Options {
            config_dir: Some(root.join("config")),
            fox_exe: Some(root.join("missing_fox")),
        };
        let report = smoke(&options, &root);
        assert!(!report.ok);
        assert!(report.checks.iter().any(|c| c.name == "native_cli" && !c.ok));
        assert!(report.checks.iter().any(|c| c.name == "fmdl_core" && c.ok));
        assert!(
            !report.graphics_started && !report.game_opened && !report.settings_written && !report.user_settings_read
        );
        assert!(!root.exists(), "smoke mode created a directory");
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["schema_version"], 1);
    }

    #[cfg(unix)]
    #[test]
    fn portable_smoke_checks_real_child_version_and_preserves_fixture_bytes() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "foxstudio_smoke_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("config")).unwrap();
        let fox = root.join("fox");
        std::fs::write(&fox, format!("#!/bin/sh\nprintf 'fox {}\\n'\n", foxstudio::VERSION)).unwrap();
        std::fs::set_permissions(&fox, std::fs::Permissions::from_mode(0o755)).unwrap();
        let settings = root.join("config/settings.json");
        std::fs::write(&settings, b"{\"ui_scale\":1.25}").unwrap();
        let before = std::fs::read(&settings).unwrap();
        let options = Options {
            config_dir: Some(root.join("config")),
            fox_exe: None,
        };
        let report = smoke(&options, &root);
        assert!(report.ok, "{:?}", report.checks);
        assert_eq!(report.fox_executable, fox);
        assert_eq!(std::fs::read(&settings).unwrap(), before);
        std::fs::write(&fox, b"#!/bin/sh\nprintf 'wrong tool\\n'\n").unwrap();
        assert!(!smoke(&options, &root).ok);
        assert_eq!(std::fs::read(&settings).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }
}
