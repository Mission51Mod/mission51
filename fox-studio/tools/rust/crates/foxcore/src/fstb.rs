//! Stage block files (.fstb, "FSTB"): which packs a location loads (common packs, large-block levels).
//!
//! Generalises crates/foxm3/src/fstb.rs (tooling's FLYK writer) and ports tools/location/flyk_m2.py read_fstb /
//! write_fstb / write_fstb5. Reads and writes every vanilla file. Notes: docs/formats/fstb.md.
//!
//!   "FSTB" 00 00 00 <type>
//!   type 4: u32 0, u32 strings offset, u32 common count, u32 common path offset[n], the paths (NUL-terminated)
//!   type 5: u32 level count, u32 level table offset, u32 common count, u32 common path offset[n];
//!           level table u32[levels]; per level u32 count + u32 entry offset[count]; entries; then the string pool
//!           (every string once).
//!   type-5 entry: u32 StrCode32(pack stem), u32 (1), u32 z, u32 offset of an extra string (0 = none; MAFR: "",
//!           AFGH: the area's spl_*.fmdl), u32 sub-record count, u32 sub-table offset, u32 block list offset,
//!           u32 block count, u16 bbox (min X, min Y, max X + 1, max Y + 1), z + 1 string offsets (the pack path, then
//!           for z = 2 ".fpk" and the stagelow LOD pack), u32 sub-record offset[count], the sub-records, zero pad
//!           to 4, the block list (count x (u16 X, u16 Y)).
//!   sub-record (a coverage bitmap): u16 origin X, u16 origin Y, u16 width, u16 height, ceil(w * h / 8) bytes of
//!           bits; vanilla z = 2 entries carry two: an empty 0 x 0 one, then the LOD area.
//!   Layout rules: vanilla type-5 files put each level's table right before its entries and store the string pool
//!   sorted (bytewise); flyk_m2.write_fstb5 puts all level tables first and keeps the pool in first-use order after
//!   a leading "". `read` records both (`Fstb::layout`), `write` follows them.
use crate::hash::strcode32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bitmap {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    /// ceil(w * h / 8) bytes
    pub bits: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub hash: u32,
    /// the u32 after the hash (1 in vanilla)
    pub one: u32,
    pub z: u32,
    /// the string at +12: None = offset 0
    pub extra: Option<String>,
    pub bbox: [u16; 4],
    /// the pack path (strings[0])
    pub pack: String,
    /// strings[1..=z] (None = offset 0)
    pub more_strings: Vec<Option<String>>,
    pub subs: Vec<Bitmap>,
    pub blocks: Vec<(u16, u16)>,
    /// absolute offset of the entry in the file it was read from
    pub offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Layout {
    /// flyk_m2.write_fstb5: all level tables first; string pool "" then first use
    #[default]
    Pipeline,
    /// vanilla: each level's table before its entries; string pool sorted
    Vanilla,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fstb {
    pub typ: u8,
    pub common: Vec<String>,
    pub levels: Vec<Vec<Entry>>,
    pub layout: Layout,
}

fn u32at(b: &[u8], i: usize) -> Result<u32, String> {
    b.get(i..i + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("fstb: truncated at {i:#x}"))
}
fn u16at(b: &[u8], i: usize) -> Result<u16, String> {
    b.get(i..i + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or_else(|| format!("fstb: truncated at {i:#x}"))
}
fn s_at(b: &[u8], o: usize) -> Result<String, String> {
    let s = b.get(o..).ok_or("fstb: string out of range")?;
    let n = s.iter().position(|&c| c == 0).ok_or("fstb: unterminated string")?;
    Ok(s[..n].iter().map(|&c| c as char).collect())
}
fn opt_s(b: &[u8], o: u32) -> Result<Option<String>, String> {
    if o == 0 { Ok(None) } else { s_at(b, o as usize).map(Some) }
}
fn latin1(s: &str) -> Vec<u8> {
    s.chars().map(|c| c as u32 as u8).collect()
}

/// read_fstb
pub fn read(b: &[u8]) -> Result<Fstb, String> {
    if b.get(0..7) != Some(b"FSTB\0\0\0") {
        return Err("not an fstb".into());
    }
    let typ = b[7];
    if typ == 4 {
        let n = u32at(b, 16)? as usize;
        let common = (0..n).map(|i| s_at(b, u32at(b, 20 + 4 * i)? as usize)).collect::<Result<_, _>>()?;
        return Ok(Fstb { typ, common, levels: Vec::new(), layout: Layout::Pipeline });
    }
    let (nl, lvtab, nc) = (u32at(b, 8)? as usize, u32at(b, 12)? as usize, u32at(b, 16)? as usize);
    let mut common_off = Vec::with_capacity(nc);
    for i in 0..nc {
        common_off.push(u32at(b, 20 + 4 * i)?);
    }
    let common = common_off.iter().map(|&o| s_at(b, o as usize)).collect::<Result<_, _>>()?;
    let mut levels = Vec::with_capacity(nl);
    let mut lv_offs = Vec::with_capacity(nl);
    let mut string_offs: Vec<u32> = common_off.clone();
    for i in 0..nl {
        let lo = u32at(b, lvtab + 4 * i)? as usize;
        lv_offs.push(lo);
        let n = u32at(b, lo)? as usize;
        let mut ents = Vec::with_capacity(n);
        for j in 0..n {
            let o = u32at(b, lo + 4 + 4 * j)? as usize;
            let (hash, one, z) = (u32at(b, o)?, u32at(b, o + 4)?, u32at(b, o + 8)?);
            if z > 64 {
                return Err(format!("fstb: entry at {o:#x}: z = {z}"));
            }
            let extra_off = u32at(b, o + 12)?;
            let (nsub, subtab, bl, cnt) = (u32at(b, o + 16)? as usize, u32at(b, o + 20)? as usize,
                                          u32at(b, o + 24)? as usize, u32at(b, o + 28)? as usize);
            let bbox = [u16at(b, o + 32)?, u16at(b, o + 34)?, u16at(b, o + 36)?, u16at(b, o + 38)?];
            let mut strs = Vec::with_capacity(z as usize + 1);
            for k in 0..=z as usize {
                let so = u32at(b, o + 40 + 4 * k)?;
                string_offs.push(so);
                strs.push(opt_s(b, so)?);
            }
            string_offs.push(extra_off);
            let pack = strs[0].clone().ok_or("fstb: entry without a pack path")?;
            let mut subs = Vec::with_capacity(nsub);
            for k in 0..nsub {
                let so = u32at(b, subtab + 4 * k)? as usize;
                let (x, y, w, h) = (u16at(b, so)?, u16at(b, so + 2)?, u16at(b, so + 4)?, u16at(b, so + 6)?);
                let n = (w as usize * h as usize).div_ceil(8);
                subs.push(Bitmap { x, y, w, h, bits: b.get(so + 8..so + 8 + n).ok_or("fstb: bitmap out of range")?.to_vec() });
            }
            let blocks = (0..cnt).map(|k| Ok((u16at(b, bl + 4 * k)?, u16at(b, bl + 4 * k + 2)?))).collect::<Result<_, String>>()?;
            ents.push(Entry { hash, one, z, extra: opt_s(b, extra_off)?, bbox, pack, more_strings: strs[1..].to_vec(), subs,
                              blocks, offset: o });
        }
        levels.push(ents);
    }
    // layout: level 1's table after level 0's entries -> interleaved (vanilla); pool sorted -> vanilla
    let interleaved = nl > 1 && levels[0].first().is_some_and(|e| lv_offs[1] > e.offset);
    let mut pool: Vec<u32> = string_offs.into_iter().filter(|&o| o != 0).collect();
    pool.sort();
    pool.dedup();
    let texts: Vec<Vec<u8>> = pool.iter().map(|&o| s_at(b, o as usize).map(|s| latin1(&s))).collect::<Result<_, _>>()?;
    let sorted = texts.windows(2).all(|w| w[0] < w[1]);
    let layout = if interleaved || (nl <= 1 && sorted && texts.len() > 2) { Layout::Vanilla } else { Layout::Pipeline };
    Ok(Fstb { typ, common, levels, layout })
}

/// write_fstb (type 4: common packs only, the mbqf form)
pub fn write_fstb(common: &[&str]) -> Vec<u8> {
    let n = common.len();
    let strings_at = 20 + 4 * n;
    let mut out = b"FSTB\x00\x00\x00\x04".to_vec();
    for v in [0, strings_at as u32, n as u32] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    let mut blob = Vec::new();
    for s in common {
        out.extend_from_slice(&((strings_at + blob.len()) as u32).to_le_bytes());
        blob.extend_from_slice(s.as_bytes());
        blob.push(0);
    }
    out.extend(blob);
    out
}

fn entry_size(e: &Entry) -> usize {
    let subs: usize = e.subs.iter().map(|s| 8 + s.bits.len()).sum();
    (40 + 4 * (e.z as usize + 1) + 4 * e.subs.len() + subs).next_multiple_of(4) + 4 * e.blocks.len()
}

/// Write a parsed or constructed file (type 4 or 5) with its layout.
pub fn write(f: &Fstb) -> Result<Vec<u8>, String> {
    let common: Vec<&str> = f.common.iter().map(|s| s.as_str()).collect();
    match f.typ {
        4 => return Ok(write_fstb(&common)),
        5 => {}
        t => return Err(format!("fstb: type {t}")),
    }
    for e in f.levels.iter().flatten() {
        if e.more_strings.len() != e.z as usize {
            return Err(format!("fstb: entry {} has z = {} but {} extra strings", e.pack, e.z, e.more_strings.len()));
        }
        for s in &e.subs {
            if s.bits.len() != (s.w as usize * s.h as usize).div_ceil(8) {
                return Err("fstb: bitmap size does not match its width x height".into());
            }
        }
    }
    let (nl, nc) = (f.levels.len(), f.common.len());
    let lvtab = 20 + 4 * nc;
    // positions of level tables and entries
    let mut pos = lvtab + 4 * nl;
    let mut lv_off = vec![0usize; nl];
    let mut ent_off: Vec<Vec<usize>> = f.levels.iter().map(|l| vec![0; l.len()]).collect();
    if f.layout == Layout::Vanilla {
        for (i, lv) in f.levels.iter().enumerate() {
            lv_off[i] = pos;
            pos += 4 + 4 * lv.len();
            for (j, e) in lv.iter().enumerate() {
                ent_off[i][j] = pos;
                pos += entry_size(e);
            }
        }
    } else {
        for (i, lv) in f.levels.iter().enumerate() {
            lv_off[i] = pos;
            pos += 4 + 4 * lv.len();
        }
        for (i, lv) in f.levels.iter().enumerate() {
            for (j, e) in lv.iter().enumerate() {
                ent_off[i][j] = pos;
                pos += entry_size(e);
            }
        }
    }
    // string pool
    let pool_at = pos;
    let mut order: Vec<Vec<u8>> = Vec::new();
    let push = |s: &str, order: &mut Vec<Vec<u8>>| {
        let v = latin1(s);
        if !order.contains(&v) {
            order.push(v);
        }
    };
    if f.layout == Layout::Pipeline {
        push("", &mut order);
    }
    for c in &f.common {
        push(c, &mut order);
    }
    for e in f.levels.iter().flatten() {
        if let Some(x) = &e.extra {
            push(x, &mut order);
        }
        push(&e.pack, &mut order);
        for s in e.more_strings.iter().flatten() {
            push(s, &mut order);
        }
    }
    if f.layout == Layout::Vanilla {
        order.sort();
    }
    let mut pool = Vec::new();
    let mut at: std::collections::HashMap<Vec<u8>, u32> = std::collections::HashMap::new();
    for s in &order {
        at.insert(s.clone(), (pool_at + pool.len()) as u32);
        pool.extend_from_slice(s);
        pool.push(0);
    }
    let off = |s: &str| at[&latin1(s)];
    let opt = |s: &Option<String>| s.as_ref().map_or(0, |s| off(s));
    let mut out = b"FSTB\x00\x00\x00\x05".to_vec();
    let put = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
    put(&mut out, nl as u32);
    put(&mut out, lvtab as u32);
    put(&mut out, nc as u32);
    for c in &f.common {
        put(&mut out, off(c));
    }
    for &o in &lv_off {
        put(&mut out, o as u32);
    }
    let level_table = |out: &mut Vec<u8>, i: usize| {
        debug_assert_eq!(out.len(), lv_off[i]);
        put(out, f.levels[i].len() as u32);
        for &o in &ent_off[i] {
            put(out, o as u32);
        }
    };
    let entries = |out: &mut Vec<u8>, i: usize| {
        for (j, e) in f.levels[i].iter().enumerate() {
            let o = ent_off[i][j];
            debug_assert_eq!(out.len(), o);
            let subtab = o + 40 + 4 * (e.z as usize + 1);
            let mut so = subtab + 4 * e.subs.len();
            let mut sub_offs = Vec::with_capacity(e.subs.len());
            for s in &e.subs {
                sub_offs.push(so);
                so += 8 + s.bits.len();
            }
            let bl = so.next_multiple_of(4);
            for v in [e.hash, e.one, e.z, opt(&e.extra), e.subs.len() as u32, subtab as u32, bl as u32, e.blocks.len() as u32] {
                put(out, v);
            }
            for v in e.bbox {
                out.extend_from_slice(&v.to_le_bytes());
            }
            put(out, off(&e.pack));
            for s in &e.more_strings {
                put(out, opt(s));
            }
            for &s in &sub_offs {
                put(out, s as u32);
            }
            for s in &e.subs {
                for v in [s.x, s.y, s.w, s.h] {
                    out.extend_from_slice(&v.to_le_bytes());
                }
                out.extend_from_slice(&s.bits);
            }
            out.resize(bl, 0);
            for &(x, y) in &e.blocks {
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
            }
        }
    };
    if f.layout == Layout::Vanilla {
        for i in 0..nl {
            level_table(&mut out, i);
            entries(&mut out, i);
        }
    } else {
        for i in 0..nl {
            level_table(&mut out, i);
        }
        for i in 0..nl {
            entries(&mut out, i);
        }
    }
    debug_assert_eq!(out.len(), pool_at);
    out.extend(pool);
    Ok(out)
}

/// A type-5 large-block entry for write_fstb5 (the z = 0 form).
#[derive(Clone, Debug)]
pub struct NewEntry {
    /// the pack stem whose StrCode32 is the entry hash
    pub name: String,
    pub pack: String,
    /// in the caller's order (vanilla: row- or column-major)
    pub blocks: Vec<(u16, u16)>,
}

/// write_fstb5 (type 5, large-block levels in the z = 0 form; flyk_m2's layout)
pub fn write_fstb5(common: &[&str], levels: &[Vec<NewEntry>]) -> Result<Vec<u8>, String> {
    let mut lv = Vec::with_capacity(levels.len());
    for l in levels {
        let mut v = Vec::with_capacity(l.len());
        for e in l {
            if e.blocks.is_empty() {
                return Err(format!("fstb: entry {} has no blocks", e.name));
            }
            let xs = e.blocks.iter().map(|b| b.0);
            let ys = e.blocks.iter().map(|b| b.1);
            v.push(Entry {
                hash: strcode32(e.name.as_bytes()),
                one: 1,
                z: 0,
                extra: Some(String::new()),
                bbox: [xs.clone().min().unwrap(), ys.clone().min().unwrap(), xs.max().unwrap() + 1, ys.max().unwrap() + 1],
                pack: e.pack.clone(),
                more_strings: vec![],
                subs: vec![],
                blocks: e.blocks.clone(),
                offset: 0,
            });
        }
        lv.push(v);
    }
    write(&Fstb { typ: 5, common: common.iter().map(|s| s.to_string()).collect(), levels: lv, layout: Layout::Pipeline })
}

/// Rebuild a file from `read` (every vanilla file round-trips).
pub fn rebuild(f: &Fstb) -> Result<Vec<u8>, String> {
    write(f)
}

/// check_fstb5_writer for one file: every z = 0 entry re-emitted alone by write_fstb5 has the vanilla hash, block
/// list and bbox bytes. -> (z = 0 entries identical, z = 0 entries, z != 0 entries)
pub fn check_z0_entries(b: &[u8]) -> Result<(usize, usize, usize), String> {
    let f = read(b)?;
    let common: Vec<&str> = f.common.iter().map(|s| s.as_str()).collect();
    let (mut ok, mut n0, mut nz) = (0, 0, 0);
    for e in f.levels.iter().flatten() {
        if e.z != 0 {
            nz += 1;
            continue;
        }
        n0 += 1;
        let stem = e.pack.rsplit('/').next().unwrap_or(&e.pack);
        let stem = stem.strip_suffix(".fpk").unwrap_or(stem).to_string();
        let mine = write_fstb5(&common, &[vec![NewEntry { name: stem, pack: e.pack.clone(), blocks: e.blocks.clone() }]])?;
        let m = &read(&mine)?.levels[0][0];
        let k = 4 * e.blocks.len();
        let body = |x: &[u8], o: usize| [&x[o + 32..o + 40], &x[o + 44..o + 44 + k]].concat();
        if body(&mine, m.offset) == body(b, e.offset) && m.hash == e.hash && m.blocks == e.blocks {
            ok += 1;
        }
    }
    Ok((ok, n0, nz))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn type4_and_type5() {
        let b = write_fstb(&["/Assets/x/a.fpk"]);
        let f = read(&b).unwrap();
        assert_eq!(rebuild(&f).unwrap(), b);
        let lv = vec![NewEntry { name: "big".into(), pack: "/Assets/p/big.fpk".into(), blocks: vec![(130, 120), (131, 120)] }];
        let b5 = write_fstb5(&["/Assets/x/a.fpk"], &[lv]).unwrap();
        let f5 = read(&b5).unwrap();
        assert_eq!(f5.levels[0][0].bbox, [130, 120, 132, 121]);
        assert_eq!(rebuild(&f5).unwrap(), b5);
        assert_eq!(check_z0_entries(&b5).unwrap(), (1, 1, 0));
    }
    #[test]
    fn z2_vanilla_layout() {
        let bm = |w: u16, h: u16| Bitmap { x: 100, y: 90, w, h, bits: vec![0xAB; (w as usize * h as usize).div_ceil(8)] };
        let e = |pack: &str, z: u32| Entry {
            hash: 7, one: 1, z, extra: if z == 2 { Some("/Assets/m/spl.fmdl".into()) } else { None },
            bbox: [1, 2, 3, 4], pack: pack.into(),
            more_strings: if z == 2 { vec![Some(".fpk".into()), Some("/Assets/lod.fpk".into())] } else { vec![] },
            subs: if z == 2 { vec![bm(0, 0), bm(5, 3)] } else { vec![] }, blocks: vec![(1, 2), (2, 3)], offset: 0 };
        let f = Fstb { typ: 5, common: vec!["/Assets/c.fpk".into()],
                       levels: vec![vec![e("/Assets/b.fpk", 2), e("/Assets/a.fpk", 0)], vec![e("/Assets/x.fpk", 0)]],
                       layout: Layout::Vanilla };
        let b = write(&f).unwrap();
        let r = read(&b).unwrap();
        assert_eq!(r.layout, Layout::Vanilla);
        assert_eq!(write(&r).unwrap(), b);
        assert_eq!(r.levels[0][0].subs[1].bits.len(), 2);
    }
}
