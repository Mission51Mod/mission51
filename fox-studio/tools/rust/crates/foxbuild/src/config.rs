//! The stage graph file (tools/build/flyk_stages.toml): settings, pinned inputs, stages.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

fn d_mem() -> f64 {
    2.0
}
fn d_true() -> bool {
    true
}
fn d_budget() -> f64 {
    24.0
}
fn d_reserve() -> f64 {
    6.0
}
fn d_jobs() -> usize {
    4
}
fn d_one() -> usize {
    1
}
fn d_log() -> String {
    "work/build".into()
}
fn d_tmp() -> String {
    "work/tmp".into()
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)] // a misspelt key is a graph error, never silently ignored
pub struct Settings {
    #[serde(default = "d_log")]
    pub log_dir: String,
    #[serde(default = "d_tmp")]
    pub temp_dir: String,
    #[serde(default = "d_budget")]
    pub mem_budget_gb: f64,
    #[serde(default = "d_reserve")]
    pub mem_reserve_gb: f64,
    #[serde(default = "d_jobs")]
    pub max_jobs: usize,
    #[serde(default)]
    pub games: Vec<String>,
    #[serde(default = "d_one")]
    pub game_max_jobs: usize,
    #[serde(default, rename = "static")]
    pub static_roots: Vec<String>,
    #[serde(default)]
    pub ignore: Vec<String>,
    /// variables removed from every stage's environment (debug switches that must not leak into a graph build)
    #[serde(default)]
    pub unset_env: Vec<String>,
    /// environment for every stage (stage `env` entries override)
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)] // a misspelt key is a graph error, never silently ignored
pub struct Pinned {
    pub path: String,
    #[serde(default)]
    pub owner: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)] // a misspelt key is a graph error, never silently ignored
pub struct Stage {
    pub name: String,
    pub cmd: Vec<String>,
    #[serde(default)]
    pub after: Vec<String>,
    #[serde(default)]
    pub outputs: Vec<String>,
    #[serde(default)]
    pub locks: Vec<String>,
    #[serde(default = "d_mem")]
    pub mem_gb: f64,
    #[serde(default)]
    pub gpu: bool,
    #[serde(default = "d_true")]
    pub default: bool,
    #[serde(default = "d_true")]
    pub deterministic: bool,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub verify: bool,
    /// regex the stage's log must match for success (for tools that exit 0 on failure)
    #[serde(default)]
    pub require_output: Option<String>,
    /// extra environment for this stage. Part of the command fingerprint (a stage variant like FLYK_LARGE=1 is its
    /// own build); [settings.env] is not.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// reads that never make this stage dirty and never create a learned edge (an intentional soft cycle: e.g. the
    /// previous run's tree placements for a canopy darkening). Patterns as in settings.ignore.
    #[serde(default)]
    pub soft_inputs: Vec<String>,
    /// stages whose every run makes this stage re-run (it rewrites their outputs, or depends on them in ways a trace
    /// cannot see). Implies `after`.
    #[serde(default)]
    pub rerun_with: Vec<String>,
    /// the Rust implementation (RUST_PORTING.md 3.3): the same stage with the same files. foxbuild runs it instead of
    /// `cmd` only once `rs_proven = true` (byte-identity proven, switched by the coordinator), and never when
    /// FOX_IMPL=py or FOX_IMPL_<STAGE>=py (stage name upper case, '.' -> '_') forces the Python reference.
    #[serde(default)]
    pub cmd_rs: Option<Vec<String>>,
    #[serde(default)]
    pub rs_proven: bool,
    /// the stage's Python routes work into the Rust binaries (fox.exe / fox-place.exe): it needs them built from the
    /// current sources (foxbuild checks / rebuilds them at start and refuses the stage when that fails)
    #[serde(default)]
    pub rust_tools: bool,
}

impl Stage {
    /// the Rust command runs (proven, not forced back to Python)
    pub fn uses_rs(&self) -> bool {
        self.rs_proven
            && self.cmd_rs.as_ref().is_some_and(|c| !c.is_empty())
            && !forced_py(&self.name)
    }
    /// needs fresh Rust tools: a proven cmd_rs, or rust_tools
    pub fn needs_rust(&self) -> bool {
        self.rust_tools || self.uses_rs()
    }
    /// the command foxbuild runs and fingerprints (unproven cmd_rs: `cmd`, so the fingerprint is the one of before)
    pub fn run_cmd(&self) -> &[String] {
        if self.uses_rs() {
            self.cmd_rs.as_deref().unwrap()
        } else {
            &self.cmd
        }
    }
}

/// FOX_IMPL=py (every stage) or FOX_IMPL_<STAGE>=py (one stage) forces the Python reference
pub fn forced_py(stage: &str) -> bool {
    let is_py = |k: &str| {
        std::env::var(k)
            .map(|v| v.eq_ignore_ascii_case("py"))
            .unwrap_or(false)
    };
    is_py("FOX_IMPL")
        || is_py(&format!(
            "FOX_IMPL_{}",
            stage.to_uppercase().replace(['.', '-'], "_")
        ))
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)] // a misspelt key is a graph error, never silently ignored
pub struct Graph {
    pub settings: Settings,
    #[serde(default)]
    pub pinned: Vec<Pinned>,
    #[serde(default)]
    pub stage: Vec<Stage>,
}

impl Graph {
    /// parse + validate a graph from TOML text (as `load` does for a file)
    pub fn from_toml_str(text: &str) -> Result<Graph> {
        let g: Graph = toml::from_str(text).context("parsing the stage graph")?;
        g.validate()?;
        Ok(g)
    }

    pub fn load(path: &std::path::Path) -> Result<Graph> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let g: Graph =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        g.validate()?;
        Ok(g)
    }

    pub fn validate(&self) -> Result<()> {
        if self.settings.max_jobs == 0 || self.settings.game_max_jobs == 0 {
            bail!("max_jobs and game_max_jobs must be greater than zero");
        }
        if !self.settings.mem_budget_gb.is_finite() || self.settings.mem_budget_gb <= 0.0 {
            bail!("mem_budget_gb must be finite and greater than zero");
        }
        if !self.settings.mem_reserve_gb.is_finite() || self.settings.mem_reserve_gb < 0.0 {
            bail!("mem_reserve_gb must be finite and nonnegative");
        }
        let mut names = BTreeSet::new();
        for s in &self.stage {
            if !s.mem_gb.is_finite() || s.mem_gb <= 0.0 {
                bail!(
                    "stage {}: mem_gb must be finite and greater than zero",
                    s.name
                );
            }
            if !names.insert(s.name.clone()) {
                bail!("duplicate stage name {}", s.name);
            }
            if let Some(r) = &s.require_output {
                regex::Regex::new(r)
                    .map_err(|e| anyhow::anyhow!("stage {}: bad require_output: {e}", s.name))?;
            }
            if s.cmd.is_empty() {
                bail!("stage {} has an empty cmd", s.name);
            }
            if s.cmd_rs.as_ref().is_some_and(|c| c.is_empty()) {
                bail!("stage {} has an empty cmd_rs", s.name);
            }
            if s.rs_proven && s.cmd_rs.is_none() {
                bail!("stage {}: rs_proven = true without a cmd_rs", s.name);
            }
        }
        for s in &self.stage {
            for a in s.after.iter().chain(&s.rerun_with) {
                if !names.contains(a) {
                    bail!("stage {}: after / rerun_with = unknown stage {}", s.name, a);
                }
            }
        }
        // declared graph must be acyclic
        let idx: BTreeMap<&str, usize> = self
            .stage
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.as_str(), i))
            .collect();
        let edges: Vec<Vec<usize>> = self
            .stage
            .iter()
            .map(|s| {
                s.after
                    .iter()
                    .chain(&s.rerun_with)
                    .map(|a| idx[a.as_str()])
                    .collect()
            })
            .collect();
        if crate::graph::topo_order(self.stage.len(), &edges).is_none() {
            bail!("the declared `after` edges contain a cycle");
        }
        Ok(())
    }

    pub fn index(&self, name: &str) -> Option<usize> {
        self.stage.iter().position(|s| s.name == name)
    }
}

#[cfg(test)]
mod cmd_rs_tests {
    use super::*;

    fn stage(extra: &str) -> Stage {
        toml::from_str(&format!(
            "name = \"t.cmdrs_x\"
cmd = [\"python\", \"a.py\"]
{extra}"
        ))
        .unwrap()
    }

    #[test]
    fn unproven_runs_python_and_keeps_its_command() {
        let s = stage(
            "cmd_rs = [\"work/rust/target/release/fox.exe\", \"x\"]
rs_proven = false",
        );
        assert!(!s.uses_rs());
        assert_eq!(s.run_cmd(), &["python".to_string(), "a.py".to_string()][..]);
        let plain = stage("");
        assert_eq!(plain.run_cmd(), s.run_cmd());
    }

    #[test]
    fn proven_runs_rust_unless_forced_back() {
        let s: Stage = toml::from_str(
            "name = \"t.cmdrs_y\"
cmd = [\"python\", \"a.py\"]
cmd_rs = [\"fox\", \"y\"]
rs_proven = true",
        )
        .unwrap();
        assert!(s.uses_rs());
        assert_eq!(s.run_cmd()[0], "fox");
        unsafe { std::env::set_var("FOX_IMPL_T_CMDRS_Y", "py") };
        assert!(!s.uses_rs());
        unsafe { std::env::remove_var("FOX_IMPL_T_CMDRS_Y") };
    }

    #[test]
    fn validation() {
        let mk = |extra: &str| -> std::result::Result<Graph, String> {
            let g: Graph = toml::from_str(&format!(
                "[settings]
[[stage]]
name = \"a\"
cmd = [\"x\"]
{extra}"
            ))
            .map_err(|e| e.to_string())?;
            g.validate().map(|_| g).map_err(|e| e.to_string())
        };
        assert!(mk("").is_ok());
        assert!(mk("rs_proven = true").is_err());
        assert!(mk("cmd_rs = []").is_err());
        assert!(mk("cmd_rs = [\"fox\"]").is_ok());
    }
    #[test]
    fn authored_graph_accepts_native_and_unproven_commands() {
        let graph = Graph::from_toml_str(
            r#"
[settings]
max_jobs = 2
unset_env = ["FIXTURE_DEBUG"]
[settings.env]
FIXTURE_MODE = "release"
[[pinned]]
path = "input.bin"
owner = "fixture"
[[stage]]
name = "copy"
cmd = ["fox", "copy", "input.bin", "output.bin"]
outputs = ["output.bin"]
rust_tools = true
verify = true
require_output = "PASS"
[[stage]]
name = "reference"
cmd = ["python", "reference.py"]
cmd_rs = ["fox", "reference"]
rs_proven = false
after = ["copy"]
rerun_with = ["copy"]
soft_inputs = ["previous/"]
locks = ["output"]
default = false
[stage.env]
FIXTURE_VARIANT = "reference"
"#,
        )
        .unwrap();
        assert_eq!(graph.stage.len(), 2);
        assert_eq!(graph.pinned[0].owner, "fixture");
        assert!(graph.stage[0].needs_rust());
        assert_eq!(graph.stage[1].run_cmd()[0], "python");
        assert!(!graph.stage[1].uses_rs());
        let roundtrip = Graph::from_toml_str(&toml::to_string(&graph).unwrap()).unwrap();
        assert_eq!(roundtrip.stage[1].env["FIXTURE_VARIANT"], "reference");
    }

    #[test]
    fn invalid_resource_limits_do_not_enter_the_scheduler() {
        for settings in [
            "max_jobs = 0",
            "game_max_jobs = 0",
            "mem_budget_gb = nan",
            "mem_budget_gb = 0.0",
            "mem_reserve_gb = -1.0",
        ] {
            assert!(
                Graph::from_toml_str(&format!("[settings]\n{settings}")).is_err(),
                "{settings}"
            );
        }
        assert!(
            Graph::from_toml_str(
                "[settings]\n[[stage]]\nname = 'bad'\ncmd = ['fox']\nmem_gb = nan"
            )
            .is_err()
        );
    }

    #[test]
    fn unknown_stage_key_is_an_error() {
        let r: std::result::Result<Graph, _> = toml::from_str(
            "[settings]
[[stage]]
name = \"a\"
cmd = [\"x\"]
cmd_rust = [\"y\"]
",
        );
        let e = r.unwrap_err().to_string();
        assert!(e.contains("cmd_rust"), "{e}");
    }
}
