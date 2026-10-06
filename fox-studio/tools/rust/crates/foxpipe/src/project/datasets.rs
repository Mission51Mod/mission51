//! The dataset registry: every pipeline file / folder a stage reads or writes, by id. A crate asks
//! `spec.dataset_rel("heights")` instead of writing "work/flyk/m3/heightfield/heights.npy".
//!
//! Default templates reproduce the FLYK layout under `{work}`, so FLYK (`work = "work/flyk"`) resolves every id to its
//! legacy repo-relative string exactly (manifests embed these strings: byte identity depends on it) and overrides only
//! the folders that live outside work/flyk. Placeholders: `{work}`, `{code}`, `{biome}` and `{<other id>}` (resolved
//! after that id's own override, so `fauna_plan = "{fauna_out}/plan.json"` follows an override of fauna_out).
//!
//! New ids: ask track A (M3) - the registry is shared by every crate, one line per id, sorted by group.
use serde::Serialize;
use std::collections::BTreeMap;

/// file or folder
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    File,
    Dir,
}

/// Flags (bit set).
pub mod flag {
    /// an editor export target ("export to OFFICIAL" copies it back)
    pub const EDITABLE: u16 = 1;
    /// copied into an editor workspace snapshot (with `skip`)
    pub const SNAPSHOT: u16 = 2;
    /// never written by a workspace build (redirect PROTECT)
    pub const PROTECT: u16 = 4;
    /// vanilla-derived / shared read-only data: not rebased into workspaces (junctioned), never hashed by foxbuild
    pub const STATIC: u16 = 8;
    /// copied byte-identically (other manifests record its md5)
    pub const NO_REWRITE: u16 = 16;
    /// published outside the graph by its owner (foxbuild [[pinned]])
    pub const PINNED: u16 = 32;
    /// may be absent (snapshot: optional)
    pub const OPTIONAL: u16 = 64;
}

pub struct Def {
    pub id: &'static str,
    pub default: &'static str,
    pub kind: Kind,
    pub flags: u16,
    /// snapshot skip list (sub-folder / file names, or relative sub-paths)
    pub skip: &'static [&'static str],
}

use flag::*;
use Kind::{Dir, File};

const fn d(id: &'static str, default: &'static str, kind: Kind, flags: u16) -> Def {
    Def { id, default, kind, flags, skip: &[] }
}
const fn ds(id: &'static str, default: &'static str, kind: Kind, flags: u16, skip: &'static [&'static str]) -> Def {
    Def { id, default, kind, flags, skip }
}

/// The registry (order = report order).
pub static REGISTRY: &[Def] = &[
    // ---- terrain: heights, masks, materials, world texture
    ds("heightfield", "{work}/m3/heightfield", Dir, SNAPSHOT,
       &["m3_original", "preview_heights.npy", "hillshade.png", "preview.png", "preview_zoom.png", "materials_only_report.json"]),
    d("heights", "{heightfield}/heights.npy", File, EDITABLE | PINNED),
    d("combo", "{heightfield}/combo.npy", File, EDITABLE),
    d("cluster_ids", "{heightfield}/cluster_ids.npy", File, EDITABLE),
    d("cluster_cfg", "{heightfield}/cluster_cfg.npy", File, EDITABLE),
    d("material_weights", "{heightfield}/material_weights.npz", File, EDITABLE),
    d("materials_preview", "{heightfield}/materials_preview.png", File, EDITABLE),
    d("materials_report", "{heightfield}/materials_only_report.json", File, 0),
    ds("masks", "{work}/m3/masks", Dir, SNAPSHOT, &[]),
    d("masks_index", "{masks}/index.json", File, 0),
    d("masks_height", "{masks}/height.npy", File, EDITABLE),
    d("points", "{masks}/points.json", File, 0),
    d("slope", "{masks}/slope.npy", File, 0),
    d("flow_accum", "{masks}/flow_accum.npy", File, PINNED),
    d("erosion_masks", "{masks}/erosion", Dir, PINNED),
    d("mask_jungle_density", "{masks}/jungle_density.npy", File, 0),
    d("mask_savanna", "{masks}/savanna.npy", File, 0),
    d("mask_beach_lagoon", "{masks}/beach_lagoon.npy", File, 0),
    d("mask_open_clearings", "{masks}/open_clearings.npy", File, 0),
    d("mask_rock_material", "{masks}/rock_material.npy", File, 0),
    d("mask_flow_wetness", "{masks}/flow_wetness.npy", File, 0),
    d("mask_look_cove_sand", "{masks}/look_cove_sand.npy", File, 0),
    d("mask_look_camp_ground", "{masks}/look_camp_ground.npy", File, 0),
    d("editor_keepout", "{masks}/editor_keepout.npy", File, EDITABLE),
    d("editor_keepout_png", "{masks}/editor_keepout.png", File, EDITABLE),
    d("editor_density", "{masks}/editor_density.npy", File, EDITABLE),
    d("editor_density_png", "{masks}/editor_density.png", File, EDITABLE),
    d("editor_masks", "{masks}/editor_masks.json", File, EDITABLE),
    ds("worldtex", "{work}/m3/worldtex", Dir, EDITABLE | SNAPSHOT, &[]),
    d("atlas_png", "{work}/m3/scratch/{biome}_terrain_bsm_mip2.png", File, SNAPSHOT),
    d("atlas_full_png", "{work}/m3/scratch/{biome}_terrain_bsm_atlas.png", File, SNAPSHOT),
    d("atlas_white_png", "{look_terrain}/{code}_terrain_bsm_mip2.png", File, 0),
    d("seams", "{work}/seams", Dir, 0),
    d("seams_audit", "{seams}/audit_final.json", File, 0),
    d("seams_audit_png", "{seams}/audit_final.png", File, 0),
    // ---- water
    ds("water", "{work}/m3/water", Dir, SNAPSHOT, &["vanilla", "scratch", "preview", "fox2xml"]),
    d("water_plan", "{water}/plan.json", File, 0),
    d("water_masks", "{water}/masks", Dir, 0),
    d("overlay_water", "{water}/editor_overlay.json", File, EDITABLE),
    // ---- vegetation
    ds("scatter_templates", "{work}/m3/scatter/templates", Dir, SNAPSHOT, &[]),
    ds("transplant", "{work}/m3/scatter/transplant", Dir, SNAPSHOT, &[]),
    d("veg_placements", "{transplant}/placements_dense.npy", File, EDITABLE),
    d("veg_tables", "{transplant}/placements_dense_tables.json", File, EDITABLE),
    d("veg_lod", "{transplant}/lod_dense", Dir, 0),
    d("overlay_veg", "{transplant}/editor_overlay.json", File, EDITABLE),
    d("veg_premask", "{transplant}/editor_premask_dense.npy", File, EDITABLE),
    d("veg_premask_meta", "{transplant}/editor_premask_dense.json", File, EDITABLE),
    d("m6_pin", "{transplant}/m6_pin", Dir, 0),
    d("beach_zones_pin", "{transplant}/beach_zones_pin.npy", File, 0),
    d("beach_zones_pin_meta", "{transplant}/beach_zones_pin.json", File, 0),
    d("beach_keepout_pin", "{transplant}/beach_keepout_pin.json", File, 0),
    ds("scatter_out", "{work}/m3/scatter/out", Dir, SNAPSHOT, &["render", "fox2xml"]),
    d("scatter_manifest", "{scatter_out}/manifest.json", File, 0),
    d("setpieces_placements", "{scatter_out}/placements.npy", File, 0),
    d("scatter_obstacles", "{scatter_out}/obstacles.json", File, 0),
    d("census", "{work}/m3/scatter/census", Dir, STATIC),
    // ---- dressing
    ds("dressing", "{work}/dressing", Dir, SNAPSHOT,
       &["catalogue", "render", "maps", "scratch", "out/fox2xml", "out/next", "out/old"]),
    d("dressing_out", "{dressing}/out", Dir, 0),
    d("dressing_out_large", "{dressing}/out_large", Dir, 0),
    d("dressing_placements", "{dressing_out}/placements.json", File, EDITABLE | NO_REWRITE),
    d("dressing_manifest", "{dressing_out}/manifest.json", File, 0),
    d("stakes", "{dressing_out}/stakes.json", File, 0),
    d("dressing_keepout", "{dressing_out}/keepout.json", File, 0),
    d("dressing_islets", "{dressing}/islets", Dir, 0),
    d("overlay_dressing", "{dressing}/editor_overlay.json", File, EDITABLE),
    d("dd_routes", "{dressing}/inputs/dd_routes.json", File, PINNED),
    // ---- look, surface, survey, far shore, external
    ds("look_out", "{work}/look/out", Dir, SNAPSHOT, &[]),
    d("look_assets", "{look_out}/assets", Dir, 0),
    d("look_subst", "{look_out}/subst.json", File, 0),
    d("look_terrain", "{look_out}/terrain", Dir, 0),
    d("probes_out", "{look_out}/probes", Dir, 0),
    d("probes_manifest", "{probes_out}/manifest.json", File, 0),
    d("surface", "{work}/surface", Dir, 0),
    d("sand", "{surface}/sand", Dir, 0),
    d("steep_candidates", "{surface}/steep_skin_candidates.npy", File, 0),
    d("steep_paint_moss", "{surface}/steep_paint_moss.npy", File, 0),
    d("steep_view", "{surface}/steep_view_40_50.npy", File, 0),
    d("steep_view_meta", "{surface}/steep_view_40_50.json", File, 0),
    d("surface_out", "{surface}/out", Dir, 0),
    d("surface_manifest", "{surface_out}/manifest.json", File, 0),
    ds("survey", "{work}/survey", Dir, SNAPSHOT, &[]),
    d("farshore_out", "{work}/farshore/out", Dir, 0),
    ds("external", "{work}/external", Dir, EDITABLE | SNAPSHOT | OPTIONAL, &[]),
    d("external_manifest", "{external}/out/manifest.json", File, 0),
    // ---- art direction, fauna, flora (FLYK keeps these outside work/flyk: overridden there)
    d("artdir", "{work}/artdir", Dir, 0),
    d("beach_out", "{artdir}/beach/out", Dir, 0),
    d("beach_manifest", "{beach_out}/manifest.json", File, 0),
    d("beach_keepout", "{beach_out}/keepout.json", File, 0),
    d("beach_zones", "{artdir}/beach/zones", Dir, 0),
    d("artdir_probes", "{artdir}/probes", Dir, 0),
    d("artdir_probes_manifest", "{artdir_probes}/manifest.json", File, 0),
    d("artdir_jungle", "{artdir}/jungle", Dir, 0),
    d("artdir_farshore_out", "{artdir}/farshore/out", Dir, 0),
    d("artdir_farshore_manifest", "{artdir_farshore_out}/manifest.json", File, 0),
    d("fauna_out", "{work}/fauna/out", Dir, 0),
    d("fauna_manifest", "{fauna_out}/manifest.json", File, 0),
    d("fauna_plan", "{fauna_out}/plan.json", File, 0),
    d("flora_imports_out", "{work}/flora/imports/out", Dir, 0),
    d("flora_imports_manifest", "{flora_imports_out}/manifest.json", File, 0),
    d("flora_additions", "{work}/flora/additions", Dir, 0),
    d("flora_budget", "{flora_additions}/block_budget.json", File, 0),
    // ---- navigation, package
    ds("nav_out", "{work}/m4/nav", Dir, SNAPSHOT, &[]),
    d("nav_manifest", "{nav_out}/manifest.json", File, 0),
    ds("sky_out", "{work}/m4/sky", Dir, SNAPSHOT, &["scratch"]),
    d("sky_manifest", "{sky_out}/manifest.json", File, 0),
    d("m3_out", "{work}/m3", Dir, 0),
    d("m3_mod", "{m3_out}/mod", Dir, 0),
    d("m3_stage", "{m3_out}/stage", Dir, 0),
    d("mgsv", "{m3_out}/build/{code}_m3.mgsv", File, 0),
    d("m3_verify", "{m3_out}/verify.json", File, 0),
    d("merge_sets", "{m3_out}/merge_sets.json", File, 0),
    d("large_blocks", "{work}/large_blocks.json", File, 0),
    // ---- mission (FLYK: work/m6, work/boss)
    d("m6", "{work}/m6", Dir, 0),
    ds("m6_gen", "{m6}/gen", Dir, SNAPSHOT, &["fox2xml", "stage"]),
    d("m6_src", "{m6}/src", Dir, 0),
    d("m6_verify", "{m6}/verify.json", File, 0),
    ds("boss_lanes", "{work}/boss/lanes.json", File, SNAPSHOT | OPTIONAL, &[]),
    // ---- spec cache (facets)
    d("spec_cache", "{work}/spec", Dir, 0),
    // ---- static: vanilla-derived caches and shared read-only data (junctioned into workspaces)
    d("unpacked", "unpacked", Dir, STATIC),
    d("terrain_templates", "work/terrain", Dir, STATIC),
    d("nav_templates", "work/nav", Dir, STATIC),
    d("ih", "work/ih", Dir, STATIC),
    d("biome_common", "{work}/{biome}_common", Dir, STATIC),
    d("m2", "{work}/m2", Dir, STATIC),
];

pub fn def(id: &str) -> Option<&'static Def> {
    REGISTRY.iter().find(|x| x.id == id)
}

/// One resolved dataset.
#[derive(Clone, Debug, Serialize)]
pub struct Dataset {
    pub id: String,
    /// repo-relative, forward slashes: the exact string manifests record
    pub rel: String,
    pub kind: Kind,
    pub flags: u16,
    pub skip: Vec<String>,
    /// set by the spec's [datasets] table (or a workspace rebase)
    pub overridden: bool,
}

impl Dataset {
    pub fn has(&self, f: u16) -> bool {
        self.flags & f != 0
    }
    pub fn path(&self) -> std::path::PathBuf {
        crate::paths::repo(&self.rel)
    }
    /// Resolve this standalone dataset using an explicit repository root.
    /// Frontends normally use `Spec::dataset` or the root-carrying `Datasets::path` instead.
    pub fn path_in(&self, repo_root: &std::path::Path) -> std::path::PathBuf {
        repo_root.join(&self.rel)
    }
}

/// All datasets of one project, resolved.
#[derive(Clone, Debug, Serialize)]
pub struct Datasets {
    map: BTreeMap<String, Dataset>,
    #[serde(skip)]
    repo_root: Option<std::path::PathBuf>,
}

impl Datasets {
    /// resolve the registry against the spec's `work`, code, biome and overrides; `data_root` rebases every
    /// non-static dataset (editor workspaces)
    pub fn resolve(work: &str, code: &str, biome: &str, overrides: &BTreeMap<String, String>,
                   data_root: Option<&str>) -> Result<Datasets, String> {
        if let Some(root) = data_root {
            super::validate::repo_rel_ok(root, &["work/"])
                .map_err(|e| format!("workspace.data_root: {e}"))?;
        }
        for k in overrides.keys() {
            if def(k).is_none() {
                return Err(format!("[datasets] {k}: unknown dataset id (known: {})", ids().join(", ")));
            }
        }
        let mut done: BTreeMap<String, String> = BTreeMap::new();
        fn resolve_one(id: &str, work: &str, code: &str, biome: &str, ov: &BTreeMap<String, String>,
                       done: &mut BTreeMap<String, String>, stack: &mut Vec<String>) -> Result<String, String> {
            if let Some(v) = done.get(id) {
                return Ok(v.clone());
            }
            if stack.iter().any(|s| s == id) {
                return Err(format!("dataset template cycle: {} -> {id}", stack.join(" -> ")));
            }
            let tpl = match ov.get(id) {
                Some(v) => v.clone(),
                None => def(id).ok_or_else(|| format!("unknown dataset id {id}"))?.default.to_string(),
            };
            stack.push(id.to_string());
            let mut out = String::new();
            let mut rest = tpl.as_str();
            while let Some(i) = rest.find('{') {
                out.push_str(&rest[..i]);
                let j = rest[i..].find('}').ok_or_else(|| format!("dataset {id}: unclosed '{{' in {tpl:?}"))? + i;
                let name = &rest[i + 1..j];
                match name {
                    "work" => out.push_str(work),
                    "code" => out.push_str(code),
                    "biome" => out.push_str(biome),
                    other => {
                        if def(other).is_none() {
                            return Err(format!("dataset {id}: unknown placeholder {{{other}}} in {tpl:?}"));
                        }
                        out.push_str(&resolve_one(other, work, code, biome, ov, done, stack)?);
                    }
                }
                rest = &rest[j + 1..];
            }
            out.push_str(rest);
            stack.pop();
            let out = norm(&out);
            done.insert(id.to_string(), out.clone());
            Ok(out)
        }
        let mut map = BTreeMap::new();
        for x in REGISTRY {
            let rel = resolve_one(x.id, work, code, biome, overrides, &mut done, &mut vec![])?;
            // Check the expanded path BEFORE workspace rebasing; a prefix must never conceal an absolute or
            // escaping path. Static overrides are read-only, but must also stay repo-relative.
            let roots: &[&str] = if x.flags & STATIC != 0 { &[] } else { &["work/", "projects/", "build/"] };
            super::validate::repo_rel_ok(&rel, roots).map_err(|e| format!("dataset {}: {e}", x.id))?;
            let mut ds = Dataset { id: x.id.into(), rel, kind: x.kind, flags: x.flags,
                                   skip: x.skip.iter().map(|s| s.to_string()).collect(),
                                   overridden: overrides.contains_key(x.id) };
            if let Some(root) = data_root
                && ds.flags & STATIC == 0
            {
                ds.rel = format!("{}/{}", norm(root), ds.rel);
                ds.overridden = true;
            }
            map.insert(x.id.to_string(), ds);
        }
        Ok(Datasets { map, repo_root: None })
    }

    pub(super) fn with_root(mut self, root: std::path::PathBuf) -> Self {
        self.repo_root = Some(root);
        self
    }

    pub fn get(&self, id: &str) -> Option<&Dataset> {
        self.map.get(id)
    }

    /// repo-relative string of a dataset; panics on an unknown id (a programming error: ids are registry constants)
    pub fn rel(&self, id: &str) -> &str {
        &self.map.get(id).unwrap_or_else(|| panic!("unknown dataset id {id:?} (foxpipe::project::datasets)")).rel
    }

    pub fn path(&self, id: &str) -> std::path::PathBuf {
        match &self.repo_root {
            Some(root) => root.join(self.rel(id)),
            None => crate::paths::repo(self.rel(id)),
        }
    }

    /// in registry order
    pub fn iter(&self) -> impl Iterator<Item = &Dataset> {
        REGISTRY.iter().filter_map(|x| self.map.get(x.id))
    }

    /// the dataset whose path equals `rel` (case-insensitive, '/' or '\\')
    pub fn by_path(&self, rel: &str) -> Option<&Dataset> {
        let k = norm(rel).to_lowercase();
        self.iter().find(|d| d.rel.to_lowercase() == k)
    }
}

pub fn ids() -> Vec<&'static str> {
    REGISTRY.iter().map(|x| x.id).collect()
}

fn norm(p: &str) -> String {
    let mut s = p.replace('\\', "/");
    while let Some(t) = s.strip_prefix("./") {
        s = t.to_string();
    }
    s.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_ids_unique_and_templates_resolve() {
        let mut seen = std::collections::BTreeSet::new();
        for x in REGISTRY {
            assert!(seen.insert(x.id), "duplicate id {}", x.id);
        }
        let ds = Datasets::resolve("work/projects/tst", "tst", "mafr", &BTreeMap::new(), None).unwrap();
        assert_eq!(ds.rel("heights"), "work/projects/tst/m3/heightfield/heights.npy");
        assert_eq!(ds.rel("mgsv"), "work/projects/tst/m3/build/tst_m3.mgsv");
        assert_eq!(ds.rel("fauna_plan"), "work/projects/tst/fauna/out/plan.json");
        // no two datasets resolve to the same path
        let mut paths = std::collections::BTreeMap::new();
        for d in ds.iter() {
            if let Some(o) = paths.insert(d.rel.to_lowercase(), d.id.clone()) {
                panic!("{} and {} both resolve to {}", o, d.id, d.rel);
            }
        }
    }

    #[test]
    fn overrides_follow_and_unknown_is_an_error() {
        let ov: BTreeMap<String, String> = [("fauna_out".to_string(), "work/fauna/out".to_string())].into();
        let ds = Datasets::resolve("work/flyk", "flyk", "mafr", &ov, None).unwrap();
        assert_eq!(ds.rel("fauna_plan"), "work/fauna/out/plan.json");
        assert!(ds.get("fauna_out").unwrap().overridden);
        let bad: BTreeMap<String, String> = [("no_such".to_string(), "x".to_string())].into();
        assert!(Datasets::resolve("work/flyk", "flyk", "mafr", &bad, None).is_err());
        let cyc: BTreeMap<String, String> = [("m6".to_string(), "{m6_gen}/..".to_string())].into();
        assert!(Datasets::resolve("work/flyk", "flyk", "mafr", &cyc, None).unwrap_err().contains("cycle"));
    }

    #[test]
    fn workspace_rebase_keeps_static() {
        let ds = Datasets::resolve("work/flyk", "flyk", "mafr", &BTreeMap::new(), Some("work/editor/ws1/r")).unwrap();
        assert_eq!(ds.rel("heights"), "work/editor/ws1/r/work/flyk/m3/heightfield/heights.npy");
        assert_eq!(ds.rel("unpacked"), "unpacked");
        assert_eq!(ds.rel("census"), "work/flyk/m3/scatter/census");
    }
}
