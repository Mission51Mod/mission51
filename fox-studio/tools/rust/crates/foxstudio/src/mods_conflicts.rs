//! Read-only ownership diagnostics from the installer's real Manifest.
//! Archive keys, native inner paths and SnakeBite folder aliases are deliberately distinct.
use eframe::egui;
use foxcore::qar;
use foxinstall::{Manifest, windows_key};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimKind {
    ArchiveFile,
    PackMerge,
    PackEntry,
    LooseFile,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnershipClaim {
    pub mod_index: usize,
    pub mod_name: String,
    pub version: String,
    pub kind: ClaimKind,
    pub path: String,
    pub pack: Option<String>,
    pub archive: Option<String>,
    pub content_md5: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentComparison {
    Identical,
    Different,
    Unknown,
    NotComparable,
}

impl ContentComparison {
    pub fn label(self) -> &'static str {
        match self {
            Self::Identical => "Identical content",
            Self::Different => "Different content",
            Self::Unknown => "Content hash unavailable",
            Self::NotComparable => "Merged content not recorded",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    LastWriter,
    Merged,
    SeparateRecords,
    AmbiguousArchiveEntries,
    LoosePathAlias,
}

impl Resolution {
    pub fn label(self) -> &'static str {
        match self {
            Self::LastWriter => "Later manifest owner wins",
            Self::Merged => "Compatible pack merge",
            Self::SeparateRecords => "Distinct installer records",
            Self::AmbiguousArchiveEntries => "Ambiguous archive entries",
            Self::LoosePathAlias => "Windows alias not tracked consistently",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnershipDiagnostic {
    pub id: String,
    pub destination: String,
    pub owners: Vec<OwnershipClaim>,
    pub content: ContentComparison,
    pub resolution: Resolution,
    /// Indices into owners retained in this installer record group.
    /// These are metadata claims, not a verification of installed bytes.
    pub winners: Vec<usize>,
    pub explanation: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConflictReport {
    pub rows: Vec<OwnershipDiagnostic>,
    pub notes: Vec<String>,
}

fn content_comparison(owners: &[OwnershipClaim]) -> ContentComparison {
    if owners
        .iter()
        .any(|owner| owner.kind == ClaimKind::PackMerge)
    {
        return ContentComparison::NotComparable;
    }
    let hashes: Option<Vec<_>> = owners
        .iter()
        .map(|owner| {
            owner
                .content_md5
                .as_ref()
                .filter(|hash| {
                    hash.len() == 32 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
                .map(|hash| hash.to_ascii_lowercase())
        })
        .collect();
    match hashes {
        None => ContentComparison::Unknown,
        Some(hashes) if hashes.iter().all(|hash| hash == &hashes[0]) => {
            ContentComparison::Identical
        }
        Some(_) => ContentComparison::Different,
    }
}

fn last_per_key(owners: &[OwnershipClaim], key: impl Fn(&OwnershipClaim) -> String) -> Vec<usize> {
    let mut winners = BTreeMap::new();
    for (index, owner) in owners.iter().enumerate() {
        winners.insert(key(owner), index);
    }
    winners.into_values().collect()
}

fn diagnostic(
    id: String,
    destination: String,
    owners: Vec<OwnershipClaim>,
    resolution: Resolution,
    winners: Vec<usize>,
    explanation: &str,
) -> OwnershipDiagnostic {
    let content = content_comparison(&owners);
    OwnershipDiagnostic {
        id,
        destination,
        owners,
        content,
        resolution,
        winners,
        explanation: explanation.into(),
    }
}

impl ConflictReport {
    /// Manifest vector order is installer precedence; a replacement keeps its original position.
    /// This is metadata inspection, not an installation or a verification of bytes on disk.
    pub fn from_manifest(manifest: &Manifest) -> Self {
        let mut archives = BTreeMap::<(String, u64), Vec<OwnershipClaim>>::new();
        let mut archive_aliases = BTreeMap::<(String, String), Vec<OwnershipClaim>>::new();
        let mut inner = BTreeMap::<(String, String, String), Vec<OwnershipClaim>>::new();
        let mut loose = BTreeMap::<String, Vec<OwnershipClaim>>::new();
        for (mod_index, installed) in manifest.mods.iter().enumerate() {
            let claim = |kind, path: &str, pack, archive, content_md5| OwnershipClaim {
                mod_index,
                mod_name: installed.name.clone(),
                version: installed.version.clone(),
                kind,
                path: path.into(),
                pack,
                archive,
                content_md5,
            };
            for file in &installed.files {
                let owner = claim(
                    ClaimKind::ArchiveFile,
                    &file.path,
                    None,
                    Some(file.archive.clone()),
                    Some(file.md5.clone()),
                );
                archives
                    .entry((file.archive.clone(), qar::file_hash(&file.path)))
                    .or_default()
                    .push(owner.clone());
                archive_aliases
                    .entry((file.archive.clone(), windows_key(&file.path)))
                    .or_default()
                    .push(owner);
            }
            for pack in &installed.packs {
                let owner = claim(
                    ClaimKind::PackMerge,
                    &pack.pack,
                    None,
                    Some(pack.archive.clone()),
                    None,
                );
                archives
                    .entry((pack.archive.clone(), qar::file_hash(&pack.pack)))
                    .or_default()
                    .push(owner.clone());
                archive_aliases
                    .entry((pack.archive.clone(), windows_key(&pack.pack)))
                    .or_default()
                    .push(owner);
                for (path, md5) in &pack.entries {
                    inner
                        .entry((pack.archive.clone(), pack.pack.clone(), windows_key(path)))
                        .or_default()
                        .push(claim(
                            ClaimKind::PackEntry,
                            path,
                            Some(pack.pack.clone()),
                            Some(pack.archive.clone()),
                            Some(md5.clone()),
                        ));
                }
            }
            for file in &installed.loose {
                loose
                    .entry(windows_key(&file.path))
                    .or_default()
                    .push(claim(
                        ClaimKind::LooseFile,
                        &file.path,
                        None,
                        None,
                        Some(file.md5.clone()),
                    ));
            }
        }
        let mut report = Self::default();
        report.notes.push("Ownership comes from the installed manifest. Verify checks the actual installed bytes.".into());
        let ambiguous_archives: BTreeSet<_> = archives
            .iter()
            .filter(|(_, owners)| {
                let packs: BTreeSet<_> = owners
                    .iter()
                    .filter(|owner| owner.kind == ClaimKind::PackMerge)
                    .map(|owner| &owner.path)
                    .collect();
                !packs.is_empty()
                    && (packs.len() > 1
                        || owners
                            .iter()
                            .any(|owner| owner.kind == ClaimKind::ArchiveFile))
            })
            .map(|(key, _)| key.clone())
            .collect();
        for ((archive, hash), owners) in archives {
            if owners.len() < 2 {
                continue;
            }
            let has_whole = owners
                .iter()
                .any(|owner| owner.kind == ClaimKind::ArchiveFile);
            let has_merge = owners
                .iter()
                .any(|owner| owner.kind == ClaimKind::PackMerge);
            let distinct_packs = owners
                .iter()
                .filter(|owner| owner.kind == ClaimKind::PackMerge)
                .map(|owner| &owner.path)
                .collect::<BTreeSet<_>>()
                .len();
            let destination = format!("{archive}.dat :: {}", owners[0].path);
            let (resolution, winners, explanation) = if has_whole && has_merge {
                (
                    Resolution::AmbiguousArchiveEntries,
                    Vec::new(),
                    "The installer writes whole files, then merged vanilla packs. Both records use one archive key; the manifest does not establish a safe final winner.",
                )
            } else if has_merge && distinct_packs > 1 {
                (
                    Resolution::AmbiguousArchiveEntries,
                    Vec::new(),
                    "The installer groups merged packs by exact pack path. These groups emit the same archive key; their final ownership is ambiguous.",
                )
            } else if has_merge {
                (
                    Resolution::Merged,
                    Vec::new(),
                    "Inner entries are merged into the vanilla pack in manifest order. Different inner paths coexist; references are accumulated, not replaced.",
                )
            } else {
                (
                    Resolution::LastWriter,
                    vec![owners.len() - 1],
                    "The installer keeps the last whole-file writer for this archive key, using manifest order and then package order.",
                )
            };
            report.rows.push(diagnostic(
                format!("archive:{archive}:{hash:016x}"),
                destination,
                owners,
                resolution,
                winners,
                explanation,
            ));
        }
        for ((archive, path), owners) in archive_aliases {
            let hashes: BTreeSet<_> = owners
                .iter()
                .map(|owner| qar::file_hash(&owner.path))
                .collect();
            if hashes.len() < 2 {
                continue;
            }
            let winners = last_per_key(&owners, |owner| format!("{}", qar::file_hash(&owner.path)))
                .into_iter()
                .filter(|&index| {
                    owners[index].kind == ClaimKind::ArchiveFile
                        && !ambiguous_archives
                            .contains(&(archive.clone(), qar::file_hash(&owners[index].path)))
                })
                .collect();
            report.rows.push(diagnostic(format!("archive-alias:{archive}:{path}"), format!("{archive}.dat :: {path}"),
                owners, Resolution::SeparateRecords, winners,
                "These paths alias on Windows, but their archive keys differ. Case folding alone does not establish an archive overwrite."));
        }
        for ((archive, pack, path), owners) in inner {
            if owners.len() < 2 {
                continue;
            }
            let native_keys: BTreeSet<_> = owners.iter().map(|owner| &owner.path).collect();
            let (resolution, winners, explanation) = if ambiguous_archives
                .contains(&(archive.clone(), qar::file_hash(&pack)))
            {
                (
                    Resolution::AmbiguousArchiveEntries,
                    Vec::new(),
                    "The parent pack has duplicate archive records. Inner merging alone cannot establish a safe final owner; inspect the parent archive diagnostic.",
                )
            } else if manifest.mode == "snakebite" || native_keys.len() == 1 {
                (
                    Resolution::LastWriter,
                    vec![owners.len() - 1],
                    if manifest.mode == "snakebite" {
                        "SnakeBite folder merging resolves case, slash and trailing-dot/space aliases to the last mod entry in manifest/package order."
                    } else {
                        "Native merging replaces the exact inner path with the last mod entry in manifest/package order."
                    },
                )
            } else {
                (
                    Resolution::SeparateRecords,
                    last_per_key(&owners, |owner| owner.path.clone()),
                    "Native merging preserves distinct exact inner paths. These Windows aliases remain separate records; there is no single overwrite winner.",
                )
            };
            report.rows.push(diagnostic(
                format!("inner:{archive}:{pack}:{path}"),
                format!("{archive}.dat :: {pack} :: {path}"),
                owners,
                resolution,
                winners,
                explanation,
            ));
        }
        for (path, owners) in loose {
            if owners.len() < 2 {
                continue;
            }
            let keys: BTreeSet<_> = owners
                .iter()
                .map(|owner| owner.path.to_ascii_lowercase())
                .collect();
            let (resolution, winners, explanation) = if keys.len() == 1 {
                (
                    Resolution::LastWriter,
                    vec![owners.len() - 1],
                    "The installer's verification/restoration owner is the last manifest mod with this case-insensitive loose path. Bytes still require Verify, especially after replacing an older mod.",
                )
            } else {
                (
                    Resolution::LoosePathAlias,
                    Vec::new(),
                    "Windows aliases share a destination, but the installer's loose-file ownership comparisons only ignore case. Slash or trailing-dot aliases may not restore consistently; do not infer a safe owner.",
                )
            };
            report.rows.push(diagnostic(
                format!("loose:{path}"),
                format!("GameDir/{path}"),
                owners,
                resolution,
                winners,
                explanation,
            ));
        }
        report.rows.sort_by(|a, b| a.id.cmp(&b.id));
        report
    }

    pub fn different_content(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| row.content == ContentComparison::Different)
            .count()
    }
}

#[derive(Default)]
pub struct ConflictsView {
    pub filter: String,
    pub different_only: bool,
    selected: Option<String>,
}

impl ConflictsView {
    /// Call when changing the game directory or manifest source.
    pub fn reset_selection(&mut self) {
        self.selected = None;
    }

    pub fn visible_rows<'a>(&self, report: &'a ConflictReport) -> Vec<&'a OwnershipDiagnostic> {
        let query = self.filter.to_lowercase();
        report
            .rows
            .iter()
            .filter(|row| {
                (!self.different_only || row.content == ContentComparison::Different)
                    && (row.destination.to_lowercase().contains(&query)
                        || row
                            .owners
                            .iter()
                            .any(|owner| owner.mod_name.to_lowercase().contains(&query)))
            })
            .collect()
    }

    pub fn select(&mut self, report: &ConflictReport, id: &str) -> bool {
        self.selected = self
            .visible_rows(report)
            .into_iter()
            .find(|row| row.id == id)
            .map(|row| row.id.clone());
        self.selected.is_some()
    }

    pub fn selected<'a>(&self, report: &'a ConflictReport) -> Option<&'a OwnershipDiagnostic> {
        let id = self.selected.as_ref()?;
        self.visible_rows(report)
            .into_iter()
            .find(|row| &row.id == id)
    }

    pub fn show(&mut self, ui: &mut egui::Ui, report: &ConflictReport) {
        ui.heading("Mod ownership conflicts");
        ui.label(format!(
            "{} diagnostic groups; {} with different content",
            report.rows.len(),
            report.different_content()
        ));
        for note in &report.notes {
            ui.label(note);
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Filter conflicts by path or mod"),
            );
            ui.checkbox(&mut self.different_only, "Different content only");
        });
        if self.selected(report).is_none() {
            self.selected = None;
        }
        egui::ScrollArea::vertical()
            .id_salt("mod-ownership-conflicts")
            .max_height(260.0)
            .show(ui, |ui| {
                for row in self.visible_rows(report) {
                    if ui
                        .selectable_label(
                            self.selected.as_ref() == Some(&row.id),
                            format!(
                                "{} | {} | {}",
                                row.resolution.label(),
                                row.content.label(),
                                row.destination
                            ),
                        )
                        .clicked()
                    {
                        self.selected = Some(row.id.clone());
                    }
                }
            });
        if let Some(row) = self.selected(report) {
            ui.separator();
            ui.add(egui::Label::new(&row.destination).selectable(true));
            ui.label(&row.explanation);
            for (index, owner) in row.owners.iter().enumerate() {
                let winner = if row.winners.contains(&index) {
                    " (retained owner)"
                } else {
                    ""
                };
                ui.label(format!(
                    "{}. {} {}{winner}",
                    owner.mod_index + 1,
                    owner.mod_name,
                    owner.version
                ));
                ui.add(egui::Label::new(&owner.path).selectable(true));
                if let Some(hash) = &owner.content_md5 {
                    ui.monospace(format!("Content MD5: {hash}"));
                }
            }
        }
    }
}
