//! Textures: .ftex (header + mip table) and its .N.ftexs data files. Read + write.
//!
//! Ports of the pipeline's Python: tools/ftex2png.py read_mip, tools/location/flyk_look.py ftex_mips /
//! pack_mip_vanilla / write_ftex_vanilla (tools/demo/retex.py write_ftex uses the same layout). Notes:
//! docs/formats/ftex.md. Credit: Atvaark's FtexTool (MIT) documented the format; this is our own implementation.
//!
//!   .ftex   0x00 "FTEX", f32 version, u16 pixel format, u16 width, u16 height, u16 depth, u8 mip count, ...
//!           (0x11..0x30 kept as read), 0x30 16-byte hash (md5 of the unpacked mip 0 data in the writer),
//!           0x40 one 16-byte entry per mip (per mip and face for cube maps: mip count x 6 entries, faces in
//!           order): u32 offset, u32 unpacked size, u32 packed size, u8 mip index, u8 ftexs number, u16 chunk count
//!   .ftexs  at each mip's offset a chunk table (u16 packed size, u16 unpacked size, u32 offset relative to the mip,
//!           bit 31 = stored raw), then the chunks (zlib streams or raw). Vanilla layout (write_ftex_vanilla): inside
//!           each .N.ftexs the smallest mip comes first, each mip block padded to 16, chunks back to back right after
//!           the table, and the file ends with 8 zero bytes per chunk of its last (largest) mip.
//!
//! Two layouts (`Layout`): the game files leave 8 bytes per chunk after EVERY mip block, holding a copy of the next
//! block's chunk table (the next mip of the same .ftexs, else the first block of the next .ftexs; zero padded or
//! cut to the length; zeros after the final block). The pipeline's Python writers (flyk_look.write_ftex_vanilla,
//! retex.write_ftex) put 8 zero bytes per chunk only after the last (largest) mip of each .ftexs.
//! `Layout::Pipeline` reproduces the Python byte for byte, `Layout::Vanilla` the game files. Vanilla chunks are zlib
//! level 6 streams (15,069 / 15,069 sampled chunks recompress identically with CPython's zlib at level 6); the Python
//! writers use level 9.
//!
//! zlib compression is injected (`compress`): the Python writers use CPython's zlib 1.2.13 at level 9, which
//! foxpil::zlibs::py_compress reproduces; foxcore stays pure Rust. Decompression (inflate) is exact by definition.
use std::collections::BTreeMap;

use std::io::Read;

use md5::{Digest, Md5};

/// Rebuilt FTEX header bytes and stream-numbered FTEXS payloads.
pub type EncodedTextureSet = (Vec<u8>, BTreeMap<u8, Vec<u8>>);

pub const CHUNK: usize = 0x4000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MipEntry {
    pub offset: u32,
    pub unpacked: u32,
    pub packed: u32,
    pub mip: u8,
    pub ftexs: u8,
    pub chunks: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ftex {
    pub head: [u8; 0x30],
    pub hash: [u8; 16],
    pub mips: Vec<MipEntry>,
    /// bytes after the last whole 16-byte entry (none in vanilla)
    pub tail: Vec<u8>,
}

impl Ftex {
    fn h16(&self, i: usize) -> u16 {
        u16::from_le_bytes([self.head[i], self.head[i + 1]])
    }
    /// 0 = RGBA8 (stored B, G, R, A), 1 = R8, 2 = DXT1, 3 = DXT3, 4 = DXT5 (see docs/formats/ftex.md for the census)
    pub fn format(&self) -> u16 {
        self.h16(8)
    }
    pub fn width(&self) -> u16 {
        self.h16(10)
    }
    pub fn height(&self) -> u16 {
        self.h16(12)
    }
    pub fn depth(&self) -> u16 {
        self.h16(14)
    }
    pub fn mip_count(&self) -> u8 {
        self.head[0x10]
    }
    /// numbers of the .N.ftexs files the mips live in, ascending
    pub fn ftexs_numbers(&self) -> Vec<u8> {
        let mut v: Vec<u8> = self.mips.iter().map(|m| m.ftexs).collect();
        v.sort();
        v.dedup();
        v
    }
}

pub fn read(b: &[u8]) -> Result<Ftex, String> {
    if b.get(0..4) != Some(b"FTEX") {
        return Err("not an ftex (FTEX)".into());
    }
    if b.len() < 0x40 {
        return Err("ftex: truncated header".into());
    }
    let nm = b[0x10] as usize;
    if b.len() < 0x40 + 16 * nm {
        return Err(format!("ftex: {} bytes, too short for {nm} mips", b.len()));
    }
    // cube maps list mip count x 6 faces entries (the header still says the mip count): read every whole entry
    let nm = if (b.len() - 0x40).is_multiple_of(16) { (b.len() - 0x40) / 16 } else { nm };
    let mips = (0..nm).map(|i| {
        let o = 0x40 + 16 * i;
        let u = |k: usize| u32::from_le_bytes(b[o + k..o + k + 4].try_into().unwrap());
        MipEntry { offset: u(0), unpacked: u(4), packed: u(8), mip: b[o + 12], ftexs: b[o + 13],
                   chunks: u16::from_le_bytes([b[o + 14], b[o + 15]]) }
    }).collect();
    Ok(Ftex { head: b[0..0x30].try_into().unwrap(), hash: b[0x30..0x40].try_into().unwrap(), mips,
              tail: b[0x40 + 16 * nm..].to_vec() })
}

pub fn write(f: &Ftex) -> Vec<u8> {
    let mut o = Vec::with_capacity(0x40 + 16 * f.mips.len());
    o.extend_from_slice(&f.head);
    o.extend_from_slice(&f.hash);
    for m in &f.mips {
        o.extend_from_slice(&m.offset.to_le_bytes());
        o.extend_from_slice(&m.unpacked.to_le_bytes());
        o.extend_from_slice(&m.packed.to_le_bytes());
        o.push(m.mip);
        o.push(m.ftexs);
        o.extend_from_slice(&m.chunks.to_le_bytes());
    }
    o.extend_from_slice(&f.tail);
    o
}

/// One stored chunk of a mip: the bytes as stored (zlib stream or raw) and its unpacked size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub stored: Vec<u8>,
    pub unpacked: u16,
    pub raw: bool,
}

/// The chunks of one mip as stored in its .ftexs (table order). Also checks that the chunk data sits where the
/// vanilla layout puts it is NOT required here; the writer decides the layout.
pub fn mip_chunks(e: &MipEntry, ftexs: &[u8]) -> Result<Vec<Chunk>, String> {
    let off = e.offset as usize;
    let mut v = Vec::with_capacity(e.chunks as usize);
    for c in 0..e.chunks as usize {
        let t = ftexs.get(off + 8 * c..off + 8 * c + 8).ok_or("ftexs: chunk table out of range")?;
        let cs = u16::from_le_bytes([t[0], t[1]]) as usize;
        let us = u16::from_le_bytes([t[2], t[3]]);
        let co = u32::from_le_bytes(t[4..8].try_into().unwrap());
        let start = off + (co & 0x7FFF_FFFF) as usize;
        let stored = ftexs.get(start..start + cs).ok_or("ftexs: chunk out of range")?.to_vec();
        v.push(Chunk { stored, unpacked: us, raw: co & 0x8000_0000 != 0 });
    }
    Ok(v)
}

pub fn inflate(z: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(z).read_to_end(&mut out).map_err(|e| format!("ftexs: zlib: {e}"))?;
    Ok(out)
}

/// Unpacked bytes of one mip entry from its .ftexs file (flyk_look.ftex_mips for one entry).
pub fn entry_data(e: &MipEntry, ftexs: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(e.unpacked as usize);
    for c in mip_chunks(e, ftexs)? {
        if c.raw {
            out.extend_from_slice(&c.stored);
        } else {
            out.extend_from_slice(&inflate(&c.stored)?);
        }
    }
    if out.len() != e.unpacked as usize {
        return Err(format!("mip {}: {} bytes, expected {}", e.mip, out.len(), e.unpacked));
    }
    Ok(out)
}

/// Unpacked bytes of mip `mip` (ftex2png.read_mip; the first entry with that index).
pub fn mip_data(f: &Ftex, mip: u8, ftexs: &[u8]) -> Result<Vec<u8>, String> {
    let e = f.mips.iter().find(|e| e.mip == mip).ok_or_else(|| format!("no mip {mip}"))?;
    entry_data(e, ftexs)
}

/// flyk_look.ftex_mips: the header and every mip's unpacked bytes. `load(n)` returns the bytes of the .n.ftexs file
/// (the caller does the I/O, so pipeline stages can trace it); each file is loaded once.
pub fn ftex_mips(ftex: &[u8], load: &mut dyn FnMut(u8) -> Result<Vec<u8>, String>)
                 -> Result<(Ftex, BTreeMap<u8, Vec<u8>>), String> {
    let f = read(ftex)?;
    let mut files: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
    let mut data = BTreeMap::new();
    for e in &f.mips {
        if let std::collections::btree_map::Entry::Vacant(stream) = files.entry(e.ftexs) {
            // Stream 0 keeps chunked data in the FTEX file, at absolute offsets.
            let data = if e.ftexs == 0 { ftex.to_vec() } else { load(e.ftexs)? };
            stream.insert(data);
        }
        data.insert(e.mip, entry_data(e, &files[&e.ftexs])?);
    }
    Ok((f, data))
}

/// chunk table + chunks of one mip block, padded to 16 (no compression decisions: the chunks as given)
fn block_from_chunks(chunks: &[Chunk]) -> Vec<u8> {
    let mut table = Vec::with_capacity(8 * chunks.len());
    let mut body = Vec::new();
    let off = 8 * chunks.len();
    for c in chunks {
        table.extend_from_slice(&(c.stored.len() as u16).to_le_bytes());
        table.extend_from_slice(&c.unpacked.to_le_bytes());
        let o = (off + body.len()) as u32 | if c.raw { 0x8000_0000 } else { 0 };
        table.extend_from_slice(&o.to_le_bytes());
        body.extend_from_slice(&c.stored);
    }
    table.extend_from_slice(&body);
    table.resize(table.len().next_multiple_of(16), 0);
    table
}

/// flyk_look.pack_mip_vanilla: 0x4000-byte chunks, each `compress`ed unless that does not make it smaller (then raw).
pub fn pack_chunks(data: &[u8], compress: &dyn Fn(&[u8]) -> Vec<u8>) -> Vec<Chunk> {
    data.chunks(CHUNK).map(|raw| {
        let z = compress(raw);
        if z.len() >= raw.len() {
            Chunk { stored: raw.to_vec(), unpacked: raw.len() as u16, raw: true }
        } else {
            Chunk { stored: z, unpacked: raw.len() as u16, raw: false }
        }
    }).collect()
}

/// flyk_look.pack_mip_vanilla -> (block bytes, chunk count)
pub fn pack_mip_vanilla(data: &[u8], compress: &dyn Fn(&[u8]) -> Vec<u8>) -> (Vec<u8>, u16) {
    let c = pack_chunks(data, compress);
    (block_from_chunks(&c), c.len() as u16)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// the game files: after every mip block 8 bytes x its chunk count holding the next block's chunk table
    /// (zero padded; zeros after the final block)
    Vanilla,
    /// the pipeline's Python writers: 8 zero bytes x chunk count only after the last mip of each .ftexs
    Pipeline,
}

/// Lay out stored chunks per mip: returns the updated header (offsets, packed sizes, chunk counts) and the .N.ftexs
/// files. Inside each file the smallest mip comes first. `chunks[i]` belongs to `template.mips[i]`. The hash is left
/// as given.
pub fn layout(template: &Ftex, chunks: &[Vec<Chunk>], mode: Layout) -> (Ftex, BTreeMap<u8, Vec<u8>>) {
    let mut f = template.clone();
    let mut order: Vec<usize> = (0..f.mips.len()).collect();
    // Python: sorted(mips, key=(ftexs, -mip)), a stable sort
    order.sort_by_key(|&i| (f.mips[i].ftexs, std::cmp::Reverse(f.mips[i].mip)));
    let mut files: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
    let mut last: BTreeMap<u8, usize> = BTreeMap::new();
    let blocks: Vec<Vec<u8>> = order.iter().map(|&i| block_from_chunks(&chunks[i])).collect();
    for (k, &i) in order.iter().enumerate() {
        let block = &blocks[k];
        let e = &mut f.mips[i];
        let buf = files.entry(e.ftexs).or_default();
        e.offset = buf.len() as u32;
        e.packed = block.len() as u32;
        e.chunks = chunks[i].len() as u16;
        buf.extend_from_slice(block);
        if mode == Layout::Vanilla {
            // the game's writer leaves 8 bytes per chunk after each block holding the NEXT block's chunk table
            // (next in this sequence, across .ftexs files), zero padded; zeros after the final block
            let n = 8 * chunks[i].len();
            let mut gap = match order.get(k + 1) {
                Some(&j) => blocks[k + 1][..(8 * chunks[j].len()).min(n)].to_vec(),
                None => Vec::new(),
            };
            gap.resize(n, 0);
            buf.extend_from_slice(&gap);
        }
        last.insert(e.ftexs, chunks[i].len());
    }
    if mode == Layout::Pipeline {
        for (n, buf) in files.iter_mut() {
            buf.resize(buf.len() + 8 * last[n], 0);
        }
    }
    (f, files)
}

/// flyk_look.write_ftex_vanilla (byte-identical to it: `Layout::Pipeline`; pass CPython zlib level 9 as `compress`).
pub fn write_ftex_vanilla(template: &Ftex, mips: &BTreeMap<u8, Vec<u8>>, compress: &dyn Fn(&[u8]) -> Vec<u8>)
                          -> Result<EncodedTextureSet, String> {
    write_ftex(template, mips, compress, Layout::Pipeline)
}

/// mips {mip index: unpacked bytes} with the template's header and mip -> ftexs assignment -> (.ftex bytes,
/// {n: .n.ftexs bytes}). The hash becomes md5(mip 0). `Layout::Vanilla` with zlib level 6 reproduces game files.
pub fn write_ftex(template: &Ftex, mips: &BTreeMap<u8, Vec<u8>>, compress: &dyn Fn(&[u8]) -> Vec<u8>, mode: Layout)
                  -> Result<EncodedTextureSet, String> {
    let mut chunks = Vec::with_capacity(template.mips.len());
    for e in &template.mips {
        let d = mips.get(&e.mip).ok_or_else(|| format!("mip {} missing", e.mip))?;
        if d.len() != e.unpacked as usize {
            return Err(format!("mip {}: {} bytes, template expects {}", e.mip, d.len(), e.unpacked));
        }
        chunks.push(pack_chunks(d, compress));
    }
    let (mut f, files) = layout(template, &chunks, mode);
    let m0 = mips.get(&0).ok_or("mip 0 missing")?;
    f.hash = Md5::digest(m0).into();
    Ok((write(&f), files))
}

/// Round trip of a vanilla texture set without recompressing: parse the header and every mip's stored chunks, lay
/// them out again (`layout`) and return the rebuilt (.ftex, {n: .ftexs}). Equal to the inputs (with
/// `Layout::Vanilla`) = the writer's layout is the game's.
pub fn roundtrip_set(ftex: &[u8], ftexs: &BTreeMap<u8, Vec<u8>>, mode: Layout)
                     -> Result<EncodedTextureSet, String> {
    let f = read(ftex)?;
    let mut chunks = Vec::with_capacity(f.mips.len());
    for e in &f.mips {
        let d = ftexs.get(&e.ftexs).ok_or_else(|| format!("missing .{}.ftexs", e.ftexs))?;
        chunks.push(mip_chunks(e, d)?);
    }
    let (g, files) = layout(&f, &chunks, mode);
    Ok((write(&g), files))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn store(b: &[u8]) -> Vec<u8> {
        // a "compressor" that never wins, so everything is stored raw (keeps the test independent of zlib)
        let mut v = b.to_vec();
        v.push(0);
        v
    }
    #[test]
    fn write_read_roundtrip() {
        let mut head = [0u8; 0x30];
        head[0..4].copy_from_slice(b"FTEX");
        head[8] = 2;
        head[10] = 8;
        head[12] = 8;
        head[0x10] = 2;
        let t = Ftex { head, hash: [0; 16], tail: vec![], mips: vec![
            MipEntry { offset: 0, unpacked: 32, packed: 0, mip: 0, ftexs: 1, chunks: 0 },
            MipEntry { offset: 0, unpacked: 8, packed: 0, mip: 1, ftexs: 1, chunks: 0 }] };
        let mips: BTreeMap<u8, Vec<u8>> = [(0u8, vec![5u8; 32]), (1, vec![6u8; 8])].into_iter().collect();
        let (fb, files) = write_ftex_vanilla(&t, &mips, &store).unwrap();
        let f = read(&fb).unwrap();
        assert_eq!(f.mips[1].offset, 0); // smallest mip first
        assert_eq!(f.mips[0].offset, 16);
        let (g, back) = ftex_mips(&fb, &mut |n| Ok(files[&n].clone())).unwrap();
        assert_eq!(g, f);
        assert_eq!(back, mips);
        let (fb2, files2) = roundtrip_set(&fb, &files, Layout::Pipeline).unwrap();
        assert_eq!((fb2, files2), (fb, files));
    }
}
