//! FMDL (TPP model, version 2.04): block-level read + write, and the static geometry view.
//!
//! Ports of tools/btg/fmdlfile.py (FmdlFile: the container, byte-identical round trip) and
//! tools/location/fmdl_geom.py (FmdlGeom: per-mesh positions + triangle lists, bounding boxes).
//! Layout after kapuragu/FoxEngineTemplates fmdl.bt; notes in docs/formats/fmdl.md.
//!
//!   0x00 "FMDL", f32 version, u32 info offset (0x40), u32 0, u64 section-0 block flags, u64 section-1 block flags
//!   0x20 u32 s0 block count, u32 s1 block count, u32 s0 offset, u32 s0 length, u32 s1 offset, u32 s1 length
//!   0x40 s0 block infos {u16 id, u16 entry count, u32 offset in s0}, then s1 infos {u32 id, u32 offset, u32 length},
//!        zero padded to the s0 offset
//!   section 0: the blocks back to back in offset order, each at its natural entry size; only block 13 (bounding
//!              boxes) is 16-aligned; the section zero padded to 16
//!   section 1: right after section 0 (block 0 = material parameter vec4s, block 2 = vertex / index buffers);
//!              carried through as bytes.
use std::collections::BTreeMap;

/// Per-mesh positions and triangle vertex indices, in their original order.
pub type MeshGeometry = (Vec<[f32; 3]>, Vec<[u32; 3]>);

/// Natural entry size of each section-0 block id (fmdlfile.ENTRY_SIZE, with block 12 corrected).
pub fn entry_size(bid: u16) -> Option<usize> {
    Some(match bid {
        0 => 48,
        1 => 8,
        2 => 32,
        3 => 48,
        4 => 16,
        5 => 68,
        6..=8 => 4,
        9 | 10 => 8,
        11 => 4,
        // string infos (u16 section, u16 length, u32 offset): 8 B. tools/btg/fmdlfile.py says 4, which truncates the
        // block on the one vanilla file that has it (cypr_door006_rbbl001.fmdl); 8 makes it byte-identical.
        12 => 8,
        13 => 32,
        14..=16 => 16,
        17..=19 => 8,
        20 => 128,
        21 | 22 => 8,
        _ => return None,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub num: u16,
    pub raw: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fmdl {
    /// the first 0x20 bytes, kept as read (magic, version, info offset, block flags)
    pub head: [u8; 0x20],
    pub s0off: u32,
    /// section-0 block ids in data (offset) order
    pub order: Vec<u16>,
    /// section-0 block ids in info-table order
    pub info_order: Vec<u16>,
    pub blocks: BTreeMap<u16, Block>,
    /// section-1 infos (id, offset, length) as read
    pub s1_infos: Vec<(u32, u32, u32)>,
    pub section1: Vec<u8>,
    pub tail: Vec<u8>,
}

fn u16at(b: &[u8], i: usize) -> Result<u16, String> {
    b.get(i..i + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or_else(|| format!("fmdl: truncated at {i:#x}"))
}
fn u32at(b: &[u8], i: usize) -> Result<u32, String> {
    b.get(i..i + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("fmdl: truncated at {i:#x}"))
}

pub fn read(d: &[u8]) -> Result<Fmdl, String> {
    if d.get(0..4) != Some(b"FMDL") {
        return Err("not an FMDL".into());
    }
    let head: [u8; 0x20] = d.get(0..0x20).ok_or("fmdl: truncated header")?.try_into().unwrap();
    let c0 = u32at(d, 0x20)? as usize;
    let c1 = u32at(d, 0x24)? as usize;
    let s0off = u32at(d, 0x28)?;
    let s0len = u32at(d, 0x2C)? as usize;
    let s1off = u32at(d, 0x30)? as usize;
    let s1len = u32at(d, 0x34)? as usize;
    let mut p = 0x40;
    let mut infos = Vec::with_capacity(c0);
    for _ in 0..c0 {
        infos.push((u32at(d, p + 4)? as usize, u16at(d, p)?, u16at(d, p + 2)?));
        p += 8;
    }
    let mut s1_infos = Vec::with_capacity(c1);
    for _ in 0..c1 {
        s1_infos.push((u32at(d, p)?, u32at(d, p + 4)?, u32at(d, p + 8)?));
        p += 12;
    }
    let info_order: Vec<u16> = infos.iter().map(|x| x.1).collect();
    let mut sorted = infos.clone();
    sorted.sort(); // Python sorted() over (offset, id, num) tuples
    let order: Vec<u16> = sorted.iter().map(|x| x.1).collect();
    let mut blocks = BTreeMap::new();
    for (i, &(off, bid, num)) in sorted.iter().enumerate() {
        let end = if i + 1 < sorted.len() { sorted[i + 1].0 } else { s0len };
        let size = match entry_size(bid) {
            Some(es) => num as usize * es,
            None => end.checked_sub(off).ok_or("fmdl: block offsets out of order")?,
        };
        if size > end.saturating_sub(off) {
            return Err(format!("fmdl: block {bid} overruns"));
        }
        let at = s0off as usize + off;
        let raw = d.get(at..at + size).ok_or("fmdl: block out of range")?.to_vec();
        if blocks.insert(bid, Block { num, raw }).is_some() {
            return Err(format!("fmdl: duplicate block {bid}"));
        }
    }
    let section1 = d.get(s1off..s1off + s1len).ok_or("fmdl: section 1 out of range")?.to_vec();
    Ok(Fmdl { head, s0off, order, info_order, blocks, s1_infos, section1, tail: d[s1off + s1len..].to_vec() })
}

impl Fmdl {
    /// the entries of a section-0 block (empty when absent)
    pub fn entries(&self, bid: u16) -> Vec<&[u8]> {
        match (self.blocks.get(&bid), entry_size(bid)) {
            (Some(b), Some(es)) => b.raw.chunks_exact(es).take(b.num as usize).collect(),
            _ => Vec::new(),
        }
    }
    pub fn set_entries(&mut self, bid: u16, entries: &[&[u8]]) {
        self.blocks.insert(bid, Block { num: entries.len() as u16, raw: entries.concat() });
    }

    pub fn build(&self) -> Vec<u8> {
        let mut s0 = Vec::new();
        let mut offs = BTreeMap::new();
        for bid in &self.order {
            let b = &self.blocks[bid];
            if *bid == 13 {
                s0.resize(s0.len().next_multiple_of(16), 0);
            }
            offs.insert(*bid, s0.len() as u32);
            s0.extend_from_slice(&b.raw);
        }
        s0.resize(s0.len().next_multiple_of(16), 0);
        let mut out = self.head.to_vec();
        for v in [self.order.len() as u32, self.s1_infos.len() as u32, self.s0off, s0.len() as u32,
                  self.s0off + s0.len() as u32, self.section1.len() as u32] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.resize(0x40, 0);
        for bid in &self.info_order {
            out.extend_from_slice(&bid.to_le_bytes());
            out.extend_from_slice(&self.blocks[bid].num.to_le_bytes());
            out.extend_from_slice(&offs[bid].to_le_bytes());
        }
        for &(a, b, c) in &self.s1_infos {
            for v in [a, b, c] {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        assert!(out.len() <= self.s0off as usize, "fmdl: block infos overrun the section-0 offset");
        out.resize(self.s0off as usize, 0);
        out.extend_from_slice(&s0);
        out.extend_from_slice(&self.section1);
        out.extend_from_slice(&self.tail);
        out
    }

    /// (section-1 absolute offset, length) of a section-1 block in a file of this layout
    fn s1_block(&self, bid: u32) -> Option<(usize, usize)> {
        self.s1_infos.iter().find(|x| x.0 == bid).map(|x| (x.1 as usize, x.2 as usize))
    }

    /// bounding boxes (min, max) from block 13 (fmdl_geom.bboxes)
    pub fn bboxes(&self) -> Vec<([f32; 3], [f32; 3])> {
        self.entries(13).iter().map(|e| {
            let f = |k: usize| f32::from_le_bytes(e[4 * k..4 * k + 4].try_into().unwrap());
            ([f(4), f(5), f(6)], [f(0), f(1), f(2)])
        }).collect()
    }

    /// Per mesh (high LOD): positions and triangles (fmdl_geom.FmdlGeom.meshes). Meshes without an R32G32B32 position
    /// element, or a file without an index buffer, are skipped exactly as the Python does; triangles referencing a
    /// vertex >= the mesh's vertex count are dropped.
    pub fn meshes(&self) -> Vec<MeshGeometry> {
        let le16 = |e: &[u8], i: usize| u16::from_le_bytes([e[i], e[i + 1]]);
        let le32 = |e: &[u8], i: usize| u32::from_le_bytes(e[i..i + 4].try_into().unwrap());
        let layouts = self.entries(9);
        let bufhdr = self.entries(10);
        let elems = self.entries(11);
        let files = self.entries(14);
        let Some((s1b2, _)) = self.s1_block(2) else { return Vec::new() };
        let base = s1b2; // relative to section 1
        let ifile = files.iter().find(|f| le16(f, 0) == 1);
        let mut out = Vec::new();
        for md in self.entries(3) {
            let lay = le16(md, 8) as usize;
            let nv = le16(md, 10) as usize;
            let istart = le32(md, 16) as usize;
            let icount = le32(md, 20) as usize;
            let Some(l) = layouts.get(lay) else { continue };
            let (nbuf, fb, fe) = (l[0] as usize, le16(l, 4) as usize, le16(l, 6) as usize);
            let mut pos: Option<Vec<[f32; 3]>> = None;
            for k in 0..nbuf {
                let Some(h) = bufhdr.get(fb + k) else { break };
                let (fbi, nelem, stride, boff) = (h[0] as usize, h[1] as usize, h[2] as usize, le32(h, 4) as usize);
                let first = fe + (0..k).map(|j| bufhdr.get(fb + j).map_or(0, |x| x[1] as usize)).sum::<usize>();
                for e in 0..nelem {
                    let Some(el) = elems.get(first + e) else { break };
                    if el[0] == 0 && el[1] == 1 {
                        let eoff = le16(el, 2) as usize;
                        let Some(f) = files.get(fbi) else { continue };
                        let start = base + le32(f, 8) as usize + boff;
                        let mut v = Vec::with_capacity(nv);
                        for i in 0..nv {
                            let a = start + stride * i + eoff;
                            let g = |k: usize| self.section1.get(a + 4 * k..a + 4 * k + 4)
                                .map_or(f32::NAN, |s| f32::from_le_bytes(s.try_into().unwrap()));
                            v.push([g(0), g(1), g(2)]);
                        }
                        pos = Some(v);
                    }
                }
            }
            let (Some(pos), Some(fi)) = (pos, ifile) else { continue };
            let at = base + le32(fi, 8) as usize + 2 * istart;
            let idx: Vec<u32> = (0..icount)
                .filter_map(|i| self.section1.get(at + 2 * i..at + 2 * i + 2).map(|s| u16::from_le_bytes([s[0], s[1]]) as u32))
                .collect();
            let tris = idx.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).filter(|t| t.iter().all(|&x| (x as usize) < nv)).collect();
            out.push((pos, tris));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn synthetic_roundtrip() {
        // a minimal file: blocks 3 (one mesh def) and 13 (one box), one section-1 block
        let mut f = Fmdl {
            head: [0; 0x20],
            s0off: 0x100,
            order: vec![3, 13],
            info_order: vec![13, 3],
            blocks: BTreeMap::new(),
            s1_infos: vec![(2, 0, 16)],
            section1: vec![7; 16],
            tail: vec![],
        };
        f.head[0..4].copy_from_slice(b"FMDL");
        f.set_entries(3, &[&[1u8; 48]]);
        f.set_entries(13, &[&[2u8; 32]]);
        let b = f.build();
        let r = read(&b).unwrap();
        assert_eq!(r, f);
        assert_eq!(r.build(), b);
        assert_eq!(b.len(), 0x100 + 48 + 32 + 16);
    }
}
