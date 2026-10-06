//! Small native setup sources with invented keys, enough pack observations to learn a profile.
use foxcore::{fpk, qar, runtime_data::QarKeys};
use std::fs::{self, File};
use std::path::Path;

pub fn context() -> qar::Context {
    qar::Context::new(QarKeys {
        header_masks: [11, 22, 33, 44],
        layer1: [1, 2, 3, 4, 5, 6, 7, 8],
    })
}

/// `system_entries` use full game paths with extensions. No installed game is accessed.
pub fn write_game(game: &Path, system_entries: &[(&str, &[u8])]) {
    fs::create_dir_all(game.join("master/0")).unwrap();
    fs::write(game.join("mgsvtpp.exe"), b"synthetic placeholder").unwrap();
    let context = context();
    let mut packages = Vec::new();
    for kind in [fpk::Kind::Fpk, fpk::Kind::Fpkd] {
        for index in 0..8 {
            let data = fpk::write_in_order(
                kind,
                &[("/synthetic/a.alpha", b"a"), ("/synthetic/b.beta", b"b")],
                &[],
            );
            let extension = if kind == fpk::Kind::Fpk {
                "fpk"
            } else {
                "fpkd"
            };
            let hash = (qar::ext_id(extension) << 51) | index;
            let (_, raw) = context.encode_plain(hash, &data).unwrap();
            packages.push(qar::RawEntry {
                hash,
                raw,
                pad: None,
            });
        }
    }
    context
        .write_archive(
            &mut File::create(game.join("master/data1.dat")).unwrap(),
            0,
            1,
            &packages,
        )
        .unwrap();
    let entries: Vec<_> = system_entries
        .iter()
        .map(|(name, data)| {
            let hash = qar::file_hash(name);
            let (_, raw) = context.encode_plain(hash, data).unwrap();
            qar::RawEntry {
                hash,
                raw,
                pad: None,
            }
        })
        .collect();
    context
        .write_archive(
            &mut File::create(game.join("master/0/00.dat")).unwrap(),
            0,
            1,
            &entries,
        )
        .unwrap();
}
