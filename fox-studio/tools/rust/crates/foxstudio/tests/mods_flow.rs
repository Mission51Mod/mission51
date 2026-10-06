//! Mod selection and confirmation regressions on synthetic fixtures: no GPU, installer writes or game launch.
mod common;

use common::ModsFixture;
use eframe::egui;
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;
use foxstudio::mods_view::{self, ModsView};
use foxstudio::settings::{Settings, Tab, ThemeChoice};
use foxstudio::{AppOptions, FoxStudio};
use std::path::Path;
use std::time::{Duration, Instant};

struct View {
    mods: ModsView,
    settings: Settings,
    env: mods_view::Env,
}

fn view(f: &ModsFixture) -> Harness<'static, View> {
    let settings = Settings { game_dir: f.game.display().to_string(), runtime_profile: f.profile.display().to_string(), system_fonts: false, ..Default::default() };
    Harness::builder().with_size([1440.0, 1050.0]).build_ui_state(|ui, state: &mut View| {
        foxstudio::theme::apply(ui.ctx(), ThemeChoice::Dark, 1.0);
        state.mods.poll();
        state.mods.show(ui, &mut state.settings, &state.env);
    }, View { mods: ModsView::default(), settings, env: mods_view::Env { game_running: None } })
}

fn step_until<T>(h: &mut Harness<'_, T>, done: impl Fn(&T) -> bool) {
    let start = Instant::now();
    loop {
        h.step();
        if done(h.state()) { h.run_steps(2); return; }
        assert!(start.elapsed() < Duration::from_secs(15), "fixture UI worker did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn drop_file(path: &Path) -> egui::DroppedFileHandle {
    common::dropped(path)
}

fn select(h: &mut Harness<'_, View>, f: &ModsFixture) {
    let ctx = h.ctx.clone();
    let state = h.state_mut();
    state.mods.receive_drop(&ctx, &mut state.settings, &[drop_file(&f.package)]);
    step_until(h, |v| v.mods.package_name(&f.package).is_some() && !v.mods.busy() && v.mods.profile_ready());
    assert_eq!(h.state().mods.package_name(&f.package), Some("Fixture island"));
}

#[test]
fn dropping_from_another_tab_routes_to_mods_review_without_installing() {
    let f = ModsFixture::new();
    let settings = Settings { repo_root: f.root.display().to_string(), graph_file: "stages.toml".into(),
        last_tab: Tab::Settings, game_dir: f.game.display().to_string(), runtime_profile: f.profile.display().to_string(), system_fonts: false, setup_done: true,
        ..Default::default() };
    let mut h = Harness::builder().with_size([1440.0, 1050.0]).build_eframe(move |cc| {
        FoxStudio::new(&cc.egui_ctx, settings, None,
            AppOptions { settings_path: None, allow_real_builds: false, probe_every: Duration::from_millis(200) })
    });
    h.input_mut().dropped_files.push(drop_file(&f.package));
    step_until(&mut h, |a| a.mods.package_name(&f.package).is_some() && !a.mods.busy());
    assert_eq!(h.state().settings.last_tab, Tab::Mods);
    assert_eq!(h.state().settings.install_source, f.package.display().to_string());
    h.get_by_label("Fixture island");
    assert!(h.state().mods.last.is_none(), "drop must only inspect metadata");
    f.assert_games_unchanged();
}

#[test]
fn rejects_ambiguous_nonlocal_and_unsupported_drops_without_changing_source() {
    let f = ModsFixture::new();
    let mut h = view(&f);
    select(&mut h, &f);
    let cases = [
        vec![drop_file(&f.package), drop_file(&f.package)],
        vec![drop_file(Path::new("browser.mgsv"))],
        vec![drop_file(&f.root.join("stage"))],
        vec![drop_file(&f.root.join("missing.mgsv"))],
    ];
    for files in cases {
        let error = mods_view::dropped_package(&files).unwrap_err();
        let ctx = h.ctx.clone();
        let state = h.state_mut();
        state.mods.receive_drop(&ctx, &mut state.settings, &files);
        h.run_steps(2);
        h.get_by_label(&error);
        assert_eq!(h.state().settings.install_source, f.package.display().to_string());
        assert_eq!(h.state().mods.package_name(&f.package), Some("Fixture island"));
    }
    f.assert_games_unchanged();
}

#[test]
fn editing_source_invalidates_metadata_before_losing_focus() {
    let f = ModsFixture::new();
    let mut h = view(&f);
    select(&mut h, &f);
    h.get_by_label("Source").focus();
    h.run_steps(2);
    h.get_by_label("Source").type_text("x");
    h.run_steps(1);
    assert_ne!(h.state().settings.install_source, f.package.display().to_string());
    assert!(h.state().mods.package_name(&f.package).is_none());
    assert!(h.get_by_label("Install").accesskit_node().is_disabled());
    assert!(h.query_by_label("Fixture island").is_none());
    f.assert_games_unchanged();
}

#[test]
fn corrupt_package_cannot_reuse_the_previous_package_validation() {
    let f = ModsFixture::new();
    let mut h = view(&f);
    select(&mut h, &f);
    h.state_mut().settings.install_source = f.bad_package.display().to_string();
    h.step();
    assert!(h.state().mods.package_name(&f.bad_package).is_none());
    assert!(h.get_by_label("Install").accesskit_node().is_disabled());
    assert!(h.query_by_label("Fixture island").is_none());
    f.assert_games_unchanged();
}

#[test]
fn changing_destination_cancels_the_reviewed_install() {
    let f = ModsFixture::new();
    let mut h = view(&f);
    select(&mut h, &f);
    h.get_by_label("Install").click();
    h.run_steps(3);
    h.get_by_label("Install Fixture island?");
    h.get_by_label(&format!("Destination: {}", f.game.display()));
    h.state_mut().settings.game_dir = f.other_game.display().to_string();
    h.run_steps(3);
    assert!(h.query_by_label("Install Fixture island?").is_none());
    step_until(&mut h, |v| !v.mods.busy());
    assert!(h.state().mods.last.is_none());
    f.assert_games_unchanged();
}

#[test]
fn changing_source_cancels_the_reviewed_install() {
    let f = ModsFixture::new();
    let mut h = view(&f);
    select(&mut h, &f);
    h.get_by_label("Install").click();
    h.run_steps(3);
    h.get_by_label("Install Fixture island?");
    h.state_mut().settings.install_source = f.bad_package.display().to_string();
    h.run_steps(3);
    assert!(h.query_by_label("Install Fixture island?").is_none());
    assert!(h.state().mods.last.is_none());
    f.assert_games_unchanged();
}

#[test]
fn game_start_during_confirmation_disables_the_action() {
    let f = ModsFixture::new();
    let mut h = view(&f);
    select(&mut h, &f);
    h.get_by_label("Install").click();
    h.run_steps(3);
    h.state_mut().env.game_running = Some("mgsvtpp.exe".into());
    h.run_steps(2);
    h.get_by_label("mgsvtpp.exe is running. Close the game before continuing.");
    let buttons = h.get_all_by_label("Install").collect::<Vec<_>>();
    assert!(buttons.iter().all(|b| b.accesskit_node().is_disabled()));
    // Cancel ends the review; no installer operation is ever invoked by this test.
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(h.query_by_label("Install Fixture island?").is_none());
    assert!(h.state().mods.last.is_none());
    f.assert_games_unchanged();
}

#[test]
fn drop_while_the_game_runs_inspects_but_cannot_install() {
    let f = ModsFixture::new();
    let mut h = view(&f);
    h.state_mut().env.game_running = Some("mgsvtpp.exe".into());
    select(&mut h, &f);
    h.get_by_label("Fixture island");
    assert!(h.get_by_label("Install").accesskit_node().is_disabled());
    assert!(h.state().mods.last.is_none());
    f.assert_games_unchanged();
}
