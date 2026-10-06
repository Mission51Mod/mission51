//! Native mod installation, verification and packaging commands.
use crate::{arg_value, die, exit_traced};
use std::path::{Path, PathBuf};

/// fox mod pack <staging folder> <out.mgsv> [--level N]: the .mgsv MakeBite would build, without MakeBite
fn mod_pack(a: &[String]) -> ! {
    let stage = Path::new(
        a.get(1)
            .unwrap_or_else(|| die("mod pack <staging folder> <out.mgsv> [--level 0-9]")),
    );
    let out = Path::new(a.get(2).unwrap_or_else(|| die("missing <out.mgsv>")));
    let level: u32 = arg_value(a, "--level")
        .map(|v| v.parse().unwrap_or_else(|_| die("--level 0-9")))
        .unwrap_or(6);
    if level > 9 {
        die("compression level must be between 0 and 9");
    }
    let t = std::time::Instant::now();
    match foxinstall::mgsvpack::write_mgsv(stage, out, level) {
        Ok(st) => {
            for p in &st.inputs {
                foxpipe::trace::read(p);
            }
            foxpipe::trace::write(out);
            println!(
                "built {} ({} files, {:.1} MB -> {:.1} MB) in {:.1} s",
                out.display(),
                st.files,
                st.bytes_in as f64 / 1048576.0,
                st.bytes_out as f64 / 1048576.0,
                t.elapsed().as_secs_f64()
            );
            exit_traced(0)
        }
        Err(e) => {
            eprintln!("fox: {e}");
            std::process::exit(2)
        }
    }
}

pub fn run(a: &[String], profile_path: Option<&Path>) {
    if a.first().map(|s| s.as_str()) == Some("pack") {
        mod_pack(a);
    }
    let game = arg_value(a, "--game").unwrap_or_else(|| die("--game DIR is required"));
    let src = arg_value(a, "--source-root");
    if matches!(
        a.first().map(String::as_str),
        Some("setup" | "install" | "uninstall" | "layout")
    ) && let Some(profile) = profile_path
    {
        let profile = profile.canonicalize().unwrap_or_else(|error| die(error));
        let target = crate::asset_paths::resolved_output(Path::new(&game))
            .unwrap_or_else(|error| die(error));
        if profile.starts_with(target) {
            die(
                "move the selected runtime profile outside the target game folder before modifying it",
            );
        }
    }
    let mut g = foxinstall::Game::new(Path::new(&game), src.as_deref().map(Path::new));
    if a.first().map(String::as_str) != Some("list") || profile_path.is_some() {
        let runtime = crate::runtime::RuntimeAccess::load(
            profile_path,
            Some(Path::new(src.as_deref().unwrap_or(&game))),
        )
        .unwrap_or_else(|error| die(error));
        if let Some(profile) = &runtime.profile {
            g.set_profile(profile).unwrap_or_else(|error| die(error));
        }
    }
    if let Some(v) = arg_value(a, "--vanilla-dirs") {
        g.vanilla_dirs = v
            .split(';')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
    }
    if a.iter().any(|x| x == "--progress") {
        // phase changes, 10 % steps and the final report (the same reports Fox Studio's progress bar shows)
        let t0 = std::time::Instant::now();
        let mut last: Option<(String, f32)> = None;
        g.set_progress(move |p| {
            let show = match &last {
                None => true,
                Some((ph, f)) => p.done || *ph != p.phase || p.overall - f >= 0.1,
            };
            if show {
                eprintln!(
                    "progress {:5.1} % {:8.2} s {}",
                    p.overall * 100.0,
                    t0.elapsed().as_secs_f64(),
                    p.phase
                );
                last = Some((p.phase.clone(), p.overall));
            }
        });
    }
    let force = a.iter().any(|x| x == "--force");
    let r = match a.first().map(|s| s.as_str()) {
        Some("setup") if a.iter().any(|x| x == "--snakebite") => {
            g.setup_snakebite(a.iter().any(|x| x == "--check"))
        }
        Some("setup") => g.setup().map(|_| true),
        Some("install") => {
            let p = Path::new(
                a.get(1)
                    .unwrap_or_else(|| die("missing <pkg.mgsv | folder>")),
            );
            g.install_opts(p, force, a.iter().any(|x| x == "--replace"))
                .map(|_| true)
        }
        Some("uninstall") => g
            .uninstall(a.get(1).unwrap_or_else(|| die("missing <name>")))
            .map(|_| true),
        Some("verify") => g.verify(),
        Some("layout") => g
            .set_layout(a.get(1).unwrap_or_else(|| die("layout gzstool|inplace")))
            .map(|_| true),
        Some("list") => g.load_manifest().map(|m| {
            for x in &m.mods {
                println!(
                    "{} {} ({} files, {} merged packs, {} loose) installed {}",
                    x.name,
                    x.version,
                    x.files.len(),
                    x.packs.len(),
                    x.loose.len(),
                    x.installed
                );
            }
            true
        }),
        _ => die("mod: setup | install | uninstall | list | verify | layout | pack"),
    };
    match r {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(e) => die(e),
    }
}
