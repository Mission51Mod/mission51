//! Collision geometry (.geom, FoxData version 0x0C013644): GeoGroup parsing and the simple-shape writer.
//!
//! Port of tools/location/flyk_dressing_geom.py (parse_group / parse, group_payload, build, without_bits,
//! mech_free). The container itself round-trips through foxcore::foxdata. ring_hull (numpy hull construction) stays
//! with its caller. Notes: docs/formats/geom.md. Credit: kapuragu/FoxEngineTemplates geom_geoms_gskl.bt +
//! common/geo_common.bt (documentation).
//!
//! GeoGroup payload (in flags-6 nodes):
//!   blocks (32 B each, ended by a block whose first byte is 1): u8 isFinal, u8 header count, u16 headers data size,
//!     u16 vb offset (unused), u16 0, u16 headers offset (unused), u16 0, u32 vertex buffer offset, u32 headers
//!     offset, u32 next section offset (all relative to the block), u64 collision tags
//!   after the final block: u8 material offset, u8 material size, u8 aux offset, u8 aux size, then 12-byte entries
//!     (u32 name hash, u64 0) ended by a 0 hash
//!   shape headers (32 B + prims): u32 type:4 | flags:20 | prim count:8, i32 next, prev, child (x16, relative),
//!     u64 tags, u32 name, u32 vertex buffer offset (x16); AABB prims = vec4 radii + vec4 centre, POLY prims =
//!     i16 a, b, c, d + u8 material info x2
//!   vertex header (32 B): u32 count, u32 first index, u32 1, u32 origin index, i64 data offset (relative), 8 zero
//!     bytes; then count x vec4 (vertex 0 = the origin)

pub const GEO_VERSION: u32 = 201406020; // 0x0c013644
pub const GEOM_NAME: u32 = 0x006B_936E;
pub const GROUP_NODE: u32 = 0xBE9A_4BB7;
pub const SIBLING_NODE: u32 = 0x0A71_92FA;
pub const SAHELAN_BIT: u64 = 1 << 48;
/// vanilla solid tag sets that carry the Sahelanthropus bit, found by value where the group layout is not decoded
pub const SCAN_TAGS: [u64; 3] = [0x4061_0000_8000_003C, 0x4061_0000_8004_203C, 0x4061_0000_8000_203C];

#[derive(Clone, Debug, PartialEq)]
pub enum Prims {
    Aabb(Vec<([f32; 4], [f32; 4])>),
    Poly(Vec<(i16, i16, i16, i16, u8, u8)>),
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub offset: usize,
    pub typ: u32,
    pub flags: u32,
    pub count: u32,
    pub next: i32,
    pub prev: i32,
    pub child: i32,
    pub tags: u64,
    pub name: u32,
    pub vb: u32,
    pub prims: Prims,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub offset: usize,
    pub header_count: u8,
    pub headers_size: u16,
    pub vb_offset: u32,
    pub headers_offset: u32,
    pub next_offset: u32,
    pub tags: u64,
    pub shapes: Vec<Shape>,
    /// (count, first, one, origin, data offset)
    pub vertex_header: (u32, u32, u32, u32, i64),
    pub vertices: Vec<[f32; 4]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub blocks: Vec<Block>,
    pub final_block: usize,
    pub mat_header: [u8; 4],
    pub materials: Vec<u32>,
    pub aux: Vec<u32>,
}

fn u8at(b: &[u8], i: usize) -> Result<u8, String> {
    b.get(i).copied().ok_or_else(|| format!("geom: truncated at {i:#x}"))
}
fn u32at(b: &[u8], i: usize) -> Result<u32, String> {
    b.get(i..i + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("geom: truncated at {i:#x}"))
}
fn u64at(b: &[u8], i: usize) -> Result<u64, String> {
    b.get(i..i + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("geom: truncated at {i:#x}"))
}
fn f4(b: &[u8], i: usize) -> Result<[f32; 4], String> {
    let s = b.get(i..i + 16).ok_or_else(|| format!("geom: truncated at {i:#x}"))?;
    Ok(std::array::from_fn(|k| f32::from_le_bytes(s[4 * k..4 * k + 4].try_into().unwrap())))
}

/// parse_group: the GeoGroup at absolute `start`
pub fn parse_group(buf: &[u8], start: usize) -> Result<Group, String> {
    let mut blocks = Vec::new();
    let mut o = start;
    while u8at(buf, o)? == 0 {
        let hdo = u32at(buf, o + 16)?;
        let mut blk = Block {
            offset: o,
            header_count: u8at(buf, o + 1)?,
            headers_size: u16::from_le_bytes([u8at(buf, o + 2)?, u8at(buf, o + 3)?]),
            vb_offset: u32at(buf, o + 12)?,
            headers_offset: hdo,
            next_offset: u32at(buf, o + 20)?,
            tags: u64at(buf, o + 24)?,
            shapes: Vec::new(),
            vertex_header: (0, 0, 0, 0, 0),
            vertices: Vec::new(),
        };
        // the shape tree, depth first as the engine walks it (child before next)
        let mut stack = vec![o + hdo as usize];
        let mut seen = std::collections::BTreeSet::new();
        while let Some(h) = stack.pop() {
            if !seen.insert(h) {
                continue;
            }
            if seen.len() > 1 << 16 {
                return Err("geom: shape tree too large".into());
            }
            let w0 = u32at(buf, h)?;
            let (nxt, prv, chi) = (u32at(buf, h + 4)? as i32, u32at(buf, h + 8)? as i32, u32at(buf, h + 12)? as i32);
            let (typ, flags, pcount) = (w0 & 0xF, (w0 >> 4) & 0xF_FFFF, w0 >> 24);
            let p = h + 32;
            let prims = match typ {
                4 => Prims::Aabb((0..pcount as usize).map(|k| Ok((f4(buf, p + 32 * k)?, f4(buf, p + 32 * k + 16)?)))
                    .collect::<Result<_, String>>()?),
                2 => Prims::Poly((0..pcount as usize).map(|k| {
                    let q = p + 10 * k;
                    let i16at = |j: usize| -> Result<i16, String> {
                        Ok(i16::from_le_bytes([u8at(buf, q + j)?, u8at(buf, q + j + 1)?]))
                    };
                    Ok((i16at(0)?, i16at(2)?, i16at(4)?, i16at(6)?, u8at(buf, q + 8)?, u8at(buf, q + 9)?))
                }).collect::<Result<_, String>>()?),
                _ => Prims::Other,
            };
            blk.shapes.push(Shape { offset: h, typ, flags, count: pcount, next: nxt, prev: prv, child: chi,
                                    tags: u64at(buf, h + 16)?, name: u32at(buf, h + 24)?, vb: u32at(buf, h + 28)?, prims });
            let step = |r: i32| (h as i64 + r as i64 * 16) as usize;
            if nxt != 0 {
                stack.push(step(nxt));
            }
            if chi != 0 {
                stack.push(step(chi));
            }
        }
        let vh = o + blk.vb_offset as usize;
        let (cnt, first, one, origin) = (u32at(buf, vh)?, u32at(buf, vh + 4)?, u32at(buf, vh + 8)?, u32at(buf, vh + 12)?);
        let doff = u64at(buf, vh + 16)? as i64;
        blk.vertex_header = (cnt, first, one, origin, doff);
        let vs = (vh as i64 + doff) as usize;
        // as the Python: a vertex array that runs past the end reads as empty
        blk.vertices = if vs + 16 * cnt as usize <= buf.len() {
            (0..cnt as usize).map(|k| f4(buf, vs + 16 * k)).collect::<Result<_, _>>()?
        } else {
            Vec::new()
        };
        blocks.push(blk);
        o += 32;
    }
    let final_block = o;
    o += 32;
    let mh: [u8; 4] = buf.get(o..o + 4).ok_or("geom: truncated material header")?.try_into().unwrap();
    let base = o + 4;
    let list = |off: u8| -> Result<Vec<u32>, String> {
        let mut v = Vec::new();
        let mut q = base + off as usize * 12;
        while u32at(buf, q)? != 0 {
            v.push(u32at(buf, q)?);
            q += 12;
        }
        Ok(v)
    };
    Ok(Group { blocks, final_block, mat_header: mh, materials: list(mh[0])?, aux: list(mh[2])? })
}

/// parse: every group of the file's flags-6 nodes, with the node's name hash
pub fn parse(buf: &[u8]) -> Result<Vec<(u32, Group)>, String> {
    let f = crate::foxdata::read(buf)?;
    let mut out = Vec::new();
    for (n, p) in crate::foxdata::walk(&f) {
        if n.data.is_some() && n.flags == 6 {
            out.push((n.hash, parse_group(buf, p + 0x30)?));
        }
    }
    Ok(out)
}

fn align16(v: usize) -> usize {
    v.next_multiple_of(16)
}

/// group_payload(tags, material, vertices, quads): one block, AABB -> AABB -> POLY over `vertices`
pub fn group_payload(tags: u64, material: Option<u32>, vertices: &[[f64; 3]], quads: &[[i16; 4]]) -> Vec<u8> {
    let n_mat = material.is_some() as usize;
    let mat_size = 4 + 12 * (n_mat + 2);
    let hdr_off = align16(0x40 + mat_size);
    let (aabb1, aabb2) = (hdr_off, hdr_off + 0x40);
    let poly = aabb2 + 0x40;
    let vh = align16(poly + 32 + 10 * quads.len());
    let vdata = vh + 32;
    let end = vdata + 16 * (vertices.len() + 1);
    let mut out = vec![0u8; end];
    let put = |o: &mut Vec<u8>, at: usize, v: &[u8]| o[at..at + v.len()].copy_from_slice(v);
    put(&mut out, 0, &[0, 3]);
    put(&mut out, 2, &((end - hdr_off) as u16).to_le_bytes());
    put(&mut out, 4, &((vh & 0xFFFF) as u16).to_le_bytes());
    put(&mut out, 8, &((hdr_off & 0xFFFF) as u16).to_le_bytes());
    put(&mut out, 12, &(vh as u32).to_le_bytes());
    put(&mut out, 16, &(hdr_off as u32).to_le_bytes());
    put(&mut out, 20, &0x40u32.to_le_bytes());
    put(&mut out, 24, &tags.to_le_bytes());
    out[32] = 1; // the final (empty) block
    put(&mut out, 0x40, &[0, (n_mat + 1) as u8, (n_mat + 1) as u8, 1]);
    if let Some(m) = material {
        put(&mut out, 0x44, &m.to_le_bytes());
    }
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for v in vertices {
        for k in 0..3 {
            lo[k] = lo[k].min(v[k]);
            hi[k] = hi[k].max(v[k]);
        }
    }
    let rad: [f64; 3] = std::array::from_fn(|k| (hi[k] - lo[k]) / 2.0);
    let ctr: [f64; 3] = std::array::from_fn(|k| (hi[k] + lo[k]) / 2.0);
    for h in [aabb1, aabb2] {
        put(&mut out, h, &(4u32 | (0x2000 << 4) | (1 << 24)).to_le_bytes());
        put(&mut out, h + 12, &4i32.to_le_bytes());
        put(&mut out, h + 16, &tags.to_le_bytes());
        for k in 0..3 {
            put(&mut out, h + 32 + 4 * k, &(rad[k] as f32).to_le_bytes());
            put(&mut out, h + 48 + 4 * k, &(ctr[k] as f32).to_le_bytes());
        }
    }
    put(&mut out, poly, &(2u32 | ((quads.len() as u32) << 24)).to_le_bytes());
    put(&mut out, poly + 16, &tags.to_le_bytes());
    put(&mut out, poly + 28, &(((vh - poly) / 16) as u32).to_le_bytes());
    let info: u16 = if material.is_some() { 2 | 0xFE00 } else { 0xFFFF };
    for (k, q) in quads.iter().enumerate() {
        let at = poly + 32 + 10 * k;
        for (index, vertex_index) in q.iter().enumerate() {
            put(&mut out, at + 2 * index, &vertex_index.to_le_bytes());
        }
        put(&mut out, at + 8, &info.to_le_bytes());
    }
    put(&mut out, vh, &((vertices.len() + 1) as u32).to_le_bytes());
    put(&mut out, vh + 4, &1u32.to_le_bytes());
    put(&mut out, vh + 8, &1u32.to_le_bytes());
    put(&mut out, vh + 16, &32i64.to_le_bytes());
    for (k, v) in vertices.iter().enumerate() {
        let at = vdata + 16 * (k + 1);
        for (index, coordinate) in v.iter().enumerate() {
            put(&mut out, at + 4 * index, &(*coordinate as f32).to_le_bytes());
        }
    }
    out
}

/// A group for `build`: (tags, material, vertices, quads)
pub type GroupSpec = (u64, Option<u32>, Vec<[f64; 3]>, Vec<[i16; 4]>);

/// build(root_hash, groups, pad): group 0 under the root node (flags 0x10), every further group under its own
/// flags-0x40 root-level sibling (the layout of vanilla two-group geoms)
pub fn build(root_hash: u32, groups: &[GroupSpec], pad: [u8; 8]) -> Vec<u8> {
    let payloads: Vec<Vec<u8>> = groups.iter().map(|g| group_payload(g.0, g.1, &g.2, &g.3)).collect();
    let root = 0x20usize;
    let mut o = root + 0x30;
    let mut pos = vec![(o, o + 0x30)];
    o = align16(o + 0x30 + payloads[0].len());
    let mut sibs = Vec::new();
    for p in payloads.iter().skip(1) {
        let sb = o;
        sibs.push(sb);
        pos.push((sb + 0x30, sb + 0x60));
        o = align16(sb + 0x60 + p.len());
    }
    let size = o;
    let mut out = vec![0u8; size];
    let put = |o: &mut Vec<u8>, at: usize, v: u32| o[at..at + 4].copy_from_slice(&v.to_le_bytes());
    put(&mut out, 0, GEO_VERSION);
    put(&mut out, 4, 0x20);
    put(&mut out, 8, size as u32);
    put(&mut out, 12, GEOM_NAME);
    out[24..32].copy_from_slice(&pad);
    let rel = |at: usize, b: usize| if b != 0 { (b as i64 - at as i64) as i32 as u32 } else { 0 };
    let node = |o: &mut Vec<u8>, at: usize, h: u32, flags: u32, data: usize, dsize: usize, child: usize, prev: usize,
                nxt: usize| {
        put(o, at, h);
        put(o, at + 8, flags);
        put(o, at + 12, rel(at, data));
        put(o, at + 16, if data != 0 { dsize as u32 } else { 0 });
        put(o, at + 24, rel(at, child));
        put(o, at + 28, rel(at, prev));
        put(o, at + 32, rel(at, nxt));
    };
    node(&mut out, root, root_hash, 0x10, 0, 0, pos[0].0, 0, sibs.first().copied().unwrap_or(0));
    for (i, &(gn, pp)) in pos.iter().enumerate() {
        node(&mut out, gn, GROUP_NODE, 6, pp, payloads[i].len(), 0, 0, 0);
        out[pp..pp + payloads[i].len()].copy_from_slice(&payloads[i]);
    }
    for (k, &sb) in sibs.iter().enumerate() {
        let prev = if k == 0 { root } else { sibs[k - 1] };
        node(&mut out, sb, SIBLING_NODE, 0x40, 0, 0, pos[k + 1].0, prev, sibs.get(k + 1).copied().unwrap_or(0));
    }
    out
}

fn tag_offsets(groups: &[(u32, Group)]) -> Vec<usize> {
    groups.iter().flat_map(|(_, g)| g.blocks.iter())
        .flat_map(|b| std::iter::once(b.offset + 24).chain(b.shapes.iter().map(|s| s.offset + 16))).collect()
}

/// without_bits: a copy with `bits` cleared in every block's and shape's collision tags -> (bytes, count changed)
pub fn without_bits(buf: &[u8], bits: u64) -> Result<(Vec<u8>, usize), String> {
    let mut out = buf.to_vec();
    let mut n = 0;
    for o in tag_offsets(&parse(buf)?) {
        let t = u64at(&out, o)?;
        if t & bits != 0 {
            out[o..o + 8].copy_from_slice(&(t & !bits).to_le_bytes());
            n += 1;
        }
    }
    let left = parse(&out)?.iter().flat_map(|(_, g)| g.blocks.iter())
        .flat_map(|b| std::iter::once(b.tags).chain(b.shapes.iter().map(|s| s.tags))).filter(|t| t & bits != 0).count();
    if left != 0 {
        return Err(format!("{left} tag sets still carry the bits"));
    }
    Ok((out, n))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MechFree {
    /// no Sahelanthropus bit anywhere: use the geom as it is
    None,
    Parsed,
    Scanned,
    /// layout not decoded and no known tag value found
    Unknown,
}

/// mech_free: the geom with the Sahelanthropus bit cleared where it can be found
pub fn mech_free(buf: &[u8]) -> Result<(MechFree, Option<Vec<u8>>), String> {
    let groups = parse(buf)?;
    let parsed: Vec<u64> = groups.iter().flat_map(|(_, g)| g.blocks.iter())
        .flat_map(|b| std::iter::once(b.tags).chain(b.shapes.iter().map(|s| s.tags))).collect();
    let words = |b: &[u8]| -> Vec<(usize, u64)> {
        (0..b.len().saturating_sub(7)).step_by(8).map(|o| (o, u64::from_le_bytes(b[o..o + 8].try_into().unwrap()))).collect()
    };
    let hits: Vec<usize> = words(buf).into_iter().filter(|(_, v)| SCAN_TAGS.contains(v)).map(|(o, _)| o).collect();
    let masked: Vec<u64> = SCAN_TAGS.iter().map(|t| t & !SAHELAN_BIT).collect();
    let other = words(buf).iter().any(|(_, v)| masked.contains(&(v & !SAHELAN_BIT)));
    if parsed.iter().any(|t| t & SAHELAN_BIT != 0) {
        let (mut out, _) = without_bits(buf, SAHELAN_BIT)?;
        let rest: Vec<usize> = words(&out).into_iter().filter(|(_, v)| SCAN_TAGS.contains(v)).map(|(o, _)| o).collect();
        for o in rest {
            let v = u64::from_le_bytes(out[o..o + 8].try_into().unwrap()) & !SAHELAN_BIT;
            out[o..o + 8].copy_from_slice(&v.to_le_bytes());
        }
        return Ok((MechFree::Parsed, Some(out)));
    }
    if !hits.is_empty() {
        let mut out = buf.to_vec();
        for o in hits {
            let v = u64::from_le_bytes(out[o..o + 8].try_into().unwrap()) & !SAHELAN_BIT;
            out[o..o + 8].copy_from_slice(&v.to_le_bytes());
        }
        return Ok((MechFree::Scanned, Some(out)));
    }
    if !parsed.is_empty() || other {
        return Ok((MechFree::None, None));
    }
    Ok((MechFree::Unknown, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn build_parse() {
        let v = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 2.0, 0.0], [0.0, 2.0, 0.0]];
        let q = vec![[0i16, 1, 2, 3]];
        let b = build(0x1234, &[(0x406100008000003C, Some(0xF9F11C4C), v.clone(), q.clone()), (0x2DC2, None, v, q)], [0; 8]);
        assert_eq!(crate::foxdata::write(&crate::foxdata::read(&b).unwrap()), b);
        let g = parse(&b).unwrap();
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].1.materials, vec![0xF9F11C4C]);
        assert_eq!(g[0].1.blocks[0].vertices.len(), 5);
        assert_eq!(g[0].1.blocks[0].shapes.len(), 3);
        let (nb, n) = without_bits(&b, SAHELAN_BIT).unwrap();
        assert_eq!(n, 4); // block + 3 shapes of group 0
        assert_eq!(mech_free(&nb).unwrap().0, MechFree::None);
        assert_eq!(mech_free(&b).unwrap().0, MechFree::Parsed);
    }
}
