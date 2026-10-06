//! The stage library (M3_PLAN.md §3.1). Each kind gives, for one implementation, a foxbuild stage (command, outputs,
//! locks, memory, owner, ...) plus the datasets it reads and writes. Outputs and path arguments come from the spec's
//! datasets, so FLYK gets exactly its legacy strings.
//!
//! Implementations in M3:
//!   legacy  the FLYK Python command, exactly as tools/build/flyk_stages.toml runs it (projects with compat = "flyk")
//!   rs      `fox <area> <cmd>` (every other project); None = no Rust implementation yet (a graph error)
//!
//! New kinds (T11: heights.simple, terrain.masks, terrain.materials rules / weights) are added here by their owners
//! through track A: one match arm + one entry in KINDS.
use crate::{Emitted, FOX_EXE, Impl};
use foxbuild::config::Stage;
use foxpipe::project::Spec;
use std::collections::BTreeMap;

pub struct Ctx<'a> {
    pub spec: &'a Spec,
}

impl Ctx<'_> {
    fn ds(&self, id: &str) -> String {
        self.spec.datasets_untraced().rel(id).to_string()
    }
    /// the large-block variant environment: { switch_env = "1" } (empty when large blocks are always on / absent)
    fn large_env(&self) -> BTreeMap<String, String> {
        match self
            .spec
            .file()
            .large_blocks
            .as_ref()
            .and_then(|l| l.switch_env.clone())
        {
            Some(k) => [(k, "1".to_string())].into(),
            None => BTreeMap::new(),
        }
    }
    fn has_large(&self) -> bool {
        self.spec.file().large_blocks.is_some()
    }
}

/// The library: kind name, default stage name, implementations available (legacy, rs).
pub const KINDS: &[(&str, &str, bool, bool)] = &[
    ("heights.generate", "heights.terrain_gen", true, false),
    ("terrain.materials", "texture.materials", true, false),
    ("terrain.verify", "texture.verify", true, false),
    ("terrain.audit", "texture.audit", true, false),
    ("dressing.place", "dressing.place", true, false),
    ("dressing.pack", "dressing.pack", true, false),
    ("dressing.stakes", "dressing.stakes", true, false),
    (
        "dressing.verify_published",
        "dressing.verify_published",
        true,
        false,
    ),
    (
        "dressing.walkcheck_published",
        "dressing.walkcheck_published",
        true,
        false,
    ),
    ("vegetation.setpieces", "veg.setpieces", true, false),
    ("vegetation.dense", "veg.dense", true, false),
    ("vegetation.lod", "veg.lod", true, false),
    ("vegetation.pack", "veg.pack", true, false),
    ("vegetation.verify", "veg.verify", true, false),
    ("vegetation.obstacles", "veg.obstacles", true, false),
    ("water", "water", true, false),
    ("nav.ground", "nav.ground", true, true),
    ("nav.sky", "nav.sky", true, true),
    ("package.terrain", "m3.terrain", true, true),
    ("package.fox2", "m3.fox2", true, true),
    ("package.packs", "m3.packs", true, true),
    ("package.mgsv", "m3.mgsv", true, true),
    ("package.verify", "m3.verify", true, true),
];

fn stage(name: &str, cmd: Vec<String>) -> Stage {
    let mut t = toml::Table::new();
    t.insert("name".into(), toml::Value::String(name.into()));
    t.insert(
        "cmd".into(),
        toml::Value::Array(cmd.into_iter().map(toml::Value::String).collect()),
    );
    toml::Value::Table(t)
        .try_into()
        .expect("a minimal foxbuild stage")
}

fn py(args: &[&str]) -> Vec<String> {
    ["python", "-u"]
        .iter()
        .chain(args)
        .map(|s| s.to_string())
        .collect()
}

fn fox(args: &[&str]) -> Vec<String> {
    std::iter::once(FOX_EXE)
        .chain(args.iter().copied())
        .map(String::from)
        .collect()
}

fn ids(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// strict params of a kind: only these keys, with these types
fn params(p: &toml::Table, allowed: &[(&str, &str)]) -> Result<(), String> {
    for (k, v) in p {
        match allowed.iter().find(|a| a.0 == k) {
            None => {
                return Err(format!(
                    "unknown param {k} (allowed: {})",
                    allowed.iter().map(|a| a.0).collect::<Vec<_>>().join(", ")
                ));
            }
            Some((_, ty)) => {
                let ok = match *ty {
                    "bool" => v.is_bool(),
                    "str" => v.is_str(),
                    _ => true,
                };
                if !ok {
                    return Err(format!("param {k}: expected {ty}"));
                }
            }
        }
    }
    Ok(())
}

fn flag(p: &toml::Table, k: &str) -> bool {
    p.get(k).and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Emit one stage of `kind` with `imp`; Ok(None) = the kind has no such implementation.
pub fn emit(c: &Ctx, kind: &str, p: &toml::Table, imp: Impl) -> Result<Option<Emitted>, String> {
    let legacy = imp == Impl::Legacy;
    let f = c.spec.file();
    let e = |stage: Stage, reads: &[&str], writes: &[&str]| Emitted {
        stage,
        reads: ids(reads),
        writes: ids(writes),
    };
    // set fields on a stage in one expression
    macro_rules! st {
        ($s:expr, { $($k:ident : $v:expr),* $(,)? }) => {{ let mut s = $s; $( s.$k = $v; )* s }};
    }
    let out = |v: &[&str]| -> Vec<String> { v.iter().map(|d| c.ds(d)).collect() };
    let s = |v: &[&str]| -> Vec<String> { ids(v) };
    let em = match kind {
        // ---------------------------------------------------------------- heights
        "heights.generate" => {
            params(p, &[("recipe", "str")])?;
            let recipe = p.get("recipe").and_then(|v| v.as_str()).unwrap_or("");
            if !legacy || recipe != "flyk" {
                return Ok(None);
            }
            Some(e(
                st!(stage("heights.terrain_gen", py(&["tools/location/flyk_terrain_gen.py", "build"])), {
                    outputs: out(&["heights"]), default: false, deterministic: false, gpu: true, mem_gb: 8.0,
                    owner: "erosion agent".into(),
                }),
                &[],
                &["heights"],
            ))
        }
        // ---------------------------------------------------------------- terrain materials (FLYK painter)
        "terrain.materials" | "terrain.verify" | "terrain.audit" => {
            params(p, &[])?;
            if !legacy || f.terrain.materials.painter != "recipe:flyk" {
                return Ok(None);
            }
            let tg = "tools/location/flyk_terrain_gen.py";
            match kind {
                "terrain.materials" => Some(e(
                    st!(stage("texture.materials", py(&[tg, "materials", "--full", "--white-sand", "--masks"])), {
                        soft_inputs: out(&["veg_placements", "veg_tables"]),
                        outputs: out(&["combo", "cluster_ids", "cluster_cfg", "material_weights", "worldtex",
                                       "mask_jungle_density", "mask_savanna", "mask_beach_lagoon", "mask_open_clearings",
                                       "mask_rock_material", "mask_flow_wetness", "masks_index", "mask_look_cove_sand",
                                       "mask_look_camp_ground", "materials_report"]),
                        locks: s(&["heightfield_dir"]), mem_gb: 6.0, owner: "texture agent".into(),
                    }),
                    &["heights", "masks_index", "water", "sand", "look_terrain"],
                    &[
                        "combo",
                        "cluster_ids",
                        "cluster_cfg",
                        "material_weights",
                        "worldtex",
                        "masks_index",
                    ],
                )),
                "terrain.verify" => Some(e(
                    st!(stage("texture.verify", py(&[tg, "materials", "--verify"])), {
                        locks: s(&["heightfield_dir"]), mem_gb: 4.0, owner: "texture agent".into(),
                    }),
                    &[
                        "combo",
                        "cluster_ids",
                        "cluster_cfg",
                        "material_weights",
                        "worldtex",
                    ],
                    &[],
                )),
                _ => {
                    let (png, json) = (c.ds("seams_audit_png"), c.ds("seams_audit"));
                    Some(e(
                        st!(stage("texture.audit", py(&[tg, "materials", "--audit", "--png", &png, "--json", &json])), {
                            outputs: vec![json.clone(), png.clone()], locks: s(&["heightfield_dir"]), mem_gb: 4.0,
                            owner: "texture agent".into(),
                        }),
                        &[
                            "combo",
                            "cluster_ids",
                            "cluster_cfg",
                            "material_weights",
                            "worldtex",
                        ],
                        &["seams_audit", "seams_audit_png"],
                    ))
                }
            }
        }
        // ---------------------------------------------------------------- dressing (FLYK legacy only)
        "dressing.place" => {
            params(p, &[])?;
            if !legacy {
                return Ok(None);
            }
            Some(e(
                st!(stage("dressing.place", py(&["tools/location/flyk_dressing.py", "place"])), {
                    outputs: out(&["dressing_placements"]), mem_gb: 4.0, owner: "dressing agent".into(),
                }),
                &["combo", "masks_index"],
                &["dressing_placements"],
            ))
        }
        "dressing.pack" | "dressing.verify_published" | "dressing.walkcheck_published" => {
            params(p, &[("large", "bool")])?;
            if !legacy {
                return Ok(None);
            }
            let large = flag(p, "large");
            if large && !c.has_large() {
                return Err("large = true needs [large_blocks]".into());
            }
            let sfx = if large { "_large" } else { "" };
            let env = if large {
                c.large_env()
            } else {
                BTreeMap::new()
            };
            let dp = "tools/location/flyk_dressing_pack.py";
            let outd = if large {
                "dressing_out_large"
            } else {
                "dressing_out"
            };
            match kind {
                "dressing.pack" => Some(e(
                    st!(stage(&format!("dressing.pack{sfx}"), py(&[dp, "all", "--skip-walkcheck"])), {
                        env: env, outputs: out(&[outd]), locks: s(&["dressing_out"]), mem_gb: 5.0,
                        owner: "dressing agent".into(),
                    }),
                    &["dressing_placements"],
                    &[outd],
                )),
                "dressing.verify_published" => Some(e(
                    st!(stage(&format!("dressing.verify_published{sfx}"), py(&[dp, "verify", "--published"])), {
                        env: env, locks: s(&["dressing_out"]), mem_gb: 4.0, owner: "dressing agent".into(),
                    }),
                    &[outd, "scatter_obstacles", "stakes"],
                    &[],
                )),
                _ => Some(e(
                    st!(stage(&format!("dressing.walkcheck_published{sfx}"), py(&[dp, "walkcheck", "--published"])), {
                        rust_tools: true, env: env, locks: s(&["dressing_out"]), mem_gb: 4.0, owner: "dressing agent".into(),
                    }),
                    &[outd],
                    &[],
                )),
            }
        }
        "dressing.stakes" => {
            params(p, &[])?;
            if !legacy {
                return Ok(None);
            }
            Some(e(
                st!(stage("dressing.stakes", py(&["tools/location/flyk_dressing_field.py", "export"])), {
                    outputs: out(&["stakes"]), locks: s(&["dressing_out"]), mem_gb: 3.0, owner: "dressing agent".into(),
                }),
                &["dressing_out"],
                &["stakes"],
            ))
        }
        // ---------------------------------------------------------------- vegetation (FLYK legacy only)
        "vegetation.setpieces"
        | "vegetation.dense"
        | "vegetation.lod"
        | "vegetation.pack"
        | "vegetation.verify"
        | "vegetation.obstacles" => {
            params(
                p,
                if kind == "vegetation.dense" {
                    &[("flora_add", "bool")]
                } else {
                    &[]
                },
            )?;
            if !legacy {
                return Ok(None);
            }
            let tp = "tools/location/flyk_transplant_pack.py";
            let veg = || "vegetation".to_string();
            match kind {
                "vegetation.setpieces" => Some(e(
                    st!(stage("veg.setpieces", py(&["tools/location/flyk_scatter.py", "fix-set-pieces"])), {
                        outputs: out(&["setpieces_placements"]), locks: s(&["scatter_out"]), mem_gb: 3.0, owner: veg(),
                    }),
                    &["combo", "masks_index"],
                    &["setpieces_placements"],
                )),
                "vegetation.dense" => {
                    let env = if flag(p, "flora_add") {
                        [("FLYK_FLORA_ADD".to_string(), "1".to_string())].into()
                    } else {
                        BTreeMap::new()
                    };
                    Some(e(
                        st!(stage("veg.dense", py(&["tools/location/flyk_dense.py", "apply"])), {
                            // the Rust port (placement agent), staged unproven: switched after Gate 2
                            cmd_rs: Some(fox(&["place", "dense", "apply"])), rs_proven: false, env: env,
                            soft_inputs: out(&["beach_keepout", "nav_manifest", "sky_manifest", "fauna_manifest",
                                               "scatter_manifest", "beach_manifest", "surface_manifest", "probes_manifest",
                                               "artdir_probes_manifest", "artdir_farshore_manifest", "external_manifest",
                                               "flora_imports_manifest"]),
                            outputs: out(&["veg_placements", "veg_tables"]), mem_gb: 6.0, owner: veg(),
                        }),
                        &[
                            "setpieces_placements",
                            "dressing_out",
                            "beach_zones",
                            "beach_keepout_pin",
                        ],
                        &["veg_placements", "veg_tables"],
                    ))
                }
                "vegetation.lod" => Some(e(
                    st!(stage("veg.lod", py(&["tools/location/flyk_lod.py", "bake", "--variant", "dense"])), {
                        outputs: out(&["veg_lod"]), mem_gb: 4.0, owner: veg(),
                    }),
                    &["veg_placements"],
                    &["veg_lod"],
                )),
                "vegetation.pack" => Some(e(
                    st!(stage("veg.pack", py(&[tp, "build", "--variant", "dense", "--look"])), {
                        outputs: out(&["scatter_out"]), locks: s(&["scatter_out"]), mem_gb: 5.0, owner: veg(),
                    }),
                    &["veg_lod", "look_subst", "look_assets"],
                    &["scatter_out"],
                )),
                "vegetation.verify" => Some(e(
                    st!(stage("veg.verify", py(&[tp, "verify", "--variant", "dense"])), {
                        locks: s(&["scatter_out"]), mem_gb: 4.0, owner: veg(),
                    }),
                    &["scatter_out"],
                    &[],
                )),
                _ => Some(e(
                    st!(stage("veg.obstacles", py(&[tp, "obstacles", "--variant", "dense"])), {
                        outputs: out(&["scatter_obstacles"]), locks: s(&["scatter_out"]), mem_gb: 4.0, owner: veg(),
                    }),
                    &["scatter_out"],
                    &["scatter_obstacles"],
                )),
            }
        }
        // ---------------------------------------------------------------- water
        "water" => {
            params(p, &[])?;
            let w = f
                .water
                .as_ref()
                .ok_or("use = \"water\" needs a [water] section")?;
            if !legacy || w.impl_ != "legacy:flyk_water" {
                return Ok(None);
            }
            let mode = w
                .mode
                .as_deref()
                .ok_or("[water] legacy:flyk_water needs mode")?;
            Some(e(
                st!(stage("water", py(&["tools/location/flyk_water.py", "all", "--water", mode])), {
                    outputs: out(&["water"]), mem_gb: 6.0, owner: "water".into(),
                }),
                &["heights", "masks_index", "look_terrain"],
                &["water"],
            ))
        }
        // ---------------------------------------------------------------- navigation
        "nav.ground" | "nav.sky" => {
            params(p, &[])?;
            let ground = kind == "nav.ground";
            let cmd = match (legacy, ground) {
                (true, true) => py(&["tools/location/flyk_nav.py", "all"]),
                (true, false) => py(&["tools/location/flyk_sky.py", "all"]),
                (false, true) => fox(&["nav", "all"]),
                (false, false) => fox(&["nav", "sky"]),
            };
            let mut reads: Vec<&str> = vec!["heights", "masks_index"];
            let sets: Vec<String> = f.nav.obstacle_sets.clone();
            let sets: Vec<&str> = sets.iter().map(|x| x.as_str()).collect();
            if ground {
                reads.extend(sets.iter().copied());
            } else {
                reads.push("scatter_obstacles");
            }
            let (o, mem) = if ground {
                ("nav_out", 6.0)
            } else {
                ("sky_out", 3.0)
            };
            let st = st!(stage(kind, cmd), {
                rust_tools: true, outputs: out(&[o]), mem_gb: mem, owner: "coordinator (location)".into(),
            });
            Some(Emitted {
                stage: st,
                reads: ids(&reads),
                writes: ids(&[o]),
            })
        }
        // ---------------------------------------------------------------- the location package
        "package.terrain" | "package.fox2" | "package.packs" | "package.mgsv"
        | "package.verify" => {
            params(p, &[("large", "bool")])?;
            let cmd = kind.strip_prefix("package.").unwrap();
            let large = p
                .get("large")
                .and_then(|v| v.as_bool())
                .unwrap_or(c.has_large());
            let env = if large {
                c.large_env()
            } else {
                BTreeMap::new()
            };
            let argv = if legacy {
                py(&["tools/location/flyk_m3.py", cmd])
            } else {
                fox(&["m3", cmd])
            };
            let base = st!(stage(&format!("m3.{cmd}"), argv), {
                env: env, locks: s(&["m3_stage"]), owner: "coordinator (location)".into(), rust_tools: !legacy,
            });
            let dressing = if large {
                "dressing_out_large"
            } else {
                "dressing_out"
            };
            Some(match cmd {
                "terrain" => e(
                    st!(base, { mem_gb: 4.0 }),
                    &["heights", "combo", "cluster_ids", "cluster_cfg", "worldtex"],
                    &[],
                ),
                "fox2" => e(st!(base, { mem_gb: 3.0 }), &[dressing], &[]),
                "packs" => e(
                    st!(base, { outputs: out(&["m3_mod", "m3_stage"]), mem_gb: 4.0 }),
                    &["nav_out", "sky_out", "scatter_out", dressing, "water"],
                    &["m3_mod", "m3_stage"],
                ),
                "mgsv" => e(
                    st!(base, { outputs: out(&["mgsv"]), locks: s(&["m3_stage", "snakebite"]), mem_gb: 3.0 }),
                    &["m3_mod"],
                    &["mgsv"],
                ),
                _ => e(
                    st!(base, { outputs: out(&["m3_verify"]), mem_gb: 3.0, verify: true }),
                    &["mgsv"],
                    &["m3_verify"],
                ),
            })
        }
        _ => {
            return Err(format!(
                "unknown stage kind (library: {})",
                KINDS.iter().map(|k| k.0).collect::<Vec<_>>().join(", ")
            ));
        }
    };
    Ok(em)
}
