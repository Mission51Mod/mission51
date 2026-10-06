//! Authored native stage fixture. Compiled with the test toolchain, never shipped.
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

fn quote(value: &str) -> String {
    let mut output = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            other => output.push(other),
        }
    }
    output.push('"');
    output
}

fn signal(variable: &str) {
    if let Some(path) = std::env::var_os(variable) {
        fs::write(path, std::process::id().to_string()).unwrap();
    }
}

fn trace(input: &str, output: &str, stamp: (u64, u128)) {
    let directory = PathBuf::from(std::env::var_os("FOXBUILD_TRACE_DIR").unwrap());
    let executable = std::env::current_exe().unwrap();
    let record = format!(
        "{{\"pid\":{},\"exe\":{},\"reads\":{{{}:[{},{}]}},\"writes\":[{}]}}",
        std::process::id(),
        quote(&executable.to_string_lossy()),
        quote(input),
        stamp.0,
        stamp.1,
        quote(output)
    );
    fs::write(
        directory.join(format!("{}.json", std::process::id())),
        record,
    )
    .unwrap();
}

fn spawn_heartbeat(path: &str) -> std::process::Child {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["heartbeat", path]).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command.spawn().unwrap()
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("-I") {
        signal("FIXTURE_PROBE_SIGNAL");
        if std::env::var_os("FIXTURE_BLOCK_PROBE").is_some() {
            let heartbeat = std::env::var("FIXTURE_HEARTBEAT").unwrap();
            let child = spawn_heartbeat(&heartbeat);
            fs::write(
                std::env::var_os("FIXTURE_CHILD_PID").unwrap(),
                child.id().to_string(),
            )
            .unwrap();
            loop {
                thread::sleep(Duration::from_millis(50));
            }
        }
        println!(
            "{}\nfixture-1\nnative-fixture",
            std::env::current_exe().unwrap().display()
        );
        return;
    }
    if args.first().is_some_and(|path| path.ends_with("build.py")) {
        signal("FIXTURE_SNAPSHOT_SIGNAL");
        let heartbeat = std::env::var("FIXTURE_HEARTBEAT").unwrap();
        let child = spawn_heartbeat(&heartbeat);
        fs::write(
            std::env::var_os("FIXTURE_CHILD_PID").unwrap(),
            child.id().to_string(),
        )
        .unwrap();
        loop {
            thread::sleep(Duration::from_millis(50));
        }
    }
    match args.first().map(String::as_str) {
        Some("copy" | "gated-copy" | "prepared-copy") => {
            let input = &args[1];
            let output = &args[2];
            if args[0] == "prepared-copy" {
                let lease = fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&args[3])
                    .unwrap();
                assert!(matches!(
                    lease.try_lock(),
                    Err(fs::TryLockError::WouldBlock)
                ));
                fs::write(
                    &args[4],
                    format!(
                        "{}\n{}\n{}",
                        std::env::var("FIXTURE_BINDING").unwrap(),
                        std::env::var("FOX_PROJECT").unwrap(),
                        std::env::var("FOX_REPO_ROOT").unwrap()
                    ),
                )
                .unwrap();
            }
            let metadata = fs::metadata(input).unwrap();
            let stamp = (
                metadata.len(),
                metadata
                    .modified()
                    .unwrap()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
            );
            let bytes = fs::read(input).unwrap();
            if let Some(parent) = Path::new(output).parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(output, bytes).unwrap();
            trace(input, output, stamp);
            println!("native stage: copied {input} -> {output}");
            if args[0] == "gated-copy" {
                fs::write(&args[3], std::process::id().to_string()).unwrap();
                while !Path::new(&args[4]).exists() {
                    thread::sleep(Duration::from_millis(25));
                }
            }
        }
        Some("build") => {
            // Replicates the reviewed escape if the outer validator regresses:
            // this native tool can launch an unbundled inner scheduler.
            let result = Command::new(std::env::var_os("FIXTURE_SCHEDULER").unwrap())
                .args(&args[1..])
                .status()
                .unwrap();
            std::process::exit(result.code().unwrap_or(2));
        }
        Some("touch") => {
            fs::write(&args[1], "inner program ran").unwrap();
        }
        Some("fail") => {
            eprintln!("fixture failure");
            std::process::exit(7);
        }
        Some("spawn") => {
            let child = spawn_heartbeat(&args[1]);
            fs::write(&args[2], format!("{} {}", std::process::id(), child.id())).unwrap();
            loop {
                thread::sleep(Duration::from_millis(50));
            }
        }
        Some("heartbeat") => loop {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&args[1])
                .unwrap();
            writeln!(file, "{}", std::process::id()).unwrap();
            thread::sleep(Duration::from_millis(25));
        },
        other => panic!("unknown native fixture action {other:?}"),
    }
}
