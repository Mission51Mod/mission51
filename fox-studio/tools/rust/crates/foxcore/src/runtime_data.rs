//! Private runtime facts learned from the user's own local archives.
//!
//! This module contains the schema and the learning algorithm only. A profile is
//! local cache data: it must never be included in a public source or binary export.
use crate::{fpk, qar};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use sha2::{Digest as ShaDigest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const PROFILE_FILE: &str = "runtime_profile.json";
pub const SCHEMA_VERSION: u32 = 1;
const MIN_PACKS: u32 = 5;
const MAX_PROFILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 4 * 1024 * 1024;
const MAX_STORED_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QarKeys {
    pub header_masks: [u32; 4],
    pub layer1: [u32; 8],
}

#[derive(Debug)]
pub enum Error {
    Io { path: PathBuf, detail: String },
    Invalid(String),
    Unsupported(String),
    WrongInstall(String),
    Cancelled,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, detail } => write!(f, "{}: {detail}", path.display()),
            Self::Invalid(s) => write!(f, "invalid runtime profile: {s}; run setup again"),
            Self::Unsupported(s) => write!(f, "unsupported local archive layout: {s}"),
            Self::WrongInstall(s) => write!(
                f,
                "runtime profile belongs to a different install: {s}; run setup again"
            ),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::error::Error for Error {}

fn io_error(path: &Path, e: impl fmt::Display) -> Error {
    Error::Io {
        path: path.to_owned(),
        detail: e.to_string(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderRule {
    pub before: String,
    pub after: String,
    pub samples: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KindOrder {
    pub packages: u32,
    pub rules: Vec<OrderRule>,
    pub rank: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackOrder {
    pub fpk: KindOrder,
    pub fpkd: KindOrder,
}

impl PackOrder {
    pub fn kind(&self, kind: fpk::Kind) -> &KindOrder {
        match kind {
            fpk::Kind::Fpk => &self.fpk,
            fpk::Kind::Fpkd => &self.fpkd,
        }
    }

    pub fn validate(&self) -> Result<(), Error> {
        for order in [&self.fpk, &self.fpkd] {
            if order.packages < MIN_PACKS {
                return Err(Error::Invalid(
                    "fewer than five package observations for one kind".into(),
                ));
            }
            let derived = canonical_rank(&order.rules)?;
            if order.rank != derived {
                return Err(Error::Invalid(
                    "package type rank does not match its rules".into(),
                ));
            }
            let mut seen = BTreeSet::new();
            for rule in &order.rules {
                if rule.before.is_empty()
                    || rule.after.is_empty()
                    || rule.before == rule.after
                    || rule.samples < MIN_PACKS
                    || rule.samples > order.packages
                    || !seen.insert((&rule.before, &rule.after))
                {
                    return Err(Error::Invalid(
                        "invalid or duplicate package order rule".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn violations<'a>(
        &'a self,
        kind: fpk::Kind,
        paths: &[&str],
    ) -> Vec<(usize, usize, &'a str, &'a str)> {
        let types: Vec<String> = paths.iter().map(|p| fpk::entry_type(p)).collect();
        self.kind(kind)
            .rules
            .iter()
            .filter_map(|rule| {
                let last = types.iter().rposition(|t| t == &rule.before)?;
                let first = types.iter().position(|t| t == &rule.after)?;
                (last > first).then_some((last, first, rule.before.as_str(), rule.after.as_str()))
            })
            .collect()
    }

    /// Keep the input order whenever it already satisfies every learned constraint.
    pub fn vanilla_order(&self, kind: fpk::Kind, paths: &[&str]) -> Result<Vec<usize>, String> {
        let types: Vec<String> = paths.iter().map(|p| fpk::entry_type(p)).collect();
        let rules = &self.kind(kind).rules;
        let mut remaining = BTreeMap::<&str, usize>::new();
        for ty in &types {
            *remaining.entry(ty).or_default() += 1;
        }
        let mut done = vec![false; paths.len()];
        let mut output = Vec::with_capacity(paths.len());
        while output.len() < paths.len() {
            let selected = (0..paths.len())
                .find(|&i| {
                    !done[i]
                        && rules
                            .iter()
                            .filter(|r| r.after == types[i])
                            .all(|r| remaining.get(r.before.as_str()).copied().unwrap_or(0) == 0)
                })
                .ok_or("package ordering rules contain a cycle")?;
            done[selected] = true;
            *remaining
                .get_mut(types[selected].as_str())
                .ok_or("missing package type")? -= 1;
            output.push(selected);
        }
        Ok(output)
    }

    /// Stable canonical sorting, with unknown types following their previous known type.
    pub fn sort_entries(&self, kind: fpk::Kind, paths: &[&str]) -> Result<Vec<usize>, String> {
        let rank: BTreeMap<&str, i64> = self
            .kind(kind)
            .rank
            .iter()
            .enumerate()
            .map(|(i, t)| (t.as_str(), i as i64))
            .collect();
        let mut current = -1;
        let keys: Vec<(i64, u8)> = paths
            .iter()
            .map(|p| match rank.get(fpk::entry_type(p).as_str()) {
                Some(&r) => {
                    current = r;
                    (r, 0)
                }
                None => (current, 1),
            })
            .collect();
        let mut indices: Vec<usize> = (0..paths.len()).collect();
        indices.sort_by_key(|&i| keys[i]);
        let sorted: Vec<&str> = indices.iter().map(|&i| paths[i]).collect();
        if !self.violations(kind, &sorted).is_empty() {
            return Err("canonical package sort still violates learned rules".into());
        }
        Ok(indices)
    }

    pub fn write(
        &self,
        kind: fpk::Kind,
        entries: &[(&str, &[u8])],
        references: &[&str],
    ) -> Result<Vec<u8>, String> {
        let paths: Vec<&str> = entries.iter().map(|e| e.0).collect();
        let order = self.vanilla_order(kind, &paths)?;
        let sorted: Vec<(&str, &[u8])> = order.iter().map(|&i| entries[i]).collect();
        Ok(fpk::write_in_order(kind, &sorted, references))
    }

    #[cfg(feature = "internal-game-data")]
    pub fn internal() -> Self {
        fn historical(rules: &[(&str, &str)], rank: &[&str]) -> KindOrder {
            KindOrder {
                packages: MIN_PACKS,
                rules: rules
                    .iter()
                    .map(|&(a, b)| OrderRule {
                        before: a.to_owned(),
                        after: b.to_owned(),
                        samples: MIN_PACKS,
                    })
                    .collect(),
                rank: rank.iter().map(|s| (*s).to_owned()).collect(),
            }
        }
        Self {
            fpk: historical(
                crate::packorder_rules::FPK,
                crate::packorder_rules::FPK_RANK,
            ),
            fpkd: historical(
                crate::packorder_rules::FPKD,
                crate::packorder_rules::FPKD_RANK,
            ),
        }
    }
}

fn canonical_rank(rules: &[OrderRule]) -> Result<Vec<String>, Error> {
    let mut successors = BTreeMap::<&str, BTreeSet<&str>>::new();
    let mut incoming = BTreeMap::<&str, usize>::new();
    for rule in rules {
        incoming.entry(&rule.before).or_default();
        incoming.entry(&rule.after).or_default();
        if successors
            .entry(&rule.before)
            .or_default()
            .insert(&rule.after)
        {
            *incoming
                .get_mut(rule.after.as_str())
                .ok_or_else(|| Error::Invalid("missing type".into()))? += 1;
        }
    }
    let mut ready: BTreeSet<&str> = incoming
        .iter()
        .filter_map(|(&t, &n)| (n == 0).then_some(t))
        .collect();
    let mut output = Vec::new();
    while let Some(ty) = ready.pop_first() {
        output.push(ty.to_owned());
        if let Some(next) = successors.get(ty) {
            for &child in next {
                let n = incoming
                    .get_mut(child)
                    .ok_or_else(|| Error::Invalid("missing type".into()))?;
                *n -= 1;
                if *n == 0 {
                    ready.insert(child);
                }
            }
        }
    }
    if output.len() != incoming.len() {
        return Err(Error::Invalid("cyclic package ordering rules".into()));
    }
    Ok(output)
}

#[derive(Default)]
pub struct OrderLearner {
    fpk: Observations,
    fpkd: Observations,
}

#[derive(Default)]
struct Observations {
    packages: u32,
    pairs: BTreeMap<(String, String), (u32, u32)>,
}

impl OrderLearner {
    pub fn observe(&mut self, package: &fpk::Package) {
        let observations = match package.kind {
            fpk::Kind::Fpk => &mut self.fpk,
            fpk::Kind::Fpkd => &mut self.fpkd,
        };
        observations.packages += 1;
        let mut entries: Vec<&fpk::Entry> = package.entries.iter().collect();
        entries.sort_by_key(|e| e.offset);
        let mut positions = BTreeMap::<String, (usize, usize)>::new();
        for (i, entry) in entries.iter().enumerate() {
            let ty = fpk::entry_type(&entry.path);
            positions
                .entry(ty)
                .and_modify(|p| p.1 = i)
                .or_insert((i, i));
        }
        for (a, (_, last)) in &positions {
            for (b, (first, _)) in &positions {
                if a == b {
                    continue;
                }
                let counts = observations
                    .pairs
                    .entry((a.clone(), b.clone()))
                    .or_default();
                counts.0 += 1;
                counts.1 += u32::from(last < first);
            }
        }
    }

    pub fn finish(self) -> Result<PackOrder, Error> {
        fn finish_kind(observations: Observations) -> Result<KindOrder, Error> {
            let rules: Vec<OrderRule> = observations
                .pairs
                .into_iter()
                .filter(|(_, (both, before))| *both >= MIN_PACKS && both == before)
                .map(|((before, after), (samples, _))| OrderRule {
                    before,
                    after,
                    samples,
                })
                .collect();
            let rank = canonical_rank(&rules)?;
            Ok(KindOrder {
                packages: observations.packages,
                rules,
                rank,
            })
        }
        let order = PackOrder {
            fpk: finish_kind(self.fpk)?,
            fpkd: finish_kind(self.fpkd)?,
        };
        order.validate()?;
        Ok(order)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveSource {
    pub relative_path: String,
    pub bytes: u64,
    /// SHA-256 of the header and section table, not game content.
    pub index_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProfile {
    pub schema_version: u32,
    pub keys: QarKeys,
    pub order: PackOrder,
    pub sources: Vec<ArchiveSource>,
}

impl RuntimeProfile {
    pub fn qar_context(&self) -> qar::Context {
        qar::Context::new(self.keys.clone())
    }
    pub fn pack_order(&self) -> &PackOrder {
        &self.order
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(Error::Invalid("unsupported schema version".into()));
        }
        self.order.validate()?;
        if self.sources.is_empty() {
            return Err(Error::Invalid("no source archive provenance".into()));
        }
        let mut seen = BTreeSet::new();
        for source in &self.sources {
            let path = Path::new(&source.relative_path);
            if path.is_absolute()
                || source.relative_path.contains('\\')
                || source.relative_path.contains(':')
                || path
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
                || !seen.insert(&source.relative_path)
                || source.bytes < 32
                || source.index_sha256.len() != 64
                || !source.index_sha256.bytes().all(|c| c.is_ascii_hexdigit())
            {
                return Err(Error::Invalid("invalid source archive provenance".into()));
            }
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, Error> {
        let file = File::open(path).map_err(|e| {
            io_error(
                path,
                format!("{e}; complete game setup to create a runtime profile"),
            )
        })?;
        if file.metadata().map_err(|e| io_error(path, e))?.len() > MAX_PROFILE_BYTES {
            return Err(Error::Invalid("profile exceeds size limit".into()));
        }
        let profile: Self = serde_json::from_reader(BufReader::new(file))
            .map_err(|e| Error::Invalid(e.to_string()))?;
        profile.validate()?;
        Ok(profile)
    }

    /// Atomic cache write. The caller must choose a cache outside the game install.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| Error::Invalid(e.to_string()))?;
        let parent = path
            .parent()
            .ok_or_else(|| Error::Invalid("profile needs a parent directory".into()))?;
        fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).map_err(|e| io_error(parent, e))?;
        use std::io::Write;
        temporary.write_all(&bytes).map_err(|e| io_error(path, e))?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|e| io_error(path, e))?;
        temporary
            .persist(path)
            .map_err(|e| io_error(path, e.error))?;
        Ok(())
    }

    pub fn validate_for_game(&self, game: &Path) -> Result<(), Error> {
        self.validate()?;
        let mut recovered = [None; 4];
        for source in &self.sources {
            let archive = source_archive(game, &source.relative_path);
            let mut actual = fingerprint(game, &archive, &self.qar_context())?;
            actual.relative_path = source.relative_path.clone();
            if actual != *source {
                return Err(Error::WrongInstall(source.relative_path.clone()));
            }
            let mut reader =
                BufReader::new(File::open(&archive).map_err(|e| io_error(&archive, e))?);
            validate_header_masks(&mut reader, self.keys.header_masks)
                .map_err(Error::Unsupported)?;
            if recovered.iter().any(Option::is_none) {
                recover_layer1_pairs(&mut reader, self.keys.header_masks, &mut recovered)
                    .map_err(Error::Unsupported)?;
            }
        }
        if complete_pairs(recovered)? != self.keys.layer1 {
            return Err(Error::Invalid(
                "first-layer keys do not match the local archives".into(),
            ));
        }
        Ok(())
    }

    /// Read only canonical base archives. Modded override archives never teach ordering facts.
    pub fn learn(game: &Path, progress: &mut dyn FnMut(f32, &str) -> bool) -> Result<Self, Error> {
        if !progress(0.0, "Checking local base archives") {
            return Err(Error::Cancelled);
        }
        let mut archives: Vec<PathBuf> = [
            "data1.dat",
            "chunk0.dat",
            "chunk1.dat",
            "chunk2.dat",
            "chunk3.dat",
            "chunk4.dat",
        ]
        .iter()
        .map(|n| game.join("master").join(n))
        .filter(|p| p.is_file())
        .collect();
        if archives.is_empty() {
            return Err(Error::Unsupported(
                "no canonical base archives under master/".into(),
            ));
        }
        for name in ["00", "01"] {
            let base = game.join("master/0").join(format!("{name}.dat"));
            let original = base.with_extension("dat.original");
            let foxbase = base.with_extension("dat.foxbase");
            if original.is_file() {
                archives.push(original);
            } else if foxbase.is_file() {
                archives.push(foxbase);
            } else if base.is_file() {
                if game.join("snakebite.xml").is_file()
                    || game.join("foxinstall/manifest.json").is_file()
                {
                    return Err(Error::Unsupported(format!(
                        "missing unmodified base for master/0/{name}.dat"
                    )));
                }
                archives.push(base);
            }
        }
        let first = &archives[0];
        let mut reader = BufReader::new(File::open(first).map_err(|e| io_error(first, e))?);
        let masks = derive_header_masks(&mut reader).map_err(Error::Unsupported)?;
        for archive in &archives {
            let mut reader = BufReader::new(File::open(archive).map_err(|e| io_error(archive, e))?);
            validate_header_masks(&mut reader, masks).map_err(Error::Unsupported)?;
        }
        let mut pairs = [None; 4];
        for archive in &archives {
            let mut reader = BufReader::new(File::open(archive).map_err(|e| io_error(archive, e))?);
            recover_layer1_pairs(&mut reader, masks, &mut pairs).map_err(Error::Unsupported)?;
            if pairs.iter().all(Option::is_some) {
                break;
            }
        }
        let layer1 = complete_pairs(pairs)?;
        let keys = QarKeys {
            header_masks: masks,
            layer1,
        };
        let context = qar::Context::new(keys.clone());
        let mut learner = OrderLearner::default();
        let mut sources = Vec::new();
        for (i, archive) in archives.iter().enumerate() {
            if !progress(
                i as f32 / archives.len() as f32,
                "Learning local archive keys and package ordering",
            ) {
                return Err(Error::Cancelled);
            }
            sources.push(fingerprint(game, archive, &context)?);
            let mut reader = BufReader::new(File::open(archive).map_err(|e| io_error(archive, e))?);
            let index = context
                .read_index(&mut reader)
                .map_err(Error::Unsupported)?;
            let mut entries: Vec<&qar::Entry> = index
                .entries
                .iter()
                .filter(|e| matches!(qar::ext_of(e.hash), Some("fpk" | "fpkd")))
                .collect();
            entries.sort_by_key(|e| e.offset);
            for (n, entry) in entries.iter().enumerate() {
                if n % 64 == 0
                    && !progress(
                        (i as f32 + n as f32 / entries.len().max(1) as f32) / archives.len() as f32,
                        "Reading local package headers",
                    )
                {
                    return Err(Error::Cancelled);
                }
                let package = read_package_metadata(&mut reader, entry, &context)
                    .map_err(Error::Unsupported)?;
                learner.observe(&package);
            }
        }
        let profile = Self {
            schema_version: SCHEMA_VERSION,
            keys,
            order: learner.finish()?,
            sources,
        };
        profile.validate_for_game(game)?;
        if !progress(1.0, "Local runtime profile ready") {
            return Err(Error::Cancelled);
        }
        Ok(profile)
    }
}

/// A fresh install's mutable override archive becomes an immutable .foxbase at
/// installer setup. Its stored index fingerprint must still match byte for byte.
fn source_archive(game: &Path, relative: &str) -> PathBuf {
    let path = game.join(relative);
    if matches!(relative, "master/0/00.dat" | "master/0/01.dat") {
        for extension in ["dat.original", "dat.foxbase"] {
            let base = path.with_extension(extension);
            if base.is_file() {
                return base;
            }
        }
    }
    path
}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(
        bytes[at..at + 4]
            .try_into()
            .expect("checked archive header length"),
    )
}

/// Version-one headers store a reserved zero and use a block-aligned archive length.
/// All inferred words are checked against the section table before key recovery.
pub fn derive_header_masks<R: Read + Seek>(reader: &mut R) -> Result<[u32; 4], String> {
    let length = reader.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut header = [0; 32];
    reader.read_exact(&mut header).map_err(|e| e.to_string())?;
    if word(&header, 0) != qar::MAGIC {
        return Err("expected SQAR base archive".into());
    }
    let first = word(&header, 24) ^ 1;
    let second = word(&header, 28);
    let flags = word(&header, 4) ^ first;
    let shift = if flags & 0x800 == 0 { 10 } else { 12 };
    if length < 1 << shift || length % (1 << shift) != 0 || length >> shift > u32::MAX as u64 {
        return Err("base archive is not block aligned".into());
    }
    let masks = [
        first,
        second,
        word(&header, 12),
        word(&header, 16) ^ (length >> shift) as u32,
    ];
    let count = word(&header, 8) ^ second;
    let data_offset = word(&header, 20) ^ first;
    if count == 0
        || 32u64 + 8u64 * count as u64 > data_offset as u64
        || data_offset as u64 >= length
        || !(data_offset as u64).is_multiple_of(1 << shift)
    {
        return Err("unsupported version-one base header or extra table".into());
    }
    Ok(masks)
}

fn validate_header_masks<R: Read + Seek>(reader: &mut R, masks: [u32; 4]) -> Result<(), String> {
    let length = reader.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut header = [0; 32];
    reader.read_exact(&mut header).map_err(|e| e.to_string())?;
    let count = word(&header, 8) ^ masks[1];
    let extra = word(&header, 12) ^ masks[2];
    let data = word(&header, 20) ^ masks[0];
    let version = word(&header, 24) ^ masks[0];
    let shift = if (word(&header, 4) ^ masks[0]) & 0x800 == 0 {
        10
    } else {
        12
    };
    if word(&header, 0) != qar::MAGIC
        || !matches!(version, 1 | 3)
        || word(&header, 28) != masks[1]
        || length % (1 << shift) != 0
        || (word(&header, 16) ^ masks[3]) as u64 != length >> shift
    {
        return Err("inconsistent version, length or header masks across base archives".into());
    }
    if 32u64 + 8u64 * count as u64 + 16u64 * extra as u64 > data as u64 || data as u64 > length {
        return Err("extra-table records exceed the archive table region".into());
    }
    Ok(())
}

fn recover_layer1_pairs<R: Read + Seek>(
    reader: &mut R,
    masks: [u32; 4],
    pairs: &mut [Option<[u32; 2]>; 4],
) -> Result<(), String> {
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut header = [0; 32];
    reader.read_exact(&mut header).map_err(|e| e.to_string())?;
    let count = word(&header, 8) ^ masks[1];
    let shift = if (word(&header, 4) ^ masks[0]) & 0x800 == 0 {
        10
    } else {
        12
    };
    let table_size = 8u64 * count as u64;
    if table_size > MAX_METADATA_BYTES {
        return Err("archive index exceeds learning limit".into());
    }
    let mut table = vec![0; table_size as usize];
    reader.read_exact(&mut table).map_err(|e| e.to_string())?;
    for i in 0..count as usize {
        let lo = word(&table, 8 * i) ^ masks[(i + (8 * i) / 5) % 4];
        let hi = word(&table, 8 * i + 4) ^ masks[(i + (8 * i).div_ceil(5)) % 4];
        let offset = (((hi as u64) << 32 | lo as u64) >> 40) << shift;
        let mut entry = [0; 48];
        reader
            .seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        reader.read_exact(&mut entry).map_err(|e| e.to_string())?;
        let hash =
            ((word(&entry, 4) ^ masks[0]) as u64) << 32 | (word(&entry, 0) ^ masks[0]) as u64;
        let ty = hash >> 51;
        let stored = word(&entry, 8) ^ masks[1];
        let unpacked = word(&entry, 12) ^ masks[2];
        if stored >= 16 && unpacked >= 16 {
            // Extended encryption headers record the payload's two sizes at 8 and 12.
            // The first eight stream bytes and the next eight share the same pair.
            for (first, second) in [(unpacked - 16, stored - 16), (stored - 16, unpacked - 16)] {
                let pair = [word(&entry, 40) ^ first, word(&entry, 44) ^ second];
                if word(&entry, 32) ^ pair[0] == qar::ENC_MAGIC2 {
                    let slot = (hash as u32 % 4) as usize;
                    if pairs[slot].is_some_and(|old| old != pair) {
                        return Err("conflicting local encrypted-header key evidence".into());
                    }
                    pairs[slot] = Some(pair);
                    break;
                }
            }
            if pairs.iter().all(Option::is_some) {
                break;
            }
        }
        if ty != qar::ext_id("fpk") && ty != qar::ext_id("fpkd") {
            continue;
        }
        if stored < 48 || stored != unpacked {
            continue;
        }
        // Bytes 8..16 of a plain package: "in", its own byte length, two reserved zero bytes.
        // At stream offsets 0 and 8 the same pair applies; this also checks the magic.
        let size = stored.to_le_bytes();
        let known = [b'i', b'n', size[0], size[1], size[2], size[3], 0, 0];
        let pair = [
            word(&entry, 40) ^ word(&known, 0),
            word(&entry, 44) ^ word(&known, 4),
        ];
        let mut magic = [0; 8];
        for n in 0..8 {
            magic[n] = entry[32 + n] ^ pair[n / 4].to_le_bytes()[n % 4];
        }
        let kind = if ty == qar::ext_id("fpkd") {
            fpk::Kind::Fpkd
        } else {
            fpk::Kind::Fpk
        };
        let valid_kind = match kind {
            fpk::Kind::Fpk => matches!(magic[6], 0 | b' '),
            fpk::Kind::Fpkd => magic[6] == b'd',
        };
        if !magic.starts_with(b"foxfpk") || magic[7] != b'w' || !valid_kind {
            continue;
        }
        let slot = (hash as u32 % 4) as usize;
        if pairs[slot].is_some_and(|old| old != pair) {
            return Err("conflicting local first-layer key evidence".into());
        }
        pairs[slot] = Some(pair);
        if pairs.iter().all(Option::is_some) {
            break;
        }
    }
    Ok(())
}

fn complete_pairs(pairs: [Option<[u32; 2]>; 4]) -> Result<[u32; 8], Error> {
    let mut output = [0; 8];
    for (i, pair) in pairs.into_iter().enumerate() {
        let pair = pair.ok_or_else(|| {
            Error::Unsupported(format!(
                "no local entry header proves first-layer key pair {i}"
            ))
        })?;
        output[2 * i..2 * i + 2].copy_from_slice(&pair);
    }
    Ok(output)
}

/// Read only the package's header, tables and strings when its entry is plain.
/// Large payloads remain on disk. Encrypted/compressed packages take the validated decoder path.
fn read_package_metadata<R: Read + Seek>(
    reader: &mut R,
    entry: &qar::Entry,
    context: &qar::Context,
) -> Result<fpk::Package, String> {
    reader
        .seek(SeekFrom::Start(entry.offset + 32))
        .map_err(|e| e.to_string())?;
    let mut head = vec![0; 48];
    reader.read_exact(&mut head).map_err(|e| e.to_string())?;
    context.layer1(&mut head, entry.hash, 0);
    if !entry.compressed() && head.starts_with(b"foxfpk") {
        let count = word(&head, 36) as u64;
        let references = word(&head, 40) as u64;
        let table_end = 48u64 + 48 * count + 16 * references;
        if table_end > entry.stored as u64 || table_end > MAX_METADATA_BYTES {
            return Err("package table exceeds learning limit".into());
        }
        let mut table = vec![0; table_end as usize];
        reader
            .seek(SeekFrom::Start(entry.offset + 32))
            .map_err(|e| e.to_string())?;
        reader.read_exact(&mut table).map_err(|e| e.to_string())?;
        context.layer1(&mut table, entry.hash, 0);
        let mut names_end = table_end;
        for i in 0..count as usize {
            let at = 48 + 48 * i;
            names_end =
                names_end.max(word(&table, at + 16) as u64 + word(&table, at + 24) as u64 + 1);
        }
        for i in 0..references as usize {
            let at = 48 + 48 * count as usize + 16 * i;
            names_end = names_end.max(word(&table, at) as u64 + word(&table, at + 8) as u64 + 1);
        }
        if names_end > entry.stored as u64 || names_end > MAX_METADATA_BYTES {
            return Err("package strings exceed learning limit".into());
        }
        let start = table.len();
        table.resize(names_end as usize, 0);
        reader
            .read_exact(&mut table[start..])
            .map_err(|e| e.to_string())?;
        context.layer1(&mut table[start..], entry.hash, start);
        let package = fpk::read_metadata(&table, entry.stored as usize)?;
        for e in &package.entries {
            if e.offset as u64 + e.size as u64 > entry.stored as u64 {
                return Err("package payload range exceeds entry".into());
            }
            if Md5::digest(e.path.as_bytes()).as_slice() != e.md5 {
                return Err("package path checksum does not match recovered keys".into());
            }
        }
        return Ok(package);
    }
    if entry.stored as u64 > MAX_STORED_PACKAGE_BYTES {
        return Err("compressed package exceeds learning limit".into());
    }
    let mut stored = vec![0; entry.stored as usize];
    reader
        .seek(SeekFrom::Start(entry.offset + 32))
        .map_err(|e| e.to_string())?;
    reader.read_exact(&mut stored).map_err(|e| e.to_string())?;
    fpk::read(&context.decode(entry, &stored)?)
}

fn fingerprint(
    game: &Path,
    archive: &Path,
    context: &qar::Context,
) -> Result<ArchiveSource, Error> {
    let mut file = File::open(archive).map_err(|e| io_error(archive, e))?;
    let bytes = file.metadata().map_err(|e| io_error(archive, e))?.len();
    let mut head = [0; 32];
    file.read_exact(&mut head)
        .map_err(|e| io_error(archive, e))?;
    let header = context.read_header(&head).map_err(Error::Unsupported)?;
    let size = 32u64 + 8u64 * header.count as u64 + 16u64 * header.extra_count as u64;
    if size > bytes || size > MAX_METADATA_BYTES {
        return Err(Error::Unsupported(
            "archive index exceeds learning limit".into(),
        ));
    }
    let mut hasher = Sha256::new();
    hasher.update(head);
    let mut table = vec![0; (size - 32) as usize];
    file.read_exact(&mut table)
        .map_err(|e| io_error(archive, e))?;
    hasher.update(table);
    let digest = hasher.finalize();
    let index_sha256 = digest.iter().map(|b| format!("{b:02x}")).collect();
    let relative_path = archive
        .strip_prefix(game)
        .map_err(|_| Error::Invalid("archive outside install".into()))?
        .to_string_lossy()
        .replace('\\', "/");
    Ok(ArchiveSource {
        relative_path,
        bytes,
        index_sha256,
    })
}
