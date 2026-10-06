//! Biome descriptors (`tools/rust/biomes/<name>.toml`): the source location a new project borrows its materials,
//! common-pack templates, nav world and IH defaults from. Tool data (names, ids, paths into the user's unpack cache,
//! our own tuning), never game bytes.
//!
//! The descriptor CONTENT (mafr.toml, `fox biome extract`) is M3 track C's task T9; this file only fixes the type.
//! Field additions are additive: ask track A.
use super::model::MapSec;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Biome {
    pub name: String,
    /// the source location's grid (samples per side; MAFR 4096): nav tile_shift = (grid - project grid) / 128
    pub grid: usize,
    /// logical material names, in order, and their atlas ids (same length)
    pub logical: Vec<String>,
    pub mat_id: Vec<u8>,
    /// where a material's weight goes when the cluster set lacks it (first available wins)
    #[serde(default)]
    pub similar: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub preview_colors: BTreeMap<String, [u8; 3]>,
    #[serde(default)]
    pub meter_per_repeat: Option<f64>,
    /// IH location add-on defaults
    pub heli_space: i64,
    pub weather: Vec<(String, i64)>,
    #[serde(default)]
    pub extra_weather: Vec<(String, i64)>,
    pub map: MapSec,
    /// common-pack contents referenced by path
    #[serde(default)]
    pub common_vfx: Vec<String>,
    #[serde(default)]
    pub fpkd_helpers: Vec<String>,
    #[serde(default)]
    pub star_dome: Option<String>,
    #[serde(default)]
    pub resident00: Option<String>,
    /// template locations (in the user's unpack cache / extracted by `fox biome extract`), by key; the keys are
    /// defined by T9 (common_fox2.<k>, stage_fox2, twpf, wrtx, slod0_htre, common_nav, nav_sky, pftxs, ...)
    #[serde(default)]
    pub templates: BTreeMap<String, String>,
}

impl Biome {
    /// tools/rust/biomes/<name>.toml
    pub fn path(name: &str) -> std::path::PathBuf {
        crate::paths::repo(&format!("tools/rust/biomes/{name}.toml"))
    }

    pub fn load(name: &str) -> Result<Biome, String> {
        Self::load_in(&crate::paths::repo_root(), name)
    }

    /// Descriptor from an explicitly selected repository, without changing process-wide root state.
    pub fn load_in(repo_root: &std::path::Path, name: &str) -> Result<Biome, String> {
        let p = repo_root.join(format!("tools/rust/biomes/{name}.toml"));
        let t = std::fs::read_to_string(&p).map_err(|e| format!("biome {name}: {}: {e}", p.display()))?;
        Self::from_toml_str(&t).map_err(|e| format!("biome {name}: {}: {e}", p.display()))
    }

    pub fn from_toml_str(text: &str) -> Result<Biome, String> {
        let b: Biome = toml::from_str(text).map_err(|e| e.to_string())?;
        if b.logical.len() != b.mat_id.len() {
            return Err(format!("logical ({}) and mat_id ({}) differ in length", b.logical.len(), b.mat_id.len()));
        }
        for (k, v) in &b.similar {
            for n in std::iter::once(k).chain(v) {
                if !b.logical.contains(n) {
                    return Err(format!("similar: unknown logical material {n}"));
                }
            }
        }
        Ok(b)
    }

    /// atlas id of a logical material
    pub fn mat_id_of(&self, logical: &str) -> Option<u8> {
        self.logical.iter().position(|l| l == logical).map(|i| self.mat_id[i])
    }
}
