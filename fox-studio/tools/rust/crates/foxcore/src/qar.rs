//! QAR archives (SQAR, the game's master/*.dat): table, entries, decryption, extraction, raw-block rebuild.
//!
//! Format notes: the community's GzsTool (Atvaark, MIT) documents the layout; this is our own implementation.
//!   header (32): "SQAR", then 7 u32 each XOR a fixed mask: flags (^m1), entry count (^m2), extra-table count (^m3),
//!     end block (^m4), data offset (^m1), version (^m1; 1 = TPP), 0 (^m2). Block size = 4096 if flags & 0x800
//!     else 1024.
//!   section table: entry count x u64 (XOR-scrambled by position); section = block << 40 | hash-derived bits.
//!   entry: 32-byte header (u64 hash, sizes, md5, all XOR-masked) + data. The data is scrambled with a stream keyed by
//!     (hash low 32 bits, offset inside the entry) - never by its position in the archive, so an entry's bytes can be
//!     moved between archives unchanged (the installer's raw-block copy). Optional second layer (magic 0xA0F8EFE6 /
//!     0xE3F8EFE6 + key) and zlib.
use crate::hash::city64_with_seeds;

pub const MAGIC: u32 = 0x5241_5153; // "SQAR"
pub const ENC_MAGIC1: u32 = 0xA0F8_EFE6;
pub const ENC_MAGIC2: u32 = 0xE3F8_EFE6;
pub const META_FLAG: u64 = 0x4_0000_0000_0000;

/// File type names in Fox Engine archives (the type id is a hash of the name, bits 51.. of an entry hash).
pub const EXTENSIONS: &[&str] = &[
    "1.ftexs", "1.nav2", "2.ftexs", "3.ftexs", "4.ftexs", "5.ftexs", "6.ftexs", "ag.evf", "aia",
    "aib", "aibc", "aig", "aigc", "aim", "aip", "ait", "atsh", "bnd", "bnk", "cc.evf", "clo",
    "csnav", "dat", "des", "dnav", "dnav2", "eng.lng", "ese", "evb", "evf", "fag", "fage", "fago",
    "fagp", "fagx", "fclo", "fcnp", "fcnpx", "fdes", "fdmg", "ffnt", "fmdl", "fmdlb", "fmtt",
    "fnt", "fova", "fox", "fox2", "fpk", "fpkd", "fpkl", "frdv", "fre.lng", "frig", "frt", "fsd",
    "fsm", "fsml", "fsop", "fstb", "ftex", "fv2", "fx.evf", "fxp", "gani", "geom", "ger.lng",
    "gpfp", "grxla", "grxoc", "gskl", "htre", "info", "ita.lng", "jpn.lng", "json", "lad", "ladb",
    "lani", "las", "lba", "lng", "lpsh", "lua", "mas", "mbl", "mog", "mtar", "mtl", "nav2", "nta",
    "obr", "obrb", "param", "parts", "path", "pftxs", "ph", "phep", "phsd", "por.lng", "qar",
    "rbs", "rdb", "rdf", "rnav", "rus.lng", "sad", "sand", "sani", "sbp", "sd.evf", "sdf", "sim",
    "simep", "snav", "spa.lng", "spch", "sub", "subp", "tgt", "tre2", "txt", "uia", "uif", "uig",
    "uigb", "uil", "uilb", "utxl", "veh", "vfx", "vfxbin", "vfxdb", "vnav", "vo.evf", "vpc", "wem",
    "wmv", "xml",
];

/// hash of a path text (no extension): "/Assets/" stripped (else the meta flag; "tpptest" paths keep it too),
/// leading '/' trimmed, CityHash64WithSeeds(text, K2, last 8 bytes reversed), 50 bits.
pub fn path_hash(text: &str) -> u64 {
    let mut meta = true;
    let mut t = text;
    if let Some(rest) = t.strip_prefix("/Assets/") {
        t = rest;
        meta = t.starts_with("tpptest");
    }
    let t = t.trim_start_matches('/');
    let b = t.as_bytes();
    let mut seed1 = 0u64;
    for (j, &c) in b.iter().rev().take(8).enumerate() {
        seed1 |= (c as u64) << (8 * j);
    }
    let h = city64_with_seeds(b, 0x9ae16a3b2f90404f, seed1) & 0x3_FFFF_FFFF_FFFF;
    if meta { h | META_FLAG } else { h }
}

pub fn ext_id(ext: &str) -> u64 {
    path_hash(ext) & 0x1FFF
}

/// full entry hash of a file path with extension ("/Assets/tpp/x.fpk" -> type id << 51 | path hash)
pub fn file_hash(path: &str) -> u64 {
    let p = path.replace('\\', "/");
    let (stem, ext) = match p.find('.') {
        Some(i) => (&p[..i], &p[i + 1..]),
        None => (&p[..], ""),
    };
    let ty = if EXTENSIONS.contains(&ext) {
        ext_id(ext)
    } else {
        0
    };
    (ty << 51) | path_hash(stem)
}

/// extension name of an entry hash's type id
pub fn ext_of(hash: u64) -> Option<&'static str> {
    let id = hash >> 51;
    EXTENSIONS.iter().copied().find(|e| ext_id(e) == id)
}

#[derive(Clone, Debug)]
pub struct Header {
    pub flags: u32,
    pub count: u32,
    pub extra_count: u32,
    pub end_block: u32,
    pub data_offset: u32,
    pub version: u32,
    pub block_shift: u32,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub hash: u64,
    /// absolute offset of the 32-byte entry header
    pub offset: u64,
    pub uncompressed: u32,
    pub stored: u32,
    pub md5: [u8; 16],
}

impl Entry {
    pub fn compressed(&self) -> bool {
        self.uncompressed != self.stored
    }
    /// header + stored data
    pub fn raw_len(&self) -> u64 {
        32 + self.stored as u64
    }
}

fn u32le(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}

pub struct Index {
    pub header: Header,
    pub sections: Vec<u64>,
    pub entries: Vec<Entry>,
    /// the extra 16-byte records after the section table (kept verbatim)
    pub extra: Vec<u8>,
}

/// second layer (keyed), over the data after its 8/16-byte header; trailing 0-3 bytes stay plain (an involution)
pub fn layer2(data: &mut [u8], key: u32) {
    let k = key.wrapping_mul(278);
    let mut block = key | ((key ^ 25974) << 16);
    for w in 0..data.len() / 4 {
        let v = u32le(data, 4 * w) ^ block;
        data[4 * w..4 * w + 4].copy_from_slice(&v.to_le_bytes());
        block = k.wrapping_add(48828125u32.wrapping_mul(block));
    }
}

/// section value for an entry block at `offset`
pub fn section_of(hash: u64, offset: u64, shift: u32) -> u64 {
    (offset >> shift) << 40 | (hash & 0xFF) << 32 | ((hash >> 32) & 0xFFFF_FFFF)
}

pub struct RawEntry {
    pub hash: u64,
    pub raw: Vec<u8>,
    pub pad: Option<Vec<u8>>,
}

/// the 40 hash bits a section value carries (hash low byte and high 32 bits), for lookups without entry headers
pub fn section_key(hash: u64) -> u64 {
    (hash & 0xFF) << 32 | ((hash >> 32) & 0xFFFF_FFFF)
}

/// Offsets for writing `lens` (raw block lengths, in order) as an archive with a section table sized for `capacity`
/// entries (>= lens.len(); == lens.len() is the GzsTool layout). Returns (data_offset, offsets, end).
pub fn layout(lens: &[u64], capacity: usize, shift: u32) -> (u64, Vec<u64>, u64) {
    let align = 1u64 << shift;
    let table_end = 32 + 8 * capacity.max(lens.len()) as u64;
    let data_offset = table_end.div_ceil(align) * align;
    let mut pos = data_offset;
    let mut offs = Vec::with_capacity(lens.len());
    for l in lens {
        offs.push(pos);
        pos = (pos + l).div_ceil(align) * align;
    }
    (data_offset, offs, pos)
}

/// Archive operations bound to one explicit, locally learned key set.
#[derive(Clone, Debug)]
pub struct Context {
    keys: crate::runtime_data::QarKeys,
}

impl Context {
    pub fn new(keys: crate::runtime_data::QarKeys) -> Self {
        Self { keys }
    }
    pub fn keys(&self) -> &crate::runtime_data::QarKeys {
        &self.keys
    }

    #[cfg(feature = "internal-game-data")]
    pub fn internal() -> Self {
        Self::new(crate::runtime_data::QarKeys {
            header_masks: crate::internal_qar_data::XOR_TABLE,
            layer1: crate::internal_qar_data::D1,
        })
    }
    pub fn read_header(&self, b: &[u8]) -> Result<Header, String> {
        if b.len() < 32 || u32le(b, 0) != MAGIC {
            return Err("not a QAR (SQAR) archive".into());
        }
        let flags = u32le(b, 4) ^ self.keys.header_masks[0];
        Ok(Header {
            flags,
            count: u32le(b, 8) ^ self.keys.header_masks[1],
            extra_count: u32le(b, 12) ^ self.keys.header_masks[2],
            end_block: u32le(b, 16) ^ self.keys.header_masks[3],
            data_offset: u32le(b, 20) ^ self.keys.header_masks[0],
            version: u32le(b, 24) ^ self.keys.header_masks[0],
            block_shift: if flags & 0x800 != 0 { 12 } else { 10 },
        })
    }

    /// section-table scramble (an involution: the same function encrypts and decrypts)
    fn section_xor(&self, i: usize, word: u32, half: usize) -> u32 {
        let off = i * 8 + half * 4;
        word ^ self.keys.header_masks[(i + off / 5) % 4]
    }

    pub fn decode_sections(&self, t: &[u8], n: usize) -> Vec<u64> {
        (0..n)
            .map(|i| {
                let lo = self.section_xor(i, u32le(t, 8 * i), 0) as u64;
                let hi = self.section_xor(i, u32le(t, 8 * i + 4), 1) as u64;
                hi << 32 | lo
            })
            .collect()
    }

    pub fn encode_sections(&self, sections: &[u64]) -> Vec<u8> {
        let mut out = Vec::with_capacity(sections.len() * 8);
        for (i, s) in sections.iter().enumerate() {
            out.extend_from_slice(&self.section_xor(i, *s as u32, 0).to_le_bytes());
            out.extend_from_slice(&self.section_xor(i, (*s >> 32) as u32, 1).to_le_bytes());
        }
        out
    }

    /// entry header (32 bytes) found at `offset`
    pub fn read_entry_header(&self, hb: &[u8], offset: u64) -> Result<Entry, String> {
        if hb.len() < 32 {
            return Err("truncated QAR entry header".into());
        }
        let lo = u32le(hb, 0) ^ self.keys.header_masks[0];
        let hi = u32le(hb, 4) ^ self.keys.header_masks[0];
        let mut md5 = [0u8; 16];
        for (k, m) in [
            self.keys.header_masks[3],
            self.keys.header_masks[0],
            self.keys.header_masks[0],
            self.keys.header_masks[1],
        ]
        .iter()
        .enumerate()
        {
            md5[4 * k..4 * k + 4].copy_from_slice(&(u32le(hb, 16 + 4 * k) ^ m).to_le_bytes());
        }
        // size fields (version 1): +8 = stored size (what follows the header), +12 = content size. Equal for plain
        // entries; for a second-layer entry without compression both include its 8/16-byte header. (GzsTool names the
        // two the other way round; it never checks them, so its archives work either way.)
        Ok(Entry {
            hash: (hi as u64) << 32 | lo as u64,
            offset,
            stored: u32le(hb, 8) ^ self.keys.header_masks[1],
            uncompressed: u32le(hb, 12) ^ self.keys.header_masks[2],
            md5,
        })
    }

    pub fn entry_header_bytes(&self, e: &Entry) -> [u8; 32] {
        let mut h = [0u8; 32];
        h[0..4].copy_from_slice(&((e.hash as u32) ^ self.keys.header_masks[0]).to_le_bytes());
        h[4..8]
            .copy_from_slice(&(((e.hash >> 32) as u32) ^ self.keys.header_masks[0]).to_le_bytes());
        h[8..12].copy_from_slice(&(e.stored ^ self.keys.header_masks[1]).to_le_bytes());
        h[12..16].copy_from_slice(&(e.uncompressed ^ self.keys.header_masks[2]).to_le_bytes());
        for (k, m) in [
            self.keys.header_masks[3],
            self.keys.header_masks[0],
            self.keys.header_masks[0],
            self.keys.header_masks[1],
        ]
        .iter()
        .enumerate()
        {
            let w = u32::from_le_bytes(e.md5[4 * k..4 * k + 4].try_into().unwrap()) ^ m;
            h[16 + 4 * k..20 + 4 * k].copy_from_slice(&w.to_le_bytes());
        }
        h
    }

    pub fn read_index<R: std::io::Read + std::io::Seek>(&self, r: &mut R) -> Result<Index, String> {
        use std::io::SeekFrom;
        let archive_len = r.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
        let mut head = [0u8; 32];
        r.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        r.read_exact(&mut head).map_err(|e| e.to_string())?;
        let h = self.read_header(&head)?;
        if h.version == 2 {
            return Err("QAR version 2 (MGS) is not supported".into());
        }
        let table_end = 32u64 + 8u64 * h.count as u64 + 16u64 * h.extra_count as u64;
        if table_end > archive_len || table_end > h.data_offset as u64 {
            return Err("QAR table exceeds archive data offset".into());
        }
        let mut tbl = vec![0u8; 8 * h.count as usize];
        r.read_exact(&mut tbl).map_err(|e| e.to_string())?;
        let sections = self.decode_sections(&tbl, h.count as usize);
        let mut extra = vec![0u8; 16 * h.extra_count as usize];
        r.read_exact(&mut extra).map_err(|e| e.to_string())?;
        let mut entries = Vec::with_capacity(sections.len());
        let mut eh = [0u8; 32];
        for s in &sections {
            let off = (s >> 40) << h.block_shift;
            if off < h.data_offset as u64 || off.checked_add(32).is_none_or(|n| n > archive_len) {
                return Err(format!("QAR entry offset {off} outside archive"));
            }
            r.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
            r.read_exact(&mut eh)
                .map_err(|e| format!("entry header at {off}: {e}"))?;
            let entry = self.read_entry_header(&eh, off)?;
            if off
                .checked_add(entry.raw_len())
                .is_none_or(|n| n > archive_len)
            {
                return Err(format!("QAR entry at {off} exceeds archive"));
            }
            entries.push(entry);
        }
        Ok(Index {
            header: h,
            sections,
            entries,
            extra,
        })
    }

    /// first layer: (de)scramble `data` in place; `pos` = offset of data[0] inside the entry data (an involution)
    pub fn layer1(&self, data: &mut [u8], hash: u64, pos: usize) {
        let hash_low = hash as u32 as u64;
        for (i, b) in data.iter_mut().enumerate() {
            let abs = pos + i;
            let block = (abs - abs % 8) as u64;
            let idx = (2 * ((hash_low + block / 11) % 4)) as usize;
            let k = abs % 8;
            let w = if k < 4 {
                self.keys.layer1[idx]
            } else {
                self.keys.layer1[idx + 1]
            };
            *b ^= (w >> (8 * (k % 4))) as u8;
        }
    }

    /// Content of one entry from its stored bytes (the bytes after the 32-byte header). For a second-layer entry the
    /// content excludes the layer's 8/16-byte header. Compressed content must match
    /// the declared decoded size exactly.
    pub fn decode(&self, e: &Entry, stored: &[u8]) -> Result<Vec<u8>, String> {
        if stored.len() != e.stored as usize {
            return Err("QAR stored length does not match entry header".into());
        }
        let mut body = stored.to_vec();
        self.layer1(&mut body, e.hash, 0);
        if body.len() >= 8 {
            let enc = u32le(&body, 0);
            if enc == ENC_MAGIC1 || enc == ENC_MAGIC2 {
                let key = u32le(&body, 4);
                let hs = if enc == ENC_MAGIC1 { 8 } else { 16 };
                if body.len() < hs {
                    return Err("truncated QAR second-layer header".into());
                }
                let mut rest = body[hs..].to_vec();
                layer2(&mut rest, key);
                body = rest;
            }
        }
        if e.compressed() {
            use std::io::Read;
            let z = flate2::read::ZlibDecoder::new(&body[..]);
            // A corrupt size word must not trigger a speculative multi-gigabyte allocation.
            // Read one extra byte to detect a stream exceeding its declared content size.
            let mut out = Vec::new();
            z.take(e.uncompressed as u64 + 1)
                .read_to_end(&mut out)
                .map_err(|x| format!("zlib: {x}"))?;
            if out.len() as u64 > e.uncompressed as u64 {
                return Err("QAR decompressed content exceeds its declared size".into());
            }
            if (out.len() as u64) < e.uncompressed as u64 {
                return Err("QAR decompressed content is shorter than its declared size".into());
            }
            return Ok(out);
        }
        Ok(body)
    }

    /// a new plain entry (uncompressed, first layer only) for `content`: (entry, header + stored bytes)
    pub fn encode_plain(&self, hash: u64, content: &[u8]) -> Result<(Entry, Vec<u8>), String> {
        let length = u32::try_from(content.len()).map_err(|_| "QAR entry exceeds 4 GiB")?;
        use md5::{Digest, Md5};
        let md5: [u8; 16] = Md5::digest(content).into();
        let e = Entry {
            hash,
            offset: 0,
            uncompressed: length,
            stored: length,
            md5,
        };
        let mut raw = self.entry_header_bytes(&e).to_vec();
        let start = raw.len();
        raw.extend_from_slice(content);
        self.layer1(&mut raw[start..], hash, 0);
        Ok((e, raw))
    }

    /// archive header bytes
    pub fn header_bytes(&self, h: &Header) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, v) in [
            MAGIC,
            h.flags ^ self.keys.header_masks[0],
            h.count ^ self.keys.header_masks[1],
            h.extra_count ^ self.keys.header_masks[2],
            h.end_block ^ self.keys.header_masks[3],
            h.data_offset ^ self.keys.header_masks[0],
            h.version ^ self.keys.header_masks[0],
            self.keys.header_masks[1],
        ]
        .iter()
        .enumerate()
        {
            out[4 * i..4 * i + 4].copy_from_slice(&v.to_le_bytes());
        }
        out
    }

    /// length of the second-layer header at the start of an entry's data (0, 8 or 16)
    pub fn layer2_header_len(&self, e: &Entry, stored: &[u8]) -> usize {
        if stored.len() < 8 {
            return 0;
        }
        let mut h = stored[..8].to_vec();
        self.layer1(&mut h, e.hash, 0);
        match u32le(&h, 0) {
            ENC_MAGIC1 => 8,
            ENC_MAGIC2 => 16,
            _ => 0,
        }
    }

    /// Write an archive from raw entry blocks, in the given order (GzsTool layout: header, section table, zero pad to a
    /// block, then each entry block-aligned). Returns the header written.
    pub fn write_archive<W: std::io::Write>(
        &self,
        w: &mut W,
        flags: u32,
        version: u32,
        entries: &[RawEntry],
    ) -> std::io::Result<Header> {
        let shift = if flags & 0x800 != 0 { 12 } else { 10 };
        let align = 1u64 << shift;
        let table_end = 32 + 8 * entries.len() as u64;
        let data_offset = table_end.div_ceil(align) * align;
        let mut sections = Vec::with_capacity(entries.len());
        let mut pos = data_offset;
        for e in entries {
            sections.push(section_of(e.hash, pos, shift));
            pos = (pos + e.raw.len() as u64).div_ceil(align) * align;
        }
        let h = Header {
            flags,
            count: entries.len() as u32,
            extra_count: 0,
            end_block: (pos >> shift) as u32,
            data_offset: data_offset as u32,
            version,
            block_shift: shift,
        };
        w.write_all(&self.header_bytes(&h))?;
        w.write_all(&self.encode_sections(&sections))?;
        w.write_all(&vec![0u8; (data_offset - table_end) as usize])?;
        let mut at = data_offset;
        let zeros = vec![0u8; align as usize];
        for e in entries {
            w.write_all(&e.raw)?;
            at += e.raw.len() as u64;
            let next = at.div_ceil(align) * align;
            let n = (next - at) as usize;
            match &e.pad {
                Some(p) if p.len() == n => w.write_all(p)?,
                _ => w.write_all(&zeros[..n])?,
            }
            at = next;
        }
        Ok(h)
    }

    /// header + section table only (no entry headers): [(section key, entry offset)] - one read, no seeks
    pub fn read_table<R: std::io::Read + std::io::Seek>(
        &self,
        r: &mut R,
    ) -> Result<(Header, Vec<(u64, u64)>), String> {
        use std::io::SeekFrom;
        let archive_len = r.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
        let mut head = [0u8; 32];
        r.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        r.read_exact(&mut head).map_err(|e| e.to_string())?;
        let h = self.read_header(&head)?;
        if 32u64 + 8u64 * h.count as u64 > archive_len {
            return Err("QAR section table exceeds archive".into());
        }
        let mut tbl = vec![0u8; 8 * h.count as usize];
        r.read_exact(&mut tbl).map_err(|e| e.to_string())?;
        let out = self
            .decode_sections(&tbl, h.count as usize)
            .into_iter()
            .map(|s| (s & 0xFF_FFFF_FFFF, (s >> 40) << h.block_shift))
            .collect();
        Ok((h, out))
    }

    /// header + section table region [0, data_offset) for entries (hash, offset), zero-filled after the table
    pub fn table_region(
        &self,
        flags: u32,
        version: u32,
        entries: &[(u64, u64)],
        data_offset: u64,
        end: u64,
        shift: u32,
    ) -> Vec<u8> {
        let h = Header {
            flags,
            count: entries.len() as u32,
            extra_count: 0,
            end_block: (end >> shift) as u32,
            data_offset: data_offset as u32,
            version,
            block_shift: shift,
        };
        let sections: Vec<u64> = entries
            .iter()
            .map(|(hash, off)| section_of(*hash, *off, shift))
            .collect();
        let mut out = Vec::with_capacity(data_offset as usize);
        out.extend_from_slice(&self.header_bytes(&h));
        out.extend_from_slice(&self.encode_sections(&sections));
        out.resize(data_offset as usize, 0);
        out
    }
}

#[cfg(feature = "internal-game-data")]
pub fn read_header(b: &[u8]) -> Result<Header, String> {
    Context::internal().read_header(b)
}

#[cfg(feature = "internal-game-data")]
pub fn decode_sections(t: &[u8], n: usize) -> Vec<u64> {
    Context::internal().decode_sections(t, n)
}

#[cfg(feature = "internal-game-data")]
pub fn encode_sections(sections: &[u64]) -> Vec<u8> {
    Context::internal().encode_sections(sections)
}

#[cfg(feature = "internal-game-data")]
pub fn read_entry_header(hb: &[u8], offset: u64) -> Entry {
    Context::internal()
        .read_entry_header(hb, offset)
        .expect("private compatibility caller supplied invalid input")
}

#[cfg(feature = "internal-game-data")]
pub fn entry_header_bytes(e: &Entry) -> [u8; 32] {
    Context::internal().entry_header_bytes(e)
}

#[cfg(feature = "internal-game-data")]
pub fn read_index<R: std::io::Read + std::io::Seek>(r: &mut R) -> Result<Index, String> {
    Context::internal().read_index(r)
}

#[cfg(feature = "internal-game-data")]
pub fn layer1(data: &mut [u8], hash: u64, pos: usize) {
    Context::internal().layer1(data, hash, pos)
}

#[cfg(feature = "internal-game-data")]
pub fn decode(e: &Entry, stored: &[u8]) -> Result<Vec<u8>, String> {
    Context::internal().decode(e, stored)
}

#[cfg(feature = "internal-game-data")]
pub fn encode_plain(hash: u64, content: &[u8]) -> (Entry, Vec<u8>) {
    Context::internal()
        .encode_plain(hash, content)
        .expect("private compatibility caller supplied invalid input")
}

#[cfg(feature = "internal-game-data")]
pub fn header_bytes(h: &Header) -> [u8; 32] {
    Context::internal().header_bytes(h)
}

#[cfg(feature = "internal-game-data")]
pub fn layer2_header_len(e: &Entry, stored: &[u8]) -> usize {
    Context::internal().layer2_header_len(e, stored)
}

#[cfg(feature = "internal-game-data")]
pub fn write_archive<W: std::io::Write>(
    w: &mut W,
    flags: u32,
    version: u32,
    entries: &[RawEntry],
) -> std::io::Result<Header> {
    Context::internal().write_archive(w, flags, version, entries)
}

#[cfg(feature = "internal-game-data")]
pub fn read_table<R: std::io::Read + std::io::Seek>(
    r: &mut R,
) -> Result<(Header, Vec<(u64, u64)>), String> {
    Context::internal().read_table(r)
}

#[cfg(feature = "internal-game-data")]
pub fn table_region(
    flags: u32,
    version: u32,
    entries: &[(u64, u64)],
    data_offset: u64,
    end: u64,
    shift: u32,
) -> Vec<u8> {
    Context::internal().table_region(flags, version, entries, data_offset, end, shift)
}
