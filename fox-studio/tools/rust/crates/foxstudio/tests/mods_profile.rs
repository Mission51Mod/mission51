//! Public Mods flow with invented keys/archives. No installation, game, sandbox or GPU is called.
mod common;

use common::ModsFixture;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use foxstudio::{
    mods_view::{self, ModTarget, ModsView, Op},
    settings::{Settings, Tab, ThemeChoice},
};
use std::time::{Duration, Instant};

struct View {
    mods: ModsView,
    settings: Settings,
    env: mods_view::Env,
}

fn settings(fixture: &ModsFixture) -> Settings {
    Settings {
        game_dir: fixture.game.display().to_string(),
        runtime_profile: fixture.profile.display().to_string(),
        system_fonts: false,
        ..Default::default()
    }
}

fn harness(settings: Settings) -> Harness<'static, View> {
    Harness::builder().with_size([1440.0, 1200.0]).build_ui_state(
        |ui, view: &mut View| {
            foxstudio::theme::apply(ui.ctx(), ThemeChoice::Dark, 1.0);
            view.mods.sync_target(ui.ctx(), &view.settings);
            view.mods.poll();
            view.mods.show(ui, &mut view.settings, &view.env);
        },
        View {
            mods: ModsView::default(),
            settings,
            env: mods_view::Env { game_running: None },
        },
    )
}

fn wait(view: &mut Harness<'_, View>, ready: impl Fn(&View) -> bool) {
    let started = Instant::now();
    loop {
        view.step();
        if ready(view.state()) {
            view.run_steps(2);
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "fixture UI worker did not finish"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn select(view: &mut Harness<'_, View>, fixture: &ModsFixture) {
    let context = view.ctx.clone();
    let state = view.state_mut();
    state
        .mods
        .receive_drop(&context, &mut state.settings, &[common::dropped(&fixture.package)]);
    wait(view, |state| {
        state.mods.profile_ready() && !state.mods.busy() && state.mods.package_name(&fixture.package).is_some()
    });
}

#[test]
fn mods_profile_target_captures_source_and_explicit_or_cache_profile() {
    let fixture = ModsFixture::new();
    let selected = Settings {
        game_dir: format!("  {}  ", fixture.other_game.display()),
        game_install: format!("  {}  ", fixture.game.display()),
        cache_dir: fixture.root.join("chosen-cache").display().to_string(),
        ..Default::default()
    };
    let target = ModTarget::from_settings(&selected).unwrap();
    assert_eq!(target.game, fixture.other_game);
    assert_eq!(target.source_game, fixture.game);
    assert_eq!(
        target.runtime_profile,
        Some(fixture.root.join("chosen-cache/runtime_profile.json"))
    );
    let explicit = Settings {
        runtime_profile: format!(" {} ", fixture.profile.display()),
        ..selected
    };
    assert_eq!(
        ModTarget::from_settings(&explicit).unwrap().runtime_profile,
        Some(fixture.profile.clone())
    );
    assert!(ModTarget::from_settings(&Settings::default()).is_none());
    fixture.assert_games_unchanged();
}

#[test]
fn mods_profile_missing_or_wrong_profile_rejects_every_archive_action_before_writes() {
    let fixture = ModsFixture::new();
    let mut target = ModTarget::from_settings(&settings(&fixture)).unwrap();
    let actions = [
        Op::Verify,
        Op::Setup,
        Op::Install {
            source: fixture.package.clone(),
            replace: false,
        },
        Op::Uninstall {
            name: "not installed".into(),
        },
    ];
    for profile in [
        None,
        Some(fixture.other_profile.clone()),
        Some(fixture.root.join("absent-profile.json")),
    ] {
        target.runtime_profile = profile;
        for action in &actions {
            let error = mods_view::run_op_for_target(&target, &[], action, None).unwrap_err();
            assert!(error.to_lowercase().contains("profile"), "{action:?}: {error}");
            fixture.assert_games_unchanged();
        }
    }
}

#[test]
fn mods_profile_valid_profile_verifies_and_profile_free_reads_and_packing_stay_available() {
    let fixture = ModsFixture::new();
    let target = ModTarget::from_settings(&settings(&fixture)).unwrap();
    assert!(
        mods_view::run_op_for_target(&target, &[], &Op::Verify, None)
            .unwrap()
            .ok
    );
    assert!(mods_view::run_op(&fixture.game, &[], &Op::List).unwrap().ok);
    let package = fixture.root.join("profile-free.mgsv");
    let operation = Op::Pack {
        stage: fixture.root.join("stage"),
        out: package.clone(),
        level: 0,
    };
    assert!(mods_view::run_op(std::path::Path::new(""), &[], &operation).unwrap().ok);
    assert_eq!(foxinstall::package_name(&package).unwrap(), "Fixture island");
    fixture.assert_games_unchanged();
}

#[test]
fn mods_profile_missing_profile_allows_review_but_disables_install_and_opens_setup() {
    let fixture = ModsFixture::new();
    let mut selected = settings(&fixture);
    selected.runtime_profile.clear();
    selected.cache_dir.clear();
    let mut view = harness(selected);
    let context = view.ctx.clone();
    let state = view.state_mut();
    state
        .mods
        .receive_drop(&context, &mut state.settings, &[common::dropped(&fixture.package)]);
    wait(&mut view, |state| {
        !state.mods.busy() && state.mods.package_name(&fixture.package).is_some()
    });
    assert!(!view.state().mods.profile_ready());
    assert!(view.get_by_label("Install").accesskit_node().is_disabled());
    assert!(view.get_by_label("Verify").accesskit_node().is_disabled());
    view.get_by_label("Open Setup").click();
    view.run_steps(2);
    assert_eq!(view.state().settings.last_tab, Tab::Setup);
    assert!(view.state().mods.last.is_none());
    fixture.assert_games_unchanged();
}

#[test]
fn mods_profile_changing_profile_cancels_review_and_wrong_profile_cannot_enable_actions() {
    let fixture = ModsFixture::new();
    let mut view = harness(settings(&fixture));
    select(&mut view, &fixture);
    view.get_by_label("Install").click();
    view.run_steps(3);
    view.get_by_label("Install Fixture island?");
    assert_eq!(
        view.get_all_by_label(&format!("Runtime profile: {}", fixture.profile.display()))
            .count(),
        2,
        "source profile must be visible in the panel and captured review"
    );
    view.state_mut().settings.runtime_profile = fixture.other_profile.display().to_string();
    view.run_steps(3);
    assert!(view.query_by_label("Install Fixture island?").is_none());
    wait(&mut view, |state| {
        !state.mods.busy() && state.mods.profile_error().is_some()
    });
    assert!(!view.state().mods.profile_ready());
    assert!(view.get_by_label("Install").accesskit_node().is_disabled());
    assert!(view.state().mods.last.is_none());
    fixture.assert_games_unchanged();
}

#[test]
fn mods_profile_conflict_panel_is_wired_and_selection_is_reset_on_destination_change() {
    let fixture = ModsFixture::new();
    let mut view = harness(settings(&fixture));
    wait(&mut view, |state| !state.mods.busy() && state.mods.conflicts.is_some());
    view.get_by_label("File ownership and conflicts").click();
    view.run_steps(3);
    view.get_by_label("Mod ownership conflicts");
    view.state_mut().settings.game_dir = fixture.other_game.display().to_string();
    view.state_mut().settings.runtime_profile = fixture.other_profile.display().to_string();
    view.step();
    assert!(view.state().mods.conflicts.is_none());
    wait(&mut view, |state| {
        state.mods.profile_ready() && !state.mods.busy() && state.mods.conflicts.is_some()
    });
    fixture.assert_games_unchanged();
}

#[test]
fn mods_profile_inside_target_keeps_reads_but_rejects_mutation_before_writes() {
    let fixture = ModsFixture::with_profile_inside_game();
    let selected = settings(&fixture);
    let target = ModTarget::from_settings(&selected).unwrap();
    target.load_profile().unwrap();
    for action in [
        Op::Setup,
        Op::Install {
            source: fixture.package.clone(),
            replace: false,
        },
        Op::Uninstall {
            name: "not installed".into(),
        },
    ] {
        let error = mods_view::run_op_for_target(&target, &[], &action, None).unwrap_err();
        assert!(error.contains("outside the target game folder"), "{action:?}: {error}");
        fixture.assert_games_unchanged();
    }
    assert!(
        mods_view::run_op_for_target(&target, &[], &Op::Verify, None)
            .unwrap()
            .ok
    );
    let mut view = harness(selected);
    select(&mut view, &fixture);
    assert!(view.state().mods.profile_ready());
    assert!(!view.state().mods.writes_ready());
    view.get_by_label(
        "Move the runtime profile outside the target game folder before installing, uninstalling or setting up mods.",
    );
    assert!(view.get_by_label("Install").accesskit_node().is_disabled());
    assert!(!view.get_by_label("Verify").accesskit_node().is_disabled());
    assert!(view.state().mods.last.is_none());
    fixture.assert_games_unchanged();
}
