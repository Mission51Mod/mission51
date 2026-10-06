use foxcore::{
    fpk, qar,
    runtime_data::{OrderLearner, QarKeys, RuntimeProfile},
};
use std::fs::{self, File};

fn synthetic_keys() -> QarKeys {
    QarKeys {
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
    }
}

fn synthetic_install() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("master")).unwrap();
    let context = qar::Context::new(synthetic_keys());
    let mut entries = Vec::new();
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
            entries.push(qar::RawEntry {
                hash,
                raw,
                pad: None,
            });
        }
    }
    context
        .write_archive(
            &mut File::create(dir.path().join("master/data1.dat")).unwrap(),
            0,
            1,
            &entries,
        )
        .unwrap();
    dir
}

#[test]
fn synthetic_learning_round_trip_and_correct_order() {
    let install = synthetic_install();
    let profile = RuntimeProfile::learn(install.path(), &mut |_, _| true).unwrap();
    assert_eq!(profile.keys, synthetic_keys());
    for kind in [fpk::Kind::Fpk, fpk::Kind::Fpkd] {
        let order = profile.order.kind(kind);
        assert_eq!(order.packages, 8);
        assert_eq!(order.rules.len(), 1);
        assert_eq!(order.rules[0].before, "alpha");
        assert_eq!(order.rules[0].after, "beta");
        let paths = ["/a/q.beta", "/a/z.unknown", "/a/p.alpha"];
        assert_eq!(
            profile.order.vanilla_order(kind, &paths).unwrap(),
            [1, 2, 0]
        );
        assert_eq!(profile.order.sort_entries(kind, &paths).unwrap(), [2, 0, 1]);
    }
    let cache = tempfile::tempdir().unwrap();
    let path = cache.path().join("runtime_profile.json");
    profile.save(&path).unwrap();
    profile.save(&path).unwrap();
    let loaded = RuntimeProfile::load(&path).unwrap();
    assert_eq!(profile, loaded);
    loaded.validate_for_game(install.path()).unwrap();
}

#[test]
fn modified_archive_and_cancelled_setup_are_rejected() {
    let install = synthetic_install();
    let profile = RuntimeProfile::learn(install.path(), &mut |_, _| true).unwrap();
    let path = install.path().join("master/data1.dat");
    let mut bytes = fs::read(&path).unwrap();
    bytes[32] ^= 1;
    fs::write(path, bytes).unwrap();
    assert!(profile.validate_for_game(install.path()).is_err());
    let install = synthetic_install();
    assert!(
        RuntimeProfile::learn(install.path(), &mut |_, _| false)
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
}

#[test]
fn a_profile_with_tampered_keys_is_rejected() {
    let install = synthetic_install();
    let mut profile = RuntimeProfile::learn(install.path(), &mut |_, _| true).unwrap();
    profile.keys.layer1[3] ^= 1;
    assert!(
        profile
            .validate_for_game(install.path())
            .unwrap_err()
            .to_string()
            .contains("keys")
    );
}

#[test]
fn contradictory_observations_remove_the_unsupported_rule() {
    let mut learner = OrderLearner::default();
    for kind in [fpk::Kind::Fpk, fpk::Kind::Fpkd] {
        for i in 0..6 {
            let entries: [(&str, &[u8]); 2] = if i == 5 {
                [("/a/b.beta", b"b"), ("/a/a.alpha", b"a")]
            } else {
                [("/a/a.alpha", b"a"), ("/a/b.beta", b"b")]
            };
            learner.observe(&fpk::read(&fpk::write_in_order(kind, &entries, &[])).unwrap());
        }
    }
    let order = learner.finish().unwrap();
    assert!(order.fpk.rules.is_empty());
    assert!(order.fpkd.rules.is_empty());
}

#[test]
fn partial_keys_and_truncated_second_layer_are_errors() {
    let context = qar::Context::new(synthetic_keys());
    assert!(context.read_entry_header(&[0; 31], 0).is_err());
    let (entry, mut raw) = context.encode_plain(9, &[0; 8]).unwrap();
    let mut body = [0; 8];
    body[..4].copy_from_slice(&qar::ENC_MAGIC2.to_le_bytes());
    context.layer1(&mut body, entry.hash, 0);
    raw[32..].copy_from_slice(&body);
    assert!(
        context
            .decode(&entry, &raw[32..])
            .unwrap_err()
            .contains("truncated")
    );
}

#[test]
fn forged_package_counts_and_payload_ranges_return_errors() {
    let mut bytes = fpk::write_in_order(fpk::Kind::Fpk, &[("/synthetic/a.bin", b"a")], &[]);
    bytes[36..40].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(fpk::read(&bytes).unwrap_err().contains("table"));
    let mut bytes = fpk::write_in_order(fpk::Kind::Fpk, &[], &[]);
    bytes[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(fpk::read(&bytes).unwrap_err().contains("table"));
    let mut bytes = fpk::write_in_order(fpk::Kind::Fpk, &[("/synthetic/a.bin", b"a")], &[]);
    bytes[48..52].copy_from_slice(&u32::MAX.to_le_bytes());
    bytes[56..60].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(fpk::read(&bytes).unwrap_err().contains("payload"));
    let mut bytes = fpk::write_in_order(fpk::Kind::Fpk, &[("/synthetic/a.bin", b"a")], &[]);
    bytes[64..68].copy_from_slice(&u32::MAX.to_le_bytes());
    bytes[72..76].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(fpk::read(&bytes).unwrap_err().contains("string"));
}

#[test]
fn encrypted_size_headers_bootstrap_compressed_package_learning() {
    use std::io::Write;
    let install = synthetic_install();
    let context = qar::Context::new(synthetic_keys());
    let mut entries = Vec::new();
    for i in 0..4 {
        let hash = i;
        let key = 1234 + i as u32;
        let mut body = Vec::new();
        body.extend_from_slice(&qar::ENC_MAGIC2.to_le_bytes());
        body.extend_from_slice(&key.to_le_bytes());
        body.extend_from_slice(&8u32.to_le_bytes());
        body.extend_from_slice(&8u32.to_le_bytes());
        let mut content = *b"example!";
        qar::layer2(&mut content, key);
        body.extend_from_slice(&content);
        let entry = qar::Entry {
            hash,
            offset: 0,
            stored: body.len() as u32,
            uncompressed: body.len() as u32,
            md5: [0; 16],
        };
        let mut raw = context.entry_header_bytes(&entry).to_vec();
        context.layer1(&mut body, hash, 0);
        raw.extend_from_slice(&body);
        entries.push(qar::RawEntry {
            hash,
            raw,
            pad: None,
        });
    }
    for kind in [fpk::Kind::Fpk, fpk::Kind::Fpkd] {
        for i in 0..8 {
            let bytes = fpk::write_in_order(
                kind,
                &[("/synthetic/a.alpha", b"a"), ("/synthetic/b.beta", b"b")],
                &[],
            );
            let mut compressor =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            compressor.write_all(&bytes).unwrap();
            let mut body = compressor.finish().unwrap();
            let extension = match kind {
                fpk::Kind::Fpk => "fpk",
                fpk::Kind::Fpkd => "fpkd",
            };
            let hash = (qar::ext_id(extension) << 51) | i;
            let entry = qar::Entry {
                hash,
                offset: 0,
                stored: body.len() as u32,
                uncompressed: bytes.len() as u32,
                md5: [0; 16],
            };
            let mut raw = context.entry_header_bytes(&entry).to_vec();
            context.layer1(&mut body, hash, 0);
            raw.extend_from_slice(&body);
            entries.push(qar::RawEntry {
                hash,
                raw,
                pad: None,
            });
        }
    }
    context
        .write_archive(
            &mut File::create(install.path().join("master/data1.dat")).unwrap(),
            0,
            1,
            &entries,
        )
        .unwrap();
    let profile = RuntimeProfile::learn(install.path(), &mut |_, _| true).unwrap();
    assert_eq!(profile.keys, synthetic_keys());
    assert_eq!(profile.order.fpk.rules.len(), 1);
    assert_eq!(profile.order.fpkd.rules.len(), 1);
}

#[test]
fn compressed_payload_must_match_its_declared_decoded_size() {
    use std::io::Write;
    let context = qar::Context::new(synthetic_keys());
    let mut compressor =
        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    compressor.write_all(b"abc").unwrap();
    let compressed = compressor.finish().unwrap();
    // Compression follows the optional second-layer header and encryption.
    for magic in [None, Some(qar::ENC_MAGIC1), Some(qar::ENC_MAGIC2)] {
        let mut stored = Vec::new();
        let mut payload = compressed.clone();
        if let Some(magic) = magic {
            let key = 1234u32;
            stored.extend_from_slice(&magic.to_le_bytes());
            stored.extend_from_slice(&key.to_le_bytes());
            if magic == qar::ENC_MAGIC2 {
                stored.extend_from_slice(&(payload.len() as u32).to_le_bytes());
                stored.extend_from_slice(&3u32.to_le_bytes());
            }
            qar::layer2(&mut payload, key);
        }
        stored.extend_from_slice(&payload);
        let mut entry = qar::Entry {
            hash: 9,
            offset: 0,
            stored: stored.len() as u32,
            uncompressed: 3,
            md5: [0; 16],
        };
        context.layer1(&mut stored, entry.hash, 0);
        assert_eq!(context.decode(&entry, &stored).unwrap(), b"abc");
        entry.uncompressed = 4;
        assert!(
            context
                .decode(&entry, &stored)
                .unwrap_err()
                .contains("shorter than its declared size")
        );
        entry.uncompressed = 2;
        assert!(
            context
                .decode(&entry, &stored)
                .unwrap_err()
                .contains("exceeds its declared size")
        );
    }
}

#[test]
fn plain_second_layer_sizes_include_the_encryption_header() {
    let context = qar::Context::new(synthetic_keys());
    for magic in [qar::ENC_MAGIC1, qar::ENC_MAGIC2] {
        let key = 1234u32;
        let mut stored = Vec::new();
        stored.extend_from_slice(&magic.to_le_bytes());
        stored.extend_from_slice(&key.to_le_bytes());
        if magic == qar::ENC_MAGIC2 {
            stored.extend_from_slice(&3u32.to_le_bytes());
            stored.extend_from_slice(&3u32.to_le_bytes());
        }
        let mut payload = *b"abc";
        qar::layer2(&mut payload, key);
        stored.extend_from_slice(&payload);
        let entry = qar::Entry {
            hash: 9,
            offset: 0,
            stored: stored.len() as u32,
            uncompressed: stored.len() as u32,
            md5: [0; 16],
        };
        assert!(!entry.compressed());
        context.layer1(&mut stored, entry.hash, 0);
        assert_eq!(context.decode(&entry, &stored).unwrap(), b"abc");
    }
}

#[test]
fn corrupt_compression_sizes_return_errors_without_speculative_allocation() {
    use std::io::Write;
    let context = qar::Context::new(synthetic_keys());
    let mut compressor =
        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    compressor.write_all(b"eight!!!").unwrap();
    let mut body = compressor.finish().unwrap();
    let entry = qar::Entry {
        hash: 0,
        offset: 0,
        stored: body.len() as u32,
        uncompressed: 2,
        md5: [0; 16],
    };
    context.layer1(&mut body, entry.hash, 0);
    assert!(
        context
            .decode(&entry, &body)
            .unwrap_err()
            .contains("declared size")
    );
    let mut entry = entry;
    entry.stored = 8;
    entry.uncompressed = u32::MAX;
    let mut invalid_stream = [0; 8];
    context.layer1(&mut invalid_stream, entry.hash, 0);
    assert!(
        context
            .decode(&entry, &invalid_stream)
            .unwrap_err()
            .contains("zlib")
    );
}

#[test]
fn oversized_learning_index_is_rejected_before_allocation() {
    use std::io::Write;
    let install = tempfile::tempdir().unwrap();
    fs::create_dir_all(install.path().join("master")).unwrap();
    let context = qar::Context::new(synthetic_keys());
    let data_offset = 8 * 1024 * 1024;
    let length = data_offset + 1024;
    let header = qar::Header {
        flags: 0,
        count: 1_000_000,
        extra_count: 0,
        end_block: length >> 10,
        data_offset,
        version: 1,
        block_shift: 10,
    };
    let mut archive = File::create(install.path().join("master/data1.dat")).unwrap();
    archive.write_all(&context.header_bytes(&header)).unwrap();
    archive.set_len(length as u64).unwrap();
    assert!(
        RuntimeProfile::learn(install.path(), &mut |_, _| true)
            .unwrap_err()
            .to_string()
            .contains("learning limit")
    );
}

#[test]
fn forged_qar_tables_and_entry_lengths_fail_before_payload_allocation() {
    use std::io::Cursor;
    let install = synthetic_install();
    let context = qar::Context::new(synthetic_keys());
    let path = install.path().join("master/data1.dat");
    let original = fs::read(path).unwrap();
    let index = context.read_index(&mut Cursor::new(&original)).unwrap();
    let mut bytes = original.clone();
    let size_at = index.entries[0].offset as usize + 8;
    let stored_word = u32::MAX ^ synthetic_keys().header_masks[1];
    bytes[size_at..size_at + 4].copy_from_slice(&stored_word.to_le_bytes());
    assert!(
        context
            .read_index(&mut Cursor::new(&bytes))
            .err()
            .unwrap()
            .contains("exceeds archive")
    );
    let mut bytes = original;
    let count_word = u32::MAX ^ synthetic_keys().header_masks[1];
    bytes[8..12].copy_from_slice(&count_word.to_le_bytes());
    assert!(context.read_index(&mut Cursor::new(&bytes)).is_err());
    assert!(context.read_table(&mut Cursor::new(&bytes)).is_err());
}
