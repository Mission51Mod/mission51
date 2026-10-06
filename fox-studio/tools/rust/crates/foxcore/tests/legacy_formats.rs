//! Authored boundary fixtures for legacy format APIs; no game bytes or local corpus.
use foxcore::{containers, subp, twpf};

#[test]
fn regression_subtitle_raw_text_length_overflow_returns_error() {
    let text = "a".repeat(u16::MAX as usize);
    assert!(subp::write_subp(&[(1, 0, &text, 0, 0, vec![])], 0).is_err());
}

#[test]
fn regression_subtitle_utf8_size_overflow_returns_error() {
    // Latin-1 fits the raw size field, but the separate UTF-8 size does not.
    let text = "é".repeat(32768);
    assert!(subp::write_subp(&[(1, 0, &text, 0, 0, vec![])], 0).is_err());
}

#[test]
fn regression_subtitle_entry_count_overflow_returns_error() {
    let entries: Vec<_> = (0..=u16::MAX as u32)
        .map(|id| (id, 0, "", 0, 0, vec![]))
        .collect();
    assert!(subp::write_subp(&entries, 0).is_err());
}

#[test]
fn regression_subtitle_timing_count_overflow_returns_error() {
    let timings = vec![(0, 1); u8::MAX as usize + 1];
    assert!(subp::write_subp(&[(1, 0, "text", 0, 0, timings)], 0).is_err());
}

fn overlapping_weather_keys() -> Vec<u8> {
    let mut bytes = vec![0; 84];
    bytes[..4].copy_from_slice(b"TWPF");
    bytes[4..8].copy_from_slice(b"win\x01");
    for (offset, value) in [
        (8, 12u32),
        (12, 1),
        (16, 20),
        (24, 28),
        (32, 36),
        (44, 48),
        (52, 60),
        (56, 62),
    ] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[20..22].copy_from_slice(&1u16.to_le_bytes());
    bytes[28] = 1;
    bytes[40] = 1;
    bytes[50..52].copy_from_slice(&2u16.to_le_bytes());
    bytes
}

#[test]
fn regression_overlapping_weather_keys_return_error_without_panicking() {
    let result = std::panic::catch_unwind(|| twpf::parse(&overlapping_weather_keys()));
    let error = result
        .expect("malformed weather keys must return an error")
        .unwrap_err();
    assert!(error.contains("overlap"));
}

fn empty_texture_pack() -> Vec<u8> {
    containers::pftxs_write(&containers::Pftxs {
        head: [0x4000_0000, 0x10, 1],
        texl_unknown: 0,
        blocks: vec![],
    })
}

#[test]
fn regression_oversized_texture_block_returns_error_without_panicking() {
    let mut bytes = empty_texture_pack();
    bytes[24..28].copy_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(b"FTEX");
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    bytes.extend_from_slice(&[0; 24]);
    let texl_size = bytes.len() as u32 - 16;
    bytes[20..24].copy_from_slice(&texl_size.to_le_bytes());
    let result = std::panic::catch_unwind(|| containers::pftxs_read(&bytes));
    assert!(
        result
            .expect("invalid block extents must return an error")
            .is_err()
    );
}

#[test]
fn subtitle_text_and_timing_boundaries_roundtrip() {
    for text in ["a".repeat(65534), "é".repeat(32767)] {
        let timings = vec![(0, 1); 255];
        let bytes = subp::write_subp(&[(1, -1, &text, 0, 0, timings.clone())], 1).unwrap();
        let parsed = subp::read(&bytes).unwrap();
        assert_eq!(parsed.entries[0].text.len(), text.chars().count());
        assert_eq!(parsed.entries[0].size2 as usize, text.len() + 1);
        assert_eq!(parsed.entries[0].timings, timings);
        assert_eq!(subp::write(&parsed).unwrap(), bytes);
    }
}

#[test]
fn subtitle_maximum_entry_count_roundtrips() {
    let entries: Vec<_> = (0..u16::MAX as u32)
        .map(|id| (id, 0, "", 0, 0, vec![]))
        .collect();
    let bytes = subp::write_subp(&entries, 0).unwrap();
    assert_eq!(subp::read(&bytes).unwrap().entries.len(), 65535);
}

#[test]
fn raw_subtitle_writer_rejects_invalid_counts_and_text_sizes() {
    let entry = subp::Entry {
        id: 1,
        typ: 0,
        size2: 0,
        speaker: 0,
        flags: 0,
        timings: vec![],
        text: vec![],
    };
    let mut pack = subp::Subp {
        flags: 0x13,
        lang: 1,
        entries: vec![entry.clone()],
    };
    pack.entries[0].text = vec![b'a'; 65535];
    assert!(subp::write(&pack).unwrap_err().contains("text size"));
    pack.entries[0].text.clear();
    pack.entries[0].timings = vec![(0, 1); 256];
    assert!(subp::write(&pack).unwrap_err().contains("timings"));
    pack.entries = vec![entry; 65536];
    assert!(subp::write(&pack).unwrap_err().contains("entry count"));
}

fn two_texture_blocks() -> containers::Pftxs {
    containers::Pftxs {
        head: [0x4000_0000, 0x10, 1],
        texl_unknown: 0,
        blocks: vec![
            containers::FtexBlock {
                hash: 1,
                entries: vec![(2, b"a".to_vec())],
            },
            containers::FtexBlock {
                hash: 3,
                entries: vec![(4, b"b".to_vec()), (5, vec![])],
            },
        ],
    }
}

#[test]
fn texture_pack_counts_are_checked_before_allocation() {
    // These deliberately huge count fields are tested only after the parser fix.
    let mut bytes = empty_texture_pack();
    bytes[24..28].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(
        containers::pftxs_read(&bytes)
            .unwrap_err()
            .contains("block count")
    );
    let mut bytes = containers::pftxs_write(&two_texture_blocks());
    bytes[48..52].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(
        containers::pftxs_read(&bytes)
            .unwrap_err()
            .contains("entry table")
    );
}

#[test]
fn texture_pack_header_and_block_sizes_are_checked() {
    let mut bytes = empty_texture_pack();
    bytes[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(
        containers::pftxs_read(&bytes)
            .unwrap_err()
            .contains("TEXL size")
    );
    for size in [0u32, 31, u32::MAX] {
        let mut bytes = containers::pftxs_write(&two_texture_blocks());
        bytes[36..40].copy_from_slice(&size.to_le_bytes());
        assert!(containers::pftxs_read(&bytes).is_err());
    }
}

#[test]
fn texture_payloads_cannot_borrow_another_block_or_the_entry_table() {
    let original = containers::pftxs_write(&two_texture_blocks());
    let first_block_size = u32::from_le_bytes(original[36..40].try_into().unwrap());
    for (offset, size, error) in [
        (first_block_size, 1, "FTEX block"),
        (0, 1, "FTEX table"),
        (u32::MAX, u32::MAX, "FTEX block"),
    ] {
        let mut bytes = original.clone();
        bytes[72..76].copy_from_slice(&offset.to_le_bytes());
        bytes[76..80].copy_from_slice(&size.to_le_bytes());
        assert!(containers::pftxs_read(&bytes).unwrap_err().contains(error));
    }
}

#[test]
fn valid_texture_pack_preserves_blocks_payloads_and_empty_entries() {
    let pack = two_texture_blocks();
    let bytes = containers::pftxs_write(&pack);
    let parsed = containers::pftxs_read(&bytes).unwrap();
    assert_eq!(parsed, pack);
    assert_eq!(containers::pftxs_write(&parsed), bytes);
}

#[test]
fn sound_pack_255_entries_roundtrip_and_256_are_rejected() {
    let pack = containers::Sbp {
        header_pad: 9,
        entries: vec![(*b"bnk\0", b"sound".to_vec()); 255],
    };
    let bytes = containers::sbp_write(&pack).unwrap();
    assert_eq!(containers::sbp_read(&bytes).unwrap(), pack);
    let mut oversized = pack;
    oversized.entries.push((*b"bnk\0", vec![]));
    assert!(
        containers::sbp_write(&oversized)
            .unwrap_err()
            .contains("entry count")
    );
}

#[test]
fn adjacent_weather_keys_and_empty_values_roundtrip() {
    let mut bytes = overlapping_weather_keys();
    bytes[56..60].copy_from_slice(&64u32.to_le_bytes());
    let parsed = twpf::parse(&bytes).unwrap();
    let keys = &parsed.sections[0].props[0].areas[0].curves[0].keys;
    assert!(keys[0].value.is_empty());
    assert_eq!(keys[1].value.len(), 16);
    assert_eq!(twpf::write(&parsed), bytes);
}

#[test]
fn empty_texture_payloads_can_use_null_offsets() {
    let pack = containers::Pftxs {
        head: [0x4000_0000, 0x10, 1],
        texl_unknown: 0,
        blocks: vec![containers::FtexBlock {
            hash: 1,
            entries: vec![(2, vec![])],
        }],
    };
    let mut bytes = containers::pftxs_write(&pack);
    bytes[72..76].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(containers::pftxs_read(&bytes).unwrap(), pack);
}
