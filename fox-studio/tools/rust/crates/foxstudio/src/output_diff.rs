//! Compare recorded build outputs without treating missing metadata as deleted files.
//! Snapshots use foxbuild's actual persisted State; reading them never changes build state.
use eframe::egui;
use foxbuild::State;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotSource {
    pub root: PathBuf,
    pub graph: PathBuf,
    pub state_file: PathBuf,
    pub label: String,
}

impl SnapshotSource {
    fn target_key(&self) -> (String, String) {
        (
            path_key(&self.root.to_string_lossy()),
            path_key(&self.graph.to_string_lossy()),
        )
    }
}

fn path_key(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_lowercase()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MetadataIssue {
    Failed,
    IncompleteTrace,
    InputsChanged,
    Stale,
    AmbiguousPath,
    NoCurrentRecord,
}

impl MetadataIssue {
    pub fn label(self) -> &'static str {
        match self {
            Self::Failed => "Stage failed",
            Self::IncompleteTrace => "Incomplete output trace",
            Self::InputsChanged => "Inputs changed during the run",
            Self::Stale => "Stale or inconsistent metadata",
            Self::AmbiguousPath => "Conflicting hashes for one Windows path",
            Self::NoCurrentRecord => "No current stage record",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunProvenance {
    pub finished: String,
    pub command_fingerprint: String,
    pub rust_stamp: Option<String>,
    pub fallbacks: Vec<String>,
    pub issues: Vec<MetadataIssue>,
}

impl RunProvenance {
    fn mark_stale(&mut self) {
        if !self.issues.contains(&MetadataIssue::Stale) {
            self.issues.push(MetadataIssue::Stale);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OutputHash {
    paths: Vec<String>,
    hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StageSnapshot {
    provenance: RunProvenance,
    outputs: BTreeMap<String, OutputHash>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputSnapshot {
    pub source: SnapshotSource,
    stages: BTreeMap<String, StageSnapshot>,
}

impl OutputSnapshot {
    pub fn from_state(source: SnapshotSource, state: &State) -> Self {
        let stages = state
            .stages
            .iter()
            .map(|(name, record)| {
                let mut issues = Vec::new();
                if !record.ok {
                    issues.push(MetadataIssue::Failed);
                }
                if !record.trace_complete {
                    issues.push(MetadataIssue::IncompleteTrace);
                }
                if !record.changed_during_run.is_empty() {
                    issues.push(MetadataIssue::InputsChanged);
                }
                if record.finished.is_empty() || record.cmd_fp.is_empty() {
                    issues.push(MetadataIssue::Stale);
                }
                let mut outputs = BTreeMap::<String, OutputHash>::new();
                for (path, hash) in &record.outputs {
                    let output = outputs.entry(path_key(path)).or_insert_with(|| OutputHash {
                        paths: Vec::new(),
                        hash: Some(hash.clone()),
                    });
                    output.paths.push(path.clone());
                    if output.hash.as_ref() != Some(hash) {
                        output.hash = None;
                    }
                }
                if outputs.values().any(|output| output.hash.is_none()) {
                    issues.push(MetadataIssue::AmbiguousPath);
                }
                (
                    name.clone(),
                    StageSnapshot {
                        provenance: RunProvenance {
                            finished: record.finished.clone(),
                            command_fingerprint: record.cmd_fp.clone(),
                            rust_stamp: record.rust_stamp.clone(),
                            fallbacks: record.fallbacks.clone(),
                            issues,
                        },
                        outputs,
                    },
                )
            })
            .collect();
        Self { source, stages }
    }

    /// Missing state is explicitly absent; malformed or unreadable state is an error.
    /// Do not use State::load here: it silently replaces unreadable state with an empty baseline.
    pub fn read(source: SnapshotSource) -> Result<Option<Self>, String> {
        let bytes = match std::fs::read(&source.state_file) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "Cannot read build snapshot {}: {error}",
                    source.state_file.display()
                ));
            }
        };
        let state: State = serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "Invalid build snapshot {}: {error}",
                source.state_file.display()
            )
        })?;
        Ok(Some(Self::from_state(source, &state)))
    }

    /// The run owner can mark records left behind by an interrupted or otherwise unconfirmed run.
    pub fn mark_stale(&mut self, stage: &str) {
        if let Some(record) = self.stages.get_mut(stage) {
            record.provenance.mark_stale();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeKind {
    Added,
    Removed,
    Changed,
    Unchanged,
    NoBaseline,
    Unverified,
}

impl ChangeKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "Added",
            Self::Removed => "Removed",
            Self::Changed => "Changed",
            Self::Unchanged => "Unchanged",
            Self::NoBaseline => "No baseline",
            Self::Unverified => "Unverified",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputChange {
    pub stage: String,
    pub path: String,
    pub before_paths: Vec<String>,
    pub after_paths: Vec<String>,
    pub before_hash: Option<String>,
    pub after_hash: Option<String>,
    pub kind: ChangeKind,
    pub issues: Vec<MetadataIssue>,
    key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageProvenance {
    pub before: Option<RunProvenance>,
    pub after: Option<RunProvenance>,
}

impl StageProvenance {
    pub fn issues(&self) -> Vec<MetadataIssue> {
        let mut issues: BTreeSet<_> = self
            .before
            .iter()
            .chain(self.after.iter())
            .flat_map(|run| run.issues.iter().copied())
            .collect();
        if self.after.is_none() {
            issues.insert(MetadataIssue::NoCurrentRecord);
        }
        issues.into_iter().collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputDiff {
    pub before_source: Option<SnapshotSource>,
    pub after_source: Option<SnapshotSource>,
    pub notes: Vec<String>,
    pub stages: BTreeMap<String, StageProvenance>,
    pub rows: Vec<OutputChange>,
}

impl OutputDiff {
    pub fn compare(before: Option<&OutputSnapshot>, after: Option<&OutputSnapshot>) -> Self {
        let mut diff = Self {
            before_source: before.map(|snapshot| snapshot.source.clone()),
            after_source: after.map(|snapshot| snapshot.source.clone()),
            ..Self::default()
        };
        let comparable = before
            .zip(after)
            .is_none_or(|(a, b)| a.source.target_key() == b.source.target_key());
        if !comparable {
            diff.notes
                .push("Baseline belongs to a different root or build graph.".into());
        }
        let before = before.filter(|_| comparable);
        if before.is_none() {
            diff.notes
                .push("No baseline snapshot; new outputs are not classified as additions.".into());
        }
        if after.is_none() {
            diff.notes.push(
                "No current snapshot; previous outputs are not classified as removals.".into(),
            );
        }
        let names: BTreeSet<_> = before
            .into_iter()
            .flat_map(|s| s.stages.keys())
            .chain(after.into_iter().flat_map(|s| s.stages.keys()))
            .cloned()
            .collect();
        for stage in names {
            let old = before.and_then(|s| s.stages.get(&stage));
            let new = after.and_then(|s| s.stages.get(&stage));
            let mut after_provenance = new.map(|s| s.provenance.clone());
            if let (Some(a), Some(b), Some(provenance)) = (old, new, &mut after_provenance)
                && a.provenance.finished == b.provenance.finished
                && (a.outputs != b.outputs
                    || a.provenance.command_fingerprint != b.provenance.command_fingerprint)
            {
                provenance.mark_stale();
            }
            let provenance = StageProvenance {
                before: old.map(|s| s.provenance.clone()),
                after: after_provenance,
            };
            let issues = provenance.issues();
            diff.stages.insert(stage.clone(), provenance);
            let paths: BTreeSet<_> = old
                .into_iter()
                .flat_map(|s| s.outputs.keys())
                .chain(new.into_iter().flat_map(|s| s.outputs.keys()))
                .cloned()
                .collect();
            for key in paths {
                let a = old.and_then(|s| s.outputs.get(&key));
                let b = new.and_then(|s| s.outputs.get(&key));
                let before_hash = a.and_then(|v| v.hash.clone());
                let after_hash = b.and_then(|v| v.hash.clone());
                let kind = if old.is_none() {
                    ChangeKind::NoBaseline
                } else if !issues.is_empty() {
                    ChangeKind::Unverified
                } else {
                    match (&before_hash, &after_hash) {
                        (None, Some(_)) => ChangeKind::Added,
                        (Some(_), None) => ChangeKind::Removed,
                        (Some(a), Some(b)) if a == b => ChangeKind::Unchanged,
                        _ => ChangeKind::Changed,
                    }
                };
                let path = b
                    .or(a)
                    .and_then(|v| v.paths.first())
                    .cloned()
                    .unwrap_or_else(|| key.clone());
                diff.rows.push(OutputChange {
                    stage: stage.clone(),
                    path,
                    before_paths: a.map(|v| v.paths.clone()).unwrap_or_default(),
                    after_paths: b.map(|v| v.paths.clone()).unwrap_or_default(),
                    before_hash,
                    after_hash,
                    kind,
                    issues: issues.clone(),
                    key,
                });
            }
        }
        diff
    }

    pub fn counts(&self) -> BTreeMap<ChangeKind, usize> {
        let mut counts = BTreeMap::new();
        for row in &self.rows {
            *counts.entry(row.kind).or_default() += 1;
        }
        counts
    }
}

/// A reusable component; P owns capture scheduling and placement in the Build view.
#[derive(Default)]
pub struct OutputDiffView {
    pub filter: String,
    pub changes_only: bool,
    selected: Option<(String, String)>,
    target: Option<(String, String)>,
}

impl OutputDiffView {
    pub fn visible_rows<'a>(&self, diff: &'a OutputDiff) -> Vec<&'a OutputChange> {
        let query = self.filter.to_lowercase();
        diff.rows
            .iter()
            .filter(|row| {
                (!self.changes_only || row.kind != ChangeKind::Unchanged)
                    && format!("{} {}", row.stage, row.path)
                        .to_lowercase()
                        .contains(&query)
            })
            .collect()
    }

    pub fn select(&mut self, diff: &OutputDiff, stage: &str, path: &str) -> bool {
        self.target = diff
            .after_source
            .as_ref()
            .or(diff.before_source.as_ref())
            .map(SnapshotSource::target_key);
        self.selected = self
            .visible_rows(diff)
            .into_iter()
            .find(|row| row.stage == stage && row.key == path_key(path))
            .map(|row| (row.stage.clone(), row.key.clone()));
        self.selected.is_some()
    }

    pub fn selected<'a>(&self, diff: &'a OutputDiff) -> Option<&'a OutputChange> {
        let key = self.selected.as_ref()?;
        self.visible_rows(diff)
            .into_iter()
            .find(|row| (&row.stage, &row.key) == (&key.0, &key.1))
    }

    pub fn show(&mut self, ui: &mut egui::Ui, diff: &OutputDiff) {
        let target = diff
            .after_source
            .as_ref()
            .or(diff.before_source.as_ref())
            .map(SnapshotSource::target_key);
        if self.target != target {
            self.selected = None;
            self.target = target;
        }
        ui.heading("Build output changes");
        for note in &diff.notes {
            ui.label(note);
        }
        for (stage, provenance) in &diff.stages {
            for issue in provenance.issues() {
                ui.label(format!("{stage}: {}", issue.label()));
            }
        }
        ui.horizontal_wrapped(|ui| {
            for (kind, count) in diff.counts() {
                ui.label(format!("{}: {count}", kind.label()));
            }
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Filter build outputs by path or stage"),
            );
            ui.checkbox(&mut self.changes_only, "Changes only");
        });
        if self.selected(diff).is_none() {
            self.selected = None;
        }
        egui::ScrollArea::vertical()
            .id_salt("build-output-diff")
            .max_height(260.0)
            .show(ui, |ui| {
                for row in self.visible_rows(diff) {
                    let key = (row.stage.clone(), row.key.clone());
                    if ui
                        .selectable_label(
                            self.selected.as_ref() == Some(&key),
                            format!("{} | {} | {}", row.kind.label(), row.stage, row.path),
                        )
                        .clicked()
                    {
                        self.selected = Some(key);
                    }
                }
            });
        if let Some(row) = self.selected(diff) {
            ui.separator();
            ui.label(format!("Stage: {}", row.stage));
            ui.add(egui::Label::new(&row.path).selectable(true));
            ui.monospace(format!(
                "Before: {}",
                row.before_hash.as_deref().unwrap_or("not recorded")
            ));
            ui.monospace(format!(
                "After: {}",
                row.after_hash.as_deref().unwrap_or("not recorded")
            ));
            for issue in &row.issues {
                ui.label(issue.label());
            }
            if let Some(stage) = diff.stages.get(&row.stage) {
                for (label, run) in [("Before run", &stage.before), ("After run", &stage.after)] {
                    if let Some(run) = run {
                        ui.label(format!("{label}: {}", run.finished));
                        ui.monospace(format!("Command: {}", run.command_fingerprint));
                        if let Some(stamp) = &run.rust_stamp {
                            ui.monospace(format!("Rust tools: {stamp}"));
                        }
                        for fallback in &run.fallbacks {
                            ui.label(format!("Python fallback: {fallback}"));
                        }
                    }
                }
            }
        }
    }
}
