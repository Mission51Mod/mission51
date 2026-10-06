//! SnakeBite-compatible mode: adopt a SnakeBite-managed game without touching its files, keep snakebite.xml in sync.
//! Design: docs/release/REPLACE_THIRD_PARTY.md ("SnakeBite-compatible mode").
use crate::snakebite::{self as sb, MergedPack, SbState};
use crate::xmltree::{self, Elem};
use crate::{
    ARCHIVES, FileItem, FileSig, Game, InstalledMod, LooseItem, Manifest, PackItem, archive_for,
    md5_file,
};
use foxcore::qar;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

impl Game {
    fn master(&self) -> PathBuf {
        self.root.join("master")
    }

    /// Adopt a SnakeBite-managed game.
    ///   base 00 = the entries of master/0/00.dat.original that SnakeBite kept in 00.dat (those not in a_chunk7.dat)
    ///   base 01 = an empty archive (SnakeBite moved every vanilla texture to a_texture7.dat)
    ///   mods    = snakebite.xml's ModEntries, in order
    /// Then the archives this state implies are computed in temp files and compared with the game's current
    /// 00.dat / 01.dat by decoded content, entry by entry. Only if they agree is the manifest written; the game's
    /// files are never touched by the adoption. check_only: compare and report, write nothing.
    pub fn setup_snakebite(&mut self, check_only: bool) -> Result<bool, String> {
        let r = self.setup_snakebite_op(check_only);
        self.finish_op(r)
    }

    fn setup_snakebite_op(&mut self, check_only: bool) -> Result<bool, String> {
        self.require_profile()?;
        let xml_path = self.root.join("snakebite.xml");
        let text =
            fs::read_to_string(&xml_path).map_err(|e| format!("{}: {e}", xml_path.display()))?;
        let st = SbState::read(&text)?;
        let m0 = self.master().join("0");
        let orig00 = m0.join("00.dat.original");
        let orig01 = m0.join("01.dat.original");
        let chunk7 = self.master().join("a_chunk7.dat");
        for p in [&orig00, &orig01, &chunk7] {
            if !p.exists() {
                return Err(format!("not a SnakeBite layout: missing {}", p.display()));
            }
        }
        fs::create_dir_all(self.state().join("shadow")).map_err(|e| e.to_string())?;
        // base archives
        let t = std::time::Instant::now();
        let in7: HashSet<u64> = {
            let mut f = BufReader::new(File::open(&chunk7).map_err(|e| e.to_string())?);
            self.qar_context()?
                .read_index(&mut f)?
                .entries
                .iter()
                .map(|e| e.hash)
                .collect()
        };
        let b00 = self.base("00");
        let b01 = self.base("01");
        {
            let mut f =
                BufReader::with_capacity(1 << 20, File::open(&orig00).map_err(|e| e.to_string())?);
            let idx = self.qar_context()?.read_index(&mut f)?;
            // SnakeBite setup: the system files of 00.dat.original not moved to a_chunk7 stay; its lua files go to
            // 01.dat ("Lua files will remain in 01.dat, due to foxfs.dat limitations"), the rest (foxpatch.dat) to 00.dat
            let mut list00 = vec![];
            let mut list01 = vec![];
            for e in idx.entries.iter().filter(|e| !in7.contains(&e.hash)) {
                let mut raw = vec![0u8; e.raw_len() as usize];
                f.seek(SeekFrom::Start(e.offset))
                    .map_err(|x| x.to_string())?;
                f.read_exact(&mut raw).map_err(|x| x.to_string())?;
                let is_lua = qar::ext_of(e.hash) == Some("lua");
                if is_lua { &mut list01 } else { &mut list00 }.push(qar::RawEntry {
                    hash: e.hash,
                    raw,
                    pad: None,
                });
            }
            let mut w = BufWriter::new(File::create(&b00).map_err(|e| e.to_string())?);
            self.qar_context()?
                .write_archive(&mut w, idx.header.flags, idx.header.version, &list00)
                .map_err(|e| e.to_string())?;
            w.flush().map_err(|e| e.to_string())?;
            let mut f1 = BufReader::new(File::open(&orig01).map_err(|e| e.to_string())?);
            let h = self.qar_context()?.read_index(&mut f1)?.header;
            let mut w = BufWriter::new(File::create(&b01).map_err(|e| e.to_string())?);
            self.qar_context()?
                .write_archive(&mut w, h.flags, h.version, &list01)
                .map_err(|e| e.to_string())?;
            w.flush().map_err(|e| e.to_string())?;
            self.say(format!("base: {} system entries of 00.dat.original kept by SnakeBite ({} in 00.dat, {} lua in 01.dat)",
                             list00.len() + list01.len(), list00.len(), list01.len()));
        }
        // mods from snakebite.xml
        let merged: HashMap<String, MergedPack> = st
            .merged()
            .into_iter()
            .map(|m| (m.path.clone(), m))
            .collect();
        let mut man = Manifest {
            format: 1,
            tool: format!("foxinstall {}", env!("CARGO_PKG_VERSION")),
            layout: "gzstool".into(),
            mode: "snakebite".into(),
            ..Default::default()
        };
        for a in ARCHIVES {
            let b = self.base(a);
            man.base.insert(
                a.into(),
                FileSig {
                    size: fs::metadata(&b).map_err(|e| e.to_string())?.len(),
                    md5: md5_file(&b)?,
                },
            );
        }
        for me in st.mods() {
            let mut m = InstalledMod {
                name: me.attr("Name").unwrap_or("").into(),
                version: me.attr("Version").unwrap_or("").into(),
                author: me.attr("Author").unwrap_or("").into(),
                website: me.attr("Website").unwrap_or("").into(),
                description: me
                    .child("Description")
                    .and_then(|d| d.text.clone())
                    .unwrap_or_default(),
                package_md5: String::new(),
                installed: "imported from snakebite.xml".into(),
                files: vec![],
                packs: vec![],
                loose: vec![],
                sb_entry: Some(xmltree::write_elem(me)),
            };
            let fpk: Vec<&Elem> = me
                .child("FpkEntries")
                .map(|f| f.children.iter().collect())
                .unwrap_or_default();
            for q in me
                .child("QarEntries")
                .map(|q| q.children.iter().collect::<Vec<_>>())
                .unwrap_or_default()
            {
                let path = q.attr("FilePath").unwrap_or("").to_string();
                let hash: u64 = q
                    .attr("Hash")
                    .and_then(|h| h.parse().ok())
                    .unwrap_or_else(|| qar::file_hash(&path));
                if merged.contains_key(&path) {
                    let entries = fpk
                        .iter()
                        .filter(|e| e.attr("FpkFile") == Some(path.as_str()))
                        .map(|e| {
                            (
                                e.attr("FilePath").unwrap_or("").to_string(),
                                e.attr("ContentHash").unwrap_or("").to_string(),
                            )
                        })
                        .collect();
                    m.packs.push(PackItem {
                        pack: path.clone(),
                        archive: archive_for(&path).into(),
                        entries,
                        references: vec![],
                    });
                } else {
                    m.files.push(FileItem {
                        path: path.clone(),
                        hash: format!("{hash:016x}"),
                        archive: archive_for(&path).into(),
                        md5: q.attr("ContentHash").unwrap_or("").to_string(),
                    });
                }
            }
            for f in me
                .child("FileEntries")
                .map(|f| f.children.iter().collect::<Vec<_>>())
                .unwrap_or_default()
            {
                m.loose.push(LooseItem {
                    path: f
                        .attr("FilePath")
                        .unwrap_or("")
                        .trim_start_matches('/')
                        .to_string(),
                    md5: f.attr("ContentHash").unwrap_or("").into(),
                    replaced_original: false,
                });
            }
            man.mods.push(m);
        }
        self.say(format!(
            "imported {} mods from snakebite.xml ({} merged packs) in {:.1} s",
            man.mods.len(),
            merged.len(),
            t.elapsed().as_secs_f64()
        ));
        // compute the implied archives (temp files) and compare with the game's by content
        let mut vanilla = self.vanilla_sources()?;
        let mut current: Vec<crate::Archive> = ARCHIVES
            .iter()
            .map(|a| crate::Archive::open(self.qar_context()?, &self.dat(a)))
            .collect::<Result<_, _>>()?;
        let mut renames = vec![];
        self.force_rewrite = true;
        let r = self.write_targets(
            &man,
            &mut current,
            &mut vanilla,
            &HashMap::new(),
            &HashMap::new(),
            &mut renames,
        );
        self.force_rewrite = false;
        r?;
        let mut ok = true;
        for (tmp, dst) in &renames {
            let (same, diffs) = compare_content_with_context(self.qar_context()?, tmp, dst)?;
            self.say(format!(
                "{}: {same} entries agree, {} differ",
                dst.display(),
                diffs.len()
            ));
            for d in diffs.iter().take(10) {
                self.say(format!("   {d}"));
            }
            ok &= diffs.is_empty();
        }
        for (tmp, _) in &renames {
            let _ = fs::remove_file(tmp);
        }
        if !ok || check_only {
            let _ = fs::remove_file(&b00);
            let _ = fs::remove_file(&b01);
            self.say(if ok {
                "adoption check: OK (nothing written)"
            } else {
                "adoption check: DIFFERENCES - not adopted"
            });
            return Ok(ok);
        }
        self.save_manifest(&man)?;
        self.say(
            "adopted: foxinstall now manages this SnakeBite game (snakebite.xml kept in sync)",
        );
        Ok(true)
    }

    /// snakebite.xml for manifest `m` into a temp file: (tmp, final path)
    pub(crate) fn stage_snakebite_xml(
        &self,
        m: &Manifest,
        renames: &[(PathBuf, PathBuf)],
    ) -> Result<(PathBuf, PathBuf), String> {
        let dst = self.root.join("snakebite.xml");
        let text = fs::read_to_string(&dst).map_err(|e| format!("{}: {e}", dst.display()))?;
        let old = SbState::read(&text)?;
        // merged packs: keep SnakeBite's records (order, vanilla entries) for packs still merged; add new ones
        let still: Vec<&PackItem> = m.mods.iter().flat_map(|x| x.packs.iter()).collect();
        let still_paths: HashSet<&str> = still.iter().map(|p| p.pack.as_str()).collect();
        let mut merged: Vec<MergedPack> = old
            .merged()
            .into_iter()
            .filter(|p| still_paths.contains(p.path.as_str()))
            .collect();
        let modded: BTreeMap<&str, HashSet<String>> =
            still.iter().fold(BTreeMap::new(), |mut acc, p| {
                acc.entry(p.pack.as_str())
                    .or_insert_with(HashSet::new)
                    .extend(p.entries.iter().map(|(ip, _)| sb::sb_inner_path(ip)));
                acc
            });
        for p in &still {
            if merged.iter().any(|x| x.path == p.pack) {
                continue;
            }
            let vanilla = self
                .merge_vanilla_entries
                .get(&p.pack)
                .cloned()
                .unwrap_or_default();
            let mine = modded.get(p.pack.as_str()).cloned().unwrap_or_default();
            merged.push(MergedPack {
                path: p.pack.clone(),
                hash: qar::file_hash(&p.pack),
                source_name: self.merge_sources.get(&p.pack).cloned().unwrap_or_default(),
                vanilla_entries: vanilla
                    .iter()
                    .map(|v| sb::sb_inner_path(v))
                    .filter(|v| !mine.contains(v))
                    .collect(),
            });
        }
        let mods: Vec<Elem> = m
            .mods
            .iter()
            .filter_map(|x| {
                x.sb_entry
                    .as_ref()
                    .and_then(|t| xmltree::parse_elem(t).ok())
            })
            .collect();
        // SnakeBite's DatHash (SettingsManager.UpdateDatHash): MD5(00.dat) + MD5(01.dat), uppercase hex, of the files
        // as they will be after this commit (a staged temp file, or the archive already updated in place). SnakeBite
        // validates it at start and complains about a game "modified outside SnakeBite" when it does not match.
        let mut dat_hash = String::new();
        for a in ["00", "01"] {
            let dat = self.dat(a);
            let src = renames
                .iter()
                .find(|(_, d)| d == &dat)
                .map(|(t, _)| t.clone())
                .unwrap_or(dat);
            dat_hash.push_str(&crate::md5_file_pub(&src)?);
        }
        let doc = sb::build(&old, &dat_hash, &merged, &mods);
        let tmp = self.tmp(dst.with_extension("xml.foxtmp"));
        fs::write(&tmp, xmltree::write(&doc)).map_err(|e| e.to_string())?;
        Ok((tmp, dst))
    }
}

/// compare two archives by decoded content per entry hash: (agreeing count, differences)
pub fn compare_content_with_context(
    context: &qar::Context,
    a: &std::path::Path,
    b: &std::path::Path,
) -> Result<(usize, Vec<String>), String> {
    use md5::{Digest, Md5};
    let mut fa = BufReader::new(File::open(a).map_err(|e| e.to_string())?);
    let mut fb = BufReader::new(File::open(b).map_err(|e| e.to_string())?);
    let ia = context.read_index(&mut fa)?;
    let ib = context.read_index(&mut fb)?;
    let mb: HashMap<u64, &qar::Entry> = ib.entries.iter().map(|e| (e.hash, e)).collect();
    let mut seen = HashSet::new();
    let mut same = 0;
    let mut diffs = vec![];
    let mut reordered: Vec<String> = vec![];
    let content = |f: &mut BufReader<File>, e: &qar::Entry| -> Result<[u8; 16], String> {
        let mut s = vec![0u8; e.stored as usize];
        f.seek(SeekFrom::Start(e.offset + 32))
            .map_err(|x| x.to_string())?;
        f.read_exact(&mut s).map_err(|x| x.to_string())?;
        Ok(Md5::digest(context.decode(e, &s)?).into())
    };
    for e in &ia.entries {
        seen.insert(e.hash);
        match mb.get(&e.hash) {
            None => diffs.push(format!("{:016x} only in the computed archive", e.hash)),
            Some(o) => {
                if content(&mut fa, e)? == content(&mut fb, o)? {
                    same += 1;
                } else if same_pack_contents(context, &mut fa, e, &mut fb, o)? {
                    // merged packs: same inner files and references, only the entry order differs (our writer puts
                    // the fox2 first, as every vanilla .fpkd does)
                    same += 1;
                    reordered.push(format!("{:016x}", e.hash));
                } else {
                    diffs.push(format!("{:016x} content differs", e.hash));
                }
            }
        }
    }
    for e in &ib.entries {
        if !seen.contains(&e.hash) {
            diffs.push(format!("{:016x} only in the game's archive", e.hash));
        }
    }
    if !reordered.is_empty() {
        diffs.retain(|_| true);
        eprintln!(
            "note: {} merged pack(s) agree as sets of inner files (entry order differs)",
            reordered.len()
        );
    }
    Ok((same, diffs))
}

fn decoded(
    context: &qar::Context,
    f: &mut BufReader<File>,
    e: &qar::Entry,
) -> Result<Vec<u8>, String> {
    let mut s = vec![0u8; e.stored as usize];
    f.seek(SeekFrom::Start(e.offset + 32))
        .map_err(|x| x.to_string())?;
    f.read_exact(&mut s).map_err(|x| x.to_string())?;
    context.decode(e, &s)
}

/// two fpk/fpkd blobs with the same (inner path -> bytes) map and the same references
fn same_pack_contents(
    context: &qar::Context,
    fa: &mut BufReader<File>,
    a: &qar::Entry,
    fb: &mut BufReader<File>,
    b: &qar::Entry,
) -> Result<bool, String> {
    let (x, y) = (decoded(context, fa, a)?, decoded(context, fb, b)?);
    if !(x.starts_with(b"foxfpk") && y.starts_with(b"foxfpk")) {
        return Ok(false);
    }
    let (px, py) = (foxcore::fpk::read(&x)?, foxcore::fpk::read(&y)?);
    let map =
        |p: &foxcore::fpk::Package, d: &[u8]| -> std::collections::BTreeMap<String, Vec<u8>> {
            p.entries
                .iter()
                .map(|e| {
                    (
                        e.path.clone(),
                        d[e.offset as usize..(e.offset + e.size) as usize].to_vec(),
                    )
                })
                .collect()
        };
    let mut rx = px.references.clone();
    let mut ry = py.references.clone();
    rx.sort();
    ry.sort();
    Ok(px.kind == py.kind && rx == ry && map(&px, &x) == map(&py, &y))
}

#[cfg(feature = "internal-game-data")]
pub fn compare_content(
    a: &std::path::Path,
    b: &std::path::Path,
) -> Result<(usize, Vec<String>), String> {
    compare_content_with_context(&qar::Context::internal(), a, b)
}
