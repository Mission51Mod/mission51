//! Exercise the installed command boundary using only synthetic containers.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct Scratch {
    parent: PathBuf,
    path: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = std::env::temp_dir();
        let path = parent.join(format!(
            "fox-cli-release-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("create a new fixture folder without replacing anything");
        Self { parent, path }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        assert_eq!(self.path.parent(), Some(self.parent.as_path()));
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn unpack(input: &Path, output: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fox"))
        .arg("unpack")
        .arg(input)
        .arg(output)
        .env_remove("FOX_RUNTIME_PROFILE")
        .output()
        .expect("run fox unpack")
}

#[test]
fn normal_package_extracts_its_original_payload_and_definition() {
    let scratch = Scratch::new();
    let input = scratch.path.join("synthetic.fpk");
    let output = scratch.path.join("extracted");
    let bytes = foxcore::fpk::write_in_order(
        foxcore::fpk::Kind::Fpk,
        &[("/Assets/example.txt", b"synthetic payload")],
        &[],
    );
    std::fs::write(&input, bytes).unwrap();
    let result = unpack(&input, &output);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        std::fs::read(output.join("Assets/example.txt")).unwrap(),
        b"synthetic payload"
    );
    let definition: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output.with_extension("json")).unwrap()).unwrap();
    assert_eq!(definition["type"], "fpk");
    assert_eq!(definition["entries"][0]["filePath"], "/Assets/example.txt");
}

#[test]
fn traversal_is_rejected_before_any_archive_payload_is_written() {
    let scratch = Scratch::new();
    let input = scratch.path.join("unsafe.fpk");
    let output = scratch.path.join("extracted");
    let bytes = foxcore::fpk::write_in_order(
        foxcore::fpk::Kind::Fpk,
        &[
            ("/Assets/first.txt", b"first"),
            ("../escaped.txt", b"escaped"),
        ],
        &[],
    );
    std::fs::write(&input, bytes).unwrap();
    let result = unpack(&input, &output);
    assert_eq!(result.status.code(), Some(2));
    assert!(!output.exists());
    assert!(!scratch.path.join("escaped.txt").exists());
}

#[test]
fn case_aliases_cannot_silently_replace_an_extracted_entry() {
    let scratch = Scratch::new();
    let input = scratch.path.join("aliases.fpk");
    let output = scratch.path.join("extracted");
    let bytes = foxcore::fpk::write_in_order(
        foxcore::fpk::Kind::Fpk,
        &[("/Assets/a.txt", b"one"), ("/assets/A.txt", b"two")],
        &[],
    );
    std::fs::write(&input, bytes).unwrap();
    let result = unpack(&input, &output);
    assert_eq!(result.status.code(), Some(2));
    assert!(!output.exists());
}

#[test]
fn malformed_definition_has_a_useful_error_and_no_partial_output() {
    let scratch = Scratch::new();
    let input = scratch.path.join("malformed.json");
    let output = scratch.path.join("out.pftxs");
    std::fs::write(&input, br#"{"type":"pftxs","head":[],"blocks":[]}"#).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_fox"))
        .arg("pack")
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("exactly three"));
    assert!(!output.exists());
}

#[test]
fn file_hash_matches_the_published_empty_blake3_vector() {
    let scratch = Scratch::new();
    let input = scratch.path.join("empty");
    std::fs::write(&input, []).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_fox"))
        .args(["hash", "file"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(
        String::from_utf8(result.stdout).unwrap().trim(),
        "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
    );
}

#[test]
fn sound_package_checks_later_output_names_before_writing_the_first() {
    let scratch = Scratch::new();
    let input = scratch.path.join("unsafe.sbp");
    let output = scratch.path.join("extracted");
    let bytes = foxcore::containers::sbp_write(&foxcore::containers::Sbp {
        header_pad: 0,
        entries: vec![
            (*b"bnk\0", b"valid".to_vec()),
            (*b"bad:", b"invalid".to_vec()),
        ],
    })
    .unwrap();
    std::fs::write(&input, bytes).unwrap();
    let result = unpack(&input, &output);
    assert_eq!(result.status.code(), Some(2));
    assert!(!output.exists());
}

#[test]
fn missing_roundtrip_folder_is_an_error_instead_of_a_successful_empty_proof() {
    let scratch = Scratch::new();
    let result = Command::new(env!("CARGO_BIN_EXE_fox"))
        .args(["roundtrip", "sbp"])
        .arg(scratch.path.join("missing"))
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("missing"));
}

#[test]
fn extraction_cannot_replace_its_source_archive() {
    let scratch = Scratch::new();
    let input = scratch.path.join("source.fpk");
    let bytes = foxcore::fpk::write_in_order(
        foxcore::fpk::Kind::Fpk,
        &[("/first.txt", b"first"), ("/source.fpk", b"replacement")],
        &[],
    );
    std::fs::write(&input, &bytes).unwrap();
    let result = unpack(&input, &scratch.path);
    assert_eq!(result.status.code(), Some(2));
    assert_eq!(std::fs::read(&input).unwrap(), bytes);
    assert!(!scratch.path.join("first.txt").exists());
}

#[test]
fn file_and_directory_conflicts_are_rejected_before_extraction() {
    let scratch = Scratch::new();
    let input = scratch.path.join("conflict.fpk");
    let output = scratch.path.join("extracted");
    let bytes = foxcore::fpk::write_in_order(
        foxcore::fpk::Kind::Fpk,
        &[("/Assets/a", b"file"), ("/assets/A/b.txt", b"child")],
        &[],
    );
    std::fs::write(&input, bytes).unwrap();
    let result = unpack(&input, &output);
    assert_eq!(result.status.code(), Some(2));
    assert!(!output.exists());
}

#[test]
fn invalid_definition_destination_is_rejected_before_payload_writes() {
    let scratch = Scratch::new();
    let input = scratch.path.join("source.fpk");
    let output = scratch.path.join("extracted");
    let bytes = foxcore::fpk::write_in_order(
        foxcore::fpk::Kind::Fpk,
        &[("/payload.txt", b"new content")],
        &[],
    );
    std::fs::write(&input, bytes).unwrap();
    std::fs::create_dir(output.with_extension("json")).unwrap();
    let result = unpack(&input, &output);
    assert_eq!(result.status.code(), Some(2));
    assert!(!output.exists());
}

#[cfg(unix)]
#[test]
fn definition_sidecar_cannot_follow_a_link_outside_the_selected_folder() {
    let scratch = Scratch::new();
    let input = scratch.path.join("source.fpk");
    let output = scratch.path.join("extracted");
    let original = scratch.path.join("original.json");
    let bytes = foxcore::fpk::write_in_order(
        foxcore::fpk::Kind::Fpk,
        &[("/payload.txt", b"new content")],
        &[],
    );
    std::fs::write(&input, bytes).unwrap();
    std::fs::write(&original, b"original metadata").unwrap();
    std::os::unix::fs::symlink(&original, output.with_extension("json")).unwrap();
    let result = unpack(&input, &output);
    assert_eq!(result.status.code(), Some(2));
    assert_eq!(std::fs::read(original).unwrap(), b"original metadata");
    assert!(!output.exists());
}

#[test]
fn replacing_an_extracted_hard_link_preserves_the_source_inode() {
    let scratch = Scratch::new();
    let input = scratch.path.join("source.fpk");
    let output = scratch.path.join("extracted");
    let bytes =
        foxcore::fpk::write_in_order(foxcore::fpk::Kind::Fpk, &[("/linked.fpk", b"payload")], &[]);
    std::fs::write(&input, &bytes).unwrap();
    std::fs::create_dir(&output).unwrap();
    std::fs::hard_link(&input, output.join("linked.fpk")).unwrap();
    let result = unpack(&input, &output);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(std::fs::read(input).unwrap(), bytes);
    assert_eq!(
        std::fs::read(output.join("linked.fpk")).unwrap(),
        b"payload"
    );
}

#[test]
fn packing_cannot_replace_its_definition_or_payload() {
    let scratch = Scratch::new();
    let definition = scratch.path.join("package.json");
    let folder = scratch.path.join("package");
    let payload = folder.join("payload.bnk");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(&payload, b"original bank").unwrap();
    let source = br#"{"type":"sbp","entries":[{"tag":"bnk","file":"payload.bnk"}]}"#;
    std::fs::write(&definition, source).unwrap();
    for output in [&definition, &payload] {
        let result = Command::new(env!("CARGO_BIN_EXE_fox"))
            .arg("pack")
            .arg(&definition)
            .arg(output)
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert_eq!(std::fs::read(&definition).unwrap(), source);
        assert_eq!(std::fs::read(&payload).unwrap(), b"original bank");
    }
}

#[test]
fn sound_package_count_limit_is_checked_before_replacing_output() {
    let scratch = Scratch::new();
    let payload = scratch.path.join("payload.bin");
    std::fs::write(&payload, b"sound payload").unwrap();
    let definition = scratch.path.join("sound.json");
    let output = scratch.path.join("sound.sbp");
    for count in [255, 256] {
        let entries = vec![serde_json::json!({"tag":"bnk","file":"payload.bin"}); count];
        let data = serde_json::json!({"type":"sbp","folder":".","entries":entries});
        std::fs::write(&definition, serde_json::to_vec(&data).unwrap()).unwrap();
        let before = std::fs::read(&output).ok();
        let result = Command::new(env!("CARGO_BIN_EXE_fox"))
            .arg("pack")
            .arg(&definition)
            .arg(&output)
            .env_remove("FOX_RUNTIME_PROFILE")
            .output()
            .unwrap();
        if count == 255 {
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let packed = foxcore::containers::sbp_read(&std::fs::read(&output).unwrap()).unwrap();
            assert_eq!(packed.entries.len(), 255);
            assert!(
                packed
                    .entries
                    .iter()
                    .all(|(_, bytes)| bytes == b"sound payload")
            );
        } else {
            assert_eq!(result.status.code(), Some(2));
            assert!(String::from_utf8_lossy(&result.stderr).contains("255"));
            assert_eq!(std::fs::read(&output).unwrap(), before.unwrap());
        }
    }
    assert_eq!(std::fs::read(payload).unwrap(), b"sound payload");
}
