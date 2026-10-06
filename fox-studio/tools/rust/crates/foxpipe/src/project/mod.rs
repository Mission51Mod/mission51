//! foxpipe::project: the location project spec (docs/release/PROJECT_API.md, M3_PLAN.md §2).
//!
//! A project is `projects/<code>/project.toml` (format 1). Every stage process finds its spec with
//! [`Spec::current`]: `FOX_PROJECT` (absolute or repo-relative), else `projects/flyk/project.toml`, so hand runs and the
//! existing proof harnesses keep resolving FLYK.
//!
//! ```ignore
//! let spec = foxpipe::project::Spec::current();
//! let loc = spec.loc();                       // grid geometry + game paths (records the "location" facet)
//! let h = spec.dataset("heights");             // <repo>/work/flyk/m3/heightfield/heights.npy (records "datasets")
//! let s = spec.dataset_rel("nav_out");         // "work/flyk/m4/nav": the exact string manifests embed
//! ```
//!
//! **Facets** (precise invalidation): the resolved spec is split into one JSON file per section under the
//! `spec_cache` dataset (`location.json`, `datasets.json`, `nav.json`, ...). The section accessors record a foxbuild
//! trace read of their facet, so editing [nav] re-runs only the stages that read nav. `Spec::file()` is the raw
//! model and records NOTHING: for tooling (the graph generator, the editor, `fox project`), never for stage code.
//!
//! Loading: `include` (stage files) and `extends` (a base spec; editor workspaces) are resolved relative to the file
//! that names them; tables merge recursively, arrays and values of the derived file replace the base's. Template
//! paths are stored repo-relative after loading.
pub mod biome;
pub mod datasets;
pub mod loc;
pub mod model;
pub mod validate;

pub use biome::Biome;
pub use datasets::{Dataset, Datasets};
pub use loc::Loc;
pub use model::SpecFile;

use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// the spec format this build reads
pub const FORMAT: i64 = 1;
/// the spec used when FOX_PROJECT is unset
pub const DEFAULT_SPEC: &str = "projects/flyk/project.toml";

/// Facet (section) names, in resolved-JSON order.
pub const FACETS: &[&str] = &["project", "location", "biome", "terrain", "water", "nav", "large_blocks", "test_mission",
                              "mission", "mod", "package", "datasets", "protected_zone", "sites", "build", "stages",
                              "recipe"];

#[derive(Debug, Clone)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl From<String> for Error {
    fn from(s: String) -> Self {
        Error(s)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug, Serialize)]
pub struct Issue {
    pub severity: Severity,
    /// dotted field path, e.g. "location.grid"
    pub field: String,
    pub msg: String,
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        write!(f, "{s}: {}: {}", self.field, self.msg)
    }
}

#[derive(Clone, Debug, Default)]
pub struct LoadOptions {
    /// accept values the engine has not been probed with yet (grid 1024, first_block != 101): synthetic-spec tests
    pub allow_unproven: bool,
}

/// IH location add-on values (spec, else the biome's defaults)
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct IhParams {
    pub location_id: i64,
    pub location_name: String,
    pub description: String,
    pub heli_space: i64,
    pub weather: Vec<(String, i64)>,
    pub extra_weather: Vec<(String, i64)>,
    pub map: model::MapSec,
}

/// The test mission, resolved.
#[derive(Clone, Debug, Serialize)]
pub struct TestMission {
    pub code: i64,
    /// IH missionName
    pub name: String,
    /// the IH mission file name: <name>.lua
    pub file: String,
    pub description: String,
    /// mission pack (no extension)
    pub pack: String,
    pub seq_fox2: String,
    pub seq_lua: String,
    pub start: [f64; 4],
    pub ih_minimal: String,
}

/// A loaded, validated, resolved project spec.
#[derive(Debug)]
pub struct Spec {
    repo_root: PathBuf,
    path: PathBuf,
    rel: String,
    file: SpecFile,
    loc: Loc,
    datasets: Datasets,
    issues: Vec<Issue>,
    resolved: OnceLock<Value>,
    facets_written: OnceLock<std::result::Result<Vec<String>, String>>,
}

static CURRENT: OnceLock<std::result::Result<Spec, String>> = OnceLock::new();

impl Spec {
    /// The project of this process: FOX_PROJECT (absolute or repo-relative), else projects/flyk/project.toml.
    /// Loaded once. An invalid spec is fatal: the message names the file and every problem.
    pub fn current() -> &'static Spec {
        match Self::try_current() {
            Ok(s) => s,
            Err(e) => panic!("{e}"),
        }
    }

    pub fn try_current() -> Result<&'static Spec> {
        CURRENT
            .get_or_init(|| Spec::load(current_path()).map_err(|e| e.0))
            .as_ref()
            .map_err(|e| Error(e.clone()))
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Spec> {
        Self::load_with(path, &LoadOptions::default())
    }

    pub fn load_with(path: impl AsRef<Path>, opts: &LoadOptions) -> Result<Spec> {
        Self::load_with_in(crate::paths::repo_root(), path, opts)
    }

    /// Load against an explicit repository root without changing process environment or working directory.
    pub fn load_in(repo_root: impl AsRef<Path>, path: impl AsRef<Path>) -> Result<Spec> {
        Self::load_with_in(repo_root, path, &LoadOptions::default())
    }

    pub fn load_with_in(repo_root: impl AsRef<Path>, path: impl AsRef<Path>, opts: &LoadOptions) -> Result<Spec> {
        let root = load_root(repo_root.as_ref())?;
        let path = abs(&root, path.as_ref());
        let table = load_table(&root, &path, &mut vec![])?;
        Self::from_table(table, &root, &path, opts)
    }

    /// A spec from text (tests, `fox project new`): `origin` is the file it would be (relative includes / extends /
    /// templates resolve against its folder).
    pub fn from_toml_str(text: &str, origin: &Path, opts: &LoadOptions) -> Result<Spec> {
        Self::from_toml_str_in(&crate::paths::repo_root(), text, origin, opts)
    }

    /// Text/origin loading with the same explicit-root include/template and dataset semantics as `load_in`.
    pub fn from_toml_str_in(repo_root: &Path, text: &str, origin: &Path, opts: &LoadOptions) -> Result<Spec> {
        let root = load_root(repo_root)?;
        let origin = abs(&root, origin);
        let table = parse_table(text, &root, &origin, &mut vec![])?;
        Self::from_table(table, &root, &origin, opts)
    }

    fn from_table(table: toml::Table, repo_root: &Path, path: &Path, opts: &LoadOptions) -> Result<Spec> {
        let shown = path.display().to_string();
        let file: SpecFile = toml::Value::Table(table)
            .try_into()
            .map_err(|e: toml::de::Error| Error(format!("{shown}: {e}")))?;
        let code = file.location.code.clone();
        let work = file.paths.work.clone().unwrap_or_else(|| format!("work/projects/{code}"));
        let mut overrides = file.datasets.clone();
        if !overrides.contains_key("spec_cache") && file.paths.spec_cache != "{work}/spec" {
            overrides.insert("spec_cache".into(), file.paths.spec_cache.clone());
        }
        let ds = Datasets::resolve(&work, &code, &file.biome.source, &overrides,
                                   file.workspace.as_ref().map(|w| w.data_root.as_str()))
            .map(|d| d.with_root(repo_root.to_path_buf()));
        let ignore = build_ignore(&file);
        let mut issues = validate::validate(&file, repo_root, opts, ds.as_ref().ok(), &ignore);
        let ds = match ds {
            Ok(d) => Some(d),
            Err(e) => {
                issues.push(Issue { severity: Severity::Error, field: "datasets".into(), msg: e });
                None
            }
        };
        let errors: Vec<String> = issues.iter().filter(|i| i.severity == Severity::Error).map(|i| i.to_string()).collect();
        if !errors.is_empty() {
            return Err(Error(format!("{shown}: invalid project spec:\n  {}", errors.join("\n  "))));
        }
        let l = &file.location;
        let extent = l.grid as f64 * l.grid_distance;
        let loc = Loc {
            code: code.clone(),
            id: l.id,
            ih_name: l.ih_name.clone().unwrap_or_else(|| code.to_uppercase()),
            grid: l.grid,
            d: l.grid_distance,
            first_block: l.first_block,
            htre_n: loc::HTRE_N,
            ring_small: l.ring_small,
            ring_slod0: l.ring_slod0,
            cluster: file.terrain.cluster,
            wt_tiles: file.terrain.world_texture.tiles.unwrap_or((extent / 1024.0) as usize),
            wt_px: file.terrain.world_texture.px,
            wt_path_fmt: file.terrain.world_texture.game_path.clone().unwrap_or_else(|| loc::DEFAULT_WT_PATH.into()),
            lake_y: file.terrain.lake_y,
        };
        let rel = rel_of(repo_root, path);
        Ok(Spec { repo_root: repo_root.to_path_buf(), path: path.to_path_buf(), rel, file, loc, datasets: ds.unwrap(),
                  issues: issues.into_iter().filter(|i| i.severity == Severity::Warning).collect(),
                  resolved: OnceLock::new(), facets_written: OnceLock::new() })
    }

    // ---- identity, raw model (unrecorded)

    /// Absolute repository root captured at load time; no process-global lookup.
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    /// absolute path of the spec file
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// repo-relative path of the spec file ('/'), or the absolute path when outside the repo
    pub fn rel(&self) -> &str {
        &self.rel
    }
    pub fn code(&self) -> &str {
        &self.loc.code
    }
    pub fn name(&self) -> &str {
        &self.file.project.name
    }
    /// the raw (defaulted) model. Records NO facet read: tooling only, never stage code.
    pub fn file(&self) -> &SpecFile {
        &self.file
    }
    /// warnings of the last validation (errors fail the load)
    pub fn issues(&self) -> &[Issue] {
        &self.issues
    }
    /// "flyk": legacy Python stage implementations allowed
    pub fn compat(&self) -> Option<&str> {
        self.file.stages.compat.as_deref()
    }

    // ---- section accessors (each records its facet)

    pub fn loc(&self) -> &Loc {
        self.touch("location");
        &self.loc
    }
    pub fn datasets(&self) -> &Datasets {
        self.touch("datasets");
        &self.datasets
    }
    /// Dataset metadata for graph planners and frontends; records no trace or facet writes.
    /// Stage code uses `datasets()` so its dataset dependencies remain traced.
    pub fn datasets_untraced(&self) -> &Datasets {
        &self.datasets
    }
    /// absolute path of a dataset (panics on an unknown id: ids are registry constants)
    pub fn dataset(&self, id: &str) -> PathBuf {
        self.datasets().path(id)
    }
    /// repo-relative string of a dataset: the exact string manifests embed
    pub fn dataset_rel(&self, id: &str) -> &str {
        self.datasets().rel(id)
    }
    pub fn terrain(&self) -> &model::TerrainSec {
        self.touch("terrain");
        &self.file.terrain
    }
    pub fn water(&self) -> Option<&model::WaterSec> {
        self.touch("water");
        self.file.water.as_ref()
    }
    pub fn nav(&self) -> &model::NavSec {
        self.touch("nav");
        &self.file.nav
    }
    /// nav tile shift: the spec's, else (biome grid - grid) / 128 (needs the biome descriptor)
    pub fn tile_shift(&self) -> Result<i64> {
        if let Some(s) = self.nav().tile_shift {
            return Ok(s);
        }
        let b = self.biome()?;
        let d = b.grid as i64 - self.loc().grid as i64;
        if d < 0 || d % 128 != 0 {
            return Err(Error(format!("nav.tile_shift: biome grid {} - grid {} is not a non-negative multiple of 128",
                                     b.grid, self.loc.grid)));
        }
        Ok(d / 128)
    }
    /// sky nav cover [x0, z0, x1, z1]: the spec's, else the extent +/- 256 m
    pub fn sky_cover(&self) -> [f64; 4] {
        let h = self.loc().half();
        self.nav().sky.cover.unwrap_or([-h - 256.0, -h - 256.0, h + 256.0, h + 256.0])
    }
    pub fn large_blocks(&self) -> Option<&model::LargeBlocksSec> {
        self.touch("large_blocks");
        self.file.large_blocks.as_ref()
    }
    /// large blocks in effect for this process: present and (no switch_env, or switch_env = "1")
    pub fn large_blocks_active(&self) -> Option<&model::LargeBlocksSec> {
        self.large_blocks().filter(|lb| match &lb.switch_env {
            None => true,
            Some(k) => std::env::var(k).map(|v| v == "1").unwrap_or(false),
        })
    }
    pub fn test_mission(&self) -> Option<TestMission> {
        self.touch("test_mission");
        self.file.test_mission.as_ref().map(|t| {
            let sub = |s: &str| s.replace("{code}", &self.loc.code).replace("{mission}", &t.code.to_string());
            let level = sub(&t.level);
            TestMission {
                code: t.code,
                name: t.name.clone(),
                file: format!("{}.lua", t.name),
                description: t.description.clone(),
                pack: sub(&t.pack),
                seq_fox2: format!("{level}/{}_test_sequence.fox2", self.loc.code),
                seq_lua: format!("{level}/{}_test_sequence.lua", self.loc.code),
                start: t.start,
                ih_minimal: t.ih_minimal.clone(),
            }
        })
    }
    pub fn missions(&self) -> &[model::MissionSec] {
        self.touch("mission");
        &self.file.mission
    }
    pub fn mod_info(&self) -> &model::ModSec {
        self.touch("mod");
        &self.file.mod_
    }
    /// the mod name ([mod] name, else "<CODE> location")
    pub fn mod_name(&self) -> String {
        self.mod_info().name.clone().unwrap_or_else(|| format!("{} location", self.loc.code.to_uppercase()))
    }
    /// text templates (repo-relative paths; None = foxm3's built-in)
    pub fn templates(&self) -> &model::TemplatesSec {
        self.touch("package");
        &self.file.package.templates
    }
    pub fn protected_zones(&self) -> &[model::ZoneSec] {
        self.touch("protected_zone");
        &self.file.protected_zone
    }
    pub fn site(&self, name: &str) -> Option<[f64; 3]> {
        self.touch("sites");
        self.file.sites.get(name).map(|v| [v[0], v[1], v[2]])
    }
    /// [recipe.<name>] parameters (opaque to foxpipe)
    pub fn recipe(&self, name: &str) -> Option<&toml::Value> {
        self.touch("recipe");
        self.file.recipe.get(name)
    }
    /// the biome descriptor (tools/rust/biomes/<source>.toml)
    pub fn biome(&self) -> Result<Biome> {
        self.touch("biome");
        Biome::load_in(&self.repo_root, &self.file.biome.source).map_err(Error)
    }
    /// IH location values: the spec's, else the biome's
    pub fn ih(&self) -> Result<IhParams> {
        self.touch("location");
        let l = &self.file.location;
        let need_biome = l.heli_space.is_none() || l.weather.is_none() || l.map.is_none();
        let b = if need_biome { Some(self.biome()?) } else { None };
        Ok(IhParams {
            location_id: l.id,
            location_name: self.loc.ih_name.clone(),
            description: l.description.clone(),
            heli_space: l.heli_space.unwrap_or_else(|| b.as_ref().unwrap().heli_space),
            weather: l.weather.clone().unwrap_or_else(|| b.as_ref().unwrap().weather.clone()),
            extra_weather: l.extra_weather.clone()
                .unwrap_or_else(|| b.as_ref().map(|b| b.extra_weather.clone()).unwrap_or_default()),
            map: l.map.clone().unwrap_or_else(|| b.as_ref().unwrap().map.clone()),
        })
    }

    // ---- resolved view, facets

    /// Everything resolved, by section (fox project show --json, foxrs.project, the facets).
    pub fn resolved_json(&self) -> &Value {
        self.resolved.get_or_init(|| {
            let f = &self.file;
            let ds: serde_json::Map<String, Value> =
                self.datasets.iter().map(|d| (d.id.clone(), Value::String(d.rel.clone()))).collect();
            let loc = &self.loc;
            json!({
                "project": jv(&f.project),
                "location": {
                    "spec": jv(&f.location),
                    "loc": jv(loc),
                    "half": loc.half(), "extent": loc.extent(), "nblk": loc.nblk(), "center_index": loc.center_index(),
                    "nc": loc.nc(),
                },
                "biome": jv(&f.biome),
                "terrain": jv(&f.terrain),
                "water": jv(&f.water),
                "nav": jv(&f.nav),
                "large_blocks": jv(&f.large_blocks),
                "test_mission": jv(&f.test_mission),
                "mission": jv(&f.mission),
                "mod": jv(&f.mod_),
                "package": jv(&f.package),
                "datasets": ds,
                "protected_zone": jv(&f.protected_zone),
                "sites": jv(&f.sites),
                "build": jv(&f.build),
                "stages": jv(&f.stages),
                "recipe": jv(&f.recipe),
            })
        })
    }

    /// One section of the resolved spec; records the facet read.
    pub fn facet(&self, name: &str) -> &Value {
        self.touch(name);
        &self.resolved_json()[name]
    }

    /// Write every facet whose content changed (atomic; plain fs: not a stage output). Returns the changed names.
    pub fn write_facets(&self) -> Result<Vec<String>> {
        self.write_facets_to(&self.datasets.path("spec_cache"))
    }

    fn write_facets_to(&self, dir: &Path) -> Result<Vec<String>> {
        std::fs::create_dir_all(dir).map_err(|e| Error(format!("{}: {e}", dir.display())))?;
        let mut changed = vec![];
        for name in FACETS {
            let mut text = serde_json::to_string_pretty(&self.resolved_json()[*name]).unwrap_or_default();
            text.push('\n');
            let p = dir.join(format!("{name}.json"));
            if std::fs::read(&p).ok().as_deref() == Some(text.as_bytes()) {
                continue;
            }
            let tmp = dir.join(format!("{name}.json.{}.tmp", std::process::id()));
            std::fs::write(&tmp, text.as_bytes())
                .and_then(|_| std::fs::rename(&tmp, &p))
                .map_err(|e| Error(format!("{}: {e}", p.display())))?;
            changed.push(name.to_string());
        }
        Ok(changed)
    }

    /// the facet file of a section
    pub fn facet_path(&self, name: &str) -> PathBuf {
        self.datasets.path("spec_cache").join(format!("{name}.json"))
    }

    /// record a facet read (traced processes only; the first one brings stale facets up to date)
    fn touch(&self, name: &str) {
        // FOXBUILD_TRACE_DIR checked once here (not trace::active(), which would fix the trace state of the process)
        static TRACED: OnceLock<bool> = OnceLock::new();
        if !*TRACED.get_or_init(|| std::env::var_os("FOXBUILD_TRACE_DIR").is_some()) {
            return;
        }
        let w = self.facets_written.get_or_init(|| self.write_facets().map_err(|e| e.0));
        if let Err(e) = w {
            eprintln!("[foxpipe::project] facets not written ({e}): the stage records the spec file instead");
            crate::trace::read(&self.path);
            return;
        }
        crate::trace::read(self.facet_path(name));
    }
}

/// FOX_PROJECT, else projects/flyk/project.toml
pub fn current_path() -> PathBuf {
    match std::env::var_os("FOX_PROJECT") {
        Some(p) if !p.is_empty() => {
            let p = PathBuf::from(p);
            if p.is_absolute() { p } else { crate::paths::repo_root().join(p) }
        }
        _ => crate::paths::repo(DEFAULT_SPEC),
    }
}

/// the build.ignore list in effect (spec, else the defaults)
pub fn build_ignore(f: &SpecFile) -> Vec<String> {
    f.build.ignore.clone().unwrap_or_else(default_ignore)
}

pub fn default_ignore() -> Vec<String> {
    ["work/tmp/", "work/build/", "**/__pycache__/", "**/*.pyc", "**/*.log", "**/scratch/"].iter().map(|s| s.to_string()).collect()
}

pub fn default_static() -> Vec<String> {
    vec!["unpacked/".to_string()]
}

// ---- loading: include / extends / relative paths

fn load_root(root: &Path) -> Result<PathBuf> {
    if root.as_os_str().is_empty() {
        return Err(Error("repository root: expected a nonempty directory path".into()));
    }
    let root = std::path::absolute(root).map_err(|e| Error(format!("repository root: {e}")))?;
    if !root.is_dir() {
        return Err(Error(format!("repository root: {} is not a directory", root.display())));
    }
    Ok(root)
}

fn abs(root: &Path, p: &Path) -> PathBuf {
    let p = if p.is_absolute() { p.to_path_buf() } else { root.join(p) };
    std::path::absolute(&p).unwrap_or(p)
}

/// repo-relative '/' form, or the absolute path outside the repo
fn rel_of(root: &Path, p: &Path) -> String {
    match p.strip_prefix(root) {
        Ok(r) => r.to_string_lossy().replace('\\', "/"),
        Err(_) => p.to_string_lossy().replace('\\', "/"),
    }
}

/// a path written in a spec file, relative to that file's folder -> repo-relative (or absolute outside the repo)
fn resolve_rel(root: &Path, from_file: &Path, p: &str) -> String {
    let base = from_file.parent().unwrap_or(Path::new("."));
    let joined = base.join(p.replace('/', std::path::MAIN_SEPARATOR_STR));
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            c => out.push(c),
        }
    }
    rel_of(root, &out)
}

fn load_table(root: &Path, path: &Path, stack: &mut Vec<PathBuf>) -> Result<toml::Table> {
    let text = std::fs::read_to_string(path).map_err(|e| Error(format!("{}: {e}", path.display())))?;
    parse_table(&text, root, path, stack)
}

fn parse_table(text: &str, root: &Path, path: &Path, stack: &mut Vec<PathBuf>) -> Result<toml::Table> {
    if stack.iter().any(|p| p == path) {
        return Err(Error(format!("extends cycle: {} -> {}", stack.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(" -> "),
                                 path.display())));
    }
    stack.push(path.to_path_buf());
    let mut t: toml::Table = toml::from_str(text).map_err(|e| Error(format!("{}: {e}", path.display())))?;
    // These values are consumed before serde sees the merged model. Validate their shape here, so loading
    // includes or a base cannot silently discard a malformed value in the derived file.
    let base = match t.remove("extends") {
        None => None,
        Some(toml::Value::String(s)) if !s.is_empty() => Some(s),
        Some(_) => return Err(Error(format!("{}: extends: expected a non-empty path string", path.display()))),
    };
    let includes = match t.get("project").and_then(|p| p.get("include")) {
        None => vec![],
        Some(toml::Value::Array(a)) => a.iter().enumerate().map(|(i, v)| {
            v.as_str().filter(|s| !s.is_empty()).map(String::from)
                .ok_or_else(|| Error(format!("{}: project.include[{i}]: expected a non-empty path string", path.display())))
        }).collect::<Result<Vec<_>>>()?,
        Some(_) => return Err(Error(format!("{}: project.include: expected an array of path strings", path.display()))),
    };
    if t.get("stage").is_some_and(|v| v.as_array().is_none_or(|a| a.iter().any(|v| !v.is_table()))) {
        return Err(Error(format!("{}: stage: expected [[stage]] tables", path.display())));
    }
    // templates: relative to this file
    if let Some(toml::Value::Table(pkg)) = t.get_mut("package")
        && let Some(toml::Value::Table(tp)) = pkg.get_mut("templates")
    {
        for (_, v) in tp.iter_mut() {
            if let toml::Value::String(s) = v {
                *s = resolve_rel(root, path, s);
            }
        }
    }
    // includes: their [[stage]] entries first, then this file's own
    if !includes.is_empty() {
        let mut stages: Vec<toml::Value> = vec![];
        let mut resolved = vec![];
        for inc in &includes {
            for f in expand_glob(root, path, inc)? {
                let it = std::fs::read_to_string(&f).map_err(|e| Error(format!("{} (include of {}): {e}", f.display(), path.display())))?;
                let mut tt: toml::Table = toml::from_str(&it).map_err(|e| Error(format!("{}: {e}", f.display())))?;
                let st = tt.remove("stage");
                if let Some(k) = tt.keys().next() {
                    return Err(Error(format!("{}: an included stage file holds only [[stage]] entries (found {k})", f.display())));
                }
                match st {
                    None => {}
                    Some(toml::Value::Array(a)) if a.iter().all(toml::Value::is_table) => stages.extend(a),
                    Some(_) => return Err(Error(format!("{}: stage: expected [[stage]] tables", f.display()))),
                }
                resolved.push(toml::Value::String(rel_of(root, &f)));
            }
        }
        if let Some(toml::Value::Array(own)) = t.remove("stage") {
            stages.extend(own);
        }
        t.insert("stage".into(), toml::Value::Array(stages));
        if let Some(toml::Value::Table(p)) = t.get_mut("project") {
            p.insert("include".into(), toml::Value::Array(resolved));
        }
    }
    // extends: base first, this file on top
    let out = if let Some(base) = base {
        let bp = abs(root, &PathBuf::from(resolve_rel(root, path, &base)));
        let mut b = load_table(root, &bp, stack)?;
        merge(&mut b, t);
        b
    } else {
        t
    };
    stack.pop();
    Ok(out)
}

/// "stages/*.toml": '*' in the last component only, sorted by name
fn expand_glob(root: &Path, from: &Path, pat: &str) -> Result<Vec<PathBuf>> {
    let full = abs(root, &PathBuf::from(resolve_rel(root, from, pat)));
    let name = full.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if pat.matches('*').count() > usize::from(name.contains('*')) {
        return Err(Error(format!("{}: project.include {pat:?}: only one '*' in the final component is supported", from.display())));
    }
    if !name.contains('*') {
        return Ok(vec![full]);
    }
    let (pre, post) = name.split_once('*').unwrap();
    let dir = full.parent().unwrap_or(Path::new("."));
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| Error(format!("{}: {e}", dir.display())))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            n.len() >= pre.len() + post.len() && n.starts_with(pre) && n.ends_with(post) && p.is_file()
        })
        .collect();
    v.sort();
    if v.is_empty() {
        return Err(Error(format!("{}: project.include {pat:?}: no stage files matched", from.display())));
    }
    Ok(v)
}

/// deep merge: tables recursively, everything else (arrays included) replaced by `top`
fn merge(base: &mut toml::Table, top: toml::Table) {
    for (k, v) in top {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(t)) => merge(b, t),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

fn jv<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// all registry ids with their resolved paths (fox project datasets)
pub fn dataset_table(spec: &Spec) -> BTreeMap<String, String> {
    spec.datasets.iter().map(|d| (d.id.clone(), d.rel.clone())).collect()
}

#[cfg(test)]
mod tests;
