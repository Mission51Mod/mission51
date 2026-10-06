//! Semantic validation (pass 2; pass 1 is serde: types, unknown keys, required fields). Every problem is collected,
//! so `fox project check` prints all of them at once.
use super::model::*;
use super::{Issue, LoadOptions, Severity};
use std::cmp::Ordering;
use std::path::Path;

/// TppDefine.LOCATION_ID of the vanilla game (code, id, ships location data). Fallback table: `fox setup` reads the
/// user's own install and passes its table to `check_install` (T4); these are only the registered names / numbers.
pub const VANILLA_LOCATIONS: &[(&str, i64, bool)] = &[
    ("init", 1, true),
    ("afgh", 10, true),
    ("mafr", 20, true),
    ("cypr", 30, true),
    ("gntn", 40, false),
    ("ombs", 45, false),
    ("mtbs", 50, true),
    ("mbqf", 55, true),
    ("hlsp", 60, true),
    ("flyk", 70, false),
    ("sand_afgh", 91, true),
    ("sand_mafr", 92, true),
    ("sand_mtbs", 95, true),
];

/// community location mods known to use a location id (docs/location_feasibility.md)
pub const COMMUNITY_IDS: &[(i64, &str)] = &[
    (40, "US Naval Prison Facility (GNTN port)"),
    (68, "Florest"),
];

/// First id outside the known registration/community tables. This does not inspect the user's install.
pub fn suggested_location_id() -> i64 {
    (1..=65535)
        .find(|id| {
            !VANILLA_LOCATIONS.iter().any(|entry| entry.1 == *id)
                && !COMMUNITY_IDS.iter().any(|entry| entry.0 == *id)
        })
        .unwrap_or(0)
}

pub const WEATHERS: &[&str] = &["SUNNY", "CLOUDY", "RAINY", "SANDSTORM", "FOGGY", "POURING"];
pub const ZONE_RULES: &[&str] = &["no_carve", "no_veg", "no_dressing", "flat"];

struct V {
    out: Vec<Issue>,
}

impl V {
    fn err(&mut self, field: &str, msg: impl Into<String>) {
        self.out.push(Issue {
            severity: Severity::Error,
            field: field.into(),
            msg: msg.into(),
        });
    }
    fn warn(&mut self, field: &str, msg: impl Into<String>) {
        self.out.push(Issue {
            severity: Severity::Warning,
            field: field.into(),
            msg: msg.into(),
        });
    }
}

fn is_hex32(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn re_like(s: &str, first: fn(u8) -> bool, rest: fn(u8) -> bool, min: usize, max: usize) -> bool {
    let b = s.as_bytes();
    (min..=max).contains(&b.len()) && first(b[0]) && b[1..].iter().all(|&c| rest(c))
}

/// Repo-relative, '/'-separated, no '..', under one of the roots (empty = any repo root).
pub fn repo_rel_ok(p: &str, roots: &[&str]) -> Result<(), String> {
    if p.is_empty() || p.starts_with('/') || p.contains('\\') || p.contains(':') {
        return Err(format!(
            "{p:?}: must be repo-relative with '/' (no drive, no leading '/')"
        ));
    }
    if p.split('/').any(|c| c == ".." || c == "." || c.is_empty()) {
        return Err(format!("{p:?}: no '.', '..' or empty components"));
    }
    if p.chars().any(|c| c.is_control() || "<>\"|?*{}".contains(c)) {
        return Err(format!(
            "{p:?}: no control characters, wildcards or unresolved placeholders"
        ));
    }
    if !roots.is_empty()
        && !roots.iter().any(|r| {
            let r = r.trim_end_matches('/');
            p == r || p.strip_prefix(r).is_some_and(|rest| rest.starts_with('/'))
        })
    {
        return Err(format!("{p:?}: must lie under {}", roots.join(", ")));
    }
    Ok(())
}

/// foxbuild's ignore / static pattern semantics (foxbuild::hashing::matches)
pub fn pattern_matches(rel: &str, pat: &str) -> bool {
    let rel = rel.to_lowercase();
    let pat = pat.to_lowercase();
    if let Some(rest) = pat.strip_prefix("**/") {
        if let Some(ext) = rest.strip_prefix('*') {
            return rel.ends_with(ext);
        }
        if let Some(seg) = rest.strip_suffix('/') {
            return rel.starts_with(&format!("{seg}/")) || rel.contains(&format!("/{seg}/"));
        }
        return rel == rest || rel.ends_with(&format!("/{rest}"));
    }
    if let Some(dir) = pat.strip_suffix('/') {
        return rel == dir || rel.starts_with(&format!("{dir}/"));
    }
    rel == pat || rel.starts_with(&format!("{pat}/"))
}

pub(crate) fn validate(
    f: &SpecFile,
    repo_root: &Path,
    opts: &LoadOptions,
    ds: Option<&super::Datasets>,
    ignore: &[String],
) -> Vec<Issue> {
    let mut v = V { out: vec![] };
    let l = &f.location;
    // ---- format, project
    if f.format != super::FORMAT {
        v.err(
            "format",
            format!(
                "unsupported format {} (this build reads {})",
                f.format,
                super::FORMAT
            ),
        );
    }
    if !re_like(
        &f.project.name,
        |c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_',
        |c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_',
        2,
        16,
    ) {
        v.err(
            "project.name",
            format!("{:?}: [a-z0-9_]{{2,16}}", f.project.name),
        );
    }
    match f.project.kind.as_str() {
        "new-location" => {}
        "vanilla-edit" => v.err(
            "project.kind",
            "vanilla-edit is reserved (editor.md §4), not available in M3",
        ),
        k => v.err(
            "project.kind",
            format!("{k:?}: new-location | vanilla-edit"),
        ),
    }
    // ---- identity
    if !re_like(
        &l.code,
        |c| c.is_ascii_lowercase(),
        |c| c.is_ascii_lowercase() || c.is_ascii_digit(),
        3,
        8,
    ) {
        v.err(
            "location.code",
            format!("{:?}: [a-z][a-z0-9]{{2,7}}", l.code),
        );
    }
    if let Some(&(c, id, data)) = VANILLA_LOCATIONS.iter().find(|x| x.0 == l.code) {
        if data {
            v.err(
                "location.code",
                format!("{c} is a vanilla location with data"),
            );
        } else if id != l.id {
            v.err(
                "location.id",
                format!("{c} is vanilla-registered with id {id}, not {}", l.id),
            );
        } else {
            v.warn(
                "location.code",
                format!("{c} = {id} is vanilla-registered without data (registration only)"),
            );
        }
    }
    if let Some(&(c, id, _)) = VANILLA_LOCATIONS.iter().find(|x| x.1 == l.id)
        && c != l.code
    {
        v.err(
            "location.id",
            format!("id {id} is the vanilla location {c}"),
        );
    }
    if let Some((id, who)) = COMMUNITY_IDS.iter().find(|x| x.0 == l.id) {
        v.warn(
            "location.id",
            format!("id {id} is used by the community mod {who}"),
        );
    }
    if !(1..=65535).contains(&l.id) {
        v.err("location.id", format!("{}: 1..65535", l.id));
    }
    if l.ih_name.as_deref().is_some_and(|s| s.is_empty()) {
        v.err("location.ih_name", "empty");
    }
    // ---- grid
    if !l.grid.is_multiple_of(512) || !(1024..=4096).contains(&l.grid) {
        v.err(
            "location.grid",
            format!("{}: a multiple of 512 in 1024..4096", l.grid),
        );
    } else if l.grid < 2048 && !opts.allow_unproven {
        v.err(
            "location.grid",
            format!("grid {} needs the engine probe (M3 T0b) first", l.grid),
        );
    }
    if l.grid_distance != 2.0 {
        v.err(
            "location.grid_distance",
            "must be 2.0 (other values are unproven in the engine)",
        );
    }
    if l.first_block != 101 && !opts.allow_unproven {
        v.err("location.first_block", "must be 101 (the Fox convention)");
    }
    if l.ring_small < 2 || l.ring_slod0 < 2 {
        v.err(
            "location.ring_small",
            "ring_small / ring_slod0 must be >= 2 (the streaming window reaches 2)",
        );
    }
    let half = l.grid as f64 * l.grid_distance / 2.0;
    let inside = |x: f64, z: f64| x.abs() < half && z.abs() < half;
    for (k, w) in [
        ("location.weather", &l.weather),
        ("location.extra_weather", &l.extra_weather),
    ] {
        if let Some(w) = w {
            let sum: i64 = w.iter().map(|x| x.1).sum();
            if !w.is_empty() && sum != 100 {
                v.err(k, format!("probabilities sum to {sum}, not 100"));
            }
            for (n, p) in w {
                if !WEATHERS.contains(&n.as_str()) {
                    v.err(k, format!("{n}: one of {}", WEATHERS.join(", ")));
                }
                if *p < 0 {
                    v.err(k, format!("{n}: negative probability"));
                }
            }
        }
    }
    if let Some(m) = &l.map {
        for (k, p) in [("height", &m.height), ("photo", &m.photo)] {
            if !p.starts_with("/Assets/") {
                v.err(
                    &format!("location.map.{k}"),
                    format!("{p:?}: an absolute game path /Assets/..."),
                );
            }
        }
    }
    // ---- terrain
    let t = &f.terrain;
    if !["pinned", "generate", "import", "recipe"].contains(&t.heights.source.as_str()) {
        v.err(
            "terrain.heights.source",
            format!(
                "{:?}: pinned | generate | import | recipe",
                t.heights.source
            ),
        );
    }
    if let Some(d) = &t.heights.dataset
        && super::datasets::def(d).is_none()
    {
        v.err(
            "terrain.heights.dataset",
            format!("{d}: unknown dataset id"),
        );
    }
    if t.heights.source == "recipe" && t.heights.recipe.is_none() {
        v.err(
            "terrain.heights.recipe",
            "source = \"recipe\" needs recipe = <name>",
        );
    }
    if let Some(e) = &t.heights.expect
        && !is_hex32(e)
    {
        v.err(
            "terrain.heights.expect",
            format!("{e:?}: 32 lower-case hex digits"),
        );
    }
    for (k, pin) in &t.heights_pins {
        if !is_hex32(pin) {
            v.err(
                &format!("terrain.heights_pins.{k}"),
                format!("{pin:?}: 32 lower-case hex digits"),
            );
        }
    }
    let p = &t.materials.painter;
    if let Some(r) = p.strip_prefix("recipe:") {
        if f.stages.compat.as_deref() != Some(r) {
            v.err(
                "terrain.materials.painter",
                format!("recipe:{r} needs [stages] compat = \"{r}\" (a project recipe)"),
            );
        }
    } else if p != "rules" && p != "weights" {
        v.err(
            "terrain.materials.painter",
            format!("{p:?}: rules | weights | recipe:<name>"),
        );
    }
    if t.cluster == 0 || !l.grid.is_multiple_of(t.cluster) {
        v.err(
            "terrain.cluster",
            format!("{} must divide the grid {}", t.cluster, l.grid),
        );
    }
    let extent = l.grid as f64 * l.grid_distance;
    if let Some(n) = t.world_texture.tiles {
        if (n * 1024) as f64 != extent {
            v.err(
                "terrain.world_texture.tiles",
                format!("{n} tiles x 1024 m != the extent {extent} m"),
            );
        }
    } else if extent % 1024.0 != 0.0 {
        v.err(
            "terrain.world_texture.tiles",
            "the extent is not a multiple of 1024 m",
        );
    }
    if let Some(gp) = &t.world_texture.game_path
        && (!gp.starts_with("/Assets/") || !gp.contains("{i}") || !gp.contains("{j}"))
    {
        v.err(
            "terrain.world_texture.game_path",
            "an /Assets/... template with {i} and {j}",
        );
    }
    // ---- water
    if let Some(w) = &f.water {
        if w.impl_.starts_with("legacy:") && f.stages.compat.is_none() {
            v.err(
                "water.impl",
                format!(
                    "{}: legacy Python implementations need [stages] compat",
                    w.impl_
                ),
            );
        }
        if f.terrain.lake_y.is_none() {
            v.err("terrain.lake_y", "[water] needs terrain.lake_y");
        }
    }
    // ---- nav
    let n = &f.nav;
    if n.band[0].partial_cmp(&n.band[1]) != Some(Ordering::Less) {
        v.err("nav.band", "band[0] < band[1]");
    }
    if n.max_wade <= 0.0 {
        v.err("nav.max_wade", "must be > 0");
    }
    if n.tile_shift.is_some_and(|s| s < 0) {
        v.err("nav.tile_shift", "must be >= 0");
    }
    for s in &n.obstacle_sets {
        if super::datasets::def(s).is_none() {
            v.err("nav.obstacle_sets", format!("{s}: unknown dataset id"));
        }
    }
    let mut names = std::collections::BTreeSet::new();
    for a in &n.area {
        if !names.insert(&a.name) {
            v.err("nav.area", format!("duplicate area {}", a.name));
        }
        if !(inside(a.x0, a.z0) && inside(a.x0 + a.size - 1e-9, a.z0 + a.size - 1e-9))
            || a.size <= 0.0
        {
            v.err(
                &format!("nav.area.{}", a.name),
                "the rect must lie inside the extent",
            );
        }
        for (k, p) in [("nav2", &a.nav2), ("fox2", &a.fox2)] {
            if !p.starts_with("/Assets/") {
                v.err(
                    &format!("nav.area.{}.{k}", a.name),
                    format!("{p:?}: an absolute game path /Assets/..."),
                );
            }
        }
    }
    let s = &n.sky;
    if let Some(c) = s.cover
        && !(c[0] < c[2] && c[1] < c[3])
    {
        v.err("nav.sky.cover", "[x0, z0, x1, z1] with x0 < x1, z0 < z1");
    }
    if s.step <= 0.0 || s.hole_margin < 0.0 {
        v.err("nav.sky.step", "step > 0, hole_margin >= 0");
    }
    // ---- large blocks
    if let Some(lb) = &f.large_blocks {
        let (b0, b1) = (l.first_block, l.first_block + (l.grid / 64) as i32 - 1);
        if lb.size_in_bytes <= 0 {
            v.err("large_blocks.size_in_bytes", "must be > 0");
        }
        for b in &lb.block {
            let r = &b.region;
            let ok = |a: [i32; 2]| a[0] <= a[1] && a[0] >= b0 && a[1] <= b1;
            if !ok(r.x) || !ok(r.y) {
                v.err(
                    &format!("large_blocks.block.{}", b.name),
                    format!("region must lie inside blocks {b0}..{b1}"),
                );
            }
            if !b.pack.starts_with("/Assets/") || !b.fox2_dir.starts_with("/Assets/") {
                v.err(
                    &format!("large_blocks.block.{}", b.name),
                    "pack / fox2_dir: absolute game paths",
                );
            }
        }
    }
    // ---- missions
    if let Some(tm) = &f.test_mission {
        if !(12000..=12999).contains(&tm.code) {
            v.err(
                "test_mission.code",
                format!("{}: use the community test range 12000-12999", tm.code),
            );
        }
        if tm.name.is_empty() {
            v.err("test_mission.name", "empty");
        }
        if !inside(tm.start[0], tm.start[2]) {
            v.err("test_mission.start", "must lie inside the extent");
        }
        if !tm.pack.starts_with("/Assets/") || !tm.level.starts_with("/Assets/") {
            v.err("test_mission.pack", "pack / level: absolute game paths");
        }
    }
    let mut codes = std::collections::BTreeSet::new();
    for m in &f.mission {
        if !codes.insert(m.code) || f.test_mission.as_ref().is_some_and(|t| t.code == m.code) {
            v.err("mission", format!("mission code {} used twice", m.code));
        }
    }
    // ---- paths, datasets
    if let Some(w) = &f.paths.work
        && let Err(e) = repo_rel_ok(w, &["work/"])
    {
        v.err("paths.work", e);
    }
    for (field, p) in [
        ("build.log_dir", f.build.log_dir.as_deref()),
        ("build.temp_dir", Some(f.build.temp_dir.as_str())),
    ] {
        if let Some(p) = p
            && let Err(e) = repo_rel_ok(p, &["work/"])
        {
            v.err(field, e);
        }
    }
    if let Some(ds) = ds {
        for d in ds.iter() {
            let roots: &[&str] = if d.has(super::datasets::flag::STATIC) {
                &[]
            } else {
                &["work/", "projects/", "build/"]
            };
            if let Err(e) = repo_rel_ok(&d.rel, roots) {
                v.err(&format!("datasets.{}", d.id), e);
            }
        }
        let sc = ds.rel("spec_cache");
        if let Some(p) = ignore.iter().find(|p| pattern_matches(sc, p)) {
            v.err(
                "paths.spec_cache",
                format!("{sc} is ignored by build.ignore {p:?}: facets would never invalidate"),
            );
        }
    }
    // ---- zones, sites
    for z in &f.protected_zone {
        let k = format!("protected_zone.{}", z.name);
        let shapes = z.box_.is_some() as u8 + z.circle.is_some() as u8 + z.polygon.is_some() as u8;
        if shapes != 1 {
            v.err(&k, "exactly one of box / circle / polygon");
        }
        if let Some(b) = z.box_
            && !(b[0] < b[1] && b[2] < b[3] && inside(b[0], b[2]) && inside(b[1], b[3]))
        {
            v.err(&k, "box [x0, x1, z0, z1] inside the extent");
        }
        if let Some(c) = z.circle
            && (!inside(c[0], c[1]) || c[2] <= 0.0)
        {
            v.err(&k, "circle [x, z, r] inside the extent, r > 0");
        }
        if let Some(pg) = &z.polygon
            && (pg.len() < 3 || pg.iter().any(|p| !inside(p[0], p[1])))
        {
            v.err(&k, "polygon: >= 3 points inside the extent");
        }
        for r in &z.rules {
            if !ZONE_RULES.contains(&r.as_str()) {
                v.err(&k, format!("rule {r}: one of {}", ZONE_RULES.join(", ")));
            }
        }
    }
    for (k, p) in &f.sites {
        if p.len() != 3 || !inside(p[0], p[2]) {
            v.err(&format!("sites.{k}"), "[x, y, z] inside the extent");
        }
    }
    // ---- templates
    let t = &f.package.templates;
    for (k, p) in [
        ("ih_location", &t.ih_location),
        ("ih_mission", &t.ih_mission),
        ("metadata", &t.metadata),
        ("seq_lua", &t.seq_lua),
    ] {
        if let Some(p) = p {
            let full = repo_root.join(p);
            if !full.is_file() {
                v.err(
                    &format!("package.templates.{k}"),
                    format!("{p}: no such file"),
                );
            }
        }
    }
    // ---- build, stages, workspace
    for (i, pn) in f.build.pinned.iter().enumerate() {
        match (&pn.dataset, &pn.path) {
            (Some(d), None) => {
                if super::datasets::def(d).is_none() {
                    v.err(
                        &format!("build.pinned[{i}]"),
                        format!("{d}: unknown dataset id"),
                    );
                }
            }
            (None, Some(p)) => {
                if let Err(e) = repo_rel_ok(p, &[]) {
                    v.err(&format!("build.pinned[{i}].path"), e);
                }
            }
            _ => v.err(
                &format!("build.pinned[{i}]"),
                "exactly one of dataset / path",
            ),
        }
    }
    if let Some(c) = &f.stages.compat {
        if c != "flyk" {
            v.err(
                "stages.compat",
                format!("{c:?}: the only compatibility mode is \"flyk\""),
            );
        } else if l.code != "flyk" {
            v.err(
                "stages.compat",
                "compat = \"flyk\" is for the FLYK project only (Python references are FLYK-only)",
            );
        }
    }
    if let Some(w) = &f.workspace
        && let Err(e) = repo_rel_ok(&w.data_root, &["work/"])
    {
        v.err("workspace.data_root", e);
    }
    v.out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert!(is_hex32("7acf01657512133b7eb1dbaff4cb6647"));
        assert!(!is_hex32("7ACF01657512133b7eb1dbaff4cb6647"));
        assert!(repo_rel_ok("work/flyk/m3", &["work/"]).is_ok());
        assert!(repo_rel_ok("work/../x", &["work/"]).is_err());
        assert!(repo_rel_ok("C:/x", &["work/"]).is_err());
        assert!(repo_rel_ok("tools/x", &["work/"]).is_err());
        assert!(pattern_matches("work/build/x", "work/build/"));
        assert!(pattern_matches("a/scratch/b", "**/scratch/"));
        assert!(pattern_matches("a/b.log", "**/*.log"));
        assert!(!pattern_matches("work/flyk/spec", "work/build/"));
    }
}
