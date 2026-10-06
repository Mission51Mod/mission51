//! Build dispatch keeps project publication inside the scheduler's build lease.
use std::path::{Path, PathBuf};

pub fn run(arguments: &[String]) -> Result<i32, String> {
    if !arguments.iter().any(|argument| argument == "--project") {
        return Ok(foxbuild::main_with(&legacy_arguments(arguments)));
    }

    let mut forwarded = arguments.to_vec();
    let project = take_option(&mut forwarded, "--project")?.ok_or("--project needs a value")?;
    if forwarded.iter().any(|argument| argument == "--config") {
        return Err("--project and --config select different build sources; choose one".into());
    }
    let root = take_option(&mut forwarded, "--root")?
        .map(PathBuf::from)
        .unwrap_or_else(foxpipe::paths::repo_root);
    let manifest = take_option(&mut forwarded, "--bundled-tools")?;
    if forwarded
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        return Ok(foxbuild::main_with(&["--help".into()]));
    }
    let manifest =
        manifest.ok_or("--project requires --bundled-tools with a validated native bundle")?;
    let root = root
        .canonicalize()
        .map_err(|error| format!("Project root {}: {error}", root.display()))?;
    if !root.is_dir() {
        return Err(format!(
            "Project root {} is not a directory",
            root.display()
        ));
    }
    let manifest = resolve(&root, Path::new(&manifest));
    let bundle = foxbuild::bundle::BundledTools::load_compiled(&manifest)
        .map_err(|error| format!("Native bundle: {error:#}"))?;
    let executable = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .map_err(|error| format!("Running native tool: {error}"))?;
    let expected = bundle
        .resolve_program("fox")
        .map_err(|error| format!("Native bundle: {error:#}"))?;
    if executable != expected {
        return Err("Running fox executable does not match the selected native bundle".into());
    }

    let plan = foxproject::plan_build_in(&root, Path::new(&project))?.with_tool(expected)?;
    let prepared = plan.prepared_graph()?;
    let mut scheduler = vec![
        "--root".into(),
        plan.repo_root().to_string_lossy().into_owned(),
        "--config".into(),
        plan.graph_path().to_string_lossy().into_owned(),
        "--python".into(),
        default_python(),
        "--bundled-tools".into(),
        manifest.to_string_lossy().into_owned(),
    ];
    scheduler.extend(forwarded);
    Ok(foxbuild::main_with_prepared(&scheduler, prepared, || {
        bundle
            .verify_current()
            .map_err(|error| format!("Native bundle: {error:#}"))?;
        plan.materialize().map(|_| ())
    }))
}

fn legacy_arguments(arguments: &[String]) -> Vec<String> {
    let root = foxpipe::paths::repo_root();
    let mut scheduler = vec![
        "--root".into(),
        root.to_string_lossy().into_owned(),
        "--config".into(),
        root.join("tools/build/flyk_stages.toml")
            .to_string_lossy()
            .into_owned(),
        "--python".into(),
        default_python(),
    ];
    scheduler.extend_from_slice(arguments);
    scheduler
}

fn default_python() -> String {
    std::env::var("FOX_PYTHON").unwrap_or_else(|_| "python".into())
}

fn resolve(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    }
}

/// Consume one frontend option while retaining scheduler options in their original order.
fn take_option(arguments: &mut Vec<String>, option: &str) -> Result<Option<String>, String> {
    let positions: Vec<_> = arguments
        .iter()
        .enumerate()
        .filter_map(|(index, argument)| (argument == option).then_some(index))
        .collect();
    match positions.as_slice() {
        [] => Ok(None),
        [index] => {
            let value = arguments
                .get(index + 1)
                .filter(|value| !value.is_empty() && !value.starts_with("--"))
                .ok_or_else(|| format!("{option} needs a value"))?
                .clone();
            arguments.drain(*index..=*index + 1);
            Ok(Some(value))
        }
        _ => Err(format!("{option} must be specified once")),
    }
}
