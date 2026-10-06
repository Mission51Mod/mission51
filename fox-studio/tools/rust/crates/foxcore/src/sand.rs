//! Demo node data (.sand): the track tree a demo stream's segments belong to. Read + write.
//!
//! tools/demo/sand.py reads these; tools/demo/sand_write.py writes new ones with the layout mirrored from the game
//! files. This module reads the tree and writes it back with that layout: node payloads (TrackHeaders, string
//! lists) and parameter records are kept as bytes; positions, links, the track offset table and the section table
//! are rebuilt. Notes: docs/formats/sand.md. Credit: kapuragu/FoxEngineTemplates sand.bt / anim_FoxData.bt
//! (documentation).
//!
//!   header  u32 version, u32 file size, u32 4 (sections), u32 track count; 4 x {u32 type, u32 count,
//!           u32 offset (from this descriptor), u32 size}
//!   1 TRACK_OFFSETS  u32[count]: absolute offsets of the track nodes' TrackHeaders (index = track id), 16-aligned
//!   2 NODES          FoxData nodes, depth first: node header 0x28 (+ 8 bytes to 0x30), the payload at +0x30
//!                    (16-aligned after), the parameters (16-aligned after), the children, then the next sibling
//!   3 HASH_TABLES    {u32 name, u32 track id} (starts 16-aligned; section 4 follows directly)
//!   4 TARGET_NAME_TABLES {u32 name, i16 move, skel, fragment, motion point, u32 SHADER node offset}
//! node header: {u32 name hash, u32 name string offset}, u32 flags (1 = TrackHeader payload, 0 = other),
//!              i32 data offset, u32 data size (0 for tracks), i32 parent, child, prev, next, params (relative)

fn align16(v: usize) -> usize {
    v.next_multiple_of(16)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub hash: u32,
    pub name_offset: u32,
    pub flags: u32,
    /// the payload bytes at +0x30 (TrackHeader with its units, or a string list), None without data
    pub payload: Option<Vec<u8>>,
    /// the data-size field as stored (0 for TrackHeaders)
    pub data_size: u32,
    /// bytes 0x28..0x30 of the node header
    pub pad: [u8; 8],
    /// the parameter block as stored: the records (u16 type, i16 next, {name hash, string offset}, value; string
    /// values are {hash, offset}) and the strings they point to, which follow the records. Empty = no parameters.
    pub params: Vec<u8>,
    pub children: Vec<Node>,
    /// zero bytes after this node's own records, before its first child / the next node. In vanilla files 0, 16 or
    /// 32 after string parameters: room of the stripped TARGET_NAME strings, not derivable from the file.
    pub trailing: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sand {
    pub version: u32,
    pub root: Node,
    /// section 3 entries (name, track id)
    pub hash_table: Vec<(u32, u32)>,
    /// section 4 entries, raw 16 bytes each (the SHADER offset in them is absolute: valid while the layout is)
    pub targets: Vec<[u8; 16]>,
}

fn u32at(d: &[u8], i: usize) -> Result<u32, String> {
    d.get(i..i + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("sand: truncated at {i:#x}"))
}
fn i32at(d: &[u8], i: usize) -> Result<i32, String> {
    Ok(u32at(d, i)? as i32)
}

/// byte length of a TrackHeader with its units (header, unit offsets, units of 8 + 8 per segment)
fn track_len(d: &[u8], p: usize) -> Result<usize, String> {
    let uc = i32at(d, p)?.max(0) as usize;
    let mut end = p + 20 + 4 * uc;
    for k in 0..uc {
        let q = p + u32at(d, p + 20 + 4 * k)? as usize;
        let nseg = *d.get(q + 4).ok_or("sand: unit out of range")? as usize;
        end = end.max(q + 8 + 8 * nseg);
    }
    Ok(end - p)
}

pub fn read(d: &[u8]) -> Result<Sand, String> {
    let version = u32at(d, 0)?;
    if u32at(d, 8)? != 4 || u32at(d, 4)? as usize != d.len() {
        return Err("not a sand (4 sections, size)".into());
    }
    let mut sec = [(0usize, 0usize, 0usize); 5];
    for i in 0..4 {
        let p = 0x10 + 16 * i;
        let t = u32at(d, p)? as usize;
        if !(1..=4).contains(&t) {
            return Err(format!("sand: section type {t}"));
        }
        sec[t] = (u32at(d, p + 4)? as usize, p + u32at(d, p + 8)? as usize, u32at(d, p + 12)? as usize);
    }
    // follow = where the next node after this subtree starts (the end of the node section for the last one)
    fn node(d: &[u8], p: usize, depth: usize, follow: usize) -> Result<Node, String> {
        if depth > 64 {
            return Err("sand: node tree too deep".into());
        }
        let flags = u32at(d, p + 8)?;
        let doff = i32at(d, p + 12)?;
        let dsize = u32at(d, p + 16)?;
        let child = i32at(d, p + 24)?;
        let poff = i32at(d, p + 36)?;
        let payload = if doff != 0 {
            let q = (p as i64 + doff as i64) as usize;
            if doff != 0x30 {
                return Err(format!("sand: payload at +{doff:#x}"));
            }
            let n = if flags == 1 { track_len(d, q)? } else { dsize as usize };
            Some(d.get(q..q + n).ok_or("sand: payload out of range")?.to_vec())
        } else {
            None
        };
        let mut params = Vec::new();
        if poff != 0 {
            let start = (p as i64 + poff as i64) as usize;
            let mut q = start;
            let mut end = start;
            // a string a {hash, offset} field at `f` points to (offset relative to the field); its end incl. NUL
            let str_end = |f: usize| -> Result<usize, String> {
                let off = i32at(d, f + 4)?;
                if off == 0 {
                    return Ok(0);
                }
                let at = (f as i64 + off as i64) as usize;
                let n = d.get(at..).and_then(|s| s.iter().position(|&c| c == 0)).ok_or("sand: unterminated string")?;
                Ok(at + n + 1)
            };
            loop {
                let ty = u16::from_le_bytes(d.get(q..q + 2).ok_or("sand: param out of range")?.try_into().unwrap());
                let nx = i16::from_le_bytes(d[q + 2..q + 4].try_into().unwrap());
                let len = if ty == 1 { 20 } else { 16 };
                end = end.max(q + len).max(str_end(q + 4)?);
                if ty == 1 {
                    end = end.max(str_end(q + 12)?);
                }
                if nx == 0 {
                    break;
                }
                q = (q as i64 + nx as i64) as usize;
            }
            params = d.get(start..end).ok_or("sand: params out of range")?.to_vec();
        }
        let mut own_end = p + 0x30;
        if let Some(pl) = &payload {
            own_end = align16(own_end + pl.len());
        }
        if !params.is_empty() {
            own_end = align16((p as i64 + poff as i64) as usize + params.len());
        }
        let mut children = Vec::new();
        let mut next_start = follow;
        if child != 0 {
            let mut q = (p as i64 + child as i64) as usize;
            next_start = q;
            loop {
                let nx = i32at(d, q + 0x20)?;
                let after = if nx == 0 { follow } else { (q as i64 + nx as i64) as usize };
                children.push(node(d, q, depth + 1, after)?);
                if nx == 0 {
                    break;
                }
                q = after;
            }
        }
        let trailing = next_start.checked_sub(own_end).ok_or_else(|| format!("sand: node at {p:#x} overlaps the next"))?;
        Ok(Node { hash: u32at(d, p)?, name_offset: u32at(d, p + 4)?, flags, payload, data_size: dsize,
                  pad: d[p + 0x28..p + 0x30].try_into().unwrap(), params, children, trailing })
    }
    let root = node(d, sec[2].1, 0, sec[2].1 + sec[2].2)?;
    let (n3, o3, _) = sec[3];
    let hash_table = (0..n3).map(|i| Ok((u32at(d, o3 + 8 * i)?, u32at(d, o3 + 8 * i + 4)?))).collect::<Result<_, String>>()?;
    let (n4, o4, _) = sec[4];
    let targets = (0..n4).map(|i| d.get(o4 + 16 * i..o4 + 16 * i + 16).map(|s| s.try_into().unwrap())
        .ok_or_else(|| "sand: target out of range".to_string())).collect::<Result<_, _>>()?;
    Ok(Sand { version, root, hash_table, targets })
}

/// the track id of a TrackHeader payload (u16 at +8)
fn track_id(n: &Node) -> Option<usize> {
    match (&n.payload, n.flags) {
        (Some(p), 1) if p.len() >= 10 => Some(u16::from_le_bytes([p[8], p[9]]) as usize),
        _ => None,
    }
}

pub fn write(s: &Sand) -> Result<Vec<u8>, String> {
    // track count = number of TrackHeader nodes
    fn count(n: &Node) -> usize {
        (track_id(n).is_some() as usize) + n.children.iter().map(count).sum::<usize>()
    }
    let ntracks = count(&s.root);
    let sec1 = 0x50usize;
    let sec1_size = align16(4 * ntracks);
    let sec2 = sec1 + sec1_size;
    // layout pass: positions in depth-first order
    struct Pos {
        node: usize,
        params: usize,
    }
    fn layout<'a>(n: &'a Node, pos: usize, out: &mut Vec<(&'a Node, Pos)>) -> usize {
        let at = out.len();
        out.push((n, Pos { node: pos, params: 0 }));
        let mut p = pos + 0x30;
        if let Some(pl) = &n.payload {
            p = align16(p + pl.len());
        }
        if !n.params.is_empty() {
            out[at].1.params = p;
            p = align16(p + n.params.len());
        }
        p += n.trailing;
        for c in &n.children {
            p = layout(c, p, out);
        }
        p
    }
    let mut order = Vec::new();
    let end = layout(&s.root, sec2, &mut order);
    let sec3 = align16(end);
    let sec3_size = 8 * s.hash_table.len(); // not aligned: section 4 follows directly
    let sec4 = sec3 + sec3_size;
    let sec4_size = 16 * s.targets.len();
    let total = sec4 + sec4_size;
    let mut b = vec![0u8; total];
    let put = |b: &mut Vec<u8>, at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    for (i, v) in [s.version, total as u32, 4, ntracks as u32].into_iter().enumerate() {
        put(&mut b, 4 * i, v);
    }
    for (k, (ty, cnt, off, size)) in [(1, ntracks, sec1, sec1_size), (2, 0, sec2, end - sec2),
                                       (3, s.hash_table.len(), sec3, sec3_size), (4, s.targets.len(), sec4, sec4_size)]
        .into_iter().enumerate() {
        let d = 0x10 + 16 * k;
        for (j, v) in [ty as u32, cnt as u32, (off - d) as u32, size as u32].into_iter().enumerate() {
            put(&mut b, d + 4 * j, v);
        }
    }
    // node index by address, to resolve links
    let pos_of: std::collections::HashMap<*const Node, usize> =
        order.iter().map(|(n, p)| (*n as *const Node, p.node)).collect();
    let mut seen_ids = vec![false; ntracks];
    // Node address, parent, previous sibling and next sibling; addresses identify links.
    type NodeLinks = (*const Node, Option<*const Node>, Option<*const Node>, Option<*const Node>);
    fn links<'a>(n: &'a Node, parent: Option<&'a Node>, prev: Option<&'a Node>, next: Option<&'a Node>,
                 out: &mut Vec<NodeLinks>) {
        out.push((n, parent.map(|x| x as *const _), prev.map(|x| x as *const _), next.map(|x| x as *const _)));
        for (k, c) in n.children.iter().enumerate() {
            links(c, Some(n), if k > 0 { Some(&n.children[k - 1]) } else { None }, n.children.get(k + 1), out);
        }
    }
    let mut lk = Vec::new();
    links(&s.root, None, None, None, &mut lk);
    for ((n, ps), (_, parent, prev, next)) in order.iter().zip(&lk) {
        let p = ps.node;
        let rel = |x: Option<*const Node>| -> u32 { x.map_or(0, |x| (pos_of[&x] as i64 - p as i64) as i32 as u32) };
        put(&mut b, p, n.hash);
        put(&mut b, p + 4, n.name_offset);
        put(&mut b, p + 8, n.flags);
        put(&mut b, p + 12, if n.payload.is_some() { 0x30 } else { 0 });
        put(&mut b, p + 16, n.data_size);
        put(&mut b, p + 20, rel(*parent));
        put(&mut b, p + 24, rel(n.children.first().map(|c| c as *const Node)));
        put(&mut b, p + 28, rel(*prev));
        put(&mut b, p + 32, rel(*next));
        put(&mut b, p + 36, if n.params.is_empty() { 0 } else { (ps.params - p) as u32 });
        b[p + 0x28..p + 0x30].copy_from_slice(&n.pad);
        if let Some(pl) = &n.payload {
            b[p + 0x30..p + 0x30 + pl.len()].copy_from_slice(pl);
        }
        b[ps.params..ps.params + n.params.len()].copy_from_slice(&n.params);
        if let Some(id) = track_id(n) {
            if id >= ntracks || seen_ids[id] {
                return Err(format!("sand: track id {id} out of range or repeated"));
            }
            seen_ids[id] = true;
            put(&mut b, sec1 + 4 * id, (p + 0x30) as u32);
        }
    }
    for (k, &(h, t)) in s.hash_table.iter().enumerate() {
        put(&mut b, sec3 + 8 * k, h);
        put(&mut b, sec3 + 8 * k + 4, t);
    }
    for (k, t) in s.targets.iter().enumerate() {
        b[sec4 + 16 * k..sec4 + 16 * k + 16].copy_from_slice(t);
    }
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn track(id: u16) -> Vec<u8> {
        // 1 unit with 1 segment: header 20 + 4 offsets, aligned + 8, unit 8 + seg 8
        let mut v = vec![0u8; 0x28];
        v[0..4].copy_from_slice(&1i32.to_le_bytes());
        v[4..8].copy_from_slice(&1u32.to_le_bytes());
        v[8..10].copy_from_slice(&id.to_le_bytes());
        v[20..24].copy_from_slice(&0x28u32.to_le_bytes());
        v.extend_from_slice(&[0xAA, 0, 0, 0, 1, 0, 0, 0]);
        v.extend_from_slice(&[0, 0, 0, 0, 3, 0, 0x03, 32]);
        v
    }
    #[test]
    fn roundtrip_small() {
        let leaf = |id| Node { hash: 7, name_offset: 0, flags: 1, payload: Some(track(id)), data_size: 0, pad: [0; 8],
                               params: [1u16.to_le_bytes(), 0i16.to_le_bytes()].concat().into_iter()
                                   .chain([0u8; 16]).collect(), children: vec![], trailing: 16 };
        let root = Node { hash: 1, name_offset: 0, flags: 0, payload: None, data_size: 0, pad: [0; 8], params: vec![],
                          children: vec![leaf(1), leaf(0)], trailing: 0 };
        let s = Sand { version: 201406230, root, hash_table: vec![(5, 0)], targets: vec![[0; 16]] };
        let b = write(&s).unwrap();
        let r = read(&b).unwrap();
        assert_eq!(r, s);
        assert_eq!(write(&r).unwrap(), b);
    }
}
