//! Self-contained format fixtures: no game bytes, NumPy, Pillow or Python.
use foxcore::{
    block_texture::{decode_bc1, decode_bc3},
    npy::{Npy, RecordArray},
    terrain::{file_to_grid, read_htre},
};

fn npy_file(version: u8, dictionary: &str, payload: &[u8]) -> Vec<u8> {
    let prefix_size = if version == 1 { 10 } else { 12 };
    let spaces = (64 - (prefix_size + dictionary.len() + 1) % 64) % 64;
    let header = format!("{dictionary}{}\n", " ".repeat(spaces));
    let mut bytes = b"\x93NUMPY".to_vec();
    bytes.extend_from_slice(&[version, 0]);
    if version == 1 {
        bytes.extend_from_slice(&(header.len() as u16).to_le_bytes());
    } else {
        bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
    }
    bytes.extend_from_slice(header.as_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

const PLACEMENT_DESCR: &str = "[('origin', '<U16'), ('layer', '<U6'), ('plugin', '<i2'), ('model', '<i2'), ('props', '<i2'), ('x', '<f8'), ('y', '<f8'), ('z', '<f8'), ('L', '<f8', (3, 3)), ('sbyte', '|u1'), ('clear', '<f4'), ('kind', '<U10'), ('tile', '<i4')]";

fn record_file(descr: &str, shape: &str, payload: &[u8]) -> Vec<u8> {
    npy_file(
        3,
        &format!("{{'shape': {shape}, 'descr': {descr}, 'fortran_order': False}}"),
        payload,
    )
}

fn put_unicode(bytes: &mut [u8], offset: usize, value: &str) {
    for (index, character) in value.chars().enumerate() {
        bytes[offset + index * 4..offset + index * 4 + 4]
            .copy_from_slice(&(character as u32).to_le_bytes());
    }
}

#[test]
fn npy_placement_records_borrow_unaligned_coordinates_and_matrix() {
    // Offsets are independently fixed from TP_DTYPE: no repr(C) alignment.
    let mut payload = vec![0u8; 2 * 239];
    for (index, coordinates) in [
        [1.25, -0.0, f64::from_bits(0x7ff8_0000_0000_0123)],
        [f64::INFINITY, -2.5, 7.75],
    ]
    .into_iter()
    .enumerate()
    {
        let row = &mut payload[index * 239..(index + 1) * 239];
        put_unicode(row, 0, "test");
        put_unicode(row, 64, "brush");
        put_unicode(row, 195, "plant");
        row[88..90].copy_from_slice(&(-1i16).to_le_bytes());
        for (axis, value) in coordinates.into_iter().enumerate() {
            row[94 + axis * 8..102 + axis * 8].copy_from_slice(&value.to_le_bytes());
        }
        for element in 0..9 {
            row[118 + element * 8..126 + element * 8]
                .copy_from_slice(&(element as f64 + index as f64 / 2.0).to_le_bytes());
        }
        row[190] = 255;
        row[191..195].copy_from_slice(&1.75f32.to_le_bytes());
        row[235..239].copy_from_slice(&123i32.to_le_bytes());
    }
    let file = record_file(PLACEMENT_DESCR, "(2,)", &payload);
    let records = RecordArray::read(&file).unwrap();
    records.require_scalar_floats(&["x", "y", "z"]).unwrap();
    assert_eq!(records.shape(), [2]);
    assert_eq!(records.record_size(), 239);
    assert_eq!(records.len(), 2);
    assert!(!records.is_empty());
    assert!(!records.fortran());
    assert_eq!(records.data(), payload);
    assert_eq!(
        records.data().as_ptr(),
        file[file.len() - payload.len()..].as_ptr()
    );
    assert_eq!(records.field("x").unwrap().offset, 94);
    assert_eq!(records.field("clear").unwrap().offset, 191);
    assert_eq!(records.field("tile").unwrap().offset, 235);
    let row = records.record(0).unwrap();
    assert_eq!(row.f64("x").unwrap(), 1.25);
    assert_eq!(row.f64("y").unwrap().to_bits(), (-0.0f64).to_bits());
    assert_eq!(row.f64("z").unwrap().to_bits(), 0x7ff8_0000_0000_0123);
    assert_eq!(row.string("layer").unwrap(), "brush");
    assert_eq!(row.string("kind").unwrap(), "plant");
    assert_eq!(row.field("plugin").unwrap().bytes(), &(-1i16).to_le_bytes());
    assert_eq!(row.f64("clear").unwrap(), 1.75);
    let matrix = records.record(1).unwrap().field("L").unwrap();
    assert_eq!(matrix.info().shape, [3, 3]);
    assert_eq!(matrix.info().size, 72);
    for element in 0..9 {
        assert_eq!(matrix.f64_at(element).unwrap(), element as f64 + 0.5);
    }
    assert!(matrix.as_f64().is_err());
    assert!(matrix.f64_at(9).is_err());
    assert!(matrix.f64_at(usize::MAX).is_err());
    assert!(row.f64("plugin").is_err());
    assert!(row.string("x").is_err());
    assert!(row.field("absent").is_err());
    assert!(records.record(2).is_err());
    assert!(records.record(usize::MAX).is_err());
}

#[test]
fn npy_record_field_order_padding_and_mixed_endian_are_checked() {
    let descr = "[('z', '>f4'), ('', '|V3'), ('layer', '|S6'), ('x', '<f8'), ('y', '=f8')]";
    let mut payload = vec![0u8; 29];
    payload[0..4].copy_from_slice(&(-3.5f32).to_be_bytes());
    payload[7..13].copy_from_slice(b"A\0B\0\0\0");
    payload[13..21].copy_from_slice(&5.25f64.to_le_bytes());
    payload[21..29].copy_from_slice(&(-0.0f64).to_ne_bytes());
    let file = record_file(descr, "()", &payload);
    let records = RecordArray::read(&file).unwrap();
    records.require_scalar_floats(&["x", "y", "z"]).unwrap();
    assert!(records.shape().is_empty());
    assert_eq!(records.len(), 1);
    assert_eq!(records.field("x").unwrap().offset, 13);
    assert!(records.field("").is_err());
    let row = records.record(0).unwrap();
    assert_eq!(row.f64("x").unwrap(), 5.25);
    assert_eq!(row.f64("z").unwrap(), -3.5);
    assert_eq!(row.f64("y").unwrap().to_bits(), (-0.0f64).to_bits());
    assert_eq!(row.string("layer").unwrap(), "A\0B");
}

#[test]
fn npy_record_unicode_strings_and_header_encodings_are_explicit() {
    let mut payload = Vec::new();
    for codepoint in ['é' as u32, 0, '🦊' as u32, 0] {
        payload.extend_from_slice(&codepoint.to_be_bytes());
    }
    let file = record_file("[('種類', '>U4')]", "(1,)", &payload);
    let records = RecordArray::read(&file).unwrap();
    assert_eq!(records.record(0).unwrap().string("種類").unwrap(), "é\0🦊");
    for invalid in [0xd800u32, 0x110000] {
        let file = record_file("[('text', '>U1')]", "(1,)", &invalid.to_be_bytes());
        assert!(
            RecordArray::read(&file)
                .unwrap()
                .record(0)
                .unwrap()
                .string("text")
                .is_err()
        );
    }
    let file = record_file("[('text', '|S1')]", "(1,)", &[0xff]);
    assert!(
        RecordArray::read(&file)
            .unwrap()
            .record(0)
            .unwrap()
            .string("text")
            .is_err()
    );
    // Version 1/2 field names are Latin-1, not UTF-8.
    for version in [1, 2] {
        let dictionary = "{'descr': [('é', '<f8')], 'shape': (1,), 'fortran_order': False}";
        let mut header: Vec<u8> = dictionary
            .chars()
            .map(|character| character as u8)
            .collect();
        let prefix_size = if version == 1 { 10 } else { 12 };
        let padding = (64 - (prefix_size + header.len() + 1) % 64) % 64;
        header.extend(std::iter::repeat_n(b' ', padding));
        header.push(b'\n');
        let mut bytes = b"\x93NUMPY".to_vec();
        bytes.extend_from_slice(&[version, 0]);
        if version == 1 {
            bytes.extend_from_slice(&(header.len() as u16).to_le_bytes());
        } else {
            bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
        }
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(&4.5f64.to_le_bytes());
        assert_eq!(
            RecordArray::read(&bytes)
                .unwrap()
                .record(0)
                .unwrap()
                .f64("é")
                .unwrap(),
            4.5
        );
        bytes[6] = 3;
        if version == 2 {
            assert!(RecordArray::read(&bytes).is_err());
        }
    }
}

#[test]
fn npy_record_empty_arrays_still_require_coordinate_schema() {
    let valid_file = record_file(PLACEMENT_DESCR, "(0,)", &[]);
    let valid = RecordArray::read(&valid_file).unwrap();
    assert!(valid.is_empty());
    assert!(valid.record(0).is_err());
    valid.require_scalar_floats(&["x", "y", "z"]).unwrap();
    for descr in [
        "[('x', '<f8'), ('y', '<f8')]",
        "[('x', '<i4'), ('y', '<f4'), ('z', '<f8')]",
        "[('x', '<f8', (1,)), ('y', '<f8'), ('z', '<f8')]",
    ] {
        let file = record_file(descr, "(0,)", &[]);
        assert!(
            RecordArray::read(&file)
                .unwrap()
                .require_scalar_floats(&["x", "y", "z"])
                .is_err()
        );
    }
}

#[test]
fn npy_record_truncation_bad_types_and_overflows_return_errors() {
    let good = record_file(PLACEMENT_DESCR, "(1,)", &[0; 239]);
    for end in 0..good.len() {
        assert!(RecordArray::read(&good[..end]).is_err(), "prefix {end}");
    }
    let mut extra = good;
    extra.push(0);
    assert!(RecordArray::read(&extra).is_err());
    for descr in [
        "[]",
        "[('x', '|O8')]",
        "[('x', '<f2')]",
        "[('x', '<c8')]",
        "[('x', '<M8[ns]')]",
        "[('x', '<é4')]",
        "[('x', '<f8'), ('x', '<f8')]",
        "[('', '<f8')]",
        "[('', '|V1', (2,))]",
        "[('x', [('nested', '<f8')])]",
        "[(('title', 'x'), '<f8')]",
        "[('x', '<f8', (2))]",
    ] {
        assert!(
            RecordArray::read(&record_file(descr, "(0,)", &[])).is_err(),
            "{descr}"
        );
    }
    for descr in [
        format!("[('x', '|V{}'), ('y', '|u1')]", usize::MAX),
        format!("[('x', '<U{}')]", usize::MAX),
        format!("[('x', '<f8', ({}, 2))]", usize::MAX),
    ] {
        assert!(RecordArray::read(&record_file(&descr, "(0,)", &[])).is_err());
    }
    assert!(
        RecordArray::read(&record_file(
            "[('x', '<f8')]",
            &format!("({}, 2)", usize::MAX),
            &[]
        ))
        .is_err()
    );
    assert!(
        RecordArray::read(&record_file(
            "[('x', '<f8')]",
            &format!("({},)", usize::MAX),
            &[]
        ))
        .is_err()
    );
    assert!(RecordArray::read(&Npy::from_f32(vec![0], &[]).write()).is_err());
}

#[test]
fn npy_multidimensional_records_expose_storage_order() {
    let payload: Vec<_> = [1.0f64, 3.0, 2.0, 4.0]
        .into_iter()
        .flat_map(f64::to_le_bytes)
        .collect();
    let file = npy_file(
        3,
        "{'fortran_order': True, 'descr': [('x', '<f8')], 'shape': (2, 2)}",
        &payload,
    );
    let records = RecordArray::read(&file).unwrap();
    assert!(records.fortran());
    assert_eq!(records.shape(), [2, 2]);
    assert_eq!(
        (0..4)
            .map(|index| records.record(index).unwrap().f64("x").unwrap())
            .collect::<Vec<_>>(),
        [1.0, 3.0, 2.0, 4.0]
    );
}

#[test]
fn npy_writer_retains_canonical_simple_array_bytes() {
    let values = [
        0.0,
        -0.0,
        1.5,
        -2.25,
        f32::INFINITY,
        f32::from_bits(0x7fc0_0123),
    ];
    let payload: Vec<_> = values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    let expected = npy_file(
        1,
        "{'descr': '<f4', 'fortran_order': False, 'shape': (2, 3), }",
        &payload,
    );
    let array = Npy::from_f32(vec![2, 3], &values);
    assert_eq!(array.write(), expected);
    assert_eq!(array.try_write().unwrap(), expected);
    let parsed = Npy::read(&expected).unwrap();
    assert_eq!(parsed.data, payload);
    assert_eq!(parsed.shape, [2, 3]);
    assert_eq!(
        parsed
            .f32s()
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>(),
        values
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        Npy::from_f64(vec![], &[1.25]).write(),
        npy_file(
            1,
            "{'descr': '<f8', 'fortran_order': False, 'shape': (), }",
            &1.25f64.to_le_bytes()
        )
    );
    assert_eq!(
        Npy::from_u8(vec![2], &[1, 255]).write(),
        npy_file(
            1,
            "{'descr': '|u1', 'fortran_order': False, 'shape': (2,), }",
            &[1, 255]
        )
    );
}

#[test]
fn npy_versions_and_field_order_are_independent() {
    for version in [1, 2, 3] {
        let file = npy_file(
            version,
            "{\"shape\": (2,), \"descr\": '<f8', 'fortran_order': False}",
            &[1.25f64.to_le_bytes(), (-3.0f64).to_le_bytes()].concat(),
        );
        let array = Npy::read(&file).unwrap();
        assert_eq!(array.descr, "<f8");
        assert_eq!(array.to_f32_c_order().unwrap(), [1.25, -3.0]);
    }
}

#[test]
fn npy_big_endian_and_fortran_grid_are_converted_explicitly() {
    let stored = [1.0f64, 4.0, 2.0, 5.0, 3.0, 6.0];
    let payload: Vec<_> = stored
        .iter()
        .flat_map(|value| value.to_be_bytes())
        .collect();
    let file = npy_file(
        3,
        "{'descr': '>f8', 'shape': (2, 3), 'fortran_order': True}",
        &payload,
    );
    let array = Npy::read(&file).unwrap();
    assert!(array.fortran);
    assert_eq!(array.f64s(), stored);
    assert_eq!(
        array.to_f32_c_order().unwrap(),
        [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]
    );

    let stored: Vec<_> = (0..24).map(|value| value as f32).collect();
    let mut cube = Npy::from_f32(vec![2, 3, 4], &stored);
    cube.fortran = true;
    let expected = [
        0.0, 6.0, 12.0, 18.0, 2.0, 8.0, 14.0, 20.0, 4.0, 10.0, 16.0, 22.0, 1.0, 7.0, 13.0, 19.0,
        3.0, 9.0, 15.0, 21.0, 5.0, 11.0, 17.0, 23.0,
    ];
    assert_eq!(cube.to_f32_c_order().unwrap(), expected);
}

#[test]
fn npy_empty_shapes_and_large_v2_headers_are_supported() {
    let array = Npy::from_f32(vec![usize::MAX, 2, 0], &[]);
    assert!(
        Npy::read(&array.write())
            .unwrap()
            .to_f32_c_order()
            .unwrap()
            .is_empty()
    );
    let wide = Npy::from_u8(vec![0; 24_000], &[]);
    let bytes = wide.write();
    assert_eq!(&bytes[6..8], &[2, 0]);
    assert_eq!(bytes.len() % 64, 0);
    assert_eq!(Npy::read(&bytes).unwrap(), wide);
}

#[test]
fn npy_truncation_and_malformed_headers_return_errors() {
    let good = Npy::from_f32(vec![2], &[1.0, 2.0]).write();
    for end in 0..good.len() {
        assert!(Npy::read(&good[..end]).is_err(), "prefix {end}");
    }
    for dictionary in [
        "{'descr': '<f4', 'shape': (1,), 'fortran_order': False}", // missing payload
        "{'descr': '<f4', 'shape': (-1,), 'fortran_order': False}",
        "{'descr': '<f4', 'shape': (1), 'fortran_order': False}",
        "{'descr': '<f4', 'shape': (1.0,), 'fortran_order': False}",
        "{'descr': '<f4', 'descr': '<f4', 'shape': (0,), 'fortran_order': False}",
        "{'descr': '<f4', 'shape': (0,), 'fortran_order': Maybe}",
        "{'descr': '|O8', 'shape': (0,), 'fortran_order': False}",
        "{'descr': [('field', '<f4')], 'shape': (0,), 'fortran_order': False}",
        "{'descr': '<é4', 'shape': (0,), 'fortran_order': False}",
        "{'descr': '<f4', 'shape': (0,), 'fortran_order': False, 'extra': 1}",
        "{'descr': '<f4', 'shape': (0,)}",
    ] {
        assert!(
            Npy::read(&npy_file(3, dictionary, &[])).is_err(),
            "{dictionary}"
        );
    }
    let overflow = format!(
        "{{'descr': '<f8', 'shape': ({}, 2), 'fortran_order': False}}",
        usize::MAX
    );
    assert!(Npy::read(&npy_file(1, &overflow, &[])).is_err());
    let mut bad = good.clone();
    bad[6] = 9;
    assert!(Npy::read(&bad).is_err());
    let mut bad = good.clone();
    bad[7] = 1;
    assert!(Npy::read(&bad).is_err());
    let mut bad = good.clone();
    bad[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
    assert!(Npy::read(&bad).is_err());
    let mut bad = good;
    bad.push(0);
    assert!(Npy::read(&bad).is_err());
    assert!(Npy::from_f32(vec![2], &[1.0]).try_write().is_err());
    assert!(Npy::from_u8(vec![1], &[1]).to_f32_c_order().is_err());
}

fn put_u32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn put_relative(bytes: &mut [u8], field: usize, record: usize, target: usize) {
    put_u32(bytes, field, (target as i32 - record as i32) as u32);
}

/// A handwritten valid height-preview FoxData tree, independent of our writers.
fn htre_file(version: u32) -> Vec<u8> {
    let data_start = 0x70;
    let mut bytes = vec![0; data_start + 4096 * 4];
    for sample in 0..4096 {
        put_u32(
            &mut bytes,
            data_start + sample * 4,
            (sample as f32).to_bits(),
        );
    }
    let mut strings = Vec::new();
    for name in ["terrainHighBlock", "heightMap", "pitch", "heightFormat"] {
        strings.push(bytes.len());
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
    }
    let length = bytes.len() as u32;
    put_u32(&mut bytes, 0, version);
    put_u32(&mut bytes, 4, 0x20);
    put_u32(&mut bytes, 8, length);
    put_relative(&mut bytes, 16, 12, strings[0]);
    put_relative(&mut bytes, 0x24, 0x20, strings[1]);
    put_relative(&mut bytes, 0x2c, 0x20, data_start);
    put_u32(&mut bytes, 0x30, 4096 * 4);
    put_relative(&mut bytes, 0x44, 0x20, 0x50);
    put_relative(&mut bytes, 0x58, 0x54, strings[2]);
    put_u32(&mut bytes, 0x5c, 2);
    if version == 4 {
        bytes[0x52..0x54].copy_from_slice(&16i16.to_le_bytes());
        put_relative(&mut bytes, 0x68, 0x64, strings[3]);
        put_u32(&mut bytes, 0x6c, 1);
    }
    bytes
}

#[test]
fn htre_versions_and_cluster_mapping_match_world_axes() {
    for version in [3, 4] {
        let tile = read_htre(&htre_file(version)).unwrap();
        assert_eq!(tile.version, version);
        assert_eq!(tile.pitch, 2);
        assert_eq!(tile.heights.len(), 4096);
        let grid = file_to_grid(&tile.heights).unwrap();
        assert_eq!(grid[0], 0.0);
        assert_eq!(grid[31], 31.0);
        assert_eq!(grid[32], 1024.0);
        assert_eq!(grid[32 * 64], 2048.0);
        assert_eq!(grid[32 * 64 + 32], 3072.0);
        assert_eq!(grid[4095], 4095.0);
        for row in 0..64 {
            for col in 0..64 {
                let expected = ((row / 32) * 2 + col / 32) * 1024 + (row % 32) * 32 + col % 32;
                assert_eq!(grid[row * 64 + col], expected as f32);
            }
        }
    }
    for count in [0, 1, 4095, 4097] {
        assert!(file_to_grid(&vec![0u8; count]).is_err());
    }
}

#[test]
fn htre_preserves_float_bits_and_follows_backward_references() {
    let mut bytes = htre_file(4);
    for (index, bits) in [0x8000_0000, 0x7fc0_0123, 0x7f80_0000, 0xff80_0000]
        .into_iter()
        .enumerate()
    {
        put_u32(&mut bytes, 0x70 + index * 4, bits);
    }
    let original_node: Vec<_> = bytes[0x20..0x50].to_vec();
    let child = bytes.len();
    bytes.extend_from_slice(&original_node);
    let height_name = bytes
        .windows(10)
        .position(|value| value == b"heightMap\0")
        .unwrap();
    let other_name = bytes.len();
    bytes.extend_from_slice(b"container\0");
    put_relative(&mut bytes, 0x24, 0x20, other_name);
    put_relative(&mut bytes, 0x38, 0x20, child);
    put_relative(&mut bytes, child + 4, child, height_name);
    put_relative(&mut bytes, child + 12, child, 0x70);
    put_relative(&mut bytes, child + 36, child, 0x50);
    put_relative(&mut bytes, child + 20, child, 0x20);
    let length = bytes.len() as u32;
    put_u32(&mut bytes, 8, length);
    let tile = read_htre(&bytes).unwrap();
    assert_eq!(
        tile.heights[..4]
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>(),
        [0x8000_0000, 0x7fc0_0123, 0x7f80_0000, 0xff80_0000]
    );
}

#[test]
fn htre_bad_ranges_cycles_and_payloads_return_errors() {
    let good = htre_file(4);
    for end in 0..good.len() {
        assert!(read_htre(&good[..end]).is_err(), "prefix {end}");
    }
    for (field, value) in [
        (0, 5),
        (4, 0),
        (8, 0),
        (16, i32::MIN as u32),
        (0x2c, i32::MAX as u32),
        (0x30, 16),
        (0x44, i32::MIN as u32),
        (0x38, 1),
        (0x6c, 2),
    ] {
        let mut bad = good.clone();
        put_u32(&mut bad, field, value);
        assert!(read_htre(&bad).is_err(), "field {field:#x}");
    }
    let mut cycle = good.clone();
    cycle[0x62..0x64].copy_from_slice(&(-16i16).to_le_bytes());
    assert!(read_htre(&cycle).unwrap_err().contains("cycle"));
    let mut cycle = good.clone();
    let child = cycle.len();
    cycle.resize(child + 48, 0);
    put_relative(&mut cycle, 0x38, 0x20, child);
    put_relative(&mut cycle, child + 24, child, 0x20);
    let length = cycle.len() as u32;
    put_u32(&mut cycle, 8, length);
    assert!(read_htre(&cycle).unwrap_err().contains("cycle"));
    let mut unterminated = good;
    *unterminated.last_mut().unwrap() = b'x';
    assert!(read_htre(&unterminated).is_err());
}

#[test]
fn htre_finished_nodes_can_be_reached_by_child_and_sibling_links() {
    let mut bytes = htre_file(4);
    let middle = bytes.len();
    let last = middle + 48;
    bytes.resize(last + 48, 0);
    put_relative(&mut bytes, 0x38, 0x20, last); // first discovery through child
    put_relative(&mut bytes, 0x40, 0x20, middle);
    put_relative(&mut bytes, middle + 32, middle, last); // same node through sibling chain
    let length = bytes.len() as u32;
    put_u32(&mut bytes, 8, length);
    assert_eq!(read_htre(&bytes).unwrap().heights.len(), 4096);
    // A back edge into the currently active path remains a directed cycle.
    put_relative(&mut bytes, last + 32, last, middle);
    assert!(read_htre(&bytes).is_err());
}

fn colour_block(first: u16, second: u16, indices: u32) -> Vec<u8> {
    [
        first.to_le_bytes().as_slice(),
        second.to_le_bytes().as_slice(),
        indices.to_le_bytes().as_slice(),
    ]
    .concat()
}

#[test]
fn bc1_four_colour_and_transparent_modes_have_known_pixels() {
    let four = colour_block(0xf800, 0, 0xe4e4_e4e4); // selectors 0,1,2,3 on every row
    let expected = [
        [255, 0, 0, 255],
        [0, 0, 0, 255],
        [170, 0, 0, 255],
        [85, 0, 0, 255],
    ]
    .concat();
    assert_eq!(decode_bc1(&four, 4, 4).unwrap(), expected.repeat(4));
    let transparent = colour_block(0, 0xf800, 0xe4e4_e4e4);
    let expected = [
        [0, 0, 0, 255],
        [255, 0, 0, 255],
        [127, 0, 0, 255],
        [0, 0, 0, 0],
    ]
    .concat();
    assert_eq!(decode_bc1(&transparent, 4, 4).unwrap(), expected.repeat(4));
    assert_eq!(
        decode_bc1(&colour_block(0x0841, 0x0841, u32::MAX), 1, 1).unwrap(),
        [0, 0, 0, 0]
    );
    assert_eq!(
        decode_bc1(&colour_block(0x0841, 0, 0), 1, 1).unwrap(),
        [8, 8, 8, 255]
    );
}

fn bc3_block(first: u8, second: u8) -> Vec<u8> {
    let mut indices = 0u64;
    for texel in 0..16 {
        indices |= ((texel % 8) as u64) << (3 * texel);
    }
    let mut block = vec![first, second];
    block.extend_from_slice(&indices.to_le_bytes()[..6]);
    block.extend(colour_block(0, 0xf800, u32::MAX));
    block
}

#[test]
fn bc3_alpha_modes_and_colour_mode_are_independent() {
    for (first, second, expected) in [
        (255, 0, [255, 0, 218, 182, 145, 109, 72, 36]),
        (10, 20, [10, 20, 12, 14, 16, 18, 0, 255]),
        (17, 17, [17, 17, 17, 17, 17, 17, 0, 255]),
    ] {
        let rgba = decode_bc3(&bc3_block(first, second), 4, 4).unwrap();
        for (texel, pixel) in rgba.chunks_exact(4).enumerate() {
            assert_eq!(&pixel[..3], &[170, 0, 0]); // four-colour despite c0 <= c1
            assert_eq!(pixel[3], expected[texel % 8]);
        }
    }
}

#[test]
fn block_images_crop_npot_edges_and_ignore_following_slices() {
    for has_alpha in [false, true] {
        let make = |colour| {
            let mut block = if has_alpha {
                vec![255, 255, 0, 0, 0, 0, 0, 0]
            } else {
                Vec::new()
            };
            block.extend(colour_block(colour, 0, 0));
            block
        };
        let data = [make(0xf800), make(0x07e0), make(0x001f), make(0xffff)].concat();
        let decode = if has_alpha { decode_bc3 } else { decode_bc1 };
        let rgba = decode(&data, 5, 5).unwrap();
        for y in 0..5 {
            for x in 0..5 {
                let expected = match (x / 4, y / 4) {
                    (0, 0) => [255, 0, 0, 255],
                    (1, 0) => [0, 255, 0, 255],
                    (0, 1) => [0, 0, 255, 255],
                    _ => [255; 4],
                };
                assert_eq!(&rgba[(y * 5 + x) * 4..(y * 5 + x + 1) * 4], &expected);
            }
        }
        assert_eq!(decode(&data, 1, 1).unwrap(), [255, 0, 0, 255]);
    }
}

#[test]
fn block_truncation_zero_sizes_and_overflows_return_errors() {
    for (decode, size) in [
        (
            decode_bc1 as fn(&[u8], usize, usize) -> Result<Vec<u8>, String>,
            8,
        ),
        (decode_bc3, 16),
    ] {
        let data = vec![0; size * 4];
        for end in 0..data.len() {
            assert!(decode(&data[..end], 5, 5).is_err());
        }
        assert!(decode(&[], 0, 4).is_err());
        assert!(decode(&[], 4, 0).is_err());
        assert!(decode(&[], usize::MAX, usize::MAX).is_err());
        assert!(decode(&[], usize::MAX / 4 + 1, 4).is_err());
    }
}
