//! `Loc`: the location's grid geometry and every game path derived from its code. Replaces the per-crate consts
//! (foxm3::cfg, foxnav::flyk, foxterrain::consts, foxmission::flyk). For FLYK every value and string equals those
//! consts (foxproject's consistency tests).
use serde::Serialize;

/// htre samples per block side (a Fox Engine constant: 64 samples x 2 m = 128 m blocks)
pub const HTRE_N: usize = 64;
/// world metres per streaming block
pub const BLOCK_M: f64 = 128.0;

#[derive(Clone, Debug, Serialize)]
pub struct Loc {
    pub code: String,
    pub id: i64,
    pub ih_name: String,
    /// samples per side
    pub grid: usize,
    /// metres per sample (gridDistance)
    pub d: f64,
    pub first_block: i32,
    pub htre_n: usize,
    pub ring_small: i32,
    pub ring_slod0: i32,
    /// samples per material cluster
    pub cluster: usize,
    /// world texture tiles per side and pixels per tile
    pub wt_tiles: usize,
    pub wt_px: usize,
    /// world-texture game path template ({code} {i} {j})
    pub wt_path_fmt: String,
    /// water surface; None = no lake
    pub lake_y: Option<f64>,
}

impl Loc {
    /// half the extent (m): world x/z run from -half to +half
    pub fn half(&self) -> f64 {
        self.grid as f64 * self.d / 2.0
    }
    pub fn extent(&self) -> f64 {
        self.grid as f64 * self.d
    }
    /// blocks per side
    pub fn nblk(&self) -> i32 {
        (self.grid / self.htre_n) as i32
    }
    /// the block whose origin is world 0
    pub fn center_index(&self) -> i32 {
        self.first_block + self.nblk() / 2
    }
    /// clusters per side
    pub fn nc(&self) -> usize {
        self.grid / self.cluster
    }
    /// the real (terrain) blocks
    pub fn blocks(&self) -> std::ops::Range<i32> {
        self.first_block..self.first_block + self.nblk()
    }
    /// pack_small range incl. the empty ring
    pub fn small_range(&self) -> std::ops::Range<i32> {
        self.first_block - self.ring_small..self.first_block + self.nblk() + self.ring_small
    }
    /// small_lod0 pack indices holding terrain (2 x 2 blocks each)
    pub fn slod0_real(&self) -> Vec<i32> {
        (self.first_block - 1..self.first_block - 1 + self.nblk()).step_by(2).collect()
    }
    /// small_lod0 range incl. the empty ring
    pub fn slod0_range(&self) -> Vec<i32> {
        let r = self.slod0_real();
        let (start, stop) = (r[0], self.first_block - 1 + self.nblk());
        (start - 2 * self.ring_slod0..stop + 2 * self.ring_slod0).step_by(2).collect()
    }
    /// world (x, z) of block (x, y)'s origin corner
    pub fn block_origin(&self, x: i32, y: i32) -> (f64, f64) {
        let ox = ((x - self.first_block) as i64 * self.htre_n as i64) as f64 * self.d - self.half();
        let oz = ((y - self.first_block) as i64 * self.htre_n as i64) as f64 * self.d - self.half();
        (ox, oz)
    }
    /// world (x, z) of block (x, y)'s centre (flyk_m2.block_center)
    pub fn block_center(&self, x: i32, y: i32) -> (f64, f64) {
        let (ox, oz) = self.block_origin(x, y);
        let h = self.htre_n as f64 * self.d / 2.0;
        (ox + h, oz + h)
    }
    /// the block holding world (x, z) (flecore.spec.block_of)
    pub fn block_of(&self, x: f64, z: f64) -> (i32, i32) {
        let bm = self.htre_n as f64 * self.d;
        (((x + self.half()) / bm).floor() as i32 + self.first_block, ((z + self.half()) / bm).floor() as i32 + self.first_block)
    }
    /// world texture tiles (i, j), X fastest (flyk_m3.M.WORLD_TILES)
    pub fn world_tiles(&self) -> Vec<(usize, usize)> {
        let n = self.wt_tiles;
        (0..n).flat_map(|j| (0..n).map(move |i| (i, j))).collect()
    }

    // ---- game paths
    pub fn level(&self) -> String {
        format!("/Assets/tpp/level/location/{}", self.code)
    }
    pub fn pack_loc(&self) -> String {
        format!("/Assets/tpp/pack/location/{0}/{0}", self.code)
    }
    pub fn pack_common(&self) -> String {
        format!("/Assets/tpp/pack/location/{0}/pack_common/{0}_common", self.code)
    }
    pub fn tre2_path(&self) -> String {
        format!("{}/block_common/{}_common_terrain.tre2", self.level(), self.code)
    }
    pub fn fstb_path(&self) -> String {
        format!("{}/block_common/{}_common_packages.fstb", self.level(), self.code)
    }
    pub fn stage_fox2(&self) -> String {
        format!("{}/{}_stage.fox2", self.level(), self.code)
    }
    pub fn twpf_path(&self) -> String {
        format!("{}/block_common/{}_climateSettings_dx11.twpf", self.level(), self.code)
    }
    /// common fox2 of the common fpkd: k = weather | terrainMaterial | terrainConfig | terrain | sky | data
    pub fn common_fox2(&self, k: &str) -> String {
        format!("{}/block_common/{}_common_{k}.fox2", self.level(), self.code)
    }
    pub fn common_nav(&self) -> String {
        format!("{}/block_common/{}_common_nav.fox2", self.level(), self.code)
    }
    pub fn sky_nav(&self) -> String {
        format!("{}/block_common/{}_common_navi_sky.nav2", self.level(), self.code)
    }
    pub fn sky_fox2(&self) -> String {
        format!("{}/block_common/{}_common_nav_sky.fox2", self.level(), self.code)
    }
    pub fn htre_path(&self, x: i32, y: i32) -> String {
        format!("{}/block_small/{x}/{x}_{y}/{}_{x}_{y}_terrain.htre", self.level(), self.code)
    }
    pub fn block_fox2_path(&self, x: i32, y: i32) -> String {
        format!("{}/block_small/{x}/{x}_{y}/{}_{x}_{y}_terrain.fox2", self.level(), self.code)
    }
    /// block navmesh file: ext = nav2 | fox2
    pub fn block_nav(&self, x: i32, y: i32, ext: &str) -> String {
        format!("{}/block_small/{x}/{x}_{y}/{}_{x}_{y}_nav.{ext}", self.level(), self.code)
    }
    pub fn pack_small(&self, x: i32, y: i32) -> String {
        format!("/Assets/tpp/pack/location/{0}/pack_small/{x}/{0}_{x}_{y}", self.code)
    }
    pub fn pack_slod0(&self, px: i32, py: i32) -> String {
        format!("/Assets/tpp/pack/environ/stagelow/{0}/small_lod0/{px}/{0}_slod0_{px}_{py}", self.code)
    }
    /// world texture tile (i, j) game path
    pub fn wt_path(&self, i: usize, j: usize) -> String {
        self.wt_path_fmt.replace("{code}", &self.code).replace("{i}", &format!("{i:02}")).replace("{j}", &format!("{j:02}"))
    }

    /// bilinear ground height of a row-major grid x grid float32 heightfield (flyk_m2.ground: every product / sum a
    /// float32 operation, numpy 2 / NEP 50), the same arithmetic as foxm3::cfg::ground
    pub fn ground(&self, h: &[f32], x: f64, z: f64) -> f64 {
        let g = self.grid;
        let r = (z + self.half()) / self.d;
        let c = (x + self.half()) / self.d;
        let (r0, c0) = (r.floor() as usize, c.floor() as usize);
        let (fr, fc) = (r - r0 as f64, c - c0 as f64);
        let (r1, c1) = ((r0 + 1).min(g - 1), (c0 + 1).min(g - 1));
        let at = |r: usize, c: usize| h[r * g + c];
        let a = at(r0, c0) * (1.0 - fc) as f32 * (1.0 - fr) as f32;
        let b = at(r0, c1) * fc as f32 * (1.0 - fr) as f32;
        let cc = at(r1, c0) * (1.0 - fc) as f32 * fr as f32;
        let d = at(r1, c1) * fc as f32 * fr as f32;
        (((a + b) + cc) + d) as f64
    }
}

pub const DEFAULT_WT_PATH: &str =
    "/Assets/tpp/common_source/environ/{code}/cm_{code}_wrtx001/sourceimages/strm_wrtx/cm_{code}_wrtx001X{i}Z{j}.ftex";

#[cfg(test)]
mod tests {
    use super::*;

    pub fn tst(grid: usize) -> Loc {
        Loc { code: "tst".into(), id: 99, ih_name: "TST".into(), grid, d: 2.0, first_block: 101, htre_n: HTRE_N,
              ring_small: 2, ring_slod0: 2, cluster: 32, wt_tiles: grid * 2 / 1024, wt_px: 2048,
              wt_path_fmt: DEFAULT_WT_PATH.into(), lake_y: None }
    }

    #[test]
    fn block_round_trip_every_grid() {
        for grid in [1024, 2048, 4096] {
            let l = tst(grid);
            assert_eq!(l.nblk() as usize * 128, grid * 2);
            let (ox, oz) = l.block_origin(l.center_index(), l.center_index());
            assert_eq!((ox, oz), (0.0, 0.0));
            for x in l.blocks() {
                for y in l.blocks() {
                    let (cx, cz) = l.block_center(x, y);
                    assert_eq!(l.block_of(cx, cz), (x, y));
                    let (ox, oz) = l.block_origin(x, y);
                    assert_eq!(l.block_of(ox, oz), (x, y));
                }
            }
            assert_eq!(l.small_range().len() as i32, l.nblk() + 4);
            let s = l.slod0_range();
            assert_eq!(s.len() as i32, l.nblk() / 2 + 4);
            assert_eq!(l.world_tiles().len(), l.wt_tiles * l.wt_tiles);
        }
    }

    #[test]
    fn paths_follow_code() {
        let l = tst(1024);
        assert_eq!(l.htre_path(101, 102), "/Assets/tpp/level/location/tst/block_small/101/101_102/tst_101_102_terrain.htre");
        assert_eq!(l.wt_path(1, 0),
                   "/Assets/tpp/common_source/environ/tst/cm_tst_wrtx001/sourceimages/strm_wrtx/cm_tst_wrtx001X01Z00.ftex");
        assert_eq!(l.pack_slod0(100, 102), "/Assets/tpp/pack/environ/stagelow/tst/small_lod0/100/tst_slod0_100_102");
    }
}
