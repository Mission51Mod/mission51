//! Synthetic metadata/UI fixtures. No install, build stage, window or GPU is launched.
#[path = "../src/mods_conflicts.rs"]
mod mods_conflicts;
#[path = "../src/output_diff.rs"]
mod output_diff;

use egui_kittest::{Harness, kittest::Queryable};
use foxbuild::{StageRecord, State};
use foxinstall::{FileItem, InstalledMod, LooseItem, Manifest, PackItem};
use mods_conflicts::{ConflictReport, ConflictsView, ContentComparison, Resolution};
use output_diff::{
    ChangeKind, MetadataIssue, OutputDiff, OutputDiffView, OutputSnapshot, SnapshotSource,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn source(label: &str) -> SnapshotSource {
    SnapshotSource {
        root: PathBuf::from("H:/mission51"),
        graph: PathBuf::from("tools/build/stages.toml"),
        state_file: PathBuf::from("work/build/state.json"),
        label: label.into(),
    }
}

fn record(finished: &str, outputs: &[(&str, &str)]) -> StageRecord {
    StageRecord {
        ok: true,
        trace_complete: true,
        cmd_fp: "command-hash".into(),
        finished: finished.into(),
        outputs: outputs
            .iter()
            .map(|(path, hash)| ((*path).into(), (*hash).into()))
            .collect(),
        rust_stamp: Some("native-tools-stamp".into()),
        fallbacks: vec!["codec: not ported".into()],
        ..StageRecord::default()
    }
}

fn snapshot(label: &str, stages: &[(&str, StageRecord)]) -> OutputSnapshot {
    let state = State {
        stages: stages
            .iter()
            .map(|(name, record)| ((*name).into(), record.clone()))
            .collect(),
    };
    OutputSnapshot::from_state(source(label), &state)
}

#[test]
fn recorded_outputs_have_all_four_change_kinds_and_provenance() {
    let before = snapshot(
        "Before",
        &[(
            "pack",
            record("run-1", &[("keep", "a"), ("change", "b"), ("gone", "c")]),
        )],
    );
    let after = snapshot(
        "After",
        &[(
            "pack",
            record("run-2", &[("keep", "a"), ("change", "z"), ("new", "d")]),
        )],
    );
    let diff = OutputDiff::compare(Some(&before), Some(&after));
    let kinds: BTreeMap<_, _> = diff
        .rows
        .iter()
        .map(|row| (row.path.as_str(), row.kind))
        .collect();
    assert_eq!(
        kinds,
        BTreeMap::from([
            ("change", ChangeKind::Changed),
            ("gone", ChangeKind::Removed),
            ("keep", ChangeKind::Unchanged),
            ("new", ChangeKind::Added),
        ])
    );
    assert!(diff.counts().values().all(|count| *count == 1));
    let provenance = diff.stages["pack"].after.as_ref().unwrap();
    assert_eq!(provenance.rust_stamp.as_deref(), Some("native-tools-stamp"));
    assert_eq!(provenance.fallbacks, ["codec: not ported"]);
    assert_eq!(OutputDiff::compare(Some(&before), Some(&after)), diff);
}

#[test]
fn missing_baselines_and_target_switches_are_not_additions_or_deletions() {
    let before = snapshot("Before", &[("pack", record("1", &[("old", "a")]))]);
    let mut after = snapshot("After", &[("pack", record("2", &[("new", "b")]))]);
    let first = OutputDiff::compare(None, Some(&after));
    assert_eq!(first.rows[0].kind, ChangeKind::NoBaseline);
    let absent = OutputDiff::compare(Some(&before), None);
    assert_eq!(absent.rows[0].kind, ChangeKind::Unverified);
    assert!(
        absent.rows[0]
            .issues
            .contains(&MetadataIssue::NoCurrentRecord)
    );
    after.source.root = PathBuf::from("H:/different-project");
    let switched = OutputDiff::compare(Some(&before), Some(&after));
    assert_eq!(switched.rows.len(), 1);
    assert_eq!(switched.rows[0].kind, ChangeKind::NoBaseline);
    assert!(
        switched
            .notes
            .iter()
            .any(|note| note.contains("different root"))
    );
    after.source.root = before.source.root.clone();
    after.source.graph = PathBuf::from("tools/build/other.toml");
    assert_eq!(
        OutputDiff::compare(Some(&before), Some(&after)).rows[0].kind,
        ChangeKind::NoBaseline
    );
}

#[test]
fn failed_incomplete_and_absent_stages_cannot_report_removed_outputs() {
    let before = snapshot("Before", &[("pack", record("1", &[("output", "a")]))]);
    for (failed, incomplete, expected) in [
        (true, false, MetadataIssue::Failed),
        (false, true, MetadataIssue::IncompleteTrace),
    ] {
        let mut run = record("2", &[]);
        run.ok = !failed;
        run.trace_complete = !incomplete;
        let after = snapshot("After", &[("pack", run)]);
        let diff = OutputDiff::compare(Some(&before), Some(&after));
        assert_eq!(diff.rows[0].kind, ChangeKind::Unverified);
        assert!(diff.rows[0].issues.contains(&expected));
    }
    let empty = snapshot("After", &[]);
    let diff = OutputDiff::compare(Some(&before), Some(&empty));
    assert_eq!(diff.rows[0].kind, ChangeKind::Unverified);
    assert_eq!(
        diff.stages["pack"].issues(),
        [MetadataIssue::NoCurrentRecord]
    );
}

#[test]
fn unchanged_run_stamp_with_changed_outputs_and_owner_staleness_are_unverified() {
    let before = snapshot(
        "Before",
        &[("pack", record("same-run", &[("output", "a")]))],
    );
    let after = snapshot("After", &[("pack", record("same-run", &[("output", "b")]))]);
    let diff = OutputDiff::compare(Some(&before), Some(&after));
    assert_eq!(diff.rows[0].kind, ChangeKind::Unverified);
    assert!(diff.rows[0].issues.contains(&MetadataIssue::Stale));
    let mut confirmed = snapshot("After", &[("pack", record("new-run", &[("output", "b")]))]);
    confirmed.mark_stale("pack");
    confirmed.mark_stale("pack");
    let diff = OutputDiff::compare(Some(&before), Some(&confirmed));
    assert_eq!(diff.stages["pack"].issues(), [MetadataIssue::Stale]);
}

#[test]
fn windows_output_aliases_are_joined_and_conflicting_hashes_are_flagged() {
    let before = snapshot(
        "Before",
        &[("pack", record("1", &[("./WORK\\out\\PACK.fpk", "a")]))],
    );
    let after = snapshot(
        "After",
        &[("pack", record("2", &[("work/out/pack.fpk", "a")]))],
    );
    let diff = OutputDiff::compare(Some(&before), Some(&after));
    assert_eq!(diff.rows.len(), 1);
    assert_eq!(diff.rows[0].kind, ChangeKind::Unchanged);
    let mut run = record(
        "3",
        &[("work/out/pack.fpk", "a"), ("WORK\\out\\pack.fpk", "b")],
    );
    run.changed_during_run.push("input.npy".into());
    let ambiguous = snapshot("Ambiguous", &[("pack", run)]);
    let diff = OutputDiff::compare(Some(&before), Some(&ambiguous));
    assert_eq!(diff.rows.len(), 1);
    assert_eq!(diff.rows[0].after_hash, None);
    assert_eq!(diff.rows[0].kind, ChangeKind::Unverified);
    assert!(diff.rows[0].issues.contains(&MetadataIssue::AmbiguousPath));
    assert!(diff.rows[0].issues.contains(&MetadataIssue::InputsChanged));
}

#[test]
fn snapshot_reader_distinguishes_absent_invalid_and_valid_actual_state() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir()
        .join(format!("foxstudio_diagnostics_reader-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut input = source("Reader");
    input.state_file = dir.join("state.json");
    assert!(OutputSnapshot::read(input.clone()).unwrap().is_none());
    std::fs::write(&input.state_file, b"{broken").unwrap();
    assert!(
        OutputSnapshot::read(input.clone())
            .unwrap_err()
            .contains("Invalid build snapshot")
    );
    let state = State {
        stages: BTreeMap::from([("pack".into(), record("1", &[("file", "a")]))]),
    };
    std::fs::write(&input.state_file, serde_json::to_vec(&state).unwrap()).unwrap();
    let loaded = OutputSnapshot::read(input).unwrap().unwrap();
    assert_eq!(
        OutputDiff::compare(None, Some(&loaded)).rows[0].path,
        "file"
    );
}

#[test]
fn output_ui_filters_selects_and_shows_hashes_without_a_gpu() {
    let before = snapshot(
        "Before",
        &[(
            "pack",
            record("1", &[("change", "old-hash"), ("keep", "same")]),
        )],
    );
    let after = snapshot(
        "After",
        &[(
            "pack",
            record("2", &[("change", "new-hash"), ("keep", "same")]),
        )],
    );
    let diff = OutputDiff::compare(Some(&before), Some(&after));
    let mut h = Harness::builder()
        .with_size([1000.0, 850.0])
        .build_ui_state(
            |ui, state: &mut (OutputDiffView, OutputDiff)| state.0.show(ui, &state.1),
            (OutputDiffView::default(), diff),
        );
    h.get_by_label("Changed | pack | change").click();
    h.run_steps(2);
    h.get_by_label("Before: old-hash");
    h.get_by_label("After: new-hash");
    h.get_by_label("Changes only").click();
    h.run_steps(2);
    assert_eq!(h.state().0.visible_rows(&h.state().1).len(), 1);
    h.state_mut().0.filter = "no-match".into();
    h.run_steps(2);
    assert!(h.state().0.selected(&h.state().1).is_none());
    let (view, diff) = h.state_mut();
    view.filter = "CHANGE".into();
    assert!(view.select(diff, "pack", "change"));
    h.run_steps(2);
    assert!(h.state().0.selected(&h.state().1).is_some());
}

#[test]
fn failed_stage_with_no_outputs_is_visible() {
    let mut failed = record("2", &[]);
    failed.ok = false;
    let after = snapshot("Failed", &[("empty-stage", failed)]);
    let diff = OutputDiff::compare(None, Some(&after));
    assert!(diff.rows.is_empty());
    let h = Harness::builder().build_ui_state(
        |ui, state: &mut (OutputDiffView, OutputDiff)| state.0.show(ui, &state.1),
        (OutputDiffView::default(), diff),
    );
    h.get_by_label("empty-stage: Stage failed");
}

const MD5_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const MD5_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn installed(name: &str) -> InstalledMod {
    InstalledMod {
        name: name.into(),
        version: "1.0".into(),
        author: String::new(),
        website: String::new(),
        description: String::new(),
        package_md5: MD5_A.into(),
        installed: "2026-10-06".into(),
        files: Vec::new(),
        packs: Vec::new(),
        loose: Vec::new(),
        sb_entry: None,
    }
}

fn file(path: &str, md5: &str) -> FileItem {
    FileItem {
        path: path.into(),
        hash: "incorrect-metadata-key".into(),
        archive: "00".into(),
        md5: md5.into(),
    }
}

fn pack(path: &str, entries: &[(&str, &str)]) -> PackItem {
    PackItem {
        pack: path.into(),
        archive: "00".into(),
        entries: entries
            .iter()
            .map(|(path, hash)| ((*path).into(), (*hash).into()))
            .collect(),
        references: vec!["/Assets/other.fpk".into()],
    }
}

fn manifest(mods: Vec<InstalledMod>) -> Manifest {
    Manifest {
        format: 1,
        tool: "foxinstall".into(),
        layout: "gzstool".into(),
        mode: "native".into(),
        mods,
        ..Manifest::default()
    }
}

#[test]
fn archive_precedence_uses_manifest_order_and_actual_qar_key_not_timestamps() {
    let mut first = installed("First");
    first.installed = "2099-replacement".into();
    first.files.push(file("/Assets/test.lua", MD5_A));
    let mut second = installed("Second");
    second.files.push(file("/Assets/test.lua", MD5_B));
    let mut state = manifest(vec![first, second]);
    let report = ConflictReport::from_manifest(&state);
    let row = &report.rows[0];
    assert_eq!(row.resolution, Resolution::LastWriter);
    assert_eq!(row.content, ContentComparison::Different);
    assert_eq!(row.owners[row.winners[0]].mod_name, "Second");
    state.mods.reverse();
    let reverse = ConflictReport::from_manifest(&state);
    assert_eq!(
        reverse.rows[0].owners[reverse.rows[0].winners[0]].mod_name,
        "First"
    );
}

#[test]
fn identical_unknown_and_package_order_content_are_distinguished() {
    let mut owner = installed("Package");
    owner.files = vec![
        file("/Assets/test.lua", MD5_A),
        file("/Assets/test.lua", &MD5_A.to_uppercase()),
    ];
    let mut state = manifest(vec![owner]);
    let identical = ConflictReport::from_manifest(&state);
    assert_eq!(identical.rows[0].content, ContentComparison::Identical);
    assert_eq!(identical.rows[0].winners, [1]);
    state.mods[0].files[1].md5.clear();
    assert_eq!(
        ConflictReport::from_manifest(&state).rows[0].content,
        ContentComparison::Unknown
    );
}

#[test]
fn distinct_names_with_one_actual_archive_hash_conflict() {
    let mut first = installed("First");
    let mut second = installed("Second");
    // Unknown extensions have type zero; the archive key uses the stem before the first dot.
    first
        .files
        .push(file("/Assets/collision.unknown_one", MD5_A));
    second
        .files
        .push(file("/Assets/collision.unknown_two", MD5_B));
    assert_eq!(
        foxcore::qar::file_hash(&first.files[0].path),
        foxcore::qar::file_hash(&second.files[0].path)
    );
    let report = ConflictReport::from_manifest(&manifest(vec![first, second]));
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].resolution, Resolution::LastWriter);
    assert_eq!(
        report.rows[0].owners[report.rows[0].winners[0]].mod_name,
        "Second"
    );
}

#[test]
fn windows_archive_case_aliases_keep_distinct_case_sensitive_qar_records() {
    let mut first = installed("First");
    let mut second = installed("Second");
    first.files.push(file("/Assets/test.lua", MD5_A));
    second.files.push(file("/Assets/TEST.lua", MD5_B));
    assert_ne!(
        foxcore::qar::file_hash(&first.files[0].path),
        foxcore::qar::file_hash(&second.files[0].path)
    );
    let report = ConflictReport::from_manifest(&manifest(vec![first, second]));
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].resolution, Resolution::SeparateRecords);
    let mut retained = report.rows[0].winners.clone();
    retained.sort();
    assert_eq!(retained, [0, 1]);
}

#[test]
fn pack_merging_keeps_disjoint_entries_and_last_exact_entry_writer() {
    let mut first = installed("First");
    let mut second = installed("Second");
    first.packs.push(pack(
        "/Assets/test.fpk",
        &[("/inner/a.lua", MD5_A), ("/inner/shared.lua", MD5_A)],
    ));
    second.packs.push(pack(
        "/Assets/test.fpk",
        &[("/inner/b.lua", MD5_B), ("/inner/shared.lua", MD5_B)],
    ));
    let report = ConflictReport::from_manifest(&manifest(vec![first, second]));
    assert_eq!(report.rows.len(), 2);
    let outer = report
        .rows
        .iter()
        .find(|row| row.id.starts_with("archive:"))
        .unwrap();
    assert_eq!(outer.resolution, Resolution::Merged);
    assert_eq!(outer.content, ContentComparison::NotComparable);
    assert!(outer.winners.is_empty());
    let inner = report
        .rows
        .iter()
        .find(|row| row.id.starts_with("inner:"))
        .unwrap();
    assert_eq!(inner.content, ContentComparison::Different);
    assert_eq!(inner.owners[inner.winners[0]].mod_name, "Second");
}

#[test]
fn native_and_snakebite_inner_alias_precedence_matches_installer_mode() {
    let mut first = installed("First");
    let mut second = installed("Second");
    first
        .packs
        .push(pack("/Assets/test.fpk", &[("/inner./A.lua", MD5_A)]));
    second
        .packs
        .push(pack("/Assets/test.fpk", &[("\\inner\\a.lua", MD5_B)]));
    let mut state = manifest(vec![first, second]);
    let native = ConflictReport::from_manifest(&state);
    let native_inner = native
        .rows
        .iter()
        .find(|row| row.id.starts_with("inner:"))
        .unwrap();
    assert_eq!(native_inner.resolution, Resolution::SeparateRecords);
    assert_eq!(native_inner.winners.len(), 2);
    state.mode = "snakebite".into();
    let snakebite = ConflictReport::from_manifest(&state);
    let merged_inner = snakebite
        .rows
        .iter()
        .find(|row| row.id.starts_with("inner:"))
        .unwrap();
    assert_eq!(merged_inner.resolution, Resolution::LastWriter);
    assert_eq!(
        merged_inner.owners[merged_inner.winners[0]].mod_name,
        "Second"
    );
}

#[test]
fn whole_pack_and_merge_conflicts_have_no_safe_outer_or_inner_winner() {
    let mut whole = installed("Whole");
    whole.files.push(file("/Assets/test.fpk", MD5_A));
    let mut first = installed("Merge1");
    first
        .packs
        .push(pack("/Assets/test.fpk", &[("/inner/a.lua", MD5_A)]));
    let mut second = installed("Merge2");
    second
        .packs
        .push(pack("/Assets/test.fpk", &[("/inner/a.lua", MD5_B)]));
    let report = ConflictReport::from_manifest(&manifest(vec![whole, first, second]));
    assert_eq!(report.rows.len(), 2);
    for row in &report.rows {
        assert_eq!(row.resolution, Resolution::AmbiguousArchiveEntries);
        assert!(row.winners.is_empty());
    }
}

#[test]
fn exact_pack_path_groups_with_one_archive_key_are_ambiguous() {
    let mut first = installed("Slash");
    first.packs.push(pack("/Assets/test.fpk", &[]));
    let mut second = installed("Backslash");
    second.packs.push(pack("\\Assets\\test.fpk", &[]));
    let report = ConflictReport::from_manifest(&manifest(vec![first, second]));
    assert_eq!(report.rows.len(), 1);
    assert_eq!(
        report.rows[0].resolution,
        Resolution::AmbiguousArchiveEntries
    );
    assert!(report.rows[0].winners.is_empty());
}

#[test]
fn loose_case_owner_is_recorded_but_slash_alias_owner_is_unsafe() {
    let mut first = installed("First");
    let mut second = installed("Second");
    first.loose.push(LooseItem {
        path: "master/test.dat".into(),
        md5: MD5_A.into(),
        replaced_original: true,
    });
    second.loose.push(LooseItem {
        path: "MASTER/test.dat".into(),
        md5: MD5_B.into(),
        replaced_original: true,
    });
    let mut state = manifest(vec![first, second]);
    let case = ConflictReport::from_manifest(&state);
    assert_eq!(case.rows[0].resolution, Resolution::LastWriter);
    assert_eq!(
        case.rows[0].owners[case.rows[0].winners[0]].mod_name,
        "Second"
    );
    state.mods[1].loose[0].path = "master\\test.dat".into();
    let slash = ConflictReport::from_manifest(&state);
    assert_eq!(slash.rows[0].resolution, Resolution::LoosePathAlias);
    assert!(slash.rows[0].winners.is_empty());
}

#[test]
fn separate_archives_do_not_conflict_and_report_order_is_stable() {
    let mut first = installed("First");
    let mut second = installed("Second");
    first.files.push(file("/Assets/test.lua", MD5_A));
    let mut texture = file("/Assets/test.lua", MD5_B);
    texture.archive = "01".into();
    second.files.push(texture);
    let state = manifest(vec![first, second]);
    assert!(ConflictReport::from_manifest(&state).rows.is_empty());
    assert_eq!(
        ConflictReport::from_manifest(&state),
        ConflictReport::from_manifest(&state)
    );
}

#[test]
fn conflict_ui_selects_owner_details_and_filters_by_mod_name() {
    let mut first = installed("First");
    let mut second = installed("Second");
    first.files.push(file("/Assets/test.lua", MD5_A));
    second.files.push(file("/Assets/test.lua", MD5_B));
    let report = ConflictReport::from_manifest(&manifest(vec![first, second]));
    let label = format!(
        "{} | {} | {}",
        report.rows[0].resolution.label(),
        report.rows[0].content.label(),
        report.rows[0].destination
    );
    let mut h = Harness::builder()
        .with_size([1400.0, 850.0])
        .build_ui_state(
            |ui, state: &mut (ConflictsView, ConflictReport)| state.0.show(ui, &state.1),
            (ConflictsView::default(), report),
        );
    h.get_by_label(&label).click();
    h.run_steps(2);
    h.get_by_label("2. Second 1.0 (retained owner)");
    h.get_by_label(&format!("Content MD5: {MD5_B}"));
    h.state_mut().0.filter = "SECOND".into();
    h.run_steps(2);
    assert_eq!(h.state().0.visible_rows(&h.state().1).len(), 1);
    h.state_mut().0.filter = "absent".into();
    h.run_steps(2);
    assert!(h.state().0.selected(&h.state().1).is_none());
    let (view, report) = h.state_mut();
    view.filter.clear();
    view.different_only = true;
    assert!(view.select(report, &report.rows[0].id));
    view.reset_selection();
    assert!(view.selected(report).is_none());
}
