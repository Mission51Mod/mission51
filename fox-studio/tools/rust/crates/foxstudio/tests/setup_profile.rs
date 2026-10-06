//! Native setup proofs use invented archive keys and never require a local game or Python.
mod setup_fixture;

use foxcore::qar;
use foxstudio::{settings::Settings, setup, setup_view::SetupView};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

struct Fixture {
    root: PathBuf,
    game: PathBuf,
    cache: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "fox-setup-profile-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let game = root.join("game");
        let cache = root.join("cache");
        setup_fixture::write_game(
            &game,
            &[
                ("/Assets/tpp/script/first.lua", b"first"),
                ("/Assets/tpp/script/second.lua", b"second"),
            ],
        );
        Self { root, game, cache }
    }
    fn profile_path(&self) -> PathBuf {
        self.cache.join("runtime_profile.json")
    }
    fn settings(&self) -> Settings {
        Settings {
            game_install: self.game.display().to_string(),
            cache_dir: self.cache.display().to_string(),
            ..Default::default()
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        assert_eq!(self.root.parent(), Some(std::env::temp_dir().as_path()));
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn context() -> qar::Context {
    setup_fixture::context()
}

fn prepare(f: &Fixture) -> foxcore::runtime_data::RuntimeProfile {
    setup::prepare_profile(
        &f.game,
        &f.profile_path(),
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap()
}

#[test]
fn setup_profile_native_profile_preparation_survey_unpack_and_dictionary_keep_game_immutable() {
    let f = Fixture::new();
    let data1 = fs::read(f.game.join("master/data1.dat")).unwrap();
    let override_bytes = fs::read(f.game.join("master/0/00.dat")).unwrap();
    prepare(&f);
    let profile = setup::load_profile(&f.game, &f.profile_path()).unwrap();
    let context = profile.qar_context();
    let archive = f.game.join("master/0/00.dat");
    let survey = setup::survey_with_context(&context, &f.game, &archive);
    assert!(survey.error.is_none());
    assert_eq!(survey.entries, 2);
    let dict = setup::load_dict("/Assets/tpp/script/first\n");
    let report = setup::unpack_with_context(
        &context,
        &f.game,
        std::slice::from_ref(&archive),
        &f.cache,
        &dict,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap();
    assert_eq!((report.written, report.named, report.unnamed), (2, 1, 1));
    assert!(report.errors.is_empty());
    assert_eq!(
        fs::read(f.cache.join("0_00/Assets/tpp/script/first.lua")).unwrap(),
        b"first"
    );
    let dictionary = f.cache.join("dictionary.txt");
    setup::build_dictionary_with_context(
        &context,
        &f.game,
        &[archive],
        &dictionary,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap();
    assert!(dictionary.is_file());
    assert_eq!(fs::read(f.game.join("master/data1.dat")).unwrap(), data1);
    assert_eq!(
        fs::read(f.game.join("master/0/00.dat")).unwrap(),
        override_bytes
    );
    assert!(!f.game.join("runtime_profile.json").exists());
}

#[test]
fn setup_profile_cancelled_preparation_and_aliasing_destinations_write_nothing() {
    let f = Fixture::new();
    assert!(
        setup::prepare_profile(
            &f.game,
            &f.profile_path(),
            &AtomicBool::new(true),
            &mut |_, _| {}
        )
        .unwrap_err()
        .contains("cancelled")
    );
    assert!(!f.cache.exists());
    let before = fs::read(f.game.join("master/data1.dat")).unwrap();
    let alias = f.root.join("not-created/../game/master/data1.dat");
    assert!(
        setup::prepare_profile(&f.game, &alias, &AtomicBool::new(false), &mut |_, _| {}).is_err()
    );
    assert_eq!(fs::read(f.game.join("master/data1.dat")).unwrap(), before);
    assert!(!f.root.join("not-created").exists());
}

#[test]
fn setup_profile_changed_install_profile_is_rejected_before_unpacking() {
    let f = Fixture::new();
    prepare(&f);
    let archive = f.game.join("master/data1.dat");
    let mut bytes = fs::read(&archive).unwrap();
    bytes[32] ^= 1;
    fs::write(archive, bytes).unwrap();
    assert!(setup::load_profile(&f.game, &f.profile_path()).is_err());
    assert!(!f.cache.join("0_00").exists());
}

#[test]
fn setup_profile_native_prerequisites_omit_python_and_private_stage_workflow_retains_it() {
    let f = Fixture::new();
    prepare(&f);
    let inputs = setup::PrereqInputs {
        game: Some(f.game.clone()),
        test_install: Some(f.game.clone()),
        cache: Some(f.cache.clone()),
        runtime_profile: Some(f.profile_path()),
        fox_exe: std::env::current_exe().unwrap(),
        python: "intentionally-unavailable-python".into(),
        ..Default::default()
    };
    let checks = setup::prereqs(&inputs);
    assert!(!checks.iter().any(|c| c.name.contains("Python")));
    assert!(
        checks
            .iter()
            .any(|c| c.name == "Local game metadata" && c.level == setup::Level::Ok)
    );
    let checks = setup::prereqs(&setup::PrereqInputs {
        needs_python: true,
        ..inputs
    });
    assert!(
        checks
            .iter()
            .any(|c| c.name == "Python (stages)" && c.level == setup::Level::Warn)
    );
}

#[test]
fn setup_profile_preparation_runs_on_a_serial_task_and_marks_only_its_captured_install_ready() {
    let f = Fixture::new();
    let mut view = SetupView::default();
    let ctx = eframe::egui::Context::default();
    view.prepare_profile(&f.settings(), &ctx).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while view.busy() && std::time::Instant::now() < deadline {
        view.poll();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(!view.busy());
    assert!(view.profile_ready(), "{:?}", view.profile_error());
    assert!(f.profile_path().is_file());
}

#[test]
fn setup_profile_wizard_prepares_native_metadata_then_unpacks_and_finishes_without_python() {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use foxstudio::settings::Tab;
    use foxstudio::{AppOptions, FoxStudio};
    use std::time::{Duration, Instant};

    let f = Fixture::new();
    fs::write(
        f.root.join("stages.toml"),
        "[[stage]]\nname = \"fixture\"\ncmd = [\"unused-native-tool\"]\noutputs = [\"unused\"]\n",
    )
    .unwrap();
    let tool = f.root.join("native-fox");
    fs::write(&tool, b"harmless fixture placeholder; never executed").unwrap();
    let settings = Settings {
        repo_root: f.root.display().to_string(),
        graph_file: "stages.toml".into(),
        fox_exe: tool.display().to_string(),
        game_dir: f.game.display().to_string(),
        python: "intentionally-unavailable-python".into(),
        last_tab: Tab::Setup,
        system_fonts: false,
        ..f.settings()
    };
    let mut harness = Harness::builder()
        .with_size([1440.0, 1600.0])
        .build_eframe(move |cc| {
            FoxStudio::new(
                &cc.egui_ctx,
                settings,
                None,
                AppOptions {
                    settings_path: None,
                    allow_real_builds: false,
                    probe_every: Duration::from_millis(200),
                },
            )
        });
    harness.state_mut().setup.step = foxstudio::setup_view::Step::Unpack;
    let wait = |harness: &mut Harness<'_, FoxStudio>, ready: fn(&FoxStudio) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready(harness.state()) {
            assert!(Instant::now() < deadline, "setup UI task timed out");
            harness.step();
            std::thread::sleep(Duration::from_millis(2));
        }
        harness.run_steps(3);
    };
    wait(&mut harness, |app| {
        app.setup.game_check().is_some() && !app.setup.busy()
    });
    assert!(!harness.state().setup.profile_ready());
    harness.get_by_label("Prepare game data").click();
    wait(&mut harness, |app| app.setup.profile_ready());
    wait(&mut harness, |app| app.setup.surveys().is_some());
    harness.state_mut().settings.unpack_select = vec!["0_00".into()];
    harness.run_steps(3);
    harness.get_by_label_contains("Unpack 1 archive").click();
    wait(&mut harness, |app| app.setup.unpack_result.is_some());
    let report = harness
        .state()
        .setup
        .unpack_result
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap();
    assert_eq!((report.archives, report.written), (1, 2));
    assert!(report.errors.is_empty());
    harness.get_by_label("Next").click();
    wait(&mut harness, |app| app.setup.checks().is_some());
    let checks = harness.state().setup.checks().unwrap();
    assert!(!checks.iter().any(|check| check.name.contains("Python")));
    assert!(checks.iter().all(|check| check.level != setup::Level::Fail));
    harness.get_by_label("Finish setup").click();
    harness.run_steps(3);
    assert!(harness.state().settings.setup_done);
    assert_eq!(harness.state().settings.last_tab, Tab::Project);
    assert!(f.profile_path().is_file());
    assert!(f.cache.join("0_00.index.json").is_file());
    assert!(!f.game.join("runtime_profile.json").exists());
}

#[cfg(unix)]
#[test]
fn setup_profile_filesystem_links_cannot_put_metadata_or_payloads_in_the_game() {
    let f = Fixture::new();
    let link = f.root.join("cache-alias");
    std::os::unix::fs::symlink(&f.game, &link).unwrap();
    assert!(setup::check_cache(&link, Some(&f.game)).is_err());
    prepare(&f);
    fs::create_dir_all(f.cache.join("0_00")).unwrap();
    std::os::unix::fs::symlink(&f.game, f.cache.join("0_00/Assets")).unwrap();
    let dict = setup::load_dict("/Assets/tpp/script/first\n");
    assert!(
        setup::unpack_with_context(
            &context(),
            &f.game,
            &[f.game.join("master/0/00.dat")],
            &f.cache,
            &dict,
            &AtomicBool::new(false),
            &mut |_, _| {}
        )
        .unwrap_err()
        .contains("leaves")
    );
    assert!(!f.game.join("tpp/script/first.lua").exists());
}

#[test]
fn setup_profile_case_aliases_fail_before_any_payload_is_written() {
    let f = Fixture::new();
    prepare(&f);
    let mut dict = std::collections::HashMap::new();
    for (name, destination) in [
        ("/Assets/tpp/script/first", "/Assets/tpp/case"),
        ("/Assets/tpp/script/second", "/assets/tpp/CASE"),
    ] {
        dict.insert(
            qar::path_hash(name) & setup::PATH_MASK,
            destination.to_owned(),
        );
    }
    let error = setup::unpack_with_context(
        &context(),
        &f.game,
        &[f.game.join("master/0/00.dat")],
        &f.cache,
        &dict,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap_err();
    assert!(error.contains("same cache file"));
    assert!(!f.cache.join("0_00").exists());
}
