//! Public synthetic staging fixtures; no game installation or repository assets.
use foxinstall::{mgsv::ModPackage, mgsvpack::write_mgsv};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const METADATA: &[u8] = br#"<ModEntry Name="Package safety fixture" Version="1" Author="Tests"><Description>Authored synthetic files.</Description></ModEntry>"#;
const ASSET: &[u8] = b"-- authored synthetic Lua fixture\nreturn { test = true }\n";
const LOOSE: &[u8] = b"authored synthetic loose file\n";

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "fox-mgsv-safety-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let stage = root.join("stage");
        fs::create_dir_all(stage.join("Assets")).unwrap();
        fs::create_dir(stage.join("GameDir")).unwrap();
        fs::write(stage.join("metadata.xml"), METADATA).unwrap();
        fs::write(stage.join("Assets/synthetic.lua"), ASSET).unwrap();
        fs::write(stage.join("GameDir/notes.txt"), LOOSE).unwrap();
        Self(root)
    }

    fn stage(&self) -> PathBuf {
        self.0.join("stage")
    }

    fn assert_inputs(&self) {
        assert_eq!(
            fs::read(self.stage().join("metadata.xml")).unwrap(),
            METADATA
        );
        assert_eq!(
            fs::read(self.stage().join("Assets/synthetic.lua")).unwrap(),
            ASSET
        );
        assert_eq!(
            fs::read(self.stage().join("GameDir/notes.txt")).unwrap(),
            LOOSE
        );
    }

    fn assert_package(path: &Path) {
        let mut package = ModPackage::open(path).unwrap();
        assert_eq!(package.name, "Package safety fixture");
        assert_eq!(package.archive_files.len(), 1);
        assert_eq!(package.loose_files.len(), 1);
        assert_eq!(package.read("Assets/synthetic.lua").unwrap(), ASSET);
        assert_eq!(package.read("GameDir/notes.txt").unwrap(), LOOSE);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn metadata_cannot_be_replaced_by_its_package() {
    let fixture = Fixture::new();
    let output = fixture.stage().join("metadata.xml");
    let error = write_mgsv(&fixture.stage(), &output, 6).err().unwrap();
    assert!(error.contains("staging input"), "{error}");
    fixture.assert_inputs();
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 1);
}

#[test]
fn archive_and_loose_inputs_cannot_be_replaced_by_the_package() {
    let fixture = Fixture::new();
    for relative in ["Assets/synthetic.lua", "GameDir/notes.txt"] {
        let error = write_mgsv(&fixture.stage(), &fixture.stage().join(relative), 6)
            .err()
            .unwrap();
        assert!(error.contains("staging input"), "{error}");
        fixture.assert_inputs();
    }
}

#[test]
fn a_preexisting_fixed_temporary_file_is_preserved() {
    let fixture = Fixture::new();
    let output = fixture.0.join("package.mgsv");
    let legacy_temp = output.with_extension("mgsv.foxtmp");
    fs::write(&legacy_temp, b"unrelated preexisting bytes").unwrap();
    let stats = write_mgsv(&fixture.stage(), &output, 6).unwrap();
    assert_eq!(stats.files, 3);
    assert_eq!(
        fs::read(&legacy_temp).unwrap(),
        b"unrelated preexisting bytes"
    );
    Fixture::assert_package(&output);
    fixture.assert_inputs();
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 3);
}

#[test]
fn a_failed_staging_open_preserves_output_and_the_unowned_temporary_file() {
    let fixture = Fixture::new();
    let output = fixture.0.join("package.mgsv");
    let legacy_temp = output.with_extension("mgsv.foxtmp");
    fs::write(&output, b"original package").unwrap();
    fs::write(&legacy_temp, b"unrelated operation").unwrap();
    fs::remove_file(fixture.stage().join("metadata.xml")).unwrap();
    assert!(write_mgsv(&fixture.stage(), &output, 6).is_err());
    assert_eq!(fs::read(&output).unwrap(), b"original package");
    assert_eq!(fs::read(&legacy_temp).unwrap(), b"unrelated operation");
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 3);
}

#[test]
fn repeated_packages_are_deterministic_and_reopen_with_exact_payloads() {
    let fixture = Fixture::new();
    let first = fixture.0.join("first.mgsv");
    let second = fixture.0.join("second.mgsv");
    let stats = write_mgsv(&fixture.stage(), &first, 6).unwrap();
    write_mgsv(&fixture.stage(), &second, 6).unwrap();
    let bytes = fs::read(&first).unwrap();
    assert_eq!(bytes, fs::read(&second).unwrap());
    assert_eq!(stats.bytes_out, bytes.len() as u64);
    assert_eq!(stats.inputs.len(), 3);
    assert!(stats.inputs.contains(&fixture.stage().join("metadata.xml")));
    Fixture::assert_package(&first);
    fixture.assert_inputs();
}

#[test]
fn rejected_compression_level_preserves_the_existing_output() {
    let fixture = Fixture::new();
    let output = fixture.0.join("package.mgsv");
    fs::write(&output, b"original package").unwrap();
    let error = write_mgsv(&fixture.stage(), &output, 10).err().unwrap();
    assert!(error.contains("compression level"), "{error}");
    assert_eq!(fs::read(&output).unwrap(), b"original package");
    fixture.assert_inputs();
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 2);
}

#[cfg(unix)]
#[test]
fn a_parent_link_alias_cannot_replace_metadata() {
    let fixture = Fixture::new();
    let alias = fixture.0.join("stage-alias");
    std::os::unix::fs::symlink(fixture.stage(), &alias).unwrap();
    let error = write_mgsv(&fixture.stage(), &alias.join("metadata.xml"), 6)
        .err()
        .unwrap();
    assert!(error.contains("staging input"), "{error}");
    fixture.assert_inputs();
}

#[cfg(unix)]
#[test]
fn replacing_an_output_hardlink_preserves_the_staging_inode() {
    let fixture = Fixture::new();
    let output = fixture.0.join("hardlink.mgsv");
    fs::hard_link(fixture.stage().join("Assets/synthetic.lua"), &output).unwrap();
    write_mgsv(&fixture.stage(), &output, 6).unwrap();
    Fixture::assert_package(&output);
    fixture.assert_inputs();
}
