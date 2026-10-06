use foxcore::{
    fpk, qar,
    runtime_data::{QarKeys, RuntimeProfile},
};
use foxinstall::Game;
use std::fs::{self, File};
use std::path::PathBuf;

struct Fixture {
    root: PathBuf,
    profile: RuntimeProfile,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn fixture(name: &str) -> Fixture {
    let root =
        std::env::temp_dir().join(format!("foxinstall_profile_{name}_{}", std::process::id()));
    fs::create_dir_all(root.join("master/0")).unwrap();
    let keys = QarKeys {
        header_masks: [0x0102_0304, 0x1122_3344, 0x2233_4455, 0x3344_5566],
        layer1: [
            0x1020_3040,
            0x5060_7080,
            0x9080_7060,
            0x5040_3020,
            0x1324_3546,
            0x5768_798a,
            0x9bac_bdce,
            0xdfe0_f102,
        ],
    };
    let context = qar::Context::new(keys);
    let mut corpus = Vec::new();
    for kind in [fpk::Kind::Fpk, fpk::Kind::Fpkd] {
        for i in 0..8 {
            let package = fpk::write_in_order(
                kind,
                &[("/synthetic/a.alpha", b"a"), ("/synthetic/b.beta", b"b")],
                &[],
            );
            let extension = match kind {
                fpk::Kind::Fpk => "fpk",
                fpk::Kind::Fpkd => "fpkd",
            };
            let hash = (qar::ext_id(extension) << 51) | i;
            let (_, raw) = context.encode_plain(hash, &package).unwrap();
            corpus.push(qar::RawEntry {
                hash,
                raw,
                pad: None,
            });
        }
    }
    context
        .write_archive(
            &mut File::create(root.join("master/data1.dat")).unwrap(),
            0,
            1,
            &corpus,
        )
        .unwrap();
    let package = fpk::write_in_order(
        fpk::Kind::Fpk,
        &[
            ("/synthetic/a.alpha", b"original"),
            ("/synthetic/b.beta", b"original beta"),
        ],
        &[],
    );
    let hash = qar::file_hash("/Assets/test/base.fpk");
    let (_, raw) = context.encode_plain(hash, &package).unwrap();
    context
        .write_archive(
            &mut File::create(root.join("master/0/00.dat")).unwrap(),
            0,
            1,
            &[qar::RawEntry {
                hash,
                raw,
                pad: None,
            }],
        )
        .unwrap();
    context
        .write_archive(
            &mut File::create(root.join("master/0/01.dat")).unwrap(),
            0,
            1,
            &[],
        )
        .unwrap();
    let profile = RuntimeProfile::learn(&root, &mut |_, _| true).unwrap();
    Fixture { root, profile }
}

fn metadata(name: &str) -> String {
    format!(
        "<ModEntry Name=\"{name}\" Version=\"1\" Author=\"test\" Website=\"\"><Description>synthetic</Description></ModEntry>"
    )
}

#[test]
fn explicit_profile_installs_merges_and_restores_exact_base() {
    let fixture = fixture("roundtrip");
    let original = fs::read(fixture.root.join("master/0/00.dat")).unwrap();
    let mut game = Game::with_profile(&fixture.root, None, &fixture.profile).unwrap();
    game.setup().unwrap();
    let package = fpk::write_in_order(
        fpk::Kind::Fpk,
        &[
            ("/synthetic/b.beta", b"replaced"),
            ("/synthetic/c.alpha", b"added"),
        ],
        &[],
    );
    game.install_files(
        &metadata("profile test"),
        vec![("Assets/test/base.fpk".into(), package)],
        false,
        false,
    )
    .unwrap();
    assert!(game.verify().unwrap());
    let context = fixture.profile.qar_context();
    let mut archive = File::open(fixture.root.join("master/0/00.dat")).unwrap();
    let index = context.read_index(&mut archive).unwrap();
    let entry = index
        .entries
        .iter()
        .find(|e| e.hash == qar::file_hash("/Assets/test/base.fpk"))
        .unwrap();
    use std::io::{Read, Seek, SeekFrom};
    archive.seek(SeekFrom::Start(entry.offset + 32)).unwrap();
    let mut stored = vec![0; entry.stored as usize];
    archive.read_exact(&mut stored).unwrap();
    let bytes = context.decode(entry, &stored).unwrap();
    let package = fpk::read(&bytes).unwrap();
    let paths: Vec<&str> = package.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/synthetic/a.alpha",
            "/synthetic/c.alpha",
            "/synthetic/b.beta"
        ]
    );
    assert!(
        fixture
            .profile
            .order
            .violations(package.kind, &paths)
            .is_empty()
    );
    let replaced = package
        .entries
        .iter()
        .find(|e| e.path.ends_with("b.beta"))
        .unwrap();
    assert_eq!(
        &bytes[replaced.offset as usize..(replaced.offset + replaced.size) as usize],
        b"replaced"
    );
    game.uninstall("profile test").unwrap();
    assert_eq!(
        fs::read(fixture.root.join("master/0/00.dat")).unwrap(),
        original
    );
}

#[test]
fn wrong_install_fails_before_creating_installer_state() {
    let fixture = fixture("wrong_install");
    let mut altered = fixture.profile.clone();
    altered.sources[0].bytes += 1024;
    assert!(Game::with_profile(&fixture.root, None, &altered).is_err());
    assert!(!fixture.root.join("foxinstall").exists());
}

#[test]
#[cfg(not(feature = "internal-game-data"))]
fn incomplete_public_setup_fails_before_writes() {
    let fixture = fixture("missing");
    let before = fs::read(fixture.root.join("master/0/00.dat")).unwrap();
    let mut game = Game::new(&fixture.root, None);
    assert!(game.setup().unwrap_err().contains("setup"));
    assert!(
        game.install_files(&metadata("missing profile"), vec![], false, false)
            .is_err()
    );
    assert!(!fixture.root.join("foxinstall").exists());
    assert_eq!(
        fs::read(fixture.root.join("master/0/00.dat")).unwrap(),
        before
    );
}
