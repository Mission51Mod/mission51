//! Actual application workflows on authored fixtures. The driver gives one serial
//! child an isolated FOX_STUDIO_HOME; no global settings, stage, install, window or GPU.
use eframe::egui;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use foxstudio::{
    AppOptions, FoxStudio,
    about::{AboutView, NoticeKind},
    project_editor::LocationForm,
    settings::{self, Settings, Tab, ThemeChoice},
    theme::{self, Contrast},
};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "p_app_release_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let path = self.0.canonicalize().unwrap();
        let temporary = std::env::temp_dir().canonicalize().unwrap();
        assert!(path.starts_with(&temporary) && path != temporary);
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("p_app_release_")
        );
        std::fs::remove_dir_all(path).unwrap();
    }
}

#[test]
fn app_release_flow_with_isolated_settings() {
    let scratch = Scratch::new("config");
    let before = std::fs::read(settings::default_path()).ok();
    let inherited = std::env::var_os("FOX_STUDIO_HOME");
    let filters = std::env::var("P_APP_RELEASE_CASES").unwrap_or_else(|_| "app_release_case_".into());
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--test-threads=1", "--nocapture"])
        .args(filters.split(';').filter(|filter| !filter.is_empty()))
        .env("FOX_STUDIO_HOME", scratch.0.join("config"))
        .output()
        .unwrap();
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        output.status.success(),
        "isolated actual-app cases failed: {:?}",
        output.status
    );
    assert_eq!(std::env::var_os("FOX_STUDIO_HOME"), inherited);
    assert_eq!(std::fs::read(settings::default_path()).ok(), before);
}

struct Fixture {
    scratch: Scratch,
    root: PathBuf,
    file: PathBuf,
    original: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        assert!(
            std::env::var_os("FOX_STUDIO_HOME").is_some(),
            "run through the isolated driver"
        );
        let scratch = Scratch::new("project");
        let root = scratch.0.join("first");
        let file = write_location(&root, "newland", "Original island");
        let original = std::fs::read(&file).unwrap();
        Self {
            scratch,
            root,
            file,
            original,
        }
    }

    fn settings(&self) -> Settings {
        Settings {
            repo_root: self.root.display().to_string(),
            project_file: "project.toml".into(),
            bundle_manifest: self.scratch.0.join("missing-fox-tools.json").display().to_string(),
            fox_exe: self.scratch.0.join("missing-fox").display().to_string(),
            last_tab: Tab::Project,
            theme: ThemeChoice::Dark,
            system_fonts: false,
            setup_done: true,
            ..Default::default()
        }
    }

    fn unchanged(&self) {
        assert_eq!(std::fs::read(&self.file).unwrap(), self.original);
    }

    fn other(&self) -> PathBuf {
        let root = self.scratch.0.join("second");
        write_location(&root, "otherland", "Second island");
        root
    }
}

fn write_location(root: &Path, name: &str, title: &str) -> PathBuf {
    std::fs::create_dir(root).unwrap();
    let form = LocationForm {
        name: name.into(),
        code: "island".into(),
        title: title.into(),
        ..Default::default()
    };
    let mut text = form.starter_text().unwrap().replace("\r\n", "\n");
    text = format!("# Preserve authored comment\n{text}");
    text = text.replace(
        &format!("title = \"{title}\""),
        &format!("title = \"{title}\" # retain title note"),
    );
    text.push_str("\n[recipe.fixture]\nnote = 'unchanged custom recipe'\n");
    let file = root.join("project.toml");
    std::fs::write(&file, text.replace('\n', "\r\n")).unwrap();
    file
}

fn harness(settings: Settings) -> Harness<'static, FoxStudio> {
    Harness::builder()
        .with_size([1440.0, 1400.0])
        .build_eframe(move |context| {
            FoxStudio::new(
                &context.egui_ctx,
                settings,
                None,
                AppOptions {
                    settings_path: None,
                    allow_real_builds: false,
                    probe_every: Duration::from_millis(200),
                },
            )
        })
}

fn click(harness: &mut Harness<'_, FoxStudio>, label: &str) {
    // Tab names also occur in informational rows. The first matching control is the topmost tab.
    harness
        .get_all_by_label(label)
        .min_by(|left, right| left.rect().center().y.total_cmp(&right.rect().center().y))
        .unwrap_or_else(|| panic!("no control labelled {label}"))
        .click();
    harness.run_steps(3);
}

fn edit_title(harness: &mut Harness<'_, FoxStudio>, title: &str) {
    click(harness, "Edit location");
    harness.get_by_label("Title").focus();
    harness.run_steps(2);
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness.run_steps(2);
    harness.get_by_label("Title").type_text(title);
    harness.run_steps(2);
    assert_eq!(harness.state().location_editor.as_ref().unwrap().form.title, title);
}

fn sees_close(harness: &mut Harness<'_, FoxStudio>) -> bool {
    (0..5).any(|_| {
        harness.step();
        harness.output().viewport_output.values().any(|viewport| {
            viewport
                .commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Close))
        })
    })
}

fn wait(harness: &mut Harness<'_, FoxStudio>, ready: impl Fn(&FoxStudio) -> bool) {
    let start = Instant::now();
    loop {
        harness.step();
        if ready(harness.state()) {
            harness.run_steps(2);
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "fixture worker timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "executed serially by the isolated-settings driver"]
fn app_release_case_new_location_uses_typed_native_form() {
    let fixture = Fixture::new();
    let mut harness = harness(fixture.settings());
    click(&mut harness, "New project…");
    let folder = fixture.scratch.0.join("created-location");
    {
        let new = harness.state_mut().new_project.as_mut().unwrap();
        assert_eq!(new.kind, "location");
        assert!(new.name.is_empty());
        new.location.name = "created".into();
        new.location.code = "created".into();
        new.location.title = "Created from Studio".into();
        new.folder = folder.display().to_string();
    }
    harness.run_steps(2);
    assert!(!harness.get_by_label("Create").accesskit_node().is_disabled());
    click(&mut harness, "Create");
    let app = harness.state();
    assert!(app.new_project.is_none());
    let project = app.open_project().unwrap();
    assert_eq!(project.name(), "CREATED");
    assert_eq!(project.m3.as_ref().unwrap().title, "Created from Studio");
    assert_eq!(project.root, folder);
    assert!(folder.join("project.toml").is_file());
    assert!(!folder.join("Cargo.toml").exists() && !folder.join("tools/rust/build.py").exists());
    assert!(app.location_editor.is_some());
    assert!(!app.location_has_changes());
    assert!(app.build.run.is_none());
    fixture.unchanged();
}

#[test]
#[ignore = "executed serially by the isolated-settings driver"]
fn app_release_case_generic_mod_creator_is_retained() {
    let fixture = Fixture::new();
    let mut harness = harness(fixture.settings());
    click(&mut harness, "New project…");
    let folder = fixture.scratch.0.join("created-mod");
    {
        let new = harness.state_mut().new_project.as_mut().unwrap();
        new.kind = "mod".into();
        new.name = "Synthetic mod".into();
        new.folder = folder.display().to_string();
    }
    harness.run_steps(2);
    click(&mut harness, "Create");
    assert_eq!(harness.state().open_project().unwrap().name(), "Synthetic mod");
    assert!(folder.join("foxproject.toml").is_file() && folder.join("stages.toml").is_file());
    assert!(harness.state().location_editor.is_none());
    assert!(harness.state().build.run.is_none());
    fixture.unchanged();
}

#[test]
#[ignore = "executed serially by the isolated-settings driver"]
fn app_release_case_edit_save_updates_metadata_and_preserves_authored_source() {
    let fixture = Fixture::new();
    let mut harness = harness(fixture.settings());
    edit_title(&mut harness, "Saved through actual UI");
    click(&mut harness, "Save location");
    assert!(!harness.state().location_has_changes());
    assert_eq!(
        harness.state().open_project().unwrap().m3.as_ref().unwrap().title,
        "Saved through actual UI"
    );
    let saved = std::fs::read_to_string(&fixture.file).unwrap();
    assert!(saved.starts_with("# Preserve authored comment\r\n"));
    assert!(saved.contains("title = \"Saved through actual UI\" # retain title note"));
    assert!(saved.contains("note = 'unchanged custom recipe'"));
    assert!(!saved.replace("\r\n", "").contains('\n'), "CRLF was not preserved");
    assert!(harness.state().build.run.is_none());
}

#[test]
#[ignore = "executed serially by the isolated-settings driver"]
fn app_release_case_close_cancel_then_discard_preserves_saved_file() {
    let fixture = Fixture::new();
    let mut harness = harness(fixture.settings());
    edit_title(&mut harness, "Draft stays");
    click(&mut harness, "Close project");
    harness.get_by_label("Unsaved location changes");
    click(&mut harness, "Cancel");
    assert!(harness.state().location_has_changes());
    assert!(harness.state().open_project().is_some());
    fixture.unchanged();
    click(&mut harness, "Close project");
    click(&mut harness, "Discard changes");
    assert!(harness.state().open_project().is_none());
    assert!(!harness.state().location_has_changes());
    fixture.unchanged();
}

#[test]
#[ignore = "executed serially by the isolated-settings driver"]
fn app_release_case_save_before_navigation_selects_new_document() {
    let fixture = Fixture::new();
    let other = fixture.other();
    let mut harness = harness(fixture.settings());
    edit_title(&mut harness, "Saved before navigation");
    harness.state_mut().open_project_path(&other.join("project.toml"));
    harness.run_steps(2);
    click(&mut harness, "Save changes");
    assert_eq!(harness.state().open_project().unwrap().name(), "OTHERLAND");
    assert!(
        std::fs::read_to_string(&fixture.file)
            .unwrap()
            .contains("Saved before navigation")
    );
    assert!(!harness.state().location_has_changes());
}

#[test]
#[ignore = "executed serially by the isolated-settings driver"]
fn app_release_case_invalid_and_stale_save_cancel_pending_navigation() {
    let fixture = Fixture::new();
    let mut harness = harness(fixture.settings());
    edit_title(&mut harness, "Draft rejected");
    harness.state_mut().location_editor.as_mut().unwrap().form.id = 0;
    click(&mut harness, "Close project");
    click(&mut harness, "Save changes");
    assert!(harness.query_by_label("Unsaved location changes").is_none());
    assert!(harness.state().open_project().is_some());
    assert!(harness.state().location_has_changes());
    assert!(harness.state().editor_error.is_some());
    fixture.unchanged();
    harness.state_mut().location_editor.as_mut().unwrap().form.id = 2;
    let external = fixture
        .original
        .iter()
        .copied()
        .chain(b"# external edit\r\n".iter().copied())
        .collect::<Vec<_>>();
    std::fs::write(&fixture.file, &external).unwrap();
    click(&mut harness, "Close project");
    click(&mut harness, "Save changes");
    assert!(
        harness
            .state()
            .editor_error
            .as_ref()
            .unwrap()
            .contains("outside this editor")
    );
    assert!(harness.state().location_has_changes());
    assert!(harness.state().open_project().is_some());
    assert_eq!(std::fs::read(&fixture.file).unwrap(), external);
}

#[test]
#[ignore = "executed serially by the isolated-settings driver"]
fn app_release_case_reload_same_path_refreshes_metadata_and_editor() {
    let fixture = Fixture::new();
    let mut harness = harness(fixture.settings());
    let external = String::from_utf8(fixture.original.clone())
        .unwrap()
        .replace("Original island", "Externally edited island");
    std::fs::write(&fixture.file, &external).unwrap();
    click(&mut harness, "Reload project");
    assert_eq!(
        harness.state().open_project().unwrap().m3.as_ref().unwrap().title,
        "Externally edited island"
    );
    assert_eq!(
        harness.state().location_editor.as_ref().unwrap().form.title,
        "Externally edited island"
    );
    assert!(!harness.state().location_has_changes());
    assert!(harness.state().build.run.is_none());
    assert_eq!(std::fs::read_to_string(&fixture.file).unwrap(), external);
}

#[test]
#[ignore = "executed serially by the isolated-settings driver"]
fn app_release_case_root_apply_waits_for_draft_decision_and_rekeys_relative_file() {
    let fixture = Fixture::new();
    let other = fixture.other();
    let mut harness = harness(fixture.settings());
    edit_title(&mut harness, "Root draft");
    click(&mut harness, "Settings");
    harness.get_by_label("Project root").focus();
    harness.run_steps(2);
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness.run_steps(2);
    harness
        .get_by_label("Project root")
        .type_text(&other.display().to_string());
    harness.run_steps(2);
    assert_eq!(harness.state().settings.repo_root, fixture.root.display().to_string());
    assert!(harness.query_by_label("Unsaved location changes").is_none());
    click(&mut harness, "Apply root");
    harness.get_by_label("Unsaved location changes");
    click(&mut harness, "Cancel");
    assert_eq!(harness.state().open_project().unwrap().name(), "NEWLAND");
    assert!(harness.state().location_has_changes());
    harness.state_mut().settings.repo_root = other.display().to_string();
    harness.run_steps(2);
    click(&mut harness, "Discard changes");
    assert_eq!(harness.state().open_project().unwrap().name(), "OTHERLAND");
    assert_eq!(harness.state().open_project().unwrap().root, other);
    assert_eq!(
        harness.state().location_editor.as_ref().unwrap().form.title,
        "Second island"
    );
    fixture.unchanged();
}

#[test]
#[ignore = "executed serially by the isolated-settings driver"]
fn app_release_case_quit_cancel_and_save_share_the_draft_guard() {
    let fixture = Fixture::new();
    let mut harness = harness(fixture.settings());
    edit_title(&mut harness, "Quit-saved draft");
    harness.state_mut().request_quit();
    harness.step();
    harness.get_by_label("Unsaved location changes");
    click(&mut harness, "Cancel");
    assert!(!sees_close(&mut harness));
    assert!(harness.state().location_has_changes());
    fixture.unchanged();
    harness.state_mut().request_quit();
    harness.step();
    harness.get_by_label("Save changes").click();
    assert!(
        sees_close(&mut harness),
        "successful draft save must complete the requested quit"
    );
    assert!(
        std::fs::read_to_string(&fixture.file)
            .unwrap()
            .contains("Quit-saved draft")
    );
    assert!(!harness.state().location_has_changes());
}

#[test]
#[ignore = "executed serially by the isolated-settings driver"]
fn app_release_case_settings_apply_high_contrast_and_reuse_read_only_about() {
    let fixture = Fixture::new();
    let mut harness = harness(fixture.settings());
    let notices = fixture.scratch.0.join("notices");
    std::fs::create_dir(&notices).unwrap();
    for name in ["NOTICE", "THIRD_PARTY.txt", "LICENSE-MIT", "LICENSE-APACHE"] {
        std::fs::write(notices.join(name), format!("Synthetic portable {name}: café.\r\n")).unwrap();
    }
    assert!(harness.state().about.is_none(), "About must be lazy");
    harness.state_mut().about = Some(AboutView::load_from_dir(&notices));
    click(&mut harness, "Settings");
    harness.get_by_label("About Fox Studio");
    click(&mut harness, "High contrast");
    assert!(harness.state().settings.high_contrast);
    assert_eq!(theme::contrast(&harness.ctx), Contrast::High);
    let saved: Settings = serde_json::from_slice(&serde_json::to_vec(&harness.state().settings).unwrap()).unwrap();
    assert!(saved.high_contrast);
    std::fs::write(notices.join("NOTICE"), "changed after read").unwrap();
    harness.run_steps(3);
    assert_eq!(
        harness
            .state()
            .about
            .as_ref()
            .unwrap()
            .document(NoticeKind::Project)
            .text()
            .unwrap(),
        "Synthetic portable NOTICE: café.\r\n"
    );
    fixture.unchanged();
}

#[test]
#[ignore = "executed serially by the isolated-settings driver"]
fn app_release_case_portable_project_and_bundle_failure_keep_real_builds_blocked() {
    let fixture = Fixture::new();
    let mut harness = harness(fixture.settings());
    wait(&mut harness, |app| app.bundle_error.is_some() && app.project().probed);
    harness.get_by_label("Project and build graph");
    assert!(harness.query_by_label("Cargo.toml").is_none());
    assert!(harness.query_by_label("Python launcher").is_none());
    assert!(harness.query_by_label("Run python tools/rust/build.py").is_none());
    click(&mut harness, "Build");
    harness.get_by_label("Select a validated native tool bundle in Settings before a real build.");
    assert!(
        harness
            .get_all_by_label("Build")
            .any(|node| node.accesskit_node().is_disabled())
    );
    assert!(harness.state().build.run.is_none());
    harness
        .state_mut()
        .project
        .as_mut()
        .unwrap()
        .as_mut()
        .unwrap()
        .m3
        .as_mut()
        .unwrap()
        .compat = Some("flyk".into());
    harness.run_steps(3);
    harness.get_by_label("Compatibility recipes are unavailable in the public editor. Open a native project.");
    assert!(
        harness
            .get_all_by_label("Build")
            .any(|node| node.accesskit_node().is_disabled())
    );
    fixture.unchanged();
}
