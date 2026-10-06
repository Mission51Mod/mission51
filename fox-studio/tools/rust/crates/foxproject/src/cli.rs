//! `fox project <cmd>` and the `fox build --project` preparation (M3 T4; switched on only on the coordinator's go).
//!
//!   fox project check    [SPEC] [--json] [--allow-unproven]   validate; every problem at once (rc 1 on errors)
//!   fox project show     [SPEC] [--json]                      the resolved spec (sections, datasets, loc)
//!   fox project datasets [SPEC] [--json]                      every dataset id -> repo-relative path (+ flags)
//!   fox project graph    [SPEC] [-o FILE] [--check GRAPH.toml]  generate the build graph; --check: diff against a
//!                                                             graph file (fields + command fingerprints), rc 1 on
//!                                                             any difference
//!   fox project resolve  [SPEC]                               write the spec facets (<spec_cache>/<section>.json)
//!   fox project new CODE [--id N] [--force]                   projects/<code>/project.toml (empty template)
//!
//! SPEC defaults to FOX_PROJECT, else projects/flyk/project.toml.
use crate::{diff_graphs, generate, graph_toml};
use foxpipe::project::{LoadOptions, Spec, datasets::flag};
use serde_json::json;
use std::path::{Path, PathBuf};

fn spec_arg(args: &[String]) -> PathBuf {
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
            continue;
        }
        if matches!(a.as_str(), "-o" | "--check" | "--id") {
            skip = true;
            continue;
        }
        if !a.starts_with('-') {
            let p = PathBuf::from(a);
            let p = if p.is_dir() {
                p.join("project.toml")
            } else {
                p
            };
            return if p.is_absolute() {
                p
            } else {
                foxpipe::paths::repo_root().join(p)
            };
        }
    }
    foxpipe::project::current_path()
}

fn opt(args: &[String], k: &str) -> Option<String> {
    args.iter()
        .position(|a| a == k)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn has(args: &[String], k: &str) -> bool {
    args.iter().any(|a| a == k)
}

fn load(args: &[String]) -> Result<Spec, String> {
    let opts = LoadOptions {
        allow_unproven: has(args, "--allow-unproven"),
    };
    Spec::load_with(spec_arg(args), &opts).map_err(|e| e.0)
}

const HELP: &str = "fox project <command> [options]

Commands:
  check [SPEC] [--json] [--allow-unproven]  Validate a location spec and its graph
  show [SPEC] [--json]                     Show resolved fields and datasets
  datasets [SPEC] [--json]                 List dataset paths and flags
  graph [SPEC] [-o FILE] [--check GRAPH]    Generate or compare a build graph
  resolve [SPEC]                          Publish changed spec facets
  new CODE [--id N] [--force]              Create projects/<code>/project.toml

SPEC may be a file or project directory. It defaults to FOX_PROJECT, then projects/flyk/project.toml.
Pass a free location id with --id when creating a project. Existing projects are preserved unless --force is explicit.
New-project templates describe the internal location pipeline. The public editor distribution validates specs and packages existing assets; terrain, navigation, vegetation and mission generation are unavailable.";

/// `fox project ...`; returns the process exit code
pub fn cli(args: &[String]) -> i32 {
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("");
    if matches!(cmd, "" | "help" | "-h" | "--help") {
        println!("{HELP}");
        return 0;
    }
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    let r = match cmd {
        "check" => check(rest),
        "show" => show(rest),
        "datasets" => datasets(rest),
        "graph" => graph(rest),
        "resolve" => resolve(rest),
        "new" => new(rest),
        _ => {
            eprintln!(
                "fox project check | show | datasets | graph | resolve | new  [SPEC] (see foxproject::cli)"
            );
            return 2;
        }
    };
    match r {
        Ok(rc) => rc,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn check(args: &[String]) -> Result<i32, String> {
    let json_out = has(args, "--json");
    let opts = LoadOptions {
        allow_unproven: has(args, "--allow-unproven"),
    };
    let path = spec_arg(args);
    let (ok, v) = match Spec::load_with(&path, &opts) {
        Err(e) => (
            false,
            json!({"ok": false, "spec": path.display().to_string(), "issues": [], "error": e.0}),
        ),
        Ok(s) => match generate(&s) {
            Ok(g) => (
                true,
                json!({"ok": true, "spec": s.rel(), "issues": s.issues(), "error": null,
                                   "stages": g.graph.stage.len()}),
            ),
            Err(e) => (
                false,
                json!({"ok": false, "spec": s.rel(), "issues": s.issues(), "error": e.0}),
            ),
        },
    };
    if json_out {
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    } else {
        for i in v["issues"].as_array().into_iter().flatten() {
            println!(
                "{}: {}: {}",
                i["severity"].as_str().unwrap_or(""),
                i["field"].as_str().unwrap_or(""),
                i["msg"].as_str().unwrap_or("")
            );
        }
        match v["error"].as_str() {
            Some(e) => println!("{e}"),
            None => println!(
                "{}: OK ({} stages)",
                v["spec"].as_str().unwrap_or(""),
                v["stages"]
            ),
        }
    }
    Ok(if ok { 0 } else { 1 })
}

fn show(args: &[String]) -> Result<i32, String> {
    let s = load(args)?;
    let mut v = s.resolved_json().clone();
    if let serde_json::Value::Object(m) = &mut v {
        m.insert(
            "spec".into(),
            json!({"path": s.path().to_string_lossy(), "rel": s.rel(), "code": s.code(),
                                       "name": s.name(), "compat": s.compat()}),
        );
        m.insert(
            "issues".into(),
            serde_json::to_value(s.issues()).unwrap_or_default(),
        );
    }
    if has(args, "--json") {
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    } else {
        let l = s.loc();
        println!(
            "{}  ({})  code {} id {}  grid {} x {} m  blocks {}..{}  centre {}",
            s.name(),
            s.rel(),
            l.code,
            l.id,
            l.grid,
            l.d,
            l.first_block,
            l.first_block + l.nblk() - 1,
            l.center_index()
        );
        let work = s
            .file()
            .paths
            .work
            .clone()
            .unwrap_or_else(|| format!("work/projects/{}", s.code()));
        println!(
            "work {work}  spec cache {}  stage entries {}",
            s.dataset_rel("spec_cache"),
            s.file().stage.len()
        );
        for i in s.issues() {
            println!("{i}");
        }
    }
    Ok(0)
}

fn flag_names(f: u16) -> Vec<&'static str> {
    [
        (flag::EDITABLE, "editable"),
        (flag::SNAPSHOT, "snapshot"),
        (flag::PROTECT, "protect"),
        (flag::STATIC, "static"),
        (flag::NO_REWRITE, "no_rewrite"),
        (flag::PINNED, "pinned"),
        (flag::OPTIONAL, "optional"),
    ]
    .iter()
    .filter(|(b, _)| f & b != 0)
    .map(|(_, n)| *n)
    .collect()
}

fn datasets(args: &[String]) -> Result<i32, String> {
    let s = load(args)?;
    if has(args, "--json") {
        let v: Vec<_> = s
            .datasets()
            .iter()
            .map(|d| {
                json!({"id": d.id, "rel": d.rel, "kind": d.kind,
            "flags": flag_names(d.flags), "skip": d.skip, "overridden": d.overridden})
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    } else {
        for d in s.datasets().iter() {
            println!(
                "{:<26} {}{}  {}",
                d.id,
                d.rel,
                if d.overridden { "  (override)" } else { "" },
                flag_names(d.flags).join(",")
            );
        }
    }
    Ok(0)
}

fn graph(args: &[String]) -> Result<i32, String> {
    let s = load(args)?;
    let g = generate(&s).map_err(|e| e.0)?;
    if let Some(other) = opt(args, "--check") {
        let p = foxpipe::paths::repo_root().join(&other);
        let golden = foxbuild::config::Graph::load(&p).map_err(|e| format!("{other}: {e:#}"))?;
        let mut d = diff_graphs(&g.graph, &golden);
        for (a, b) in g.graph.stage.iter().zip(&golden.stage) {
            let id = "python";
            if a.name == b.name
                && foxbuild::command_fingerprint(a, id) != foxbuild::command_fingerprint(b, id)
            {
                d.push(format!("stage {}: command fingerprint differs", a.name));
            }
        }
        if d.is_empty() {
            println!(
                "{}: generated graph == {other} ({} stages, command fingerprints equal)",
                s.rel(),
                golden.stage.len()
            );
            return Ok(0);
        }
        for l in &d {
            println!("{l}");
        }
        println!(
            "{}: generated graph != {other} ({} differences)",
            s.rel(),
            d.len()
        );
        return Ok(1);
    }
    let text = graph_toml(&s, &g.graph).map_err(|e| e.0)?;
    match opt(args, "-o") {
        Some(o) => {
            let p = foxpipe::paths::repo_root().join(&o);
            std::fs::write(&p, &text).map_err(|e| format!("{o}: {e}"))?;
            println!("{} stages -> {o}", g.graph.stage.len());
        }
        None => print!("{text}"),
    }
    Ok(0)
}

fn resolve(args: &[String]) -> Result<i32, String> {
    let s = load(args)?;
    let changed = s.write_facets().map_err(|e| e.0)?;
    println!(
        "{}: facets in {} ({} changed{}{})",
        s.rel(),
        s.dataset_rel("spec_cache"),
        changed.len(),
        if changed.is_empty() { "" } else { ": " },
        changed.join(", ")
    );
    Ok(0)
}

fn new(args: &[String]) -> Result<i32, String> {
    let mut code = None;
    let mut id = None;
    let mut force = false;
    let mut args = args.iter();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--id" => {
                let value = args.next().ok_or("--id: expected a location id")?;
                id = Some(
                    value
                        .parse::<i64>()
                        .map_err(|error| format!("--id: {error}"))?,
                );
            }
            "--force" => force = true,
            value if value.starts_with('-') => {
                return Err(format!("unknown project creation option {value}"));
            }
            value if code.is_none() => code = Some(value),
            value => return Err(format!("unexpected project creation argument {value}")),
        }
    }
    let code = code.ok_or("fox project new CODE [--id N] [--force]")?;
    if !(3..=8).contains(&code.len())
        || !code.as_bytes()[0].is_ascii_lowercase()
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return Err(format!(
            "location.code {code:?}: expected [a-z][a-z0-9]{{2,7}}"
        ));
    }
    let dir = foxpipe::paths::repo(&format!("projects/{code}"));
    let p = dir.join("project.toml");
    if p.exists() && !force {
        return Err(format!("{} exists (--force to overwrite)", p.display()));
    }
    // An omitted id keeps the authoring placeholder, but must not hide other validation errors.
    let checked_id = id.unwrap_or_else(|| {
        foxpipe::project::validate::VANILLA_LOCATIONS
            .iter()
            .find(|entry| entry.0 == code)
            .map_or_else(foxpipe::project::validate::suggested_location_id, |entry| {
                entry.1
            })
    });
    let checked = new_project_text(code, checked_id);
    let spec =
        Spec::from_toml_str(&checked, &p, &LoadOptions::default()).map_err(|error| error.0)?;
    generate(&spec).map_err(|error| error.0)?;
    let text = new_project_text(code, id.unwrap_or(0));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(&p, text).map_err(|e| format!("{}: {e}", p.display()))?;
    println!(
        "{} written{}",
        foxpipe::paths::rel(&p),
        if id.is_none() {
            " (set [location] id: pick a free one with `fox project check`)"
        } else {
            ""
        }
    );
    Ok(0)
}

/// the empty-project template
pub fn new_project_text(code: &str, id: i64) -> String {
    format!(
        r#"# {up}: a new location project (docs/release/PROJECTS.md).
format = 1

[project]
name = "{code}"
title = "{up}"

[location]
code = "{code}"
id = {id}                        # a location id free in the vanilla game and the community (fox project check)
grid = 2048

[biome]
source = "mafr"

[terrain]
heights = {{ source = "generate" }}
materials = {{ painter = "rules" }}

[mod]
name = "{up} location"
version = "0.1.0"

# Internal pipeline library stages; these nav/m3 commands are unavailable in the public editor distribution.
# Rust implementations derive edges from the datasets they read and write.
[[stage]]
use = "nav.ground"

[[stage]]
use = "nav.sky"

[[stage]]
use = "package.terrain"

[[stage]]
use = "package.fox2"

[[stage]]
use = "package.packs"

[[stage]]
use = "package.mgsv"

[[stage]]
use = "package.verify"
"#,
        up = code.to_uppercase()
    )
}

/// `fox build --project SPEC`: generate the graph into <log_dir>/graph.generated.toml, write the spec facets and
/// export FOX_PROJECT (inherited by every stage; not fingerprinted). Returns the graph file for foxbuild's --config.
/// Gated: foxcli calls it only when `--project` is given (T4; default unchanged until the coordinator's go).
/// Single CLI main-thread compatibility adapter only; long-lived frontends use `plan_build` and a child process.
pub fn prepare_build(spec_path: &Path) -> Result<PathBuf, String> {
    let plan = crate::plan_build(spec_path)?;
    let written = plan.materialize()?;
    // SAFETY: called by foxcli's main thread before foxbuild starts any thread or child
    unsafe { std::env::set_var("FOX_PROJECT", plan.spec_path()) };
    Ok(written.graph_path)
}
