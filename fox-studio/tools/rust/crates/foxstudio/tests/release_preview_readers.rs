//! Caller proof for the clean preview readers; synthetic files only, no graphics or installation.
use foxstudio::preview::{
    terrain,
    texture::{self, PixelFormat},
};
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "foxstudio_readers_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// Independent NPY v1 header, rather than using the reader's writer as its oracle.
fn npy(header: &str, payload: &[u8]) -> Vec<u8> {
    let mut header = header.as_bytes().to_vec();
    while !(10 + header.len() + 1).is_multiple_of(64) {
        header.push(b' ');
    }
    header.push(b'\n');
    let mut bytes = b"\x93NUMPY\x01\x00".to_vec();
    bytes.extend_from_slice(&(header.len() as u16).to_le_bytes());
    bytes.extend(header);
    bytes.extend_from_slice(payload);
    bytes
}

#[test]
fn clean_preview_npy_big_endian_fortran_has_correct_world_layout() {
    let fixture = Fixture::new();
    let file = fixture.0.join("terrain.npy");
    let payload: Vec<u8> = [1.0f64, 4.0, 2.0, 5.0, 3.0, 6.0]
        .into_iter()
        .flat_map(f64::to_be_bytes)
        .collect();
    let bytes = npy("{'descr': '>f8', 'fortran_order': True, 'shape': (2, 3), }", &payload);
    std::fs::write(&file, &bytes).unwrap();
    let land = terrain::load(&file, 2.0, Some((10.0, 20.0))).unwrap();
    assert_eq!((land.rows, land.cols), (2, 3));
    assert_eq!(land.h, [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
    assert_eq!(land.extent(), (10.0, 20.0, 14.0, 22.0));
    assert_eq!(land.height(12.0, 22.0), 5.0);
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
}

#[test]
fn clean_preview_unsupported_and_truncated_inputs_keep_path_context() {
    let fixture = Fixture::new();
    let file = fixture.0.join("unsupported.npy");
    std::fs::write(
        &file,
        npy("{'descr': '<i4', 'fortran_order': False, 'shape': (2, 2), }", &[0; 16]),
    )
    .unwrap();
    let error = terrain::load(&file, 1.0, None).unwrap_err();
    assert!(error.contains("unsupported.npy") && error.contains("float"), "{error}");
    let bad = fixture.0.join("truncated.htre");
    std::fs::write(&bad, [4, 0, 0, 0]).unwrap();
    let error = terrain::load(&bad, 1.0, None).unwrap_err();
    assert!(error.contains("truncated.htre"), "{error}");
}

#[test]
fn clean_preview_block_textures_crop_edges_and_preserve_alpha() {
    let block = [0u8, 248, 31, 0, 4, 0, 0, 0];
    let image = texture::decode(PixelFormat::Bc1, &block, 3, 2).unwrap();
    assert_eq!(image.rgba.len(), 24);
    assert_eq!(&image.rgba[..8], &[255, 0, 0, 255, 0, 0, 255, 255]);
    let mut block = vec![255, 0, 8, 0, 0, 0, 0, 0, 0, 248, 31, 0, 0, 0, 0, 0];
    block.extend([99; 16]); // A subsequent slice is deliberately ignored.
    let image = texture::decode(PixelFormat::Bc3, &block, 3, 2).unwrap();
    assert_eq!(image.rgba.len(), 24);
    assert_eq!(&image.rgba[..8], &[255, 0, 0, 255, 255, 0, 0, 0]);
    assert!(texture::decode(PixelFormat::Bc3, &block[..15], 3, 2).is_err());
}
