//! Fracture descriptions (.fdes, "FDES0100"). Read + write.
//!
//! Port of tools/breakables/fdes.py parse / write (byte-identical on every vanilla file). Spec:
//! docs/formats/breakables.md 2.1; summary in docs/formats/fdes.md.
//!
//!   header (0x20) "FDES0100", u32 file size, u32 0, u32 0, u32 roots offset (0 if none), u32 root count,
//!                 u32 string table offset
//!   root  (0xA0) f32 A[16] (body / shape frame, row-major, translation in row 4), f32 B[16] (fragment mesh
//!                placement), u32 name (string index), u32 unk (0 in vanilla), u32 shapes offset, u32 shape count
//!                (offsets relative to the record), u32 children offset, u32 child count, 8 zero bytes
//!   child (0x90) the same without the children fields
//!   shape (0x50) f32 c[4] (centre), q[4] (quaternion xyzw), h[4] (size), u32 type (1 sphere, 2 box, 3 cylinder,
//!                4 capsule, 5 polyhedron), u32 vertices offset, u32 vertex count, u32 quads offset, u32 quad count
//!                (relative to the shape)
//!   then all polyhedron vertices (f32[4]), all quads (u32[4]), the string table: u32 n, n x u32 offset (from the
//!   table start), records {u32 hash, u32 length, bytes, NUL}.
//! Write order (as vanilla): header | roots | children (all roots' in order) | shapes (per root: the root's, then its
//! children's) | vertices | quads | strings. Shapes without hull data point at the start of the vertex / quad regions
//! with count 0.

pub const MAGIC: &[u8; 8] = b"FDES0100";

#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub c: [f32; 4],
    pub q: [f32; 4],
    pub h: [f32; 4],
    pub typ: u32,
    pub verts: Vec<[f32; 4]>,
    pub quads: Vec<[u32; 4]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub a: [f32; 16],
    pub b: [f32; 16],
    pub name: u32,
    pub unk: u32,
    pub shapes: Vec<Shape>,
    /// roots only
    pub children: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fdes {
    pub roots: Vec<Node>,
    /// (hash, text)
    pub strings: Vec<(u32, String)>,
}

fn u(d: &[u8], i: usize) -> Result<u32, String> {
    d.get(i..i + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("fdes: truncated at {i:#x}"))
}
fn fv<const N: usize>(d: &[u8], i: usize) -> Result<[f32; N], String> {
    let s = d.get(i..i + 4 * N).ok_or_else(|| format!("fdes: truncated at {i:#x}"))?;
    Ok(std::array::from_fn(|k| f32::from_le_bytes(s[4 * k..4 * k + 4].try_into().unwrap())))
}

pub fn parse(d: &[u8]) -> Result<Fdes, String> {
    if d.get(0..8) != Some(MAGIC) {
        return Err("not FDES0100".into());
    }
    let root = u(d, 0x14)? as usize;
    let nroot = u(d, 0x18)? as usize;
    let stab = u(d, 0x1C)? as usize;
    let n = u(d, stab)? as usize;
    let mut strings = Vec::with_capacity(n);
    for i in 0..n {
        let so = stab + u(d, stab + 4 + 4 * i)? as usize;
        let (h, ln) = (u(d, so)?, u(d, so + 4)? as usize);
        let s = d.get(so + 8..so + 8 + ln).ok_or("fdes: string out of range")?;
        strings.push((h, s.iter().map(|&c| c as char).collect()));
    }
    fn node(d: &[u8], o: usize, has_children: bool) -> Result<Node, String> {
        let (soff, scnt) = (u(d, o + 0x88)? as usize, u(d, o + 0x8C)? as usize);
        let mut shapes = Vec::with_capacity(scnt);
        for i in 0..scnt {
            let p = o + soff + 0x50 * i;
            let (va, vc, qa, qc) = (u(d, p + 52)? as usize, u(d, p + 56)? as usize, u(d, p + 60)? as usize, u(d, p + 64)? as usize);
            let verts = (0..vc).map(|k| fv::<4>(d, p + va + 16 * k)).collect::<Result<_, _>>()?;
            let quads = (0..qc).map(|k| Ok([u(d, p + qa + 16 * k)?, u(d, p + qa + 16 * k + 4)?, u(d, p + qa + 16 * k + 8)?,
                                             u(d, p + qa + 16 * k + 12)?])).collect::<Result<_, String>>()?;
            shapes.push(Shape { c: fv(d, p)?, q: fv(d, p + 16)?, h: fv(d, p + 32)?, typ: u(d, p + 48)?, verts, quads });
        }
        let mut children = Vec::new();
        if has_children {
            let (co, cc) = (u(d, o + 0x90)? as usize, u(d, o + 0x94)? as usize);
            for k in 0..cc {
                children.push(node(d, o + co + 0x90 * k, false)?);
            }
        }
        Ok(Node { a: fv(d, o)?, b: fv(d, o + 64)?, name: u(d, o + 0x80)?, unk: u(d, o + 0x84)?, shapes, children })
    }
    let mut roots = Vec::with_capacity(nroot);
    if root != 0 {
        for r in 0..nroot {
            roots.push(node(d, root + 0xA0 * r, true)?);
        }
    }
    Ok(Fdes { roots, strings })
}

pub fn write(m: &Fdes) -> Result<Vec<u8>, String> {
    let nroot = m.roots.len();
    let root_off = 0x20usize;
    let child_off = root_off + 0xA0 * nroot;
    let nchild: usize = m.roots.iter().map(|r| r.children.len()).sum();
    let shape_off = child_off + 0x90 * nchild;
    // (node, record offset, is root) in record order; shape order: per root the root, then its children
    let mut child_pos = Vec::with_capacity(nroot);
    let mut co = child_off;
    for r in &m.roots {
        child_pos.push(co);
        co += 0x90 * r.children.len();
    }
    let mut shape_nodes: Vec<(&Node, usize, bool, usize)> = Vec::new(); // node, record offset, is root, root index
    for (i, r) in m.roots.iter().enumerate() {
        shape_nodes.push((r, root_off + 0xA0 * i, true, i));
        for (k, c) in r.children.iter().enumerate() {
            shape_nodes.push((c, child_pos[i] + 0x90 * k, false, i));
        }
    }
    if shape_nodes.iter().any(|(n, ..)| n.shapes.is_empty()) {
        return Err("fdes: every element needs at least one shape (the loader fails on 0)".into());
    }
    let nshape: usize = shape_nodes.iter().map(|(n, ..)| n.shapes.len()).sum();
    let vert_off = shape_off + 0x50 * nshape;
    let nverts: usize = shape_nodes.iter().flat_map(|(n, ..)| &n.shapes).map(|s| s.verts.len()).sum();
    let quad_off = vert_off + 16 * nverts;
    let nquads: usize = shape_nodes.iter().flat_map(|(n, ..)| &n.shapes).map(|s| s.quads.len()).sum();
    let stab = quad_off + 16 * nquads;
    let mut st = Vec::new();
    st.extend_from_slice(&(m.strings.len() as u32).to_le_bytes());
    let base = 4 + 4 * m.strings.len();
    let mut recs = Vec::new();
    for (h, s) in &m.strings {
        st.extend_from_slice(&((base + recs.len()) as u32).to_le_bytes());
        let b: Vec<u8> = s.chars().map(|c| c as u32 as u8).collect();
        recs.extend_from_slice(&h.to_le_bytes());
        recs.extend_from_slice(&(b.len() as u32).to_le_bytes());
        recs.extend_from_slice(&b);
        recs.push(0);
    }
    st.extend_from_slice(&recs);
    let total = stab + st.len();
    let mut out = vec![0u8; total];
    let put = |o: &mut Vec<u8>, at: usize, v: &[u8]| o[at..at + v.len()].copy_from_slice(v);
    let putf = |o: &mut Vec<u8>, at: usize, v: &[f32]| {
        for (k, x) in v.iter().enumerate() {
            o[at + 4 * k..at + 4 * k + 4].copy_from_slice(&x.to_le_bytes());
        }
    };
    let putu = |o: &mut Vec<u8>, at: usize, v: &[u32]| {
        for (k, x) in v.iter().enumerate() {
            o[at + 4 * k..at + 4 * k + 4].copy_from_slice(&x.to_le_bytes());
        }
    };
    put(&mut out, 0, MAGIC);
    putu(&mut out, 8, &[total as u32, 0, 0, if nroot > 0 { root_off as u32 } else { 0 }, nroot as u32, stab as u32]);
    let (mut sp, mut vp, mut qp) = (shape_off, vert_off, quad_off);
    for &(n, no, is_root, ri) in &shape_nodes {
        let first_shape = sp;
        for s in &n.shapes {
            putf(&mut out, sp, &s.c);
            putf(&mut out, sp + 16, &s.q);
            putf(&mut out, sp + 32, &s.h);
            if !s.verts.is_empty() || !s.quads.is_empty() {
                putu(&mut out, sp + 48, &[s.typ, (vp - sp) as u32, s.verts.len() as u32, (qp - sp) as u32, s.quads.len() as u32]);
            } else {
                putu(&mut out, sp + 48, &[s.typ, (vert_off - sp) as u32, 0, (quad_off - sp) as u32, 0]);
            }
            for v in &s.verts {
                putf(&mut out, vp, v);
                vp += 16;
            }
            for q in &s.quads {
                putu(&mut out, qp, q);
                qp += 16;
            }
            sp += 0x50;
        }
        putf(&mut out, no, &n.a);
        putf(&mut out, no + 64, &n.b);
        putu(&mut out, no + 0x80, &[n.name, n.unk, (first_shape - no) as u32, n.shapes.len() as u32]);
        if is_root {
            let c = if n.children.is_empty() { 0 } else { (child_pos[ri] - no) as u32 };
            putu(&mut out, no + 0x90, &[c, n.children.len() as u32, 0, 0]);
        }
    }
    put(&mut out, stab, &st);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_small() {
        let shape = |t, nv| Shape { c: [0.0, 1.0, 0.0, 1.0], q: [0.0, 0.0, 0.0, 1.0], h: [0.5, 0.5, 0.5, 1.0], typ: t,
                                    verts: vec![[1.0, 2.0, 3.0, 1.0]; nv], quads: vec![[0, 1, 2, 3]; nv / 2] };
        let mut ident = [0f32; 16];
        for k in 0..4 {
            ident[5 * k] = 1.0;
        }
        let child = Node { a: ident, b: ident, name: 2, unk: 0, shapes: vec![shape(5, 4)], children: vec![] };
        let root = Node { a: ident, b: ident, name: 1, unk: 0, shapes: vec![shape(2, 0)], children: vec![child.clone(), child] };
        let m = Fdes { roots: vec![root], strings: vec![(0, String::new()), (0, "root".into()), (0, "frag".into())] };
        let b = write(&m).unwrap();
        assert_eq!(parse(&b).unwrap(), m);
        assert_eq!(write(&parse(&b).unwrap()).unwrap(), b);
    }
}
