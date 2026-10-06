//! FoxData containers (geom, geoms, gskl and other binary node trees): read + write with the layout kept.
//!
//! The FoxData header and node layout follow tools/location/terrain.py foxdata_read (docs/formats/terrain.md §4).
//! The game files store the nodes depth first: node header (0x30), its data at +0x30, its parameters, then its
//! children, then the next sibling. This codec reads that tree and writes it back; the bytes between a node's own
//! records and the next node (alignment, and whatever the game left there) are kept as read, so the round trip is
//! exact and edits that keep record sizes keep the layout. Payload formats (GeoGroup etc.) are parsed elsewhere
//! (foxcore::geom). Notes: docs/formats/geom.md.
//!
//!   header (0x20): u32 version, u32 nodes offset (0x20), u32 file size, {u32 name hash, i32 name offset}, u32 flags,
//!                  8 bytes
//!   node (0x30):   {u32 name hash, i32 name offset}, u32 flags, i32 data offset (0 or 0x30), u32 data size,
//!                  i32 parent, child, prev, next, params (relative to the node), 8 bytes
//!   parameter (0x10, 0x14 for string values): u16 type, i16 next (relative), {name hash, offset}, value

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub hash: u32,
    pub name_offset: i32,
    pub flags: u32,
    /// the data at +0x30 (data size bytes), None without data
    pub data: Option<Vec<u8>>,
    /// bytes between the data's end and the parameters (parameters present only)
    pub gap_before_params: Vec<u8>,
    /// the parameter block as stored (records + the strings they point to), empty = none
    pub params: Vec<u8>,
    /// node header bytes 0x28..0x30
    pub pad: [u8; 8],
    /// whether the parent / prev links are stored (some writers leave them 0)
    pub parent_link: bool,
    pub prev_link: bool,
    /// bytes between this node's own records and its first child / the next node / the end of the file
    pub gap: Vec<u8>,
    pub children: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FoxData {
    pub version: u32,
    pub name_hash: u32,
    pub name_offset: i32,
    pub flags: u32,
    pub pad: [u8; 8],
    /// the root-level nodes (a sibling chain starting at 0x20)
    pub roots: Vec<Node>,
}

fn u32at(d: &[u8], i: usize) -> Result<u32, String> {
    d.get(i..i + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("foxdata: truncated at {i:#x}"))
}
fn i32at(d: &[u8], i: usize) -> Result<i32, String> {
    Ok(u32at(d, i)? as i32)
}
fn rel(p: usize, r: i32) -> usize {
    (p as i64 + r as i64) as usize
}

/// end (exclusive) of a parameter block starting at `q`, including the strings its records point to
fn params_end(d: &[u8], mut q: usize) -> Result<usize, String> {
    let mut end = q;
    let str_end = |f: usize| -> Result<usize, String> {
        let off = i32at(d, f + 4)?;
        if off == 0 {
            return Ok(0);
        }
        let at = rel(f, off);
        let n = d.get(at..).and_then(|s| s.iter().position(|&c| c == 0)).ok_or("foxdata: unterminated string")?;
        Ok(at + n + 1)
    };
    for _ in 0..4096 {
        let ty = u16::from_le_bytes(d.get(q..q + 2).ok_or("foxdata: param out of range")?.try_into().unwrap());
        let nx = i16::from_le_bytes(d[q + 2..q + 4].try_into().unwrap());
        end = end.max(q + if ty == 1 { 20 } else { 16 }).max(str_end(q + 4)?);
        if ty == 1 {
            end = end.max(str_end(q + 12)?);
        }
        if nx == 0 {
            return Ok(end);
        }
        q = rel(q, nx as i32);
    }
    Err("foxdata: parameter chain too long".into())
}

pub fn read(d: &[u8]) -> Result<FoxData, String> {
    if u32at(d, 4)? != 0x20 || u32at(d, 8)? as usize != d.len() {
        return Err("not a FoxData container (nodes at 0x20, size)".into());
    }
    // `follow`: where the next node after this one's subtree starts (the end of the file for the last)
    fn chain(d: &[u8], first: usize, follow: usize, depth: usize) -> Result<Vec<Node>, String> {
        if depth > 64 {
            return Err("foxdata: tree too deep".into());
        }
        let mut out = Vec::new();
        let mut p = first;
        loop {
            let nx = i32at(d, p + 0x20)?;
            let after = if nx == 0 { follow } else { rel(p, nx) };
            if after <= p {
                return Err(format!("foxdata: node at {p:#x} is not followed in file order"));
            }
            out.push(node(d, p, after, depth)?);
            if nx == 0 {
                return Ok(out);
            }
            p = after;
        }
    }
    fn node(d: &[u8], p: usize, follow: usize, depth: usize) -> Result<Node, String> {
        let (doff, dsize) = (i32at(d, p + 12)?, u32at(d, p + 16)? as usize);
        let (child, poff) = (i32at(d, p + 24)?, i32at(d, p + 36)?);
        let mut end = p + 0x30;
        let data = if doff != 0 {
            if doff != 0x30 {
                return Err(format!("foxdata: data at +{doff:#x} (only +0x30 is modelled)"));
            }
            end += dsize;
            Some(d.get(p + 0x30..end).ok_or("foxdata: data out of range")?.to_vec())
        } else {
            None
        };
        let (mut gap_before_params, mut params) = (Vec::new(), Vec::new());
        if poff != 0 {
            let q = rel(p, poff);
            if q < end {
                return Err("foxdata: parameters before the data end".into());
            }
            let pe = params_end(d, q)?;
            gap_before_params = d[end..q].to_vec();
            params = d.get(q..pe).ok_or("foxdata: params out of range")?.to_vec();
            end = pe;
        }
        let next_start = if child != 0 { rel(p, child) } else { follow };
        let gap = d.get(end..next_start).ok_or_else(|| format!("foxdata: node at {p:#x} overlaps the next"))?.to_vec();
        let children = if child != 0 { chain(d, next_start, follow, depth + 1)? } else { Vec::new() };
        Ok(Node { hash: u32at(d, p)?, name_offset: i32at(d, p + 4)?, flags: u32at(d, p + 8)?, data, gap_before_params,
                  params, pad: d[p + 0x28..p + 0x30].try_into().unwrap(), parent_link: i32at(d, p + 20)? != 0,
                  prev_link: i32at(d, p + 28)? != 0, gap, children })
    }
    let roots = if d.len() > 0x20 { chain(d, 0x20, d.len(), 0)? } else { Vec::new() };
    Ok(FoxData { version: u32at(d, 0)?, name_hash: u32at(d, 12)?, name_offset: i32at(d, 16)?, flags: u32at(d, 20)?,
                 pad: d[24..32].try_into().unwrap(), roots })
}

pub fn write(f: &FoxData) -> Vec<u8> {
    // layout pass: node positions, depth first
    fn place(n: &Node, p: usize, pos: &mut Vec<usize>) -> usize {
        pos.push(p);
        let mut e = p + 0x30 + n.data.as_ref().map_or(0, |x| x.len());
        if !n.params.is_empty() {
            e += n.gap_before_params.len() + n.params.len();
        }
        e += n.gap.len();
        for c in &n.children {
            e = place(c, e, pos);
        }
        e
    }
    let mut pos = Vec::new();
    let mut end = 0x20;
    for r in &f.roots {
        end = place(r, end, &mut pos);
    }
    let mut b = vec![0u8; end.max(0x20)];
    let put = |b: &mut Vec<u8>, at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    put(&mut b, 0, f.version);
    put(&mut b, 4, 0x20);
    let total = b.len() as u32;
    put(&mut b, 8, total);
    put(&mut b, 12, f.name_hash);
    put(&mut b, 16, f.name_offset as u32);
    put(&mut b, 20, f.flags);
    b[24..32].copy_from_slice(&f.pad);
    // emit pass, in the same order as `place`
    fn emit(b: &mut Vec<u8>, sibs: &[Node], parent: Option<usize>, pos: &[usize], k: &mut usize) {
        let mine: Vec<usize> = {
            // positions of this sibling list: each sibling's index in `pos` advances by its subtree size
            let mut v = Vec::new();
            let mut j = *k;
            for s in sibs {
                v.push(pos[j]);
                j += count(s);
            }
            v
        };
        for (i, n) in sibs.iter().enumerate() {
            let p = mine[i];
            *k += 1;
            let r = |x: Option<usize>| -> u32 { x.map_or(0, |x| (x as i64 - p as i64) as i32 as u32) };
            let put = |b: &mut Vec<u8>, at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
            put(b, p, n.hash);
            put(b, p + 4, n.name_offset as u32);
            put(b, p + 8, n.flags);
            put(b, p + 12, if n.data.is_some() { 0x30 } else { 0 });
            put(b, p + 16, n.data.as_ref().map_or(0, |x| x.len() as u32));
            put(b, p + 20, if n.parent_link { r(parent) } else { 0 });
            put(b, p + 24, r(if n.children.is_empty() { None } else { Some(pos[*k]) }));
            put(b, p + 28, if n.prev_link { r(if i > 0 { Some(mine[i - 1]) } else { None }) } else { 0 });
            put(b, p + 32, r(mine.get(i + 1).copied()));
            let mut e = p + 0x30;
            if let Some(x) = &n.data {
                b[e..e + x.len()].copy_from_slice(x);
                e += x.len();
            }
            if !n.params.is_empty() {
                b[e..e + n.gap_before_params.len()].copy_from_slice(&n.gap_before_params);
                e += n.gap_before_params.len();
                put(b, p + 36, (e - p) as u32);
                b[e..e + n.params.len()].copy_from_slice(&n.params);
                e += n.params.len();
            } else {
                put(b, p + 36, 0);
            }
            b[p + 0x28..p + 0x30].copy_from_slice(&n.pad);
            b[e..e + n.gap.len()].copy_from_slice(&n.gap);
            emit(b, &n.children, Some(p), pos, k);
        }
    }
    fn count(n: &Node) -> usize {
        1 + n.children.iter().map(count).sum::<usize>()
    }
    let mut k = 0;
    emit(&mut b, &f.roots, None, &pos, &mut k);
    b
}

/// every node, depth first, with its absolute offset in the written layout
pub fn walk(f: &FoxData) -> Vec<(&Node, usize)> {
    fn go<'a>(n: &'a Node, p: usize, out: &mut Vec<(&'a Node, usize)>) -> usize {
        out.push((n, p));
        let mut e = p + 0x30 + n.data.as_ref().map_or(0, |x| x.len());
        if !n.params.is_empty() {
            e += n.gap_before_params.len() + n.params.len();
        }
        e += n.gap.len();
        for c in &n.children {
            e = go(c, e, out);
        }
        e
    }
    let mut out = Vec::new();
    let mut e = 0x20;
    for r in &f.roots {
        e = go(r, e, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_tree() {
        let leaf = |h, data: Option<Vec<u8>>, gap: usize| Node { hash: h, name_offset: 0, flags: 6, data,
            gap_before_params: vec![], params: vec![], pad: [0; 8], parent_link: true, prev_link: true, gap: vec![0; gap],
            children: vec![] };
        let mut root = leaf(1, None, 0);
        root.flags = 0x10;
        root.children = vec![leaf(2, Some(vec![7; 20]), 12), leaf(3, Some(vec![9; 16]), 0)];
        let f = FoxData { version: 201406020, name_hash: 0x6B936E, name_offset: 0, flags: 0, pad: [0; 8],
                          roots: vec![root, leaf(4, None, 0)] };
        let b = write(&f);
        let r = read(&b).unwrap();
        // links that are absent by position (a root's parent, a first sibling's prev) read back as not stored
        assert_eq!(write(&r), b);
        assert_eq!(read(&write(&r)).unwrap(), r);
        assert_eq!(r.roots[0].children[1].data, Some(vec![9; 16]));
        assert_eq!(walk(&r).len(), 4);
    }
}
