//! Projects. Two kinds of project file open here:
//! - an **M3 location spec** (`projects/<code>/project.toml`, read with `foxpipe::project::Spec`; PROJECTS.md): name,
//!   grid, datasets and the build graph generated from it (`foxproject::generate`). FLYK's spec declares
//!   `compat = "flyk"`: its generated graph equals tools/build/flyk_stages.toml (golden test), so that file is used.
//!   Native specs also open from standalone folders. Their app-owned graph snapshots are isolated by root/file;
//!   build execution uses explicit project identity and a selected native tool in the runner.
//! - a small Fox Studio `foxproject.toml` (`format = 1`, below): a graph file plus preview paths, for anything the
//!   M3 spec does not describe.
//!
//! ```toml
//! format = 1
//! [project]
//! name = "FLYK"
//! kind = "location"                  # location | mission | mod
//! description = "..."
//! root = "../.."                     # where stage commands run, relative to this file (default ".")
//! graph = "tools/build/flyk_stages.toml"   # relative to root
//! [preview]
//! heights = "work/flyk/m3/heightfield/heights.npy"   # .npy grid or .htre block (relative to root)
//! cell_m = 2.0
//! origin = [-2048.0, -2048.0]        # world x, z of heights[0][0] (default: grid centred on 0, 0)
//! water_y = 25.0
//! world_texture = "work/flyk/m3/worldtex"   # tiles ...X{ii}Z{jj}.ftex / .png draped on the terrain
//! nav = ["work/flyk/m4/nav/stage"]   # .nav2 files or folders
//! textures = ["work/flyk/m3/stage/worldtex"]
//! models = []
//! ```
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "foxproject.toml";
pub const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProjectFile {
    pub format: u32,
    pub project: Meta,
    #[serde(default)]
    pub preview: PreviewPaths,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub name: String,
    #[serde(default = "d_kind")]
    pub kind: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "d_root")]
    pub root: String,
    pub graph: String,
}

fn d_kind() -> String {
    "location".into()
}
fn d_root() -> String {
    ".".into()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct PreviewPaths {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub heights: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_m: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<[f32; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub water_y: Option<f32>,
    /// folder of world-texture tiles (...X00Z00.ftex / .png): draped over the terrain previews
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub world_texture: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nav: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub textures: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
}

/// What an M3 location spec adds.
#[derive(Clone, Debug, PartialEq)]
pub struct M3Info {
    pub code: String,
    pub title: String,
    /// `[stages] compat` ("flyk": legacy Python implementations allowed; the graph is flyk_stages.toml)
    pub compat: Option<String>,
    pub grid: usize,
    pub cell_m: f64,
    pub stages: usize,
    /// validation warnings (`fox project check`)
    pub issues: Vec<String>,
    /// why real builds are not offered for this project (None = they are)
    pub real_build_block: Option<String>,
}

/// A loaded project with absolute paths.
#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    pub file: PathBuf,
    pub spec: ProjectFile,
    pub root: PathBuf,
    pub graph: PathBuf,
    /// set when the file is an M3 location spec
    pub m3: Option<M3Info>,
}

impl Project {
    pub fn name(&self) -> &str {
        &self.spec.project.name
    }
    /// None, or why real builds are not offered
    pub fn real_build_block(&self) -> Option<&str> {
        self.m3.as_ref().and_then(|m| m.real_build_block.as_deref())
    }
    pub fn at_root(&self, rel: &str) -> PathBuf {
        let p = Path::new(rel);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            clean(&self.root.join(p))
        }
    }
    pub fn heights(&self) -> Option<PathBuf> {
        (!self.spec.preview.heights.is_empty()).then(|| self.at_root(&self.spec.preview.heights))
    }
    pub fn world_texture(&self) -> Option<PathBuf> {
        (!self.spec.preview.world_texture.is_empty())
            .then(|| self.at_root(&self.spec.preview.world_texture))
    }
    pub fn nav(&self) -> Vec<PathBuf> {
        self.spec
            .preview
            .nav
            .iter()
            .map(|p| self.at_root(p))
            .collect()
    }
    pub fn textures(&self) -> Vec<PathBuf> {
        self.spec
            .preview
            .textures
            .iter()
            .map(|p| self.at_root(p))
            .collect()
    }
    pub fn models(&self) -> Vec<PathBuf> {
        self.spec
            .preview
            .models
            .iter()
            .map(|p| self.at_root(p))
            .collect()
    }
}

/// `a/b/../c` -> `a/c` (lexically; no file-system access)
pub fn clean(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::CurDir => {}
            o => out.push(o.as_os_str()),
        }
    }
    out
}

/// Recognize an M3 spec even when location fields are supplied by an inherited project.
pub fn is_m3_spec(text: &str) -> bool {
    toml::from_str::<toml::Table>(text).is_ok_and(|table| {
        table.contains_key("location")
            || table.contains_key("extends")
            || table.contains_key("stage")
            || table
                .get("project")
                .and_then(toml::Value::as_table)
                .is_some_and(|project| project.contains_key("include"))
    })
}

/// Load a project from its file, or from a folder holding `foxproject.toml` or an M3 `project.toml`.
pub fn load(path: &Path) -> Result<Project, String> {
    load_project(path, None)
}

/// Open using the Settings repository. A standalone location needs no Cargo workspace marker.
pub fn load_in(root: &Path, path: &Path) -> Result<Project, String> {
    if root.as_os_str().is_empty() {
        return Err("project root: expected a nonempty directory path".into());
    }
    let root = std::path::absolute(root).map_err(|error| format!("project root: {error}"))?;
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    load_project(&path, Some(&root))
}

fn load_project(path: &Path, root: Option<&Path>) -> Result<Project, String> {
    let file = if path.is_dir() {
        if path.join(FILE_NAME).is_file() || !path.join(M3_FILE_NAME).is_file() {
            path.join(FILE_NAME)
        } else {
            path.join(M3_FILE_NAME)
        }
    } else {
        path.to_path_buf()
    };
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    if is_m3_spec(&text) {
        return match root {
            Some(root) => load_m3_in(root, &file),
            None => load_m3(&file),
        };
    }
    let spec: ProjectFile =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?;
    if spec.format == 0 || spec.format > FORMAT {
        return Err(format!(
            "{}: project format {} (this Fox Studio reads format {FORMAT})",
            file.display(),
            spec.format
        ));
    }
    if spec.project.name.trim().is_empty() {
        return Err(format!("{}: the project has no name", file.display()));
    }
    let dir = file.parent().map(|d| d.to_path_buf()).unwrap_or_default();
    let dir = if dir.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        dir
    };
    let root = {
        let r = Path::new(&spec.project.root);
        if r.is_absolute() {
            r.to_path_buf()
        } else {
            clean(&dir.join(r))
        }
    };
    let graph = {
        let g = Path::new(&spec.project.graph);
        if g.is_absolute() {
            g.to_path_buf()
        } else {
            clean(&root.join(g))
        }
    };
    Ok(Project {
        file,
        spec,
        root,
        graph,
        m3: None,
    })
}

pub const M3_FILE_NAME: &str = "project.toml";

/// the repository holding an M3 spec: the folder above `projects/<code>/project.toml` with tools/rust/Cargo.toml
fn repo_of(spec_file: &Path) -> Option<PathBuf> {
    let mut d = spec_file.parent()?.to_path_buf();
    loop {
        if crate::settings::is_repo(&d) {
            return Some(d);
        }
        if !d.pop() {
            return None;
        }
    }
}

/// Load an M3 location spec (read only: nothing is written into the repository).
pub fn load_m3(file: &Path) -> Result<Project, String> {
    let file = if file.is_absolute() {
        file.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(file))
            .unwrap_or(file.to_path_buf())
    };
    let root = repo_of(&file).unwrap_or_else(|| {
        let folder = file.parent().unwrap_or(Path::new("."));
        if folder
            .parent()
            .is_some_and(|parent| parent.file_name().is_some_and(|name| name == "projects"))
        {
            folder
                .parent()
                .and_then(Path::parent)
                .unwrap_or(folder)
                .to_path_buf()
        } else {
            folder.to_path_buf()
        }
    });
    load_m3_in(&root, &file)
}

/// Metadata and app-owned graph snapshot for a location at an explicit root. No spec/facet write occurs.
pub fn load_m3_in(root: &Path, file: &Path) -> Result<Project, String> {
    let spec = foxpipe::project::Spec::load_in(root, file).map_err(|error| error.0)?;
    let root = spec.repo_root().to_path_buf();
    let file = spec.path().to_path_buf();
    let loc = spec.loc().clone();
    let compat = spec.compat().map(|s| s.to_string());
    let generated =
        foxproject::generate(&spec).map_err(|e| format!("{}: graph: {}", file.display(), e.0))?;
    let stages = generated.graph.stage.len();
    let (graph, block) = if compat.as_deref() == Some("flyk") {
        // golden-equal to the generated graph (crates/foxproject/tests/golden_flyk.rs)
        (
            root.join("tools").join("build").join("flyk_stages.toml"),
            None,
        )
    } else {
        let text = foxproject::graph_toml(&spec, &generated.graph).map_err(|e| e.0)?;
        let dir = crate::settings::config_dir().join("graphs");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        // Two relocated projects may share a code. Their app-owned graph snapshots must not overwrite each other.
        let mut identity = std::collections::hash_map::DefaultHasher::new();
        root.hash(&mut identity);
        file.hash(&mut identity);
        let out = dir.join(format!("{}-{:016x}.toml", loc.code, identity.finish()));
        if std::fs::read_to_string(&out).ok().as_deref() != Some(text.as_str()) {
            std::fs::write(&out, &text).map_err(|e| format!("{}: {e}", out.display()))?;
        }
        (out, editor_build_block(&generated.graph))
    };
    // repo-relative dataset strings, resolved against this repository (not the process's working directory)
    let ds = |id: &str| spec.dataset_rel(id).to_string();
    let preview = PreviewPaths {
        heights: ds("heights"),
        cell_m: Some(loc.d as f32),
        origin: Some([-(loc.half() as f32), -(loc.half() as f32)]),
        water_y: loc.lake_y.map(|y| y as f32),
        world_texture: ds("worldtex"),
        nav: vec![format!("{}/stage", ds("nav_out"))],
        textures: vec![format!("{}/worldtex", ds("m3_stage")), ds("worldtex")],
        // the project's own models: imported flora and the dressing outputs
        models: vec![ds("flora_imports_out"), ds("dressing_out")],
    };
    let title = spec.file().project.title.clone();
    let pf = ProjectFile {
        format: FORMAT,
        project: Meta {
            name: spec.name().to_uppercase(),
            kind: "location (M3 spec)".into(),
            description: title.clone(),
            root: root.display().to_string(),
            graph: graph.display().to_string(),
        },
        preview,
    };
    let info = M3Info {
        code: loc.code.clone(),
        title,
        compat,
        grid: loc.grid,
        cell_m: loc.d,
        stages,
        issues: spec
            .issues()
            .iter()
            .map(|i| format!("{}: {}", i.field, i.msg))
            .collect(),
        real_build_block: block,
    };
    Ok(Project {
        file,
        spec: pf,
        root,
        graph,
        m3: Some(info),
    })
}

fn editor_build_block(graph: &foxbuild::config::Graph) -> Option<String> {
    let unavailable: Vec<_> = graph
        .stage
        .iter()
        .filter_map(|stage| {
            let program = stage.cmd.first()?.as_str();
            if !matches!(program, "fox" | "fox.exe" | foxproject::FOX_EXE) {
                return None;
            }
            let command = stage.cmd.get(1)?;
            if matches!(
                command.as_str(),
                "nav" | "terrain" | "place" | "m3" | "mission" | "parallax" | "npy-roundtrip"
            ) {
                Some(format!("{} (stage {})", command, stage.name))
            } else {
                None
            }
        })
        .collect();
    if unavailable.is_empty() {
        None
    } else {
        Some(format!(
            "These commands are unavailable in the editor distribution: {}. Project validation and packaging existing assets are available.",
            unavailable.join(", ")
        ))
    }
}

/// the M3 spec of FLYK in a repository
pub fn flyk_spec(repo: &Path) -> PathBuf {
    repo.join("projects").join("flyk").join(M3_FILE_NAME)
}

/// The starter checks the chosen project's Fox Engine path hash with the native tool.
pub fn starter_graph(name: &str) -> String {
    let path = toml::Value::String(format!("/Assets/tpp/level/location/{name}"));
    format!(
        "# Native project path identity (foxbuild: docs/release/BUILD.md).\n\
         [settings]\n\
         log_dir = \"build\"\n\
         temp_dir = \"tmp\"\n\
         ignore = [\"build/\", \"tmp/\"]\n\
         \n\
         [[stage]]\n\
         name = \"project.path_hash\"\n\
         cmd = [\"fox\", \"hash\", \"path\", {path}]\n\
         owner = \"you\"\n\
         mem_gb = 0.1\n"
    )
}

/// Create a new project in `dir` (made if missing): foxproject.toml + stages.toml. Refuses to overwrite.
pub fn create(dir: &Path, name: &str, kind: &str) -> Result<Project, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("give the project a name".into());
    }
    let file = dir.join(FILE_NAME);
    if file.exists() {
        return Err(format!(
            "{} already exists: open it instead",
            file.display()
        ));
    }
    let graph = dir.join("stages.toml");
    if graph.exists() {
        return Err(format!(
            "{} already exists: pick an empty folder",
            graph.display()
        ));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let spec = ProjectFile {
        format: FORMAT,
        project: Meta {
            name: name.into(),
            kind: kind.into(),
            description: String::new(),
            root: ".".into(),
            graph: "stages.toml".into(),
        },
        preview: PreviewPaths::default(),
    };
    std::fs::write(&graph, starter_graph(name)).map_err(|e| format!("{}: {e}", graph.display()))?;
    let text = format!(
        "# Fox Studio project (format {FORMAT})\n{}",
        toml::to_string_pretty(&spec).map_err(|e| e.to_string())?
    );
    std::fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))?;
    load(&file)
}

/// the example FLYK project shipped with Fox Studio (inside the tools repository)
pub fn flyk_example(repo: &Path) -> PathBuf {
    repo.join("tools")
        .join("rust")
        .join("crates")
        .join("foxstudio")
        .join("projects")
        .join("flyk")
        .join(FILE_NAME)
}

/// most-recent-first list without duplicates, at most `max`
pub fn push_recent(list: &mut Vec<String>, path: &str, max: usize) {
    let key = crate::steam::norm(Path::new(path));
    list.retain(|p| crate::steam::norm(Path::new(p)) != key);
    list.insert(0, path.to_string());
    list.truncate(max);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("foxstudio_proj_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn create_load_and_refuse_overwrite() {
        let d = tmp("create");
        let p = create(&d, "My Lake", "location").unwrap();
        assert_eq!(p.name(), "My Lake");
        assert_eq!(clean(&p.root), clean(&d));
        assert!(p.graph.ends_with("stages.toml"));
        // the starter graph is a valid foxbuild graph
        let g = foxbuild::config::Graph::load(&p.graph).unwrap();
        assert_eq!(g.stage.len(), 1);
        assert!(
            create(&d, "again", "location")
                .unwrap_err()
                .contains("already exists")
        );
        assert!(create(&tmp("noname"), "  ", "location").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn relative_root_graph_and_preview_paths() {
        let d = tmp("rel");
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::write(
            d.join("a/b").join(FILE_NAME),
            "format = 1\n[project]\nname = \"X\"\nroot = \"../..\"\ngraph = \"g/stages.toml\"\n[preview]\nheights = \"h/heights.npy\"\nnav = [\"n\"]\n",
        )
        .unwrap();
        let p = load(&d.join("a/b")).unwrap();
        assert_eq!(p.root, clean(&d));
        assert_eq!(p.graph, clean(&d.join("g/stages.toml")));
        assert_eq!(p.heights().unwrap(), clean(&d.join("h/heights.npy")));
        assert_eq!(p.nav(), vec![clean(&d.join("n"))]);
        assert_eq!(p.spec.project.kind, "location");
        // unknown keys and future formats are errors, not silent
        std::fs::write(
            d.join("a/b").join(FILE_NAME),
            "format = 1\n[project]\nname = \"X\"\ngraph = \"g\"\nbogus = 1\n",
        )
        .unwrap();
        assert!(load(&d.join("a/b")).is_err());
        std::fs::write(
            d.join("a/b").join(FILE_NAME),
            "format = 2\n[project]\nname = \"X\"\ngraph = \"g\"\n",
        )
        .unwrap();
        assert!(load(&d.join("a/b")).unwrap_err().contains("format 2"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn standalone_location_opens_with_its_graph_and_datasets() {
        let root = tmp("standalone");
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join(M3_FILE_NAME);
        std::fs::write(
            &file,
            r#"format = 1
[project]
name = "example"
title = "Synthetic location"
[location]
code = "example"
id = 99
grid = 2048
grid_distance = 2.0
[terrain]
lake_y = 12.5
[biome]
source = "mafr"
[build]
pinned = [{ path = "project.toml", owner = "project" }]
[[stage]]
name = "project.validate"
cmd = ["fox", "project", "check", "project.toml", "--json"]
owner = "author"
mem_gb = 1.0
"#,
        )
        .unwrap();

        let project = load_in(&root, &file).unwrap();
        let location = project.m3.as_ref().expect("a location spec");
        assert_eq!(project.name(), "EXAMPLE");
        assert_eq!(project.root, clean(&root));
        assert_eq!(location.code, "example");
        assert_eq!(location.compat, None);
        assert_eq!((location.grid, location.cell_m), (2048, 2.0));
        assert_eq!(location.stages, 1);
        assert!(project.real_build_block().is_none());
        let graph = foxbuild::config::Graph::load(&project.graph).unwrap();
        assert_eq!(graph.stage[0].name, "project.validate");
        assert_eq!(
            project.heights().unwrap(),
            root.join("work/projects/example/m3/heightfield/heights.npy")
        );
        assert_eq!(
            project.nav(),
            vec![root.join("work/projects/example/m4/nav/stage")]
        );
        assert_eq!(
            project.world_texture().unwrap(),
            root.join("work/projects/example/m3/worldtex")
        );
        assert_eq!(project.spec.preview.origin, Some([-2048.0, -2048.0]));
        assert_eq!(project.spec.preview.water_y, Some(12.5));
        assert_eq!(load(&root).unwrap().m3.unwrap().code, "example");
        assert!(!root.join("work").exists());
        assert!(!root.join("tools/rust/Cargo.toml").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    #[ignore = "requires the private Mission51 checkout and its live FLYK graph; excluded from the public source export"]
    fn flyk_example_points_at_the_real_graph() {
        let repo = clean(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.."));
        let p = load(&flyk_example(&repo)).unwrap();
        assert_eq!(p.name(), "FLYK");
        assert_eq!(p.root, repo);
        assert_eq!(
            p.graph,
            repo.join("tools").join("build").join("flyk_stages.toml")
        );
        assert!(p.graph.is_file());
    }

    #[test]
    #[ignore = "requires the private Mission51 project and live FLYK graph; excluded from the public source export"]
    fn flyk_m3_spec_opens_with_its_graph_and_datasets() {
        let repo = clean(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.."));
        let f = flyk_spec(&repo);
        let p = load(&f).unwrap();
        let m = p.m3.as_ref().expect("an M3 spec");
        assert_eq!(m.code, "flyk");
        assert_eq!(m.compat.as_deref(), Some("flyk"));
        assert_eq!((m.grid, m.cell_m), (2048, 2.0));
        assert!(m.stages > 50);
        assert!(p.real_build_block().is_none());
        assert_eq!(
            p.graph,
            repo.join("tools").join("build").join("flyk_stages.toml")
        );
        assert_eq!(
            p.heights().unwrap(),
            repo.join("work/flyk/m3/heightfield/heights.npy")
        );
        assert_eq!(p.nav(), vec![repo.join("work/flyk/m4/nav/stage")]);
        assert_eq!(
            p.world_texture().unwrap(),
            repo.join("work/flyk/m3/worldtex")
        );
        assert_eq!(p.spec.preview.origin, Some([-2048.0, -2048.0]));
        assert_eq!(p.spec.preview.water_y, Some(25.0));
        // the folder form finds project.toml too
        assert_eq!(load(f.parent().unwrap()).unwrap().m3.unwrap().code, "flyk");
    }

    #[test]
    fn recent_list() {
        let mut v = vec![];
        push_recent(&mut v, "C:/a/foxproject.toml", 3);
        push_recent(&mut v, "C:/b/foxproject.toml", 3);
        push_recent(&mut v, "c:\\A\\foxproject.toml", 3);
        assert_eq!(
            v,
            vec![
                "c:\\A\\foxproject.toml".to_string(),
                "C:/b/foxproject.toml".into()
            ]
        );
        push_recent(&mut v, "1", 3);
        push_recent(&mut v, "2", 3);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], "2");
    }
}
