//! Data-preservation checks through the public command boundary.
use foxcore::{
    fpk, qar,
    runtime_data::{QarKeys, RuntimeProfile},
};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "fox-runtime-command-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn source_archive(fixture: &Fixture) -> (qar::Context, Vec<u8>, PathBuf) {
    let context = qar::Context::new(QarKeys {
        header_masks: [0xa152_34f8, 0xb261_78ac, 0xc392_46be, 0xd483_25e0],
        layer1: [
            0x12345678, 0x90abcdef, 0x10203040, 0x50607080, 0x90807060, 0xaabbccdd, 0x2468ace0,
            0x13579bdf,
        ],
    });
    let mut entries = Vec::new();
    for kind in [fpk::Kind::Fpk, fpk::Kind::Fpkd] {
        for index in 0..8 {
            let package = fpk::write_in_order(
                kind,
                &[("/fixture/a.alpha", b"a"), ("/fixture/b.beta", b"b")],
                &[],
            );
            let extension = if kind == fpk::Kind::Fpk {
                "fpk"
            } else {
                "fpkd"
            };
            let hash = (qar::ext_id(extension) << 51) | index;
            let (_, raw) = context.encode_plain(hash, &package).unwrap();
            entries.push(qar::RawEntry {
                hash,
                raw,
                pad: None,
            });
        }
    }
    let mut bytes = Vec::new();
    context.write_archive(&mut bytes, 0, 1, &entries).unwrap();
    let game = fixture.0.join("game");
    std::fs::create_dir_all(game.join("master")).unwrap();
    std::fs::write(game.join("master/data1.dat"), &bytes).unwrap();
    let profile = RuntimeProfile::learn(&game, &mut |_, _| true).unwrap();
    let profile_path = fixture.0.join("profile.json");
    profile.save(&profile_path).unwrap();
    (context, bytes, profile_path)
}

fn rebuild(profile: &Path, input: &Path, output: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fox"))
        .arg("--runtime-profile")
        .arg(profile)
        .args(["qar", "rebuild"])
        .arg(input)
        .arg(output)
        .arg("--keep-padding")
        .env_remove("FOX_RUNTIME_PROFILE")
        .output()
        .unwrap()
}

#[test]
fn setup_cannot_replace_a_file_inside_the_selected_game() {
    let fixture = Fixture::new();
    let game = fixture.0.join("game");
    std::fs::create_dir_all(game.join("master")).unwrap();
    let archive = game.join("master/data1.dat");
    std::fs::write(&archive, b"preserved game bytes").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_fox"))
        .args(["setup", "--game"])
        .arg(&game)
        .arg("--out")
        .arg(&archive)
        .env_remove("FOX_RUNTIME_PROFILE")
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("outside the game"));
    assert_eq!(std::fs::read(&archive).unwrap(), b"preserved game bytes");
}

#[test]
fn rebuild_uses_physical_neighbors_when_the_index_order_differs() {
    let fixture = Fixture::new();
    let (context, mut bytes, profile) = source_archive(&fixture);
    let index = context.read_index(&mut Cursor::new(&bytes)).unwrap();
    let mut sections = index.sections.clone();
    sections.reverse();
    let table = context.encode_sections(&sections);
    bytes[32..32 + table.len()].copy_from_slice(&table);
    let input = fixture.0.join("reordered.dat");
    let output = fixture.0.join("rebuilt.dat");
    std::fs::write(&input, &bytes).unwrap();
    let result = rebuild(&profile, &input, &output);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let rebuilt = std::fs::read(&output).unwrap();
    let rebuilt_index = context.read_index(&mut Cursor::new(&rebuilt)).unwrap();
    assert_eq!(rebuilt_index.entries.len(), index.entries.len());
    for entry in &rebuilt_index.entries {
        let original = index
            .entries
            .iter()
            .find(|original| original.hash == entry.hash)
            .unwrap();
        let read = |data: &[u8], entry: &qar::Entry| {
            let start = entry.offset as usize + 32;
            context
                .decode(entry, &data[start..start + entry.stored as usize])
                .unwrap()
        };
        assert_eq!(read(&rebuilt, entry), read(&bytes, original));
    }
    assert_eq!(std::fs::read(&input).unwrap(), bytes);
}

#[test]
fn unsupported_extra_records_leave_existing_output_untouched() {
    let fixture = Fixture::new();
    let (context, mut bytes, profile) = source_archive(&fixture);
    let mut index = context.read_index(&mut Cursor::new(&bytes)).unwrap();
    index.header.extra_count = 1;
    bytes[..32].copy_from_slice(&context.header_bytes(&index.header));
    let extra_start = 32 + index.sections.len() * 8;
    bytes[extra_start..extra_start + 16].copy_from_slice(&[0x42; 16]);
    let input = fixture.0.join("extra.dat");
    let output = fixture.0.join("existing.dat");
    std::fs::write(&input, &bytes).unwrap();
    std::fs::write(&output, b"existing output").unwrap();
    let result = rebuild(&profile, &input, &output);
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("extra records"));
    assert_eq!(std::fs::read(&output).unwrap(), b"existing output");
    assert_eq!(std::fs::read(&input).unwrap(), bytes);
}

#[test]
fn rebuild_rejects_its_own_input_as_the_output() {
    let fixture = Fixture::new();
    let (_, bytes, profile) = source_archive(&fixture);
    let input = fixture.0.join("same.dat");
    std::fs::write(&input, &bytes).unwrap();
    let result = rebuild(&profile, &input, &input);
    assert_eq!(result.status.code(), Some(2));
    assert_eq!(std::fs::read(&input).unwrap(), bytes);
}

#[test]
fn checksum_mismatch_is_not_successful_verification_or_equal_content() {
    let fixture = Fixture::new();
    let (context, bytes, profile) = source_archive(&fixture);
    let index = context.read_index(&mut Cursor::new(&bytes)).unwrap();
    let mut changed = bytes.clone();
    changed[index.entries[0].offset as usize + 32 + 16] ^= 1;
    let original = fixture.0.join("original.dat");
    let damaged = fixture.0.join("damaged.dat");
    std::fs::write(&original, bytes).unwrap();
    std::fs::write(&damaged, changed).unwrap();
    let verify = Command::new(env!("CARGO_BIN_EXE_fox"))
        .arg("--runtime-profile")
        .arg(&profile)
        .args(["qar", "verify"])
        .arg(&damaged)
        .output()
        .unwrap();
    assert_eq!(verify.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&verify.stderr).contains("checksum"));
    let diff = Command::new(env!("CARGO_BIN_EXE_fox"))
        .arg("--runtime-profile")
        .arg(&profile)
        .args(["qar", "diff"])
        .arg(&original)
        .arg(&damaged)
        .output()
        .unwrap();
    assert!(diff.status.success());
    assert!(String::from_utf8_lossy(&diff.stdout).contains("changed 1,"));
}

#[test]
fn qar_extraction_cannot_replace_its_source_archive() {
    let fixture = Fixture::new();
    let (context, _, profile) = source_archive(&fixture);
    let hash = qar::file_hash("/source.fpk");
    let (_, raw) = context.encode_plain(hash, b"replacement").unwrap();
    let mut bytes = Vec::new();
    context
        .write_archive(
            &mut bytes,
            0,
            1,
            &[qar::RawEntry {
                hash,
                raw,
                pad: None,
            }],
        )
        .unwrap();
    let input = fixture.0.join("source.fpk");
    let dictionary = fixture.0.join("dictionary.txt");
    std::fs::write(&input, &bytes).unwrap();
    std::fs::write(&dictionary, "/source.fpk\n").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_fox"))
        .arg("--runtime-profile")
        .arg(&profile)
        .args(["qar", "extract"])
        .arg(&input)
        .arg(&fixture.0)
        .arg("--dict")
        .arg(&dictionary)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert_eq!(std::fs::read(&input).unwrap(), bytes);
}

#[test]
fn undersized_decoded_payload_cannot_replace_existing_output() {
    let fixture = Fixture::new();
    let (context, _, profile) = source_archive(&fixture);
    // Complete zlib stream for "abc", deliberately declared as four decoded bytes.
    let stream = [
        0x78, 0x9c, 0x4b, 0x4c, 0x4a, 0x06, 0, 0x02, 0x4d, 0x01, 0x27,
    ];
    let hash = qar::ext_id("fpk") << 51;
    let (mut entry, mut raw) = context.encode_plain(hash, &stream).unwrap();
    entry.uncompressed = 4;
    raw[..32].copy_from_slice(&context.entry_header_bytes(&entry));
    let mut bytes = Vec::new();
    context
        .write_archive(
            &mut bytes,
            0,
            1,
            &[qar::RawEntry {
                hash,
                raw,
                pad: None,
            }],
        )
        .unwrap();
    let input = fixture.0.join("undersized.dat");
    let output = fixture.0.join("extracted");
    std::fs::write(&input, bytes).unwrap();
    std::fs::create_dir(&output).unwrap();
    std::fs::write(output.join("0.fpk"), b"existing payload").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_fox"))
        .arg("--runtime-profile")
        .arg(&profile)
        .args(["qar", "extract"])
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("shorter than its declared size"));
    assert_eq!(
        std::fs::read(output.join("0.fpk")).unwrap(),
        b"existing payload"
    );
}

#[test]
fn rebuild_cannot_overwrite_its_selected_runtime_profile() {
    let fixture = Fixture::new();
    let (_, archive, profile) = source_archive(&fixture);
    let before = std::fs::read(&profile).unwrap();
    let input = fixture.0.join("source.dat");
    std::fs::write(&input, &archive).unwrap();
    let result = rebuild(&profile, &input, &profile);
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("input"));
    assert_eq!(std::fs::read(&profile).unwrap(), before);
    assert_eq!(std::fs::read(&input).unwrap(), archive);
}

#[test]
fn dictionary_cannot_overwrite_its_environment_runtime_profile() {
    let fixture = Fixture::new();
    let (_, archive, profile) = source_archive(&fixture);
    let before = std::fs::read(&profile).unwrap();
    let input = fixture.0.join("source.dat");
    std::fs::write(&input, &archive).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_fox"))
        .args(["dict", "build", "--out"])
        .arg(&profile)
        .arg(&input)
        .env("FOX_RUNTIME_PROFILE", &profile)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("input"));
    assert_eq!(std::fs::read(&profile).unwrap(), before);
}

#[test]
fn extraction_preflights_the_runtime_profile_before_any_payload_write() {
    let fixture = Fixture::new();
    let (context, _, original_profile) = source_archive(&fixture);
    let output = fixture.0.join("extracted");
    std::fs::create_dir(&output).unwrap();
    let profile = output.join("protected.lua");
    std::fs::copy(&original_profile, &profile).unwrap();
    let before = std::fs::read(&profile).unwrap();
    let mut entries = Vec::new();
    for name in ["first.lua", "protected.lua"] {
        let hash = qar::file_hash(name);
        let (_, raw) = context.encode_plain(hash, b"replacement").unwrap();
        entries.push(qar::RawEntry {
            hash,
            raw,
            pad: None,
        });
    }
    let mut archive = Vec::new();
    context.write_archive(&mut archive, 0, 1, &entries).unwrap();
    let input = fixture.0.join("profile-collision.dat");
    std::fs::write(&input, archive).unwrap();
    let dictionary = fixture.0.join("names.txt");
    std::fs::write(&dictionary, "first.lua\nprotected.lua\n").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_fox"))
        .arg("--runtime-profile")
        .arg(&profile)
        .args(["qar", "extract"])
        .arg(&input)
        .arg(&output)
        .arg("--dict")
        .arg(&dictionary)
        .env_remove("FOX_RUNTIME_PROFILE")
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("input"));
    assert_eq!(std::fs::read(&profile).unwrap(), before);
    assert!(!output.join("first.lua").exists());
    assert_eq!(std::fs::read_dir(&output).unwrap().count(), 1);
}

#[test]
fn mutating_mod_commands_reject_a_profile_inside_the_target_game() {
    let fixture = Fixture::new();
    let (_, archive, profile) = source_archive(&fixture);
    let target = fixture.0.join("game");
    let nested_profile = target.join("profile.json");
    std::fs::copy(profile, &nested_profile).unwrap();
    let before = std::fs::read(&nested_profile).unwrap();
    for action in ["setup", "install", "uninstall", "layout"] {
        let result = Command::new(env!("CARGO_BIN_EXE_fox"))
            .arg("--runtime-profile")
            .arg(&nested_profile)
            .args(["mod", action])
            .arg("unused")
            .arg("--game")
            .arg(&target)
            .env_remove("FOX_RUNTIME_PROFILE")
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&result.stderr).contains("outside the target game"));
        assert_eq!(std::fs::read(&nested_profile).unwrap(), before);
        assert_eq!(
            std::fs::read(target.join("master/data1.dat")).unwrap(),
            archive
        );
    }
    assert_eq!(std::fs::read_dir(target).unwrap().count(), 2);
}
