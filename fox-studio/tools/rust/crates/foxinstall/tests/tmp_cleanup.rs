mod common;
// A failed install leaves no *.foxtmp behind (loose-file temps and archive temps), and the game is unchanged.
use std::path::{Path, PathBuf};

fn fake_game(dir: &Path) {
    let d = dir.join("master").join("0");
    std::fs::create_dir_all(&d).unwrap();
    for a in ["00", "01"] {
        let mut f = std::fs::File::create(d.join(format!("{a}.dat"))).unwrap();
        common::context().write_archive(&mut f, 0, 3, &[]).unwrap();
    }
    common::game(dir).setup().unwrap();
}

fn foxtmps(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut st = vec![dir.to_path_buf()];
    while let Some(d) = st.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                st.push(p);
            } else if p.to_string_lossy().contains("foxtmp")
                || p.to_string_lossy().ends_with(".tmp")
            {
                out.push(p);
            }
        }
    }
    out
}

#[test]
fn failed_install_leaves_no_temps() {
    let root = std::env::temp_dir().join(format!("foxinstall_tmpclean_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let game = root.join("game");
    fake_game(&game);
    let before: Vec<Vec<u8>> = ["00", "01"]
        .iter()
        .map(|a| std::fs::read(game.join("master/0").join(format!("{a}.dat"))).unwrap())
        .collect();
    // make the archive write fail AFTER the loose-file temps exist: a directory where 00.dat's temp file goes
    std::fs::create_dir_all(game.join("master/0/00.dat.foxtmp")).unwrap();
    let meta = r#"<?xml version="1.0" encoding="utf-8"?>
<ModEntry xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xsd="http://www.w3.org/2001/XMLSchema" Name="tmp test" Version="1" Author="t" Website="">
  <MGSVersion Version="1.0.15.4" />
  <SBVersion Version="0.9.2.5" />
  <Description>t</Description>
</ModEntry>"#;
    let files = vec![
        (
            "Assets/tpp/script/test/x.lua".to_string(),
            b"print(1)".to_vec(),
        ),
        (
            "GameDir/mod/test/loose.lua".to_string(),
            b"-- loose".to_vec(),
        ),
    ];
    let mut g = common::game(&game);
    let r = g.install_files(meta, files, false, false);
    assert!(r.is_err(), "the install should fail");
    std::fs::remove_dir_all(game.join("master/0/00.dat.foxtmp")).unwrap();
    let left = foxtmps(&game);
    assert!(left.is_empty(), "temps left behind: {left:?}");
    assert!(
        !game.join("mod/test/loose.lua").exists(),
        "loose file installed by a failed install"
    );
    for (i, a) in ["00", "01"].iter().enumerate() {
        assert_eq!(
            std::fs::read(game.join("master/0").join(format!("{a}.dat"))).unwrap(),
            before[i],
            "{a}.dat changed"
        );
    }
    assert!(common::game(&game).load_manifest().unwrap().mods.is_empty());
    let _ = std::fs::remove_dir_all(&root);
}
