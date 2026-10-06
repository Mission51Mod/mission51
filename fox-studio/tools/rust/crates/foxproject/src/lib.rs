//! foxproject: the stage library and the build-graph generator (M3_PLAN.md §3).
//!
//! A project spec (foxpipe::project) lists its stages as `[[stage]]` entries, in order:
//!   - a **library instance**, `use = "<kind>"`: the kind (kinds.rs) gives the command, outputs, locks, memory, owner
//!     and the datasets it reads / writes; the instance may set `name`, `params`, `after` (replaces the derived edges),
//!     `after_extra`, `impl` ("legacy" | "rs") and override any foxbuild stage key;
//!   - a **custom stage**: the foxbuild [[stage]] keys verbatim (name, cmd, after, ...).
//!
//! `generate(&spec)` turns that into a `foxbuild::config::Graph`, validated by foxbuild itself
//! (`Graph::from_toml_str`: unknown keys, duplicate names, unknown / cyclic edges). Implementations: a project with
//! `[stages] compat = "flyk"` gets the legacy (Python) implementations exactly as tools/build/flyk_stages.toml has
//! them; every other project gets Rust implementations only, and a kind without one is an error naming the stage.
//! Edges: an instance without `after` gets one edge to the producer of every dataset path it reads (aliases count,
//! soft reads excluded);
//! `after_extra` adds edges.
//!
//! FLYK's generated graph equals tools/build/flyk_stages.toml (tests/golden_flyk.rs): the same stages in the same
//! order with the same keys, so the same command fingerprints.
pub mod build_plan;
pub mod cli;
pub mod document;
pub mod kinds;

pub use build_plan::{
    MaterializedBuild, NativeToolIdentity, ProjectBuildPlan, plan_build, plan_build_in,
};
pub use cli::{cli, prepare_build};
pub use document::ProjectDocument;

use foxbuild::config::{Graph, Stage};
use foxpipe::project::Spec;
use serde::Deserialize;
use std::collections::BTreeMap;

/// The Rust tools binary (repo-relative; foxbuild resolves it against the repo root).
pub const FOX_EXE: &str = "work/rust/target/release/fox.exe";

#[derive(Debug, Clone)]
pub struct GenError(pub String);

impl std::fmt::Display for GenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for GenError {}

pub type Result<T> = std::result::Result<T, GenError>;

fn err<T>(s: impl Into<String>) -> Result<T> {
    Err(GenError(s.into()))
}

/// Which implementation a library stage uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Impl {
    Legacy,
    Rs,
}

/// A library instance as written in the spec.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    #[serde(rename = "use")]
    pub use_: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub params: toml::Table,
    /// replaces the derived edges
    #[serde(default)]
    pub after: Option<Vec<String>>,
    #[serde(default)]
    pub after_extra: Vec<String>,
    /// force an implementation ("legacy" | "rs")
    #[serde(default, rename = "impl")]
    pub impl_: Option<String>,
    // foxbuild stage keys the instance may override
    #[serde(default)]
    pub outputs: Option<Vec<String>>,
    #[serde(default)]
    pub locks: Option<Vec<String>>,
    #[serde(default)]
    pub mem_gb: Option<f64>,
    #[serde(default)]
    pub gpu: Option<bool>,
    #[serde(default)]
    pub default: Option<bool>,
    #[serde(default)]
    pub deterministic: Option<bool>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub verify: Option<bool>,
    #[serde(default)]
    pub require_output: Option<String>,
    /// merged over the kind's env
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub soft_inputs: Option<Vec<String>>,
    #[serde(default)]
    pub rerun_with: Option<Vec<String>>,
    #[serde(default)]
    pub rust_tools: Option<bool>,
}

/// One stage before edges: what a kind emits.
#[derive(Debug, Clone)]
pub struct Emitted {
    pub stage: Stage,
    /// dataset ids it reads (hard: derived edges) / writes (it is their producer)
    pub reads: Vec<String>,
    pub writes: Vec<String>,
}

/// One generated stage plus where it came from (reports, `fox project graph --explain`).
#[derive(Debug, Clone)]
pub struct Origin {
    pub stage: String,
    /// "custom" or the library kind
    pub kind: String,
    pub impl_: Option<Impl>,
    pub derived_after: bool,
}

/// The generated graph and its provenance.
pub struct Generated {
    pub graph: Graph,
    pub origins: Vec<Origin>,
}

struct PendingStage {
    emitted: Emitted,
    origin: Origin,
    after: Option<Vec<String>>,
    after_extra: Vec<String>,
}

/// Generate the build graph of a project.
pub fn generate(spec: &Spec) -> Result<Generated> {
    let f = spec.file();
    let legacy_ok = spec.compat().is_some();
    let mut stages = Vec::<PendingStage>::new();
    for (i, t) in f.stage.iter().enumerate() {
        let what = t
            .get("name")
            .and_then(|n| n.as_str())
            .or_else(|| t.get("use").and_then(|u| u.as_str()))
            .map(String::from)
            .unwrap_or_else(|| format!("#{i}"));
        if t.contains_key("use") {
            let inst: Instance = toml::Value::Table(t.clone())
                .try_into()
                .map_err(|e: toml::de::Error| GenError(format!("stage {what}: {e}")))?;
            let ctx = kinds::Ctx { spec };
            let im = choose_impl(&inst, legacy_ok, &what)?;
            let emitted = kinds::emit(&ctx, &inst.use_, &inst.params, im)
                .map_err(|e| GenError(format!("stage {what} (use = {:?}): {e}", inst.use_)))?;
            let Some(mut emitted) = emitted else {
                return err(format!(
                    "stage {what}: kind {:?} has no {} implementation{}",
                    inst.use_,
                    if im == Impl::Rs { "Rust" } else { "legacy" },
                    if legacy_ok {
                        ""
                    } else {
                        " (legacy Python implementations are FLYK-only)"
                    }
                ));
            };
            apply_overrides(&mut emitted.stage, &inst);
            // Output declarations describe the files the command actually writes; changing this metadata must
            // not silently relocate a dataset contract. A relocation belongs in [datasets], which stages read.
            for w in &emitted.writes {
                let dataset = spec.datasets_untraced().get(w).expect("library dataset id");
                let path = &dataset.rel;
                // A directory dataset is a publication root. Its generated children can be declared separately
                // so input plans in the same root are still traced as inputs (not swallowed as outputs).
                if !emitted.stage.outputs.iter().any(|o| {
                    covers(o, path)
                        || (dataset.kind == foxpipe::project::datasets::Kind::Dir
                            && covers(path, o))
                }) {
                    return err(format!(
                        "stage {}: outputs omit dataset {w} ({path}); relocate it in [datasets]",
                        emitted.stage.name
                    ));
                }
            }
            let origin = Origin {
                stage: emitted.stage.name.clone(),
                kind: inst.use_.clone(),
                impl_: Some(im),
                derived_after: inst.after.is_none(),
            };
            stages.push(PendingStage {
                emitted,
                origin,
                after: inst.after.clone(),
                after_extra: inst.after_extra.clone(),
            });
        } else {
            let st: Stage = toml::Value::Table(t.clone())
                .try_into()
                .map_err(|e: toml::de::Error| GenError(format!("custom stage {what}: {e}")))?;
            let after = st.after.clone();
            let origin = Origin {
                stage: st.name.clone(),
                kind: "custom".into(),
                impl_: None,
                derived_after: false,
            };
            stages.push(PendingStage {
                emitted: Emitted {
                    stage: st,
                    reads: vec![],
                    writes: vec![],
                },
                origin,
                after: Some(after),
                after_extra: vec![],
            });
        }
    }
    // Dataset IDs may alias one path. Match the path (case-insensitive, as foxbuild does), so aliases cannot hide
    // either a writer conflict or a dependency. Directory/child contracts remain distinct: a pack and its
    // separately authored child dataset can intentionally share a directory with explicit ordering.
    let mut producer: BTreeMap<String, (String, String)> = BTreeMap::new();
    for pending in &stages {
        let em = &pending.emitted;
        for output in &em.stage.outputs {
            foxpipe::project::validate::repo_rel_ok(output, &["work/", "projects/", "build/"])
                .map_err(|e| GenError(format!("stage {}: outputs: {e}", em.stage.name)))?;
        }
        for w in &em.writes {
            let path = spec.datasets_untraced().rel(w);
            if let Some(d) = spec
                .datasets_untraced()
                .iter()
                .find(|d| d.has(foxpipe::project::datasets::flag::STATIC) && covers(&d.rel, path))
            {
                return err(format!(
                    "stage {}: dataset {w} ({path}) lies in read-only static dataset {}",
                    em.stage.name, d.id
                ));
            }
            if let Some((id, o)) =
                producer.insert(path.to_lowercase(), (w.clone(), em.stage.name.clone()))
                && o != em.stage.name
            {
                return err(format!(
                    "dataset {w} (alias of {id}, {path}) is written by both {o} and {}",
                    em.stage.name
                ));
            }
        }
    }
    let mut out_stages = vec![];
    let mut origins = vec![];
    for PendingStage {
        emitted: mut em,
        origin,
        after,
        after_extra: extra,
    } in stages
    {
        let mut a = match after {
            Some(a) => a,
            None => {
                let mut d = vec![];
                for r in &em.reads {
                    if let Some((_, p)) =
                        producer.get(&spec.datasets_untraced().rel(r).to_lowercase())
                        && p != &em.stage.name
                        && !d.contains(p)
                    {
                        d.push(p.clone());
                    }
                }
                d
            }
        };
        for x in extra {
            if !a.contains(&x) {
                a.push(x);
            }
        }
        em.stage.after = a;
        out_stages.push(em.stage);
        origins.push(origin);
    }
    let settings = settings(spec)?;
    let pinned = pinned(spec)?;
    let text = graph_toml_parts(&settings, &pinned, &out_stages)?;
    let graph = Graph::from_toml_str(&text)
        .map_err(|e| GenError(format!("generated graph rejected by foxbuild: {e:#}")))?;
    Ok(Generated { graph, origins })
}

/// Exact path or a child component; never a sibling with the same prefix.
fn covers(root: &str, path: &str) -> bool {
    let root = root.to_lowercase();
    let path = path.to_lowercase();
    path == root
        || path
            .strip_prefix(&root)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn choose_impl(inst: &Instance, legacy_ok: bool, what: &str) -> Result<Impl> {
    match inst.impl_.as_deref() {
        None => Ok(if legacy_ok { Impl::Legacy } else { Impl::Rs }),
        Some("rs") => Ok(Impl::Rs),
        Some("legacy") if legacy_ok => Ok(Impl::Legacy),
        Some("legacy") => err(format!(
            "stage {what}: impl = \"legacy\" needs [stages] compat (FLYK only)"
        )),
        Some(x) => err(format!("stage {what}: impl = {x:?}: legacy | rs")),
    }
}

fn apply_overrides(s: &mut Stage, i: &Instance) {
    if let Some(n) = &i.name {
        s.name = n.clone();
    }
    if let Some(v) = &i.outputs {
        s.outputs = v.clone();
    }
    if let Some(v) = &i.locks {
        s.locks = v.clone();
    }
    if let Some(v) = i.mem_gb {
        s.mem_gb = v;
    }
    if let Some(v) = i.gpu {
        s.gpu = v;
    }
    if let Some(v) = i.default {
        s.default = v;
    }
    if let Some(v) = i.deterministic {
        s.deterministic = v;
    }
    if let Some(v) = &i.owner {
        s.owner = v.clone();
    }
    if let Some(v) = i.verify {
        s.verify = v;
    }
    if let Some(v) = &i.require_output {
        s.require_output = Some(v.clone());
    }
    for (k, v) in &i.env {
        s.env.insert(k.clone(), v.clone());
    }
    if let Some(v) = &i.soft_inputs {
        s.soft_inputs = v.clone();
    }
    if let Some(v) = &i.rerun_with {
        s.rerun_with = v.clone();
    }
    if let Some(v) = i.rust_tools {
        s.rust_tools = v;
    }
}

/// [settings] from [build] (+ [build.machine]); unset keys take foxbuild's own defaults
fn settings(spec: &Spec) -> Result<toml::Table> {
    let b = &spec.file().build;
    let mut t = toml::Table::new();
    let s = |v: &str| toml::Value::String(v.to_string());
    let arr = |v: &[String]| {
        toml::Value::Array(v.iter().map(|x| toml::Value::String(x.clone())).collect())
    };
    t.insert(
        "log_dir".into(),
        s(&b.log_dir
            .clone()
            .unwrap_or_else(|| format!("work/build/{}", spec.code()))),
    );
    t.insert("temp_dir".into(), s(&b.temp_dir));
    let m = &b.machine;
    if let Some(v) = m.mem_budget_gb {
        t.insert("mem_budget_gb".into(), toml::Value::Float(v));
    }
    if let Some(v) = m.mem_reserve_gb {
        t.insert("mem_reserve_gb".into(), toml::Value::Float(v));
    }
    if let Some(v) = m.max_jobs {
        t.insert("max_jobs".into(), toml::Value::Integer(v as i64));
    }
    if let Some(v) = &m.games {
        t.insert("games".into(), arr(v));
    }
    if let Some(v) = m.game_max_jobs {
        t.insert("game_max_jobs".into(), toml::Value::Integer(v as i64));
    }
    t.insert(
        "static".into(),
        arr(&b
            .static_
            .clone()
            .unwrap_or_else(foxpipe::project::default_static)),
    );
    t.insert(
        "ignore".into(),
        arr(&b
            .ignore
            .clone()
            .unwrap_or_else(foxpipe::project::default_ignore)),
    );
    t.insert("unset_env".into(), arr(&b.unset_env));
    let env: toml::Table = b.env.iter().map(|(k, v)| (k.clone(), s(v))).collect();
    t.insert("env".into(), toml::Value::Table(env));
    Ok(t)
}

fn pinned(spec: &Spec) -> Result<Vec<toml::Table>> {
    let mut out = vec![];
    for p in &spec.file().build.pinned {
        let path = match (&p.dataset, &p.path) {
            (Some(d), _) => spec
                .datasets_untraced()
                .get(d)
                .ok_or_else(|| GenError(format!("build.pinned: unknown dataset {d}")))?
                .rel
                .clone(),
            (None, Some(p)) => p.clone(),
            (None, None) => return err("build.pinned: dataset or path"),
        };
        let mut t = toml::Table::new();
        t.insert("path".into(), toml::Value::String(path));
        t.insert("owner".into(), toml::Value::String(p.owner.clone()));
        out.push(t);
    }
    Ok(out)
}

fn graph_toml_parts(
    settings: &toml::Table,
    pinned: &[toml::Table],
    stages: &[Stage],
) -> Result<String> {
    let mut root = toml::Table::new();
    root.insert("settings".into(), toml::Value::Table(settings.clone()));
    root.insert(
        "pinned".into(),
        toml::Value::Array(pinned.iter().cloned().map(toml::Value::Table).collect()),
    );
    let st: Vec<toml::Value> = stages.iter().map(stage_table).collect::<Result<_>>()?;
    root.insert("stage".into(), toml::Value::Array(st));
    toml::to_string(&root).map_err(|e| GenError(format!("serialising the graph: {e}")))
}

/// a foxbuild Stage as a TOML table with only the keys that differ from foxbuild's defaults (readable output)
pub fn stage_table(s: &Stage) -> Result<toml::Value> {
    let full: toml::Value =
        toml::Value::try_from(s).map_err(|e| GenError(format!("stage {}: {e}", s.name)))?;
    let dflt: Stage =
        toml::from_str("name = \"x\"\ncmd = [\"x\"]\n").map_err(|e| GenError(e.to_string()))?;
    let dflt: toml::Value = toml::Value::try_from(&dflt).map_err(|e| GenError(e.to_string()))?;
    let (toml::Value::Table(full), toml::Value::Table(dflt)) = (full, dflt) else {
        return err("stage: not a table");
    };
    let mut out = toml::Table::new();
    // foxbuild key order of the graph file (name, cmd first)
    for k in [
        "name",
        "cmd",
        "cmd_rs",
        "rs_proven",
        "rust_tools",
        "env",
        "after",
        "rerun_with",
        "soft_inputs",
        "outputs",
        "locks",
        "mem_gb",
        "gpu",
        "default",
        "deterministic",
        "require_output",
        "verify",
        "owner",
    ] {
        if let Some(v) = full.get(k)
            && (k == "name" || k == "cmd" || dflt.get(k) != Some(v))
        {
            out.insert(k.into(), v.clone());
        }
    }
    for (k, v) in &full {
        if !out.contains_key(k) && dflt.get(k) != Some(v) {
            out.insert(k.clone(), v.clone());
        }
    }
    Ok(toml::Value::Table(out))
}

/// The graph as a TOML file (fox project graph -o): a GENERATED header, then the graph.
pub fn graph_toml(spec: &Spec, g: &Graph) -> Result<String> {
    graph_toml_from_origin(spec.rel(), g)
}

pub(crate) fn graph_toml_from_origin(origin: &str, g: &Graph) -> Result<String> {
    let settings = toml::Value::try_from(&g.settings).map_err(|e| GenError(e.to_string()))?;
    let toml::Value::Table(settings) = settings else {
        return err("settings: not a table");
    };
    let pinned: Vec<toml::Table> = g
        .pinned
        .iter()
        .map(|p| {
            let mut t = toml::Table::new();
            t.insert("path".into(), toml::Value::String(p.path.clone()));
            t.insert("owner".into(), toml::Value::String(p.owner.clone()));
            t
        })
        .collect();
    let body = graph_toml_parts(&settings, &pinned, &g.stage)?;
    Ok(format!(
        "# GENERATED from {} by foxproject (fox project graph). Edit the project spec, not this file.\n\n{body}",
        origin
    ))
}

/// A field-by-field comparison of two graphs: every difference, one line each (empty = identical).
/// Stage order, settings, pinned inputs and every stage key are compared exactly; `after` also as a set (so a pure
/// reordering is reported as such).
pub fn diff_graphs(a: &Graph, b: &Graph) -> Vec<String> {
    let mut out = vec![];
    let (sa, sb) = (j(&a.settings), j(&b.settings));
    if sa != sb {
        out.push(format!("settings differ:\n  {sa}\n  {sb}"));
    }
    if j(&a.pinned) != j(&b.pinned) {
        out.push(format!(
            "pinned differ:\n  {}\n  {}",
            j(&a.pinned),
            j(&b.pinned)
        ));
    }
    let na: Vec<&str> = a.stage.iter().map(|s| s.name.as_str()).collect();
    let nb: Vec<&str> = b.stage.iter().map(|s| s.name.as_str()).collect();
    if na != nb {
        out.push(format!("stage list / order differs:\n  {na:?}\n  {nb:?}"));
    }
    for x in &a.stage {
        let Some(y) = b.stage.iter().find(|s| s.name == x.name) else {
            continue;
        };
        let (vx, vy) = (j(x), j(y));
        if let (serde_json::Value::Object(mx), serde_json::Value::Object(my)) = (&vx, &vy) {
            for (k, v) in mx {
                if my.get(k) != Some(v) {
                    let note = if k == "after" {
                        let mut s1 = x.after.clone();
                        let mut s2 = y.after.clone();
                        s1.sort();
                        s2.sort();
                        if s1 == s2 {
                            " (same set, different order)"
                        } else {
                            ""
                        }
                    } else {
                        ""
                    };
                    out.push(format!(
                        "stage {}: {k}{note}: {} != {}",
                        x.name,
                        v,
                        my.get(k).cloned().unwrap_or_default()
                    ));
                }
            }
        }
    }
    out
}

fn j<T: serde::Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or_default()
}
