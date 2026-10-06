mod common;
// set_progress: every operation (setup, install from memory / a staging folder, uninstall with mods left, the last
// uninstall that restores the base archives, a failing operation) reports its plan phases in order, the overall
// estimate never goes back, and the last report is `done` (overall 1.0 when it succeeded).
use foxinstall::{Game, Progress};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

type Seen = Arc<Mutex<Vec<Progress>>>;

fn game(root: &Path) -> (Game, Seen) {
    let seen: Seen = Arc::new(Mutex::new(vec![]));
    let s2 = seen.clone();
    let mut g = common::game(root);
    g.set_progress(move |p| s2.lock().unwrap().push(p.clone()));
    (g, seen)
}

fn meta(name: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<ModEntry Name="{name}" Version="1" Author="t" Website="">
  <Description>t</Description>
</ModEntry>"#
    )
}

/// the reports of one operation: op name, phases contain `want` in this order, monotonic, one final report
fn check(seen: &Seen, op: &str, want: &[&str], ok: bool) {
    let v = seen.lock().unwrap().clone();
    assert!(!v.is_empty(), "{op}: no reports");
    assert!(v.iter().all(|p| p.op == op), "{op}: {v:?}");
    let phases: Vec<&str> = v.iter().map(|p| p.phase.as_str()).collect();
    let mut at = 0;
    for w in want {
        match phases[at..].iter().position(|p| p == w) {
            Some(i) => at += i,
            None => panic!("{op}: phase {w} missing or out of order: {phases:?}"),
        }
    }
    assert!(
        v.windows(2).all(|w| w[1].overall >= w[0].overall),
        "{op}: overall went back: {v:?}"
    );
    assert!(
        v.iter().all(|p| (0.0..=1.0).contains(&p.overall)),
        "{op}: out of range: {v:?}"
    );
    assert!(
        v.iter()
            .all(|p| p.fraction.is_none_or(|f| (0.0..=1.0).contains(&f))),
        "{op}: fraction: {v:?}"
    );
    let last = v.last().unwrap();
    assert!(last.done && last.ok == ok, "{op}: last report {last:?}");
    assert_eq!(
        v.iter().filter(|p| p.done).count(),
        1,
        "{op}: one final report: {phases:?}"
    );
    if ok {
        assert_eq!(last.phase, "done");
        assert_eq!(last.overall, 1.0);
        assert!(
            v[..v.len() - 1].iter().all(|p| p.overall < 1.0),
            "{op}: 1.0 before the end: {v:?}"
        );
    } else {
        assert_eq!(last.phase, "failed");
    }
}

#[test]
fn every_operation_reports_progress() {
    let root: PathBuf =
        std::env::temp_dir().join(format!("foxinstall_progress_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let d = root.join("master").join("0");
    std::fs::create_dir_all(&d).unwrap();
    for a in ["00", "01"] {
        let mut f = std::fs::File::create(d.join(format!("{a}.dat"))).unwrap();
        common::context().write_archive(&mut f, 0, 3, &[]).unwrap();
    }

    let (mut g, seen) = game(&root);
    g.setup().unwrap();
    check(
        &seen,
        "setup",
        &[
            "copy base archives",
            "copy 00.dat",
            "copy 01.dat",
            "check signatures",
            "done",
        ],
        true,
    );

    // in memory: no "open package" phase
    let (mut g, seen) = game(&root);
    g.install_files(
        &meta("first"),
        vec![
            ("Assets/tpp/script/test/x.lua".into(), b"print(1)".to_vec()),
            ("GameDir/mod/test/y.lua".into(), b"--".to_vec()),
        ],
        false,
        false,
    )
    .unwrap();
    check(
        &seen,
        "install",
        &[
            "vanilla tables",
            "current archive indexes",
            "classify package files",
            "write archives",
            "write 00.dat",
            "commit",
            "done",
        ],
        true,
    );

    // staging folder: "open package" first
    let stage = root.join("stage");
    std::fs::create_dir_all(stage.join("Assets/tpp/script/test2")).unwrap();
    std::fs::write(stage.join("metadata.xml"), meta("second")).unwrap();
    std::fs::write(stage.join("Assets/tpp/script/test2/z.lua"), b"print(2)").unwrap();
    let (mut g, seen) = game(&root);
    g.install_opts(&stage, false, false).unwrap();
    check(
        &seen,
        "install",
        &[
            "open package",
            "vanilla tables",
            "write 00.dat",
            "commit",
            "done",
        ],
        true,
    );

    // a failing operation: one final "failed" report
    let (mut g, seen) = game(&root);
    assert!(g.install_opts(&stage, false, false).is_err()); // already installed, no --replace
    check(&seen, "install", &["failed"], false);
    let (mut g, seen) = game(&root);
    assert!(
        g.install_opts(&root.join("no such package.mgsv"), false, false)
            .is_err()
    );
    check(&seen, "install", &["open package", "failed"], false);

    // uninstall with a mod left: archives rewritten
    let (mut g, seen) = game(&root);
    g.uninstall("first").unwrap();
    check(
        &seen,
        "uninstall",
        &[
            "loose files",
            "vanilla tables",
            "write archives",
            "write 00.dat",
            "commit",
            "done",
        ],
        true,
    );

    // the last mod: base archives restored
    let (mut g, seen) = game(&root);
    g.uninstall("second").unwrap();
    check(
        &seen,
        "uninstall",
        &[
            "restore base archives",
            "restore 00.dat",
            "restore 01.dat",
            "commit",
            "done",
        ],
        true,
    );
    for a in ["00", "01"] {
        assert_eq!(
            std::fs::read(d.join(format!("{a}.dat"))).unwrap(),
            std::fs::read(d.join(format!("{a}.dat.foxbase"))).unwrap()
        );
    }

    // no callback: operations still work (no reports anywhere)
    let mut g = common::game(&root);
    g.install_opts(&stage, false, false).unwrap();
    g.uninstall("second").unwrap();
    let _ = std::fs::remove_dir_all(&root);
}
