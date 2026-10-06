//! Native command-line entry point for Fox Engine tools.
mod archives;
mod asset_paths;
mod build;
mod dict;
mod mods;
mod output;
mod pack;
mod runtime;
mod verification;

use foxcore::qar;

fn die(message: impl std::fmt::Display) -> ! {
    eprintln!("fox: {message}");
    std::process::exit(2)
}

fn arg_value(arguments: &[String], key: &str) -> Option<String> {
    arguments
        .iter()
        .position(|argument| argument == key)
        .and_then(|index| arguments.get(index + 1))
        .filter(|value| !value.is_empty() && !value.starts_with("--"))
        .cloned()
}

/// Finish a pipeline command's build trace before returning its exit status.
fn exit_traced(code: i32) -> ! {
    foxpipe::trace::finish();
    std::process::exit(code)
}

fn main() {
    let mut arguments: Vec<String> = std::env::args().skip(1).collect();
    let profile_path =
        runtime::take_profile_option(&mut arguments).unwrap_or_else(|error| die(error));
    match arguments.first().map(|s| s.as_str()) {
        Some("--version" | "-V") => println!("fox {}", env!("CARGO_PKG_VERSION")),
        Some("--build-info") => println!(
            "{}",
            serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "build_id": option_env!("FOX_BUNDLE_BUILD_ID"),
                "internal_pipeline": cfg!(feature = "internal-pipeline"),
            })
        ),
        Some("project") => exit_traced(foxproject::cli::cli(&arguments[1..])),
        Some("setup") => runtime::setup(&arguments[1..]).unwrap_or_else(|error| die(error)),
        Some("qar") => {
            let runtime = runtime::RuntimeAccess::load(profile_path.as_deref(), None)
                .unwrap_or_else(|error| die(error));
            archives::run(&arguments[1..], &runtime.archive, profile_path.as_deref());
        }
        Some("mod") => mods::run(&arguments[1..], profile_path.as_deref()),
        Some("build") => {
            std::process::exit(build::run(&arguments[1..]).unwrap_or_else(|error| die(error)));
        }
        // pipeline ports: each crate owns its subcommand (docs/release/RUST_PORTING.md)
        #[cfg(feature = "internal-pipeline")]
        Some("nav") => exit_traced(foxnav::cli(&arguments[1..])),
        #[cfg(feature = "internal-pipeline")]
        Some("terrain") => exit_traced(foxterrain::cli(&arguments[1..])),
        #[cfg(feature = "internal-pipeline")]
        Some("place") => exit_traced(foxplace::cli(&arguments[1..])),
        #[cfg(feature = "internal-pipeline")]
        Some("m3") => exit_traced(foxm3::cli(&arguments[1..])),
        #[cfg(feature = "internal-pipeline")]
        Some("mission") => exit_traced(foxmission::cli(&arguments[1..])),
        #[cfg(feature = "internal-pipeline")]
        Some("parallax") => exit_traced(foxparallax::cli(&arguments[1..])),
        #[cfg(not(feature = "internal-pipeline"))]
        Some("nav" | "terrain" | "place" | "m3" | "mission" | "parallax") => {
            die("this pipeline command is not included in the editor distribution")
        }
        Some("unpack") => pack::unpack(
            arguments
                .get(1)
                .unwrap_or_else(|| die("unpack FILE OUTDIR [--dict FILE]")),
            arguments.get(2).unwrap_or_else(|| die("missing OUTDIR")),
            arg_value(&arguments, "--dict").as_deref(),
        ),
        Some("pack") => pack::pack(
            profile_path.as_deref(),
            arguments
                .get(1)
                .unwrap_or_else(|| die("pack DEFINITION.json OUTFILE")),
            arguments.get(2).unwrap_or_else(|| die("missing OUTFILE")),
        ),
        Some("sb-check" | "roundtrip" | "sb-modentry" | "xml-roundtrip" | "npy-roundtrip") => {
            verification::run(&arguments, profile_path.as_deref());
        }
        Some("dict") if arguments.get(1).map(|s| s.as_str()) == Some("build") => {
            let out =
                arg_value(&arguments, "--out").unwrap_or_else(|| die("--out FILE is required"));
            let exe = arg_value(&arguments, "--exe");
            let cmp = arg_value(&arguments, "--compare");
            let mut dats = vec![];
            let mut skip = false;
            for x in &arguments[2..] {
                if skip {
                    skip = false;
                    continue;
                }
                if x.starts_with("--") {
                    skip = true;
                    continue;
                }
                dats.push(x.clone());
            }
            let runtime = runtime::RuntimeAccess::load(profile_path.as_deref(), None)
                .unwrap_or_else(|error| die(error));
            dict::build(
                &runtime.archive,
                &dats,
                exe.as_deref(),
                &out,
                cmp.as_deref(),
                profile_path.as_deref(),
            );
        }
        Some("hash") if arguments.get(1).map(|s| s.as_str()) == Some("file") => {
            let path = arguments
                .get(2)
                .unwrap_or_else(|| die("hash file requires a path"));
            if arguments.len() != 3 {
                die("usage: fox hash file PATH");
            }
            let mut file =
                std::fs::File::open(path).unwrap_or_else(|error| die(format!("{path}: {error}")));
            let mut hasher = blake3::Hasher::new();
            hasher
                .update_reader(&mut file)
                .unwrap_or_else(|error| die(format!("{path}: {error}")));
            println!("{}", hasher.finalize().to_hex());
        }
        Some("hash") if arguments.get(1).map(|s| s.as_str()) == Some("path") => {
            for p in &arguments[2..] {
                println!("{:016x}  {p}", qar::file_hash(p));
            }
        }
        None | Some("--help" | "-h" | "help") => print_help(),
        Some(command) => die(format!("unknown command {command:?}; run fox --help")),
    }
}

fn print_help() {
    println!("{}", include_str!("help.txt"));
    if cfg!(feature = "internal-pipeline") {
        println!(
            "Internal pipeline commands: nav, terrain, place, m3, mission, parallax, npy-roundtrip"
        );
    }
}
