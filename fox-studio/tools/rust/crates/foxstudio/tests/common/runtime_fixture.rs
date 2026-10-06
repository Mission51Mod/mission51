//! Invented QAR/FPK data for public runtime-profile tests; no local game assets or private keys.
use foxcore::{
    fpk, qar,
    runtime_data::{QarKeys, RuntimeProfile},
};
use std::{fs, fs::File, path::Path};

pub fn write_game(game: &Path, seed: u32) -> RuntimeProfile {
    fs::create_dir_all(game.join("master/0")).unwrap();
    let context = qar::Context::new(QarKeys {
        header_masks: [11 + seed, 22 + seed, 33 + seed, 44 + seed],
        layer1: [
            1 + seed,
            2 + seed,
            3 + seed,
            4 + seed,
            5 + seed,
            6 + seed,
            7 + seed,
            8 + seed,
        ],
    });
    let mut entries = Vec::new();
    for kind in [fpk::Kind::Fpk, fpk::Kind::Fpkd] {
        let extension = if kind == fpk::Kind::Fpk { "fpk" } else { "fpkd" };
        let package = fpk::write_in_order(kind, &[("/synthetic/a.alpha", b"a"), ("/synthetic/b.beta", b"b")], &[]);
        for index in 0..8 {
            let hash = (qar::ext_id(extension) << 51) | index;
            let (_, raw) = context.encode_plain(hash, &package).unwrap();
            entries.push(qar::RawEntry { hash, raw, pad: None });
        }
    }
    context
        .write_archive(
            &mut File::create(game.join("master/data1.dat")).unwrap(),
            0,
            1,
            &entries,
        )
        .unwrap();
    for archive in ["00", "01"] {
        context
            .write_archive(
                &mut File::create(game.join(format!("master/0/{archive}.dat"))).unwrap(),
                0,
                1,
                &[],
            )
            .unwrap();
    }
    RuntimeProfile::learn(game, &mut |_, _| true).unwrap()
}
