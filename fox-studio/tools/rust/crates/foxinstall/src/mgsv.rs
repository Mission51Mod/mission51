//! .mgsv mod packages: a zip with metadata.xml (ModEntry Name/Version/Author/Website + Description), game files
//! under Assets/ (and any other top-level folder except GameDir/) that go into the game's archives under their path,
//! and GameDir/* loose files copied into the game folder.
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

pub struct ModPackage {
    pub path: PathBuf,
    pub name: String,
    pub version: String,
    pub author: String,
    pub website: String,
    pub description: String,
    /// (zip name, game path "/Assets/...")
    pub archive_files: Vec<(String, String)>,
    /// (zip name, path relative to the game folder)
    pub loose_files: Vec<(String, String)>,
    pub skipped: Vec<String>,
    /// the ModEntry element as SnakeBite stores it in snakebite.xml (metadata without xmlns, SourceType="Mod")
    pub mod_entry_xml: Option<String>,
    /// metadata.xml as found in the package / staging folder
    pub meta_text: String,
    zip: Option<zip::ZipArchive<File>>,
    /// a staging folder (the MakeBite input tree) instead of a .mgsv
    dir: Option<PathBuf>,
    /// a package held in memory (mod-relative path -> bytes)
    mem: Option<std::collections::HashMap<String, Vec<u8>>>,
}

fn attr(xml: &str, tag: &str, name: &str) -> String {
    // first <tag ... name="..."> (attribute values are XML-escaped)
    if let Some(i) = xml.find(&format!("<{tag}")) {
        let rest = &xml[i..];
        let end = rest.find('>').unwrap_or(rest.len());
        let head = &rest[..end];
        let key = format!(" {name}=\"");
        if let Some(j) = head.find(&key) {
            let v = &head[j + key.len()..];
            if let Some(k) = v.find('"') {
                return unescape(&v[..k]);
            }
        }
    }
    String::new()
}

fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&apos;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

impl ModPackage {
    pub fn open(path: &Path) -> Result<ModPackage, String> {
        let f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut zip = zip::ZipArchive::new(f).map_err(|e| format!("{}: not a zip ({e})", path.display()))?;
        let mut meta = String::new();
        zip.by_name("metadata.xml")
            .map_err(|_| format!("{}: no metadata.xml", path.display()))?
            .read_to_string(&mut meta)
            .map_err(|e| e.to_string())?;
        let description = meta
            .find("<Description>")
            .and_then(|i| meta[i + 13..].find("</Description>").map(|j| unescape(&meta[i + 13..i + 13 + j])))
            .unwrap_or_default();
        let mut archive_files = Vec::new();
        let mut loose_files = Vec::new();
        let mut skipped = Vec::new();
        for i in 0..zip.len() {
            let e = zip.by_index(i).map_err(|e| e.to_string())?;
            if e.is_dir() {
                continue;
            }
            let n = e.name().replace('\\', "/");
            if n == "metadata.xml" {
                continue;
            }
            if let Some(rel) = n.strip_prefix("GameDir/") {
                loose_files.push((n.clone(), rel.to_string()));
            } else if n.to_lowercase().ends_with(".wmv") {
                skipped.push(n);
            } else {
                archive_files.push((n.clone(), format!("/{n}")));
            }
        }
        Ok(ModPackage {
            path: path.to_path_buf(),
            name: attr(&meta, "ModEntry", "Name"),
            version: attr(&meta, "ModEntry", "Version"),
            author: attr(&meta, "ModEntry", "Author"),
            website: attr(&meta, "ModEntry", "Website"),
            description,
            archive_files,
            loose_files,
            skipped,
            mod_entry_xml: crate::xmltree::parse(meta.trim_start_matches('\u{feff}'))
                .ok()
                .map(|d| crate::xmltree::write_elem(&crate::snakebite::mod_entry_from_metadata(&d.root))),
            meta_text: meta.clone(),
            zip: Some(zip),
            dir: None,
            mem: None,
        })
    }

    /// A staging folder (what MakeBite would zip): metadata.xml, game files (Assets/..., Tpp/..., ...) that go into
    /// the archives, GameDir/... loose files. Raw pack folders (*_fpk / *_fpkd) are refused unless the built pack
    /// next to them exists (then they are ignored, as mbbuild does). The ModEntry for snakebite.xml is completed the
    /// way MakeBite writes it: QarEntries (Hash, FilePath, Compressed = fpk/fpkd, ContentHash), FpkEntries for every
    /// pack's inner files, FileEntries, WmvEntries.
    pub fn open_dir(path: &Path) -> Result<ModPackage, String> {
        let meta = std::fs::read_to_string(path.join("metadata.xml"))
            .map_err(|_| format!("{}: no metadata.xml", path.display()))?;
        let meta = meta.trim_start_matches('\u{feff}').to_string();
        let description = meta
            .find("<Description>")
            .and_then(|i| meta[i + 13..].find("</Description>").map(|j| unescape(&meta[i + 13..i + 13 + j])))
            .unwrap_or_default();
        let mut files: Vec<String> = vec![];
        let mut stack = vec![path.to_path_buf()];
        while let Some(d) = stack.pop() {
            let mut ents: Vec<std::fs::DirEntry> = std::fs::read_dir(&d).map_err(|e| e.to_string())?.flatten().collect();
            ents.sort_by_key(|e| e.file_name());
            let mut subdirs = vec![];
            for e in ents {
                let p = e.path();
                let rel = p.strip_prefix(path).unwrap().to_string_lossy().replace('\\', "/");
                if p.is_dir() {
                    let lname = rel.to_lowercase();
                    if lname.ends_with("_fpk") || lname.ends_with("_fpkd") {
                        let built = format!("{}.{}", &rel[..rel.rfind('_').unwrap()], &rel[rel.rfind('_').unwrap() + 1..]);
                        if !path.join(&built).exists() {
                            return Err(format!("raw pack folder {rel} without its built pack {built}: build the packs first"));
                        }
                        continue;
                    }
                    subdirs.push(p);
                } else if rel != "metadata.xml" {
                    files.push(rel);
                }
            }
            for sd in subdirs.into_iter().rev() {
                stack.push(sd);
            }
        }
        let mut archive_files = vec![];
        let mut loose_files = vec![];
        let mut skipped = vec![];
        for n in files {
            if let Some(rel) = n.strip_prefix("GameDir/") {
                loose_files.push((n.clone(), rel.to_string()));
            } else if n.to_lowercase().ends_with(".wmv") {
                skipped.push(n);
            } else {
                archive_files.push((n.clone(), format!("/{n}")));
            }
        }
        let mut pkg = ModPackage {
            path: path.to_path_buf(),
            name: attr(&meta, "ModEntry", "Name"),
            version: attr(&meta, "ModEntry", "Version"),
            author: attr(&meta, "ModEntry", "Author"),
            website: attr(&meta, "ModEntry", "Website"),
            description,
            archive_files,
            loose_files,
            skipped,
            mod_entry_xml: None,
            meta_text: meta.clone(),
            zip: None,
            dir: Some(path.to_path_buf()),
            mem: None,
        };
        pkg.mod_entry_xml = Some(pkg.makebite_mod_entry(&meta)?);
        Ok(pkg)
    }

    /// A package held in memory, laid out like a staging folder: metadata.xml text plus (mod-relative path with '/',
    /// bytes): "Assets/..." (and other top-level folders) go into the archives, "GameDir/..." are loose files.
    /// Built packs only (raw *_fpk / *_fpkd folders are refused). The ModEntry is completed as MakeBite would.
    pub fn from_memory(metadata_xml: &str, files: Vec<(String, Vec<u8>)>) -> Result<ModPackage, String> {
        let meta = metadata_xml.trim_start_matches('\u{feff}').to_string();
        let description = meta
            .find("<Description>")
            .and_then(|i| meta[i + 13..].find("</Description>").map(|j| unescape(&meta[i + 13..i + 13 + j])))
            .unwrap_or_default();
        let mut mem = std::collections::HashMap::new();
        let mut names: Vec<String> = vec![];
        for (n, b) in files {
            let n = n.replace('\\', "/").trim_start_matches('/').to_string();
            if n == "metadata.xml" {
                continue;
            }
            let dirpart = n.rsplit_once('/').map(|x| x.0.to_lowercase()).unwrap_or_default();
            if dirpart.split('/').any(|c| c.ends_with("_fpk") || c.ends_with("_fpkd")) {
                return Err(format!("raw pack folder entry {n}: hand over the built pack"));
            }
            if mem.insert(n.clone(), b).is_some() {
                return Err(format!("{n} given twice"));
            }
            names.push(n);
        }
        names.sort();
        let (mut archive_files, mut loose_files, mut skipped) = (vec![], vec![], vec![]);
        for n in names {
            if let Some(rel) = n.strip_prefix("GameDir/") {
                loose_files.push((n.clone(), rel.to_string()));
            } else if n.to_lowercase().ends_with(".wmv") {
                skipped.push(n);
            } else {
                archive_files.push((n.clone(), format!("/{n}")));
            }
        }
        let mut pkg = ModPackage {
            path: PathBuf::from("<memory>"),
            name: attr(&meta, "ModEntry", "Name"),
            version: attr(&meta, "ModEntry", "Version"),
            author: attr(&meta, "ModEntry", "Author"),
            website: attr(&meta, "ModEntry", "Website"),
            description,
            archive_files,
            loose_files,
            skipped,
            mod_entry_xml: None,
            meta_text: meta.clone(),
            zip: None,
            dir: None,
            mem: Some(mem),
        };
        pkg.mod_entry_xml = Some(pkg.makebite_mod_entry(&meta)?);
        Ok(pkg)
    }

    /// the ModEntry MakeBite would write for this package, as SnakeBite stores it
    fn makebite_mod_entry(&mut self, meta: &str) -> Result<String, String> {
        let me = self.makebite_metadata(meta)?;
        Ok(crate::xmltree::write_elem(&crate::snakebite::mod_entry_from_metadata(&me)))
    }

    /// the metadata.xml root MakeBite writes into the .mgsv for this package: the staging metadata's ModEntry
    /// (xmlns:xsd, xmlns:xsi first, as XmlSerializer writes them) with QarEntries (Hash, FilePath, Compressed =
    /// fpk/fpkd, ContentHash), FpkEntries (every pack's inner files, "Assets\x" paths), FileEntries, WmvEntries
    pub fn makebite_metadata(&mut self, meta: &str) -> Result<crate::xmltree::Elem, String> {
        use crate::xmltree::Elem;
        let doc = crate::xmltree::parse(meta.trim_start_matches('\u{feff}'))?;
        let mut me = doc.root.clone();
        let rest: Vec<(String, String)> = me.attrs.iter().filter(|(k, _)| !k.starts_with("xmlns")).cloned().collect();
        me.attrs = vec![("xmlns:xsd".into(), "http://www.w3.org/2001/XMLSchema".into()),
                        ("xmlns:xsi".into(), "http://www.w3.org/2001/XMLSchema-instance".into())];
        me.attrs.extend(rest);
        me.children.retain(|c| !matches!(c.name.as_str(), "QarEntries" | "FpkEntries" | "FileEntries" | "WmvEntries"));
        let mut q = Elem::new("QarEntries");
        let mut f = Elem::new("FpkEntries");
        for (zn, gp) in self.archive_files.clone() {
            let data = self.read(&zn)?;
            let l = gp.to_lowercase();
            let packed = l.ends_with(".fpk") || l.ends_with(".fpkd");
            q.children.push(Elem::new("QarEntry").with("Hash", foxcore::qar::file_hash(&gp).to_string()).with("FilePath", &gp)
                .with("Compressed", if packed { "true" } else { "false" }).with("ContentHash", crate::md5_hex(&data)));
            if packed {
                let pk = foxcore::fpk::read(&data)?;
                for e in &pk.entries {
                    // MakeBite hashes the entry as GzsTool unpacks it (obfuscated scripts decrypted)
                    let raw = &data[e.offset as usize..(e.offset + e.size) as usize];
                    let b = &*foxcore::fpk::gzs_view(&e.path, raw);
                    f.children.push(Elem::new("FpkEntry").with("FpkFile", &gp)
                        .with("FilePath", e.path.trim_start_matches('/').replace('/', "\\"))
                        .with("ContentHash", crate::md5_hex(b)));
                }
            }
        }
        let mut fe = Elem::new("FileEntries");
        for (zn, rel) in self.loose_files.clone() {
            let data = self.read(&zn)?;
            fe.children.push(Elem::new("FileEntry").with("FilePath", format!("/{rel}")).with("ContentHash", crate::md5_hex(&data)));
        }
        me.children.push(q);
        me.children.push(f);
        me.children.push(fe);
        me.children.push(Elem::new("WmvEntries"));
        Ok(me)
    }

    pub fn read(&mut self, zip_name: &str) -> Result<Vec<u8>, String> {
        if let Some(m) = &self.mem {
            return m.get(zip_name).cloned().ok_or_else(|| format!("{zip_name}: not in the package"));
        }
        if let Some(d) = &self.dir {
            return std::fs::read(d.join(zip_name)).map_err(|e| format!("{zip_name}: {e}"));
        }
        let mut e = self.zip.as_mut().unwrap().by_name(zip_name).map_err(|e| format!("{zip_name}: {e}"))?;
        let mut b = Vec::with_capacity(e.size() as usize);
        e.read_to_end(&mut b).map_err(|e| format!("{zip_name}: {e}"))?;
        Ok(b)
    }
}
