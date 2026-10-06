//! Native location workflows in standalone folders; no build process, GPU or game assets.
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use foxstudio::project_editor::{LocationEditor, LocationForm};
use foxstudio::projects;
use std::path::{Path, PathBuf};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("q_location_editor_{}_{nonce}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn project(&self, name: &str) -> projects::Project {
        LocationForm::default().create(&self.0.join(name)).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = std::fs::canonicalize(&self.0).unwrap();
        assert!(root.starts_with(&temp) && root != temp);
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn new_location_is_valid_native_and_opens_without_a_cargo_workspace() {
    let fixture = Fixture::new();
    let form = LocationForm {
        title: "My \"quoted\" lake".into(),
        ..Default::default()
    };
    let project = form.create(&fixture.0.join("standalone island")).unwrap();
    assert!(project.m3.is_some());
    assert_eq!(project.m3.as_ref().unwrap().title, form.title);
    assert!(project.real_build_block().is_none());
    assert!(!project.root.join("tools/rust/Cargo.toml").exists());
    assert!(
        !project.root.join("work").exists(),
        "opening must not publish project facets/build state"
    );
    let plan = foxproject::plan_build_in(&project.root, &project.file).unwrap();
    assert_eq!(
        plan.graph().stage[0].cmd,
        ["fox", "project", "check", "project.toml", "--json"]
    );
    assert_eq!(projects::load(&project.file).unwrap().root, project.root);
}

#[test]
fn relocated_projects_with_the_same_code_keep_distinct_graph_snapshots_and_explicit_roots() {
    let fixture = Fixture::new();
    let first = fixture.project("first root");
    let second = fixture.project("second root");
    let a = projects::load_in(&first.root, Path::new("project.toml")).unwrap();
    let b = projects::load_in(&second.root, Path::new("project.toml")).unwrap();
    assert_ne!(a.graph, b.graph);
    assert_eq!(a.root, first.root);
    assert_eq!(b.root, second.root);
    assert!(a.heights().unwrap().starts_with(&a.root));
    assert!(b.heights().unwrap().starts_with(&b.root));
}

#[test]
fn invalid_creation_and_existing_projects_are_preserved() {
    let fixture = Fixture::new();
    let invalid = LocationForm {
        code: "../../escape".into(),
        ..Default::default()
    };
    assert!(invalid.create(&fixture.0.join("invalid")).is_err());
    assert!(!fixture.0.join("invalid").exists());
    let project = fixture.project("existing");
    let before = std::fs::read(&project.file).unwrap();
    assert!(
        LocationForm::default()
            .create(&project.root)
            .unwrap_err()
            .contains("already holds")
    );
    assert_eq!(std::fs::read(&project.file).unwrap(), before);
}

#[test]
fn unavailable_generation_commands_are_visible_before_a_build() {
    let fixture = Fixture::new();
    let project = fixture.project("capabilities");
    let mut text = std::fs::read_to_string(&project.file).unwrap();
    text.push_str("\n[[stage]]\nuse = \"nav.ground\"\n\n[[stage]]\nuse = \"package.mgsv\"\n");
    std::fs::write(&project.file, text).unwrap();
    let loaded = projects::load_in(&project.root, &project.file).unwrap();
    let reason = loaded.real_build_block().unwrap();
    assert!(reason.contains("nav (stage nav.ground)"));
    assert!(reason.contains("m3 (stage m3.mgsv)"));
    assert!(reason.contains("editor distribution"));
}

#[test]
fn editor_preserves_includes_recipes_and_custom_stages_when_saving_fields() {
    let fixture = Fixture::new();
    let project = fixture.project("edit");
    let mut original = std::fs::read_to_string(&project.file).unwrap();
    original = original.replace("[project]", "[project]\ninclude = [\"stages.toml\"]");
    original = format!("# My authored location\n{original}");
    original = original.replace(
        "title = \"My location\"",
        "title = \"My location\" # retain this note",
    );
    original.push_str("\n[recipe.mine]\nnote = 'keep this' # leave recipe formatting\n");
    std::fs::write(&project.file, &original).unwrap();
    let include = "[[stage]]\nname = \"custom.extra\"\ncmd = [\"fox\", \"hash\", \"path\", \"/Assets/synthetic\"]\n";
    std::fs::write(project.root.join("stages.toml"), include).unwrap();
    let mut editor = LocationEditor::open_in(&project.root, &project.file).unwrap();
    editor.form.title = "Edited island".into();
    editor.form.id = 99;
    editor.form.lake_y = Some(12.5);
    let refreshed = editor.save().unwrap();
    assert!(!editor.has_changes());
    assert_eq!(refreshed.m3.as_ref().unwrap().title, "Edited island");
    assert_eq!(refreshed.spec.preview.water_y, Some(12.5));
    let saved: toml::Table =
        toml::from_str(&std::fs::read_to_string(&project.file).unwrap()).unwrap();
    let text = std::fs::read_to_string(&project.file).unwrap();
    assert!(text.starts_with("# My authored location\n"));
    assert!(text.contains("title = \"Edited island\" # retain this note"));
    assert!(text.contains("note = 'keep this' # leave recipe formatting"));
    assert_eq!(saved["project"]["include"][0].as_str(), Some("stages.toml"));
    assert_eq!(saved["recipe"]["mine"]["note"].as_str(), Some("keep this"));
    assert_eq!(
        std::fs::read_to_string(project.root.join("stages.toml")).unwrap(),
        include
    );
    assert_eq!(
        foxproject::plan_build_in(&project.root, &project.file)
            .unwrap()
            .graph()
            .stage
            .len(),
        2
    );
}

#[test]
fn invalid_and_external_edits_fail_without_losing_saved_state() {
    let fixture = Fixture::new();
    let project = fixture.project("errors");
    let mut editor = LocationEditor::open_in(&project.root, &project.file).unwrap();
    let before = std::fs::read(&project.file).unwrap();
    editor.form.id = 0;
    assert!(editor.save().is_err());
    assert!(editor.has_changes());
    assert_eq!(std::fs::read(&project.file).unwrap(), before);
    editor.form.id = 99;
    std::fs::write(&project.file, "external editor bytes").unwrap();
    assert!(editor.save().unwrap_err().contains("outside this editor"));
    assert_eq!(
        std::fs::read_to_string(&project.file).unwrap(),
        "external editor bytes"
    );
}

#[test]
fn inherited_water_cannot_be_silently_reenabled_after_disabling_it() {
    let fixture = Fixture::new();
    let project = fixture.project("inherit");
    let original = std::fs::read_to_string(&project.file).unwrap();
    std::fs::write(project.root.join("base.toml"), "[terrain]\nlake_y = 4.5\n").unwrap();
    let text = format!("extends = \"base.toml\"\n{original}");
    std::fs::write(&project.file, &text).unwrap();
    let mut editor = LocationEditor::open_in(&project.root, &project.file).unwrap();
    assert_eq!(editor.form.lake_y, Some(4.5));
    editor.form.lake_y = None;
    assert!(editor.save().unwrap_err().contains("inherited"));
    assert_eq!(std::fs::read_to_string(&project.file).unwrap(), text);
}

#[test]
fn editing_a_title_preserves_inherited_fields_and_crlf_comments() {
    let fixture = Fixture::new();
    let root = fixture.0.join("inherited settings");
    std::fs::create_dir(&root).unwrap();
    let base = "[location]\ncode = \"inherit\"\nid = 99\ngrid = 3072\n[biome]\nsource = \"afgh\"\n";
    std::fs::write(root.join("base.toml"), base).unwrap();
    let original = "# Authored CRLF project\r\nformat = 1\r\nextends = 'base.toml'\r\n[project]\r\nname = 'inherited'\r\ntitle = 'Original title' # keep title note\r\n[[stage]]\r\nname = 'project.validate'\r\ncmd = ['fox', 'project', 'check', 'project.toml', '--json']\r\n";
    let file = root.join("project.toml");
    std::fs::write(&file, original).unwrap();
    let mut editor = LocationEditor::open_in(&root, &file).unwrap();
    editor.form.title = "New title".into();
    let changed_base = base.replace("id = 99", "id = 100") + "[terrain]\nlake_y = 5.0\n";
    std::fs::write(root.join("base.toml"), &changed_base).unwrap();
    editor.save().unwrap();
    assert_eq!(editor.form.id, 100);
    assert_eq!(editor.form.lake_y, Some(5.0));
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.starts_with("# Authored CRLF project\r\n"));
    assert!(saved.contains("title = \"New title\" # keep title note\r\n"));
    assert!(!saved.replace("\r\n", "").contains('\n'));
    let table: toml::Table = toml::from_str(&saved).unwrap();
    assert!(
        !table.contains_key("location")
            && !table.contains_key("biome")
            && !table.contains_key("terrain")
    );
    assert_eq!(
        std::fs::read_to_string(root.join("base.toml")).unwrap(),
        changed_base
    );
    std::fs::write(
        root.join("base.toml"),
        changed_base.replace("grid = 3072", "grid = 4096"),
    )
    .unwrap();
    let reopened = LocationEditor::open_in(&root, &file).unwrap();
    assert_eq!(
        reopened.form.grid, 4096,
        "unchanged inherited grid was frozen by a title edit"
    );
}

#[test]
fn clicking_save_returns_the_refreshed_project_with_clear_validation_errors() {
    let fixture = Fixture::new();
    let project = fixture.project("ui");
    struct View {
        editor: LocationEditor,
        saved: Option<projects::Project>,
    }
    let mut harness = Harness::builder().with_size([720.0, 600.0]).build_ui_state(
        |ui, state: &mut View| {
            if let Some(project) = state.editor.ui(ui) {
                state.saved = Some(project);
            }
        },
        View {
            editor: LocationEditor::open_in(&project.root, &project.file).unwrap(),
            saved: None,
        },
    );
    harness.get_by_label("Title").focus();
    harness.run_steps(2);
    harness.key_press_modifiers(eframe::egui::Modifiers::COMMAND, eframe::egui::Key::A);
    harness.run_steps(2);
    harness.get_by_label("Title").type_text("Saved from UI");
    harness.run_steps(2);
    harness.get_by_label("Save location").click();
    harness.run();
    assert_eq!(
        harness
            .state()
            .saved
            .as_ref()
            .unwrap()
            .m3
            .as_ref()
            .unwrap()
            .title,
        "Saved from UI"
    );
    harness.state_mut().editor.form.id = 0;
    harness.run_steps(2);
    harness.get_by_label("Save location").click();
    harness.run();
    assert!(
        harness
            .state()
            .editor
            .error
            .as_deref()
            .unwrap()
            .contains("location.id")
    );
    assert_eq!(
        foxpipe::project::Spec::load_in(&project.root, &project.file)
            .unwrap()
            .file()
            .location
            .id,
        LocationForm::default().id
    );
}

#[test]
fn generic_starter_uses_a_native_path_hash_action() {
    let name = "My \"island\"\nwith a newline";
    let graph = foxbuild::config::Graph::from_toml_str(&projects::starter_graph(name)).unwrap();
    assert_eq!(&graph.stage[0].cmd[..3], ["fox", "hash", "path"]);
    assert_eq!(
        graph.stage[0].cmd[3],
        format!("/Assets/tpp/level/location/{name}")
    );
    assert!(!projects::starter_graph(name).contains("python"));
}
