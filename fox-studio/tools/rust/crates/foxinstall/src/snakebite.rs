//! SnakeBite-compatible state: read and write snakebite.xml exactly as SnakeBite writes it.
//!
//! Observed layout (SnakeBite 0.9.2.x, see docs/release/REPLACE_THIRD_PARTY.md):
//!   Settings
//!     SbVersion Version, MGSVersion Version
//!     GameData DatHash
//!       QarEntries: System entries (kept verbatim), then one per merged pack (Hash, FilePath, Compressed="false",
//!                   SourceType="Merged", SourceName=<archive of the vanilla pack>), sorted by FilePath
//!       FpkEntries: the vanilla inner files of the merged packs (FpkFile, FilePath "Assets\..." backslashes, no
//!                   leading slash, SourceType="Merged", SourceName)
//!       FileEntries: every installed mod's loose files (FilePath, ContentHash)
//!     Mods: one ModEntry per mod = the package's metadata.xml ModEntry without the xmlns attributes, with
//!           SourceType="Mod" added to its QarEntry and FpkEntry elements
use crate::xmltree::{self, Doc, Elem};

/// one merged pack as SnakeBite records it in GameData
#[derive(Clone, Debug)]
pub struct MergedPack {
    pub path: String,
    pub hash: u64,
    pub source_name: String,
    /// vanilla inner file paths recorded as Merged FpkEntries (as written: backslash form)
    pub vanilla_entries: Vec<String>,
}

pub struct SbState {
    pub doc: Doc,
}

impl SbState {
    pub fn read(text: &str) -> Result<SbState, String> {
        let doc = xmltree::parse(text)?;
        if doc.root.name != "Settings" {
            return Err("snakebite.xml: root is not <Settings>".into());
        }
        Ok(SbState { doc })
    }

    pub fn game_data(&self) -> Option<&Elem> {
        self.doc.root.child("GameData")
    }

    pub fn mods(&self) -> Vec<&Elem> {
        self.doc.root.child("Mods").map(|m| m.children.iter().filter(|c| c.name == "ModEntry").collect()).unwrap_or_default()
    }

    /// GameData's merged packs
    pub fn merged(&self) -> Vec<MergedPack> {
        let mut out: Vec<MergedPack> = vec![];
        let Some(gd) = self.game_data() else { return out };
        if let Some(q) = gd.child("QarEntries") {
            for e in &q.children {
                if e.attr("SourceType") == Some("Merged") {
                    out.push(MergedPack {
                        path: e.attr("FilePath").unwrap_or("").into(),
                        hash: e.attr("Hash").and_then(|h| h.parse().ok()).unwrap_or(0),
                        source_name: e.attr("SourceName").unwrap_or("").into(),
                        vanilla_entries: vec![],
                    });
                }
            }
        }
        if let Some(f) = gd.child("FpkEntries") {
            for e in &f.children {
                let pack = e.attr("FpkFile").unwrap_or("");
                if let Some(m) = out.iter_mut().find(|m| m.path == pack) {
                    m.vanilla_entries.push(e.attr("FilePath").unwrap_or("").into());
                }
            }
        }
        out
    }
}

/// the ModEntry element SnakeBite stores for a package's metadata.xml ModEntry
pub fn mod_entry_from_metadata(meta: &Elem) -> Elem {
    let mut m = meta.clone();
    m.attrs.retain(|(k, _)| !k.starts_with("xmlns"));
    for list in m.children.iter_mut() {
        if list.name == "QarEntries" || list.name == "FpkEntries" {
            for e in list.children.iter_mut() {
                // FpkEntry FilePath: MakeBite writes "Assets\tpp\x", SnakeBite stores "/Assets/tpp/x"
                if list.name == "FpkEntries" {
                    for (k, v) in e.attrs.iter_mut() {
                        if k == "FilePath" && !v.starts_with('/') {
                            *v = format!("/{}", v.replace('\\', "/"));
                        }
                    }
                }
                if e.attr("SourceType").is_none() {
                    e.attrs.push(("SourceType".into(), "Mod".into()));
                }
            }
        }
    }
    m
}

/// GameData FpkEntry FilePath form ("/Assets/tpp/x.lua" -> "Assets\tpp\x.lua")
pub fn sb_inner_path(p: &str) -> String {
    p.trim_start_matches('/').replace('/', "\\")
}

/// rebuild the whole document: keep SbVersion / MGSVersion / System entries / DatHash from `old`
pub fn build(old: &SbState, dat_hash: &str, merged: &[MergedPack], mods: &[Elem]) -> Doc {
    let mut root = Elem::new("Settings");
    root.attrs = old.doc.root.attrs.clone();
    for c in &old.doc.root.children {
        if c.name != "GameData" && c.name != "Mods" {
            root.children.push(c.clone());
        }
    }
    let mut gd = Elem::new("GameData").with("DatHash", dat_hash);
    let mut q = Elem::new("QarEntries");
    if let Some(oq) = old.game_data().and_then(|g| g.child("QarEntries")) {
        q.children.extend(oq.children.iter().filter(|e| e.attr("SourceType") == Some("System")).cloned());
    }
    // order: as SnakeBite recorded them (merge history), packs merged since then appended in the caller's order
    let ms: Vec<&MergedPack> = merged.iter().collect();
    let mut fpk = Elem::new("FpkEntries");
    for m in &ms {
        q.children.push(Elem::new("QarEntry").with("Hash", m.hash.to_string()).with("FilePath", &m.path)
            .with("Compressed", "false").with("SourceType", "Merged").with("SourceName", &m.source_name));
        for v in &m.vanilla_entries {
            fpk.children.push(Elem::new("FpkEntry").with("FpkFile", &m.path).with("FilePath", v)
                .with("SourceType", "Merged").with("SourceName", &m.source_name));
        }
    }
    let mut files = Elem::new("FileEntries");
    for m in mods {
        if let Some(f) = m.child("FileEntries") {
            files.children.extend(f.children.iter().cloned());
        }
    }
    gd.children = vec![q, fpk, files];
    // GameData goes where it was (after the version elements)
    let pos = old.doc.root.children.iter().position(|c| c.name == "GameData").unwrap_or(root.children.len());
    root.children.insert(pos.min(root.children.len()), gd);
    let mut mods_el = Elem::new("Mods");
    mods_el.children = mods.to_vec();
    root.children.push(mods_el);
    Doc { decl: old.doc.decl.clone(), nl: old.doc.nl.clone(), root }
}
