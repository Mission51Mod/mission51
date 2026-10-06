//! CLI parsing without side effects, including help and invalid-option paths.
use anyhow::{Context, Result, bail};
use std::path::PathBuf;

pub(crate) struct Opts {
    pub root: PathBuf,
    pub config: PathBuf,
    pub python: String,
    pub dry_run: bool,
    pub from: Vec<String>,
    pub only: Vec<String>,
    pub force: bool,
    pub jobs: Option<usize>,
    pub mem_gb: Option<f64>,
    pub list: bool,
    pub status: bool,
    pub graph: bool,
    pub help: bool,
    pub json: bool,
    pub bundled_tools: Option<PathBuf>,
    pub cancel_file: Option<PathBuf>,
    pub stop_on_error: bool,
    pub suspend_running: bool,
}

impl Opts {
    pub(crate) fn operation(&self) -> &'static str {
        if self.help {
            "help"
        } else if self.graph {
            "graph"
        } else if self.list {
            "list"
        } else if self.status {
            "status"
        } else if self.dry_run {
            "dry_run"
        } else {
            "build"
        }
    }
}

pub(crate) fn parse_args(args: &[String]) -> Result<Opts> {
    let cwd = std::env::current_dir()?;
    let mut args = args.iter();
    let mut opts = Opts {
        root: cwd.clone(),
        config: PathBuf::from("tools/build/flyk_stages.toml"),
        python: "python".into(),
        dry_run: false,
        from: vec![],
        only: vec![],
        force: false,
        jobs: None,
        mem_gb: None,
        list: false,
        status: false,
        graph: false,
        help: false,
        json: false,
        bundled_tools: None,
        cancel_file: None,
        stop_on_error: false,
        suspend_running: false,
    };
    let split = |value: &str| {
        value
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    while let Some(argument) = args.next() {
        let mut value = || {
            args.next()
                .filter(|value| !value.is_empty() && !value.starts_with("--"))
                .with_context(|| format!("{argument} needs a value"))
        };
        match argument.as_str() {
            "--root" => opts.root = PathBuf::from(value()?),
            "--config" => opts.config = PathBuf::from(value()?),
            "--python" => opts.python = value()?.clone(),
            "--dry-run" | "-n" => opts.dry_run = true,
            "--from" => opts.from.extend(split(value()?)),
            "--only" => opts.only.extend(split(value()?)),
            "--force" => opts.force = true,
            "--jobs" | "-j" => opts.jobs = Some(value()?.parse()?),
            "--mem-gb" => opts.mem_gb = Some(value()?.parse()?),
            "--list" => opts.list = true,
            "--status" => opts.status = true,
            "--graph" => opts.graph = true,
            "--stop-on-error" => opts.stop_on_error = true,
            "--suspend-running" => opts.suspend_running = true,
            "--json" => opts.json = true,
            "--bundled-tools" => opts.bundled_tools = Some(PathBuf::from(value()?)),
            "--cancel-file" => opts.cancel_file = Some(PathBuf::from(value()?)),
            "-h" | "--help" => opts.help = true,
            _ => bail!("unknown argument {argument} (--help)"),
        }
    }
    if opts.jobs == Some(0) {
        bail!("--jobs must be greater than zero");
    }
    if opts
        .mem_gb
        .is_some_and(|value| !value.is_finite() || value <= 0.0)
    {
        bail!("--mem-gb must be finite and greater than zero");
    }
    if opts.root.is_relative() {
        opts.root = cwd.join(&opts.root);
    }
    if opts.config.is_relative() {
        opts.config = opts.root.join(&opts.config);
    }
    for path in [&mut opts.bundled_tools, &mut opts.cancel_file]
        .into_iter()
        .flatten()
    {
        if path.is_relative() {
            *path = opts.root.join(&*path);
        }
    }
    Ok(opts)
}
