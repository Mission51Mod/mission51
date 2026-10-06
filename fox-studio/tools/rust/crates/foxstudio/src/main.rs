// No console window in release builds (a GUI app); debug builds keep it for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod portable;

use foxstudio::settings::{self, Settings};
use foxstudio::{AppOptions, FoxStudio, theme};

fn main() -> eframe::Result {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let launch = portable::parse(&arguments).unwrap_or_else(|error| {
        portable::attach_parent_console();
        eprintln!("fox-studio: {error}");
        std::process::exit(2);
    });
    let options = match launch {
        portable::Launch::Version => {
            portable::attach_parent_console();
            println!("fox-studio {}", foxstudio::VERSION);
            std::process::exit(0);
        }
        portable::Launch::Help => {
            portable::attach_parent_console();
            print!("{}", portable::HELP);
            std::process::exit(0);
        }
        portable::Launch::Smoke(options) => {
            portable::attach_parent_console();
            let executable = std::env::current_exe().unwrap_or_else(|error| {
                eprintln!("fox-studio: cannot locate editor executable: {error}");
                std::process::exit(1);
            });
            let report = portable::smoke(&options, executable.parent().unwrap_or(std::path::Path::new(".")));
            if let Err(error) = portable::print_report(&report) {
                eprintln!("fox-studio: cannot write smoke report: {error}");
                std::process::exit(1);
            }
            std::process::exit(if report.ok { 0 } else { 1 });
        }
        portable::Launch::Gui(options) => options,
    };
    let path = options
        .config_dir
        .map(|dir| dir.join("settings.json"))
        .unwrap_or_else(settings::default_path);
    let first_run = !path.exists();
    let (mut s, note) = Settings::load(&path);
    if let Some(fox) = options.fox_exe {
        s.fox_exe = fox.display().to_string();
    }
    if first_run {
        // nothing configured yet: the setup wizard is the landing page
        s.last_tab = foxstudio::settings::Tab::Setup;
    }
    // the GUI needs very little GPU: prefer the power-saving adapter (an integrated GPU when there is one) unless
    // WGPU_POWER_PREF says otherwise, so a running game keeps the discrete one to itself
    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
    if std::env::var_os("WGPU_POWER_PREF").is_none()
        && let eframe::egui_wgpu::WgpuSetup::CreateNew(adapter) = &mut wgpu_options.wgpu_setup
    {
        adapter.power_preference = eframe::wgpu::PowerPreference::LowPower;
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(foxstudio::APP_NAME)
            .with_app_id("fox-studio")
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([900.0, 560.0])
            .with_icon(std::sync::Arc::new(theme::icon())),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options,
        ..Default::default()
    };
    eframe::run_native(
        foxstudio::APP_NAME,
        options,
        Box::new(move |cc| {
            let app = FoxStudio::new(
                &cc.egui_ctx,
                s,
                note,
                AppOptions {
                    settings_path: Some(path),
                    ..Default::default()
                },
            )
            .with_wgpu(cc.wgpu_render_state.clone());
            Ok(Box::new(app))
        }),
    )
}
