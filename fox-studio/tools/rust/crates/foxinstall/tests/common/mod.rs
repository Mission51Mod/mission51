use foxcore::{
    fpk, qar,
    runtime_data::{QarKeys, RuntimeProfile},
};
use std::path::Path;

pub fn context() -> qar::Context {
    qar::Context::new(QarKeys {
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
    })
}

pub fn game(root: &Path) -> foxinstall::Game {
    let context = context();
    let path = root.join("master/data1.dat");
    if !path.exists() {
        let mut entries = Vec::new();
        for kind in [fpk::Kind::Fpk, fpk::Kind::Fpkd] {
            for i in 0..8 {
                let package = fpk::write_in_order(
                    kind,
                    &[("/test/a.alpha", b"a"), ("/test/b.beta", b"b")],
                    &[],
                );
                let ty = match kind {
                    fpk::Kind::Fpk => "fpk",
                    fpk::Kind::Fpkd => "fpkd",
                };
                let hash = (qar::ext_id(ty) << 51) | i;
                let (_, raw) = context.encode_plain(hash, &package).unwrap();
                entries.push(qar::RawEntry {
                    hash,
                    raw,
                    pad: None,
                });
            }
        }
        context
            .write_archive(&mut std::fs::File::create(path).unwrap(), 0, 1, &entries)
            .unwrap();
    }
    let profile = RuntimeProfile::learn(root, &mut |_, _| true).unwrap();
    foxinstall::Game::with_profile(root, None, &profile).unwrap()
}
