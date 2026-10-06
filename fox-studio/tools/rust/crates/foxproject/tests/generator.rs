//! Generator rules on synthetic projects: Rust-only for non-FLYK projects (refused by name), derived edges,
//! strict params / instance keys, custom stages validated by foxbuild.
use foxpipe::project::{LoadOptions, Spec};

const BASE: &str = r#"
format = 1
[project]
name = "tst"
[location]
code = "tst"
id = 99
"#;

fn spec(stages: &str) -> Spec {
    let origin = foxpipe::paths::repo("projects/tst/project.toml");
    Spec::from_toml_str(&format!("{BASE}{stages}"), &origin, &LoadOptions::default()).unwrap_or_else(|e| panic!("{e}"))
}

fn gen_err(stages: &str) -> String {
    match foxproject::generate(&spec(stages)) {
        Ok(_) => panic!("accepted:\n{stages}"),
        Err(e) => e.0,
    }
}

#[test]
fn rust_only_for_new_projects_with_derived_edges() {
    let g = foxproject::generate(&spec(r#"
[[stage]]
use = "nav.ground"
[[stage]]
use = "nav.sky"
[[stage]]
use = "package.terrain"
[[stage]]
use = "package.packs"
[[stage]]
use = "package.mgsv"
[[stage]]
use = "package.verify"
"#)).unwrap();
    let st = |n: &str| g.graph.stage.iter().find(|s| s.name == n).unwrap();
    assert_eq!(st("nav.ground").cmd, ["work/rust/target/release/fox.exe", "nav", "all"]);
    assert!(st("nav.ground").rust_tools);
    assert_eq!(st("m3.packs").cmd, ["work/rust/target/release/fox.exe", "m3", "packs"]);
    assert_eq!(st("m3.packs").outputs, ["work/projects/tst/m3/mod", "work/projects/tst/m3/stage"]);
    assert_eq!(st("m3.packs").after, ["nav.ground", "nav.sky"]);
    assert_eq!(st("m3.mgsv").after, ["m3.packs"]);
    assert_eq!(st("m3.verify").after, ["m3.mgsv"]);
    assert_eq!(st("m3.mgsv").outputs, ["work/projects/tst/m3/build/tst_m3.mgsv"]);
    assert!(st("m3.terrain").env.is_empty());
    assert_eq!(g.graph.settings.log_dir, "work/build/tst");
}

#[test]
fn legacy_kinds_are_refused_by_name() {
    let e = gen_err("[[stage]]\nuse = \"dressing.pack\"\n");
    assert!(e.contains("dressing.pack") && e.contains("no Rust implementation") && e.contains("FLYK-only"), "{e}");
    let e = gen_err("[[stage]]\nuse = \"nav.ground\"\nimpl = \"legacy\"\n");
    assert!(e.contains("compat"), "{e}");
}

#[test]
fn strict_instances() {
    assert!(gen_err("[[stage]]\nuse = \"nav.ground\"\nparams = { x = 1 }\n").contains("unknown param x"));
    assert!(gen_err("[[stage]]\nuse = \"nav.ground\"\nafterr = []\n").contains("afterr"));
    assert!(gen_err("[[stage]]\nuse = \"no.such\"\n").contains("unknown stage kind"));
    // custom stages: foxbuild's own keys and validation
    assert!(gen_err("[[stage]]\nname = \"a\"\ncmd = [\"x\"]\ncmd_rust = []\n").contains("cmd_rust"));
    assert!(gen_err("[[stage]]\nname = \"a\"\ncmd = [\"x\"]\nafter = [\"b\"]\n").contains("unknown stage b"));
    // two writers of one dataset
    assert!(gen_err("[[stage]]\nuse = \"nav.ground\"\n[[stage]]\nuse = \"nav.ground\"\nname = \"nav.ground2\"\n")
        .contains("written by both"));
}

#[test]
fn overrides_and_after_extra() {
    let g = foxproject::generate(&spec(r#"
[[stage]]
name = "prep"
cmd = ["x"]
[[stage]]
use = "nav.ground"
after_extra = ["prep"]
owner = "me"
mem_gb = 1.5
env = { A = "1" }
"#)).unwrap();
    let s = g.graph.stage.iter().find(|s| s.name == "nav.ground").unwrap();
    assert_eq!(s.after, ["prep"]);
    assert_eq!((s.owner.as_str(), s.mem_gb), ("me", 1.5));
    assert_eq!(s.env.get("A").map(|s| s.as_str()), Some("1"));
}

#[test]
fn new_project_template_is_valid() {
    let origin = foxpipe::paths::repo("projects/tst/project.toml");
    let s = Spec::from_toml_str(&foxproject::cli::new_project_text("tst", 99), &origin, &LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let g = foxproject::generate(&s).unwrap();
    assert_eq!(g.graph.stage.len(), 7);
    assert!(g.graph.stage.iter().all(|st| st.cmd[0] == foxproject::FOX_EXE));
}

#[test]
fn dataset_aliases_cannot_hide_writers_or_dependencies() {
    let e = gen_err(r#"
[datasets]
sky_out = "{nav_out}"
[[stage]]
use = "nav.ground"
[[stage]]
use = "nav.sky"
"#);
    assert!(e.contains("written by both nav.ground and nav.sky") && e.contains("sky_out"), "{e}");
    // Foxbuild treats path keys case-insensitively, even on the headless Linux proof host.
    let e = gen_err(r#"
[datasets]
sky_out = "work/projects/tst/M4/NAV"
[[stage]]
use = "nav.ground"
[[stage]]
use = "nav.sky"
"#);
    assert!(e.contains("written by both"), "{e}");
    let g = foxproject::generate(&spec(r#"
[nav]
obstacle_sets = ["fauna_out"]
[datasets]
fauna_out = "{sky_out}"
[[stage]]
use = "nav.ground"
[[stage]]
use = "nav.sky"
"#)).unwrap();
    let ground = g.graph.stage.iter().find(|s| s.name == "nav.ground").unwrap();
    assert_eq!(ground.after, ["nav.sky"]);
}

#[test]
fn output_metadata_cannot_relocate_a_library_dataset() {
    let e = gen_err("[[stage]]\nuse = \"nav.ground\"\noutputs = [\"work/unrelated\"]\n");
    assert!(e.contains("outputs omit dataset nav_out") && e.contains("[datasets]"), "{e}");
    let e = gen_err("[[stage]]\nname = \"escape\"\ncmd = [\"x\"]\noutputs = [\"../outside\"]\n");
    assert!(e.contains("stage escape: outputs"), "{e}");
    // Declaring a containing directory is valid metadata; it does not change the actual dataset path.
    let g = foxproject::generate(&spec("[[stage]]\nuse = \"nav.ground\"\noutputs = [\"work/projects/tst/m4\"]\n")).unwrap();
    assert_eq!(g.graph.stage[0].outputs, ["work/projects/tst/m4"]);
    let g = foxproject::generate(&spec("[[stage]]\nuse = \"nav.ground\"\noutputs = [\"work/projects/tst/m4/nav/generated\"]\n")).unwrap();
    assert_eq!(g.graph.stage[0].outputs, ["work/projects/tst/m4/nav/generated"]);
    let e = gen_err("[[stage]]\nuse = \"nav.ground\"\noutputs = [\"work/projects/tst/m4/nav_sibling\"]\n");
    assert!(e.contains("outputs omit dataset nav_out"), "{e}");
    let e = gen_err("[[stage]]\nuse = \"package.mgsv\"\noutputs = [\"work/projects/tst/m3/build/tst_m3.mgsv/child\"]\n");
    assert!(e.contains("outputs omit dataset mgsv"), "{e}");
}

#[test]
fn writable_dataset_cannot_alias_a_static_cache() {
    let e = gen_err(r#"
[datasets]
nav_out = "work/nav/custom"
[[stage]]
use = "nav.ground"
"#);
    assert!(e.contains("read-only static dataset nav_templates"), "{e}");
}
