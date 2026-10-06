//! Spec parse / defaults / validation / include / extends tests (one bad fixture per rule).
use super::*;

/// a minimal valid new-location spec (code tst, grid 2048)
const BASE: &str = r#"
format = 1
[project]
name = "tst"
[location]
code = "tst"
id = 99
"#;

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("foxpipe_project_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn origin() -> PathBuf {
    crate::paths::repo("projects/tst/project.toml")
}

fn load(text: &str) -> Result<Spec> {
    Spec::from_toml_str(text, &origin(), &LoadOptions::default())
}

/// BASE with `key = value` set (dotted key; tables created as needed)
fn with(key: &str, value: &str) -> String {
    let mut t: toml::Table = toml::from_str(BASE).unwrap();
    let v: toml::Value = toml::from_str::<toml::Table>(&format!("v = {value}"))
        .unwrap()
        .remove("v")
        .unwrap();
    let parts: Vec<&str> = key.split('.').collect();
    let mut cur = &mut t;
    for p in &parts[..parts.len() - 1] {
        cur = cur
            .entry(p.to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .unwrap();
    }
    cur.insert(parts[parts.len() - 1].to_string(), v);
    toml::to_string(&t).unwrap()
}

#[test]
fn minimal_spec_defaults() {
    let s = load(BASE).unwrap();
    let l = s.loc();
    assert_eq!(
        (l.grid, l.d, l.first_block, l.ring_small, l.ring_slod0),
        (2048, 2.0, 101, 4, 3)
    );
    assert_eq!(
        (l.nblk(), l.center_index(), l.wt_tiles, l.wt_px, l.nc()),
        (32, 117, 4, 2048, 64)
    );
    assert_eq!(l.ih_name, "TST");
    assert_eq!(
        s.dataset_rel("heights"),
        "work/projects/tst/m3/heightfield/heights.npy"
    );
    assert_eq!(s.dataset_rel("spec_cache"), "work/projects/tst/spec");
    assert_eq!(s.mod_name(), "TST location");
    assert!(s.test_mission().is_none());
    assert_eq!(s.sky_cover(), [-2304.0, -2304.0, 2304.0, 2304.0]);
    assert!(s.issues().is_empty(), "{:?}", s.issues());
    assert_eq!(s.rel(), "projects/tst/project.toml");
}

#[test]
fn unknown_key_is_an_error() {
    let e = load(&with("location.gird", "2048")).unwrap_err().0;
    assert!(e.contains("gird"), "{e}");
    let e = load(&with("nav.sky.stepp", "8.0")).unwrap_err().0;
    assert!(e.contains("stepp"), "{e}");
}

#[test]
fn one_bad_fixture_per_rule() {
    let cases: &[(&str, &str, &str)] = &[
        ("format", "2", "format"),
        ("project.name", "\"X\"", "project.name"),
        ("project.kind", "\"vanilla-edit\"", "project.kind"),
        ("location.code", "\"Tst\"", "location.code"),
        ("location.code", "\"mafr\"", "location.code"),
        ("location.id", "20", "location.id"),
        ("location.id", "0", "location.id"),
        ("location.ih_name", "\"\"", "location.ih_name"),
        ("location.grid", "3000", "location.grid"),
        ("location.grid", "1024", "location.grid"),
        ("location.grid_distance", "1.0", "location.grid_distance"),
        ("location.first_block", "100", "location.first_block"),
        ("location.ring_small", "1", "location.ring_small"),
        (
            "location.weather",
            "[[\"SUNNY\", 50], [\"CLOUDY\", 40]]",
            "location.weather",
        ),
        ("location.weather", "[[\"SNOWY\", 100]]", "location.weather"),
        (
            "location.map",
            "{ lang_id = \"x\", height = \"a.ftex\", photo = \"/Assets/b.ftex\" }",
            "location.map.height",
        ),
        (
            "terrain.heights.source",
            "\"magic\"",
            "terrain.heights.source",
        ),
        (
            "terrain.heights.dataset",
            "\"nope\"",
            "terrain.heights.dataset",
        ),
        (
            "terrain.heights.source",
            "\"recipe\"",
            "terrain.heights.recipe",
        ),
        (
            "terrain.heights.expect",
            "\"xyz\"",
            "terrain.heights.expect",
        ),
        (
            "terrain.heights_pins",
            "{ base = \"ABC\" }",
            "terrain.heights_pins.base",
        ),
        (
            "terrain.materials.painter",
            "\"recipe:flyk\"",
            "terrain.materials.painter",
        ),
        (
            "terrain.materials.painter",
            "\"paint\"",
            "terrain.materials.painter",
        ),
        ("terrain.cluster", "48", "terrain.cluster"),
        (
            "terrain.world_texture.tiles",
            "3",
            "terrain.world_texture.tiles",
        ),
        (
            "terrain.world_texture.game_path",
            "\"/Assets/x.ftex\"",
            "terrain.world_texture.game_path",
        ),
        ("water", "{ impl = \"legacy:flyk_water\" }", "water.impl"),
        ("water", "{ impl = \"water.flood\" }", "terrain.lake_y"),
        ("nav.band", "[1.9, 0.3]", "nav.band"),
        ("nav.max_wade", "0.0", "nav.max_wade"),
        ("nav.tile_shift", "-1", "nav.tile_shift"),
        ("nav.obstacle_sets", "[\"nope\"]", "nav.obstacle_sets"),
        (
            "nav.area",
            "[{ name = \"a\", x0 = 2000.0, z0 = 0.0, size = 640.0, nav2 = \"/Assets/a.nav2\", fox2 = \"/Assets/a.fox2\" }]",
            "nav.area.a",
        ),
        (
            "nav.area",
            "[{ name = \"a\", x0 = 0.0, z0 = 0.0, size = 64.0, nav2 = \"a.nav2\", fox2 = \"/Assets/a.fox2\" }]",
            "nav.area.a.nav2",
        ),
        ("nav.sky.cover", "[10.0, 0.0, -10.0, 5.0]", "nav.sky.cover"),
        ("nav.sky.step", "0.0", "nav.sky.step"),
        (
            "large_blocks",
            "{ size_in_bytes = 0 }",
            "large_blocks.size_in_bytes",
        ),
        (
            "large_blocks",
            "{ size_in_bytes = 1, block = [{ name = \"b\", pack = \"/Assets/p\", fox2_dir = \"/Assets/d\", region = { x = [90, 100], y = [101, 102] } }] }",
            "large_blocks.block.b",
        ),
        (
            "test_mission",
            "{ code = 10230, name = \"t\", start = [0.0, 0.0, 0.0, 0.0] }",
            "test_mission.code",
        ),
        (
            "test_mission",
            "{ code = 12052, name = \"t\", start = [5000.0, 0.0, 0.0, 0.0] }",
            "test_mission.start",
        ),
        (
            "test_mission",
            "{ code = 12052, name = \"\", start = [0.0, 0.0, 0.0, 0.0] }",
            "test_mission.name",
        ),
        ("mission", "[{ code = 10230 }, { code = 10230 }]", "mission"),
        ("paths.work", "\"../outside\"", "paths.work"),
        ("datasets", "{ heights = \"C:/x/heights.npy\" }", "datasets"),
        (
            "datasets",
            "{ spec_cache = \"work/build/spec\" }",
            "paths.spec_cache",
        ),
        ("datasets", "{ nope = \"work/x\" }", "datasets"),
        ("protected_zone", "[{ name = \"z\" }]", "protected_zone.z"),
        (
            "protected_zone",
            "[{ name = \"z\", box = [0.0, 10.0, 0.0, 10.0], rules = [\"no_fun\"] }]",
            "protected_zone.z",
        ),
        ("sites", "{ a = [0.0, 1.0] }", "sites.a"),
        (
            "package.templates",
            "{ ih_location = \"templates/missing.lua\" }",
            "package.templates.ih_location",
        ),
        ("build.pinned", "[{ owner = \"x\" }]", "build.pinned[0]"),
        (
            "build.pinned",
            "[{ dataset = \"nope\" }]",
            "build.pinned[0]",
        ),
        (
            "build.pinned",
            "[{ path = \"../outside\" }]",
            "build.pinned[0].path",
        ),
        ("build.log_dir", "\"C:/outside\"", "build.log_dir"),
        ("build.temp_dir", "\"work/../outside\"", "build.temp_dir"),
        ("stages.compat", "\"flyk\"", "stages.compat"),
        ("stages.compat", "\"other\"", "stages.compat"),
        (
            "workspace",
            "{ data_root = \"tools/x\" }",
            "workspace.data_root",
        ),
    ];
    for (key, value, field) in cases {
        let r = load(&with(key, value));
        let e = match r {
            Ok(_) => panic!("{key} = {value}: accepted"),
            Err(e) => e.0,
        };
        assert!(
            e.contains(&format!("error: {field}")),
            "{key} = {value}: expected an error on {field}, got:\n{e}"
        );
    }
}

#[test]
fn unproven_values_are_allowed_for_offline_tests() {
    let t = with("location.grid", "1024");
    assert!(load(&t).is_err());
    let s = Spec::from_toml_str(
        &t,
        &origin(),
        &LoadOptions {
            allow_unproven: true,
        },
    )
    .unwrap();
    assert_eq!(
        (s.loc().nblk(), s.loc().center_index(), s.loc().wt_tiles),
        (16, 109, 2)
    );
}

#[test]
fn warnings_for_registered_and_community_ids() {
    let s = load(&with("location.id", "68")).unwrap();
    assert!(
        s.issues()
            .iter()
            .any(|i| i.field == "location.id" && i.severity == Severity::Warning)
    );
}

#[test]
fn includes_and_extends() {
    let d = tmpdir("inc");
    std::fs::create_dir_all(d.join("p/stages")).unwrap();
    std::fs::create_dir_all(d.join("p/templates")).unwrap();
    std::fs::write(d.join("p/templates/loc.lua"), "x").unwrap();
    std::fs::write(
        d.join("p/stages/b.toml"),
        "[[stage]]\nname = \"b\"\ncmd = [\"x\"]\n",
    )
    .unwrap();
    std::fs::write(
        d.join("p/stages/a.toml"),
        "[[stage]]\nname = \"a\"\ncmd = [\"x\"]\n",
    )
    .unwrap();
    std::fs::write(
        d.join("p/project.toml"),
        "format = 1
[project]
name = \"tst\"
include = [\"stages/*.toml\"]
[location]
code = \"tst\"
id = 99
[package.templates]
ih_location = \"templates/loc.lua\"
[[stage]]
name = \"own\"
cmd = [\"y\"]
",
    )
    .unwrap();
    let s = Spec::load(d.join("p/project.toml")).unwrap();
    let names: Vec<&str> = s
        .file()
        .stage
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["a", "b", "own"]);
    let tpl = s.file().package.templates.ih_location.clone().unwrap();
    assert!(tpl.ends_with("p/templates/loc.lua"), "{tpl}");
    // a workspace extends the base: inherits stages and templates, overrides one value, rebases datasets
    std::fs::create_dir_all(d.join("ws")).unwrap();
    std::fs::write(d.join("ws/project.toml"),
                   "extends = \"../p/project.toml\"\n[nav]\nmax_wade = 0.5\n[workspace]\ndata_root = \"work/editor/ws/r\"\n").unwrap();
    let w = Spec::load(d.join("ws/project.toml")).unwrap();
    assert_eq!(w.file().stage.len(), 3);
    assert_eq!(w.nav().max_wade, 0.5);
    assert_eq!(w.loc().code, "tst");
    assert_eq!(
        w.dataset_rel("heights"),
        "work/editor/ws/r/work/projects/tst/m3/heightfield/heights.npy"
    );
    // a stage file with anything but [[stage]] is refused; extends cycles are refused
    std::fs::write(d.join("p/stages/c.toml"), "[location]\ncode = \"x\"\n").unwrap();
    assert!(
        Spec::load(d.join("p/project.toml"))
            .unwrap_err()
            .0
            .contains("only [[stage]]")
    );
    std::fs::remove_file(d.join("p/stages/c.toml")).unwrap();
    std::fs::write(d.join("ws/project.toml"), "extends = \"project.toml\"\n").unwrap();
    assert!(
        Spec::load(d.join("ws/project.toml"))
            .unwrap_err()
            .0
            .contains("cycle")
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn test_mission_paths() {
    let s = load(&with(
        "test_mission",
        "{ code = 12052, name = \"m51_tst\", start = [10.0, 5.0, 20.0, 90.0] }",
    ))
    .unwrap();
    let t = s.test_mission().unwrap();
    assert_eq!(t.pack, "/Assets/tpp/pack/mission2/custom/tst/tst_test");
    assert_eq!(
        t.seq_fox2,
        "/Assets/tpp/level/mission2/custom/tst/tst_test_sequence.fox2"
    );
    assert_eq!(t.file, "m51_tst.lua");
}

#[test]
fn facets_written_only_when_changed() {
    let d = tmpdir("facets");
    let s = load(BASE).unwrap();
    let changed = s.write_facets_to(&d).unwrap();
    assert_eq!(changed.len(), FACETS.len());
    assert!(s.write_facets_to(&d).unwrap().is_empty());
    let loc: Value =
        serde_json::from_slice(&std::fs::read(d.join("location.json")).unwrap()).unwrap();
    assert_eq!(loc["loc"]["grid"], 2048);
    assert_eq!(
        s.facet_path("nav"),
        crate::paths::repo("work/projects/tst/spec/nav.json")
    );
    let _ = std::fs::remove_dir_all(&d);
}


#[test]
fn paths_are_checked_before_workspace_rebase_including_static_overrides() {
    for (id, path) in [
        ("heights", "/outside/heights.npy"),
        ("heights", "work/../outside"),
        ("nav_templates", "C:/outside"),
        ("nav_templates", "../outside"),
        ("nav_templates", "/outside"),
        ("heights", "work/unclosed}"),
        ("heights", "work/a\nb"),
    ] {
        let path = toml::Value::String(path.into()).to_string();
        let text = format!(
            "{BASE}\n[datasets]\n{id} = {path}\n[workspace]\ndata_root = \"work/editor/ws/r\"\n"
        );
        let e = load(&text).unwrap_err().0;
        assert!(e.contains(&format!("dataset {id}:")), "{id}: {e}");
    }
    // A workspace leaves a legitimate static cache shared and rebases only project data.
    let s = load(&format!("{BASE}\n[datasets]\nnav_templates = \"work/shared/nav\"\n[workspace]\ndata_root = \"work/editor/ws/r\"\n")).unwrap();
    assert_eq!(s.dataset_rel("nav_templates"), "work/shared/nav");
    assert_eq!(
        s.dataset_rel("heights"),
        "work/editor/ws/r/work/projects/tst/m3/heightfield/heights.npy"
    );
}

#[test]
fn loader_preprocessing_never_discards_wrong_types() {
    for value in ["42", "[]", "{}", "\"\""] {
        let e = load(&format!("extends = {value}\n{BASE}")).unwrap_err().0;
        assert!(e.contains("extends: expected"), "{e}");
    }
    for value in ["42", "[42]", "[\"stages/a.toml\", 42]", "[\"\"]"] {
        let e = load(&with("project.include", value)).unwrap_err().0;
        assert!(e.contains("project.include"), "{e}");
    }
    let d = tmpdir("strict_inc");
    std::fs::create_dir_all(d.join("stages")).unwrap();
    std::fs::write(
        d.join("stages/a.toml"),
        "[[stage]]\nname = \"a\"\ncmd = [\"x\"]\n",
    )
    .unwrap();
    let inc = with("project.include", "[\"stages/*.toml\"]");
    let own = Spec::from_toml_str(
        &format!("stage = 42\n{inc}"),
        &d.join("project.toml"),
        &LoadOptions::default(),
    );
    assert!(own.unwrap_err().0.contains("stage: expected"));
    std::fs::write(
        d.join("stages/b.toml"),
        "stage = { name = \"b\", cmd = [\"x\"] }\n",
    )
    .unwrap();
    let e = Spec::from_toml_str(&inc, &d.join("project.toml"), &LoadOptions::default())
        .unwrap_err()
        .0;
    assert!(e.contains("b.toml: stage: expected"), "{e}");
    let missing = with("project.include", "[\"stages/no*.toml\"]");
    let e = Spec::from_toml_str(&missing, &d.join("project.toml"), &LoadOptions::default())
        .unwrap_err()
        .0;
    assert!(e.contains("no stage files matched"), "{e}");
    let bad_glob = with("project.include", "[\"st*/a.toml\"]");
    let e = Spec::from_toml_str(&bad_glob, &d.join("project.toml"), &LoadOptions::default())
        .unwrap_err()
        .0;
    assert!(e.contains("final component"), "{e}");
    let _ = std::fs::remove_dir_all(d);
}
