//! The project spec file model (projects/<code>/project.toml, format 1): every table is `deny_unknown_fields`, so a
//! misspelt key is an error naming it, never silently ignored. Defaults are applied here (serde) or, when they depend
//! on other values (the code, the grid, the biome), in the resolved views (`Loc`, `Datasets`, `IhParams`, ...).
//!
//! Field reference: docs/release/PROJECT_API.md (and M3_PLAN.md §2.2).
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn d_kind() -> String {
    "new-location".into()
}
fn d_grid() -> usize {
    2048
}
fn d_gd() -> f64 {
    2.0
}
fn d_first_block() -> i32 {
    101
}
fn d_ring_small() -> i32 {
    4
}
fn d_ring_slod0() -> i32 {
    3
}
fn d_biome() -> String {
    "mafr".into()
}
fn d_heights_source() -> String {
    "generate".into()
}
fn d_px() -> usize {
    2048
}
fn d_painter() -> String {
    "rules".into()
}
fn d_cluster() -> usize {
    32
}
fn d_max_wade() -> f64 {
    0.8
}
fn d_band() -> [f64; 2] {
    [0.3, 1.9]
}
fn d_step() -> f64 {
    8.0
}
fn d_one() -> i64 {
    1
}
fn d_tm_pack() -> String {
    "/Assets/tpp/pack/mission2/custom/{code}/{code}_test".into()
}
fn d_tm_level() -> String {
    "/Assets/tpp/level/mission2/custom/{code}".into()
}
fn d_ih_minimal() -> String {
    "work/ih/Assets/tpp/pack/mission2/ih/minimal_mission.fpkd".into()
}
fn d_story() -> String {
    "story".into()
}
fn d_version() -> String {
    "0.1.0".into()
}
fn d_spec_cache() -> String {
    "{work}/spec".into()
}
fn d_tmp() -> String {
    "work/tmp".into()
}

/// The whole file after `include` / `extends` resolution.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SpecFile {
    pub format: i64,
    pub project: ProjectSec,
    pub location: LocationSec,
    #[serde(default)]
    pub biome: BiomeSec,
    #[serde(default)]
    pub terrain: TerrainSec,
    #[serde(default)]
    pub water: Option<WaterSec>,
    #[serde(default)]
    pub nav: NavSec,
    #[serde(default)]
    pub large_blocks: Option<LargeBlocksSec>,
    #[serde(default)]
    pub test_mission: Option<TestMissionSec>,
    #[serde(default)]
    pub mission: Vec<MissionSec>,
    #[serde(default, rename = "mod")]
    pub mod_: ModSec,
    #[serde(default)]
    pub package: PackageSec,
    #[serde(default)]
    pub paths: PathsSec,
    /// dataset overrides by registry id (datasets.rs); unknown ids are errors
    #[serde(default)]
    pub datasets: BTreeMap<String, String>,
    #[serde(default)]
    pub protected_zone: Vec<ZoneSec>,
    /// named points [x, y, z] (project data; recipes read them by name)
    #[serde(default)]
    pub sites: BTreeMap<String, Vec<f64>>,
    #[serde(default)]
    pub build: BuildSec,
    #[serde(default)]
    pub stages: StagesSec,
    /// stage entries, in include order then this file's own: a library instance (`use = "<kind>"`) or a custom stage
    /// (`name` + `cmd`, the foxbuild [[stage]] keys). Interpreted by the generator (crate foxproject), opaque here.
    #[serde(default)]
    pub stage: Vec<toml::Table>,
    /// editor workspaces: every non-static dataset is rebased under data_root
    #[serde(default)]
    pub workspace: Option<WorkspaceSec>,
    /// project recipe parameters ([recipe.<name>]), opaque here: read by that recipe's code
    #[serde(default)]
    pub recipe: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSec {
    pub name: String,
    #[serde(default)]
    pub title: String,
    /// new-location (M3) | vanilla-edit (reserved, refused in M3)
    #[serde(default = "d_kind")]
    pub kind: String,
    /// stage files (relative to this spec file's folder); resolved and removed by the loader, kept for reporting
    #[serde(default)]
    pub include: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocationSec {
    pub code: String,
    pub id: i64,
    #[serde(default)]
    pub ih_name: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default = "d_grid")]
    pub grid: usize,
    #[serde(default = "d_gd")]
    pub grid_distance: f64,
    #[serde(default = "d_first_block")]
    pub first_block: i32,
    #[serde(default = "d_ring_small")]
    pub ring_small: i32,
    #[serde(default = "d_ring_slod0")]
    pub ring_slod0: i32,
    /// IH location add-on values; None = the biome's default
    #[serde(default)]
    pub heli_space: Option<i64>,
    #[serde(default)]
    pub weather: Option<Vec<(String, i64)>>,
    #[serde(default)]
    pub extra_weather: Option<Vec<(String, i64)>>,
    #[serde(default)]
    pub map: Option<MapSec>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MapSec {
    pub lang_id: String,
    pub height: String,
    pub photo: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BiomeSec {
    #[serde(default = "d_biome")]
    pub source: String,
    /// logical material names used by this project (default: the biome's)
    #[serde(default)]
    pub logical: Option<Vec<String>>,
}

impl Default for BiomeSec {
    fn default() -> Self {
        BiomeSec { source: d_biome(), logical: None }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerrainSec {
    /// water surface; None = no lake (masks/index.json then gets -1000.0)
    #[serde(default)]
    pub lake_y: Option<f64>,
    #[serde(default)]
    pub heights: HeightsSec,
    /// recipe-specific md5 pins of heights.npy (hex32), e.g. FLYK's base / carved / eroded / eroded_carved
    #[serde(default)]
    pub heights_pins: BTreeMap<String, String>,
    #[serde(default)]
    pub world_texture: WorldTexSec,
    #[serde(default)]
    pub materials: MaterialsSec,
    /// samples per material cluster
    #[serde(default = "d_cluster")]
    pub cluster: usize,
    /// rules painter (T11, generic): [[terrain.rule]] tables, validated by the terrain kinds
    #[serde(default)]
    pub rule: Vec<toml::Table>,
    /// generator parameters (T11): [terrain.generate] kind = "caldera", seed = ..., validated by heights.simple
    #[serde(default)]
    pub generate: Option<toml::Table>,
}

impl Default for TerrainSec {
    fn default() -> Self {
        TerrainSec { lake_y: None, heights: HeightsSec::default(), heights_pins: BTreeMap::new(),
                     world_texture: WorldTexSec::default(), materials: MaterialsSec::default(), cluster: d_cluster(),
                     rule: vec![], generate: None }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HeightsSec {
    /// pinned | generate | import | recipe
    #[serde(default = "d_heights_source")]
    pub source: String,
    /// the dataset holding the heights (default "heights")
    #[serde(default)]
    pub dataset: Option<String>,
    /// recipe name for source = "recipe" / generate by a project recipe
    #[serde(default)]
    pub recipe: Option<String>,
    /// expected md5 (hex32): a mismatch is reported (see on_mismatch)
    #[serde(default)]
    pub expect: Option<String>,
}

impl Default for HeightsSec {
    fn default() -> Self {
        HeightsSec { source: d_heights_source(), dataset: None, recipe: None, expect: None }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorldTexSec {
    /// tiles per side (default grid * grid_distance / 1024)
    #[serde(default)]
    pub tiles: Option<usize>,
    #[serde(default = "d_px")]
    pub px: usize,
    /// game path template; placeholders {code} {i} {j} ({i}/{j}: two digits)
    #[serde(default)]
    pub game_path: Option<String>,
}

impl Default for WorldTexSec {
    fn default() -> Self {
        WorldTexSec { tiles: None, px: d_px(), game_path: None }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialsSec {
    /// recipe:<name> | rules | weights
    #[serde(default = "d_painter")]
    pub painter: String,
}

impl Default for MaterialsSec {
    fn default() -> Self {
        MaterialsSec { painter: d_painter() }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WaterSec {
    /// legacy:<python tool> (FLYK only) | a library water kind once ported
    #[serde(rename = "impl")]
    pub impl_: String,
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NavSec {
    #[serde(default = "d_max_wade")]
    pub max_wade: f64,
    #[serde(default = "d_band")]
    pub band: [f64; 2],
    /// project block X = biome block X + tile_shift (same nav-world tile ids); default (biome grid - grid) / 128
    #[serde(default)]
    pub tile_shift: Option<i64>,
    /// dataset ids holding obstacles.json + manifest.json; default: none
    #[serde(default)]
    pub obstacle_sets: Vec<String>,
    #[serde(default)]
    pub area: Vec<NavAreaSec>,
    #[serde(default)]
    pub sky: SkySec,
}

impl Default for NavSec {
    fn default() -> Self {
        NavSec { max_wade: d_max_wade(), band: d_band(), tile_shift: None, obstacle_sets: vec![], area: vec![],
                 sky: SkySec::default() }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NavAreaSec {
    pub name: String,
    pub x0: f64,
    pub z0: f64,
    pub size: f64,
    pub nav2: String,
    pub fox2: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkySec {
    /// [x0, z0, x1, z1]; default: the grid extent +/- 256 m
    #[serde(default)]
    pub cover: Option<[f64; 4]>,
    #[serde(default = "d_step")]
    pub step: f64,
    #[serde(default = "d_step")]
    pub hole_margin: f64,
    #[serde(default)]
    pub shell: ShellSec,
    #[serde(default)]
    pub divide: DivideSec,
    #[serde(default = "d_coarser")]
    pub coarser: Vec<CoarserSec>,
}

impl Default for SkySec {
    fn default() -> Self {
        SkySec { cover: None, step: d_step(), hole_margin: d_step(), shell: ShellSec::default(),
                 divide: DivideSec::default(), coarser: d_coarser() }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ShellSec {
    pub clearance: f64,
    pub dilate: f64,
    pub terrace: f64,
    pub ramp_deg: f64,
    pub close: f64,
}

impl Default for ShellSec {
    fn default() -> Self {
        ShellSec { clearance: 30.0, dilate: 48.0, terrace: 10.0, ramp_deg: 15.0, close: 64.0 }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DivideSec {
    pub x0: f64,
    pub z0: f64,
    pub nx: i64,
    pub nz: i64,
    pub cell: f64,
}

impl Default for DivideSec {
    fn default() -> Self {
        DivideSec { x0: 0.0, z0: 128.0, nx: 4, nz: 4, cell: 128.0 }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CoarserSec {
    pub vertical_tol: f64,
    #[serde(default)]
    pub min_edge: Option<f64>,
}

fn d_coarser() -> Vec<CoarserSec> {
    vec![CoarserSec { vertical_tol: 4.0, min_edge: None }, CoarserSec { vertical_tol: 5.0, min_edge: None },
         CoarserSec { vertical_tol: 6.0, min_edge: Some(16.0) }]
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LargeBlocksSec {
    /// used only when this environment variable is "1" (FLYK compatibility: FLYK_LARGE); None = always
    #[serde(default)]
    pub switch_env: Option<String>,
    pub size_in_bytes: i64,
    #[serde(default = "d_one")]
    pub loading_margin: i64,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub block: Vec<LargeBlockSec>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LargeBlockSec {
    pub name: String,
    pub pack: String,
    pub fox2_dir: String,
    pub region: RegionSec,
    #[serde(default)]
    pub why: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RegionSec {
    pub x: [i32; 2],
    pub y: [i32; 2],
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestMissionSec {
    pub code: i64,
    /// IH missionName; the IH file is <name>.lua
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// placeholders {code} = location code, {mission} = test mission code
    #[serde(default = "d_tm_pack")]
    pub pack: String,
    #[serde(default = "d_tm_level")]
    pub level: String,
    /// [x, y, z, rotY]
    pub start: [f64; 4],
    #[serde(default = "d_ih_minimal")]
    pub ih_minimal: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MissionSec {
    pub code: i64,
    #[serde(default = "d_story")]
    pub kind: String,
    #[serde(default)]
    pub packs: Option<String>,
    #[serde(default, rename = "mod")]
    pub mod_: Option<String>,
    #[serde(default)]
    pub recipe: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModSec {
    /// default "<CODE> location"
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default = "d_version")]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
}

impl Default for ModSec {
    fn default() -> Self {
        ModSec { name: None, version: d_version(), author: String::new(), description: String::new() }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSec {
    #[serde(default)]
    pub templates: TemplatesSec,
}

/// text templates (relative to the spec file's folder); None = foxm3's generic built-in
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TemplatesSec {
    #[serde(default)]
    pub ih_location: Option<String>,
    #[serde(default)]
    pub ih_mission: Option<String>,
    #[serde(default)]
    pub metadata: Option<String>,
    #[serde(default)]
    pub seq_lua: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PathsSec {
    /// root of the dataset defaults; default "work/projects/{code}"
    #[serde(default)]
    pub work: Option<String>,
    /// resolved facets (one JSON per section); must not be under build.ignore
    #[serde(default = "d_spec_cache")]
    pub spec_cache: String,
}

impl Default for PathsSec {
    fn default() -> Self {
        PathsSec { work: None, spec_cache: d_spec_cache() }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ZoneSec {
    pub name: String,
    /// [x0, x1, z0, z1]
    #[serde(default, rename = "box")]
    pub box_: Option<[f64; 4]>,
    /// [x, z, r]
    #[serde(default)]
    pub circle: Option<[f64; 3]>,
    #[serde(default)]
    pub polygon: Option<Vec<[f64; 2]>>,
    #[serde(default)]
    pub flat_y: Option<f64>,
    /// no_carve | no_veg | no_dressing | flat
    #[serde(default)]
    pub rules: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildSec {
    /// default "work/build/{code}"
    #[serde(default)]
    pub log_dir: Option<String>,
    #[serde(default = "d_tmp")]
    pub temp_dir: String,
    /// static (vanilla / third-party) read roots; default ["unpacked/"]
    #[serde(default, rename = "static")]
    pub static_: Option<Vec<String>>,
    /// never inputs or outputs; default: scratch / logs / caches
    #[serde(default)]
    pub ignore: Option<Vec<String>>,
    #[serde(default)]
    pub unset_env: Vec<String>,
    /// environment for every stage (not fingerprinted)
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub pinned: Vec<PinnedSec>,
    /// project overrides of the machine settings (normally the user's config)
    #[serde(default)]
    pub machine: MachineSec,
}

impl Default for BuildSec {
    fn default() -> Self {
        BuildSec { log_dir: None, temp_dir: d_tmp(), static_: None, ignore: None, unset_env: vec![],
                   env: BTreeMap::new(), pinned: vec![], machine: MachineSec::default() }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedSec {
    /// a dataset id, or `path` (repo-relative)
    #[serde(default)]
    pub dataset: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub owner: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MachineSec {
    #[serde(default)]
    pub mem_budget_gb: Option<f64>,
    #[serde(default)]
    pub mem_reserve_gb: Option<f64>,
    #[serde(default)]
    pub max_jobs: Option<usize>,
    #[serde(default)]
    pub games: Option<Vec<String>>,
    #[serde(default)]
    pub game_max_jobs: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StagesSec {
    /// "flyk": legacy Python implementations allowed (FLYK only); None = Rust implementations only
    #[serde(default)]
    pub compat: Option<String>,
    /// generate the library's default stage set for the project kind (new projects); FLYK lists every stage itself
    #[serde(default)]
    pub defaults: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSec {
    /// repo-relative root; every non-static dataset is rebased to <data_root>/<its path>
    pub data_root: String,
}
