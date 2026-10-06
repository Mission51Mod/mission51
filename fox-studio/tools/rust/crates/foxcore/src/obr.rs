//! Object brush files: .obr (ObjectBrush, FoxData v3) and .obrb (ObjectBrushBlock, FoxData v2). Read + write.
//!
//! Port of tools/location/obr.py (the reference; byte-identical to every vanilla file). Layout notes:
//! docs/formats/objbrush.md section 2. Credit: kapuragu/FoxEngineTemplates obr.bt / obrb.bt / gr_common.bt.
//!
//!   FoxData header (0x20): u32 version, u32 nodes offset 0x20, u32 file size, {u32 StrCode32, i32 offset} name,
//!                          u32 0, 8 zero bytes.
//!   node (0x30): {StrCode32, i32 rel} name, u32 flags, i32 data offset, u32 data size, 4 x i32 links (0),
//!                i32 parameters offset, 8 zero bytes (offsets relative to the node).
//!   parameter (0x10): u16 type (0 uint, 2 float), i16 next (relative, 0 = last), {StrCode32, i32 rel} name, u32 raw.
//!   data: numObjects x 24-byte DataUnit.
//!   .obr : header name "ObjectBrush" at absolute 0x50, node name {0,0}, flags 1, params at 0x60, names 16-aligned,
//!          data 16-aligned after them, file padded to 16.
//!   .obrb: header name "ObjectBrushBlock" (offset relative to its FoxDataString), node name "" (relative), flags 0,
//!          params at 0x50, data at 0x80, then a string table starting 16-aligned (header, node, param names, each
//!          4-aligned), file padded to 4.
use crate::hash::strcode32;

pub const PARAM_UINT: u16 = 0;
pub const PARAM_FLOAT: u16 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Obr,
    Obrb,
}

/// One 24-byte DataUnit (fields as stored; the quaternion stays raw f16 bits).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct DataUnit {
    pub y: f32,
    pub x: i16,
    pub z: i16,
    pub q: [u16; 4],
    pub block: u16,
    pub plugin: u8,
    pub scale: u8,
    pub gid: u32,
}

impl DataUnit {
    pub fn from_bytes(b: &[u8]) -> DataUnit {
        let u16a = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
        DataUnit {
            y: f32::from_le_bytes(b[0..4].try_into().unwrap()),
            x: i16::from_le_bytes([b[4], b[5]]),
            z: i16::from_le_bytes([b[6], b[7]]),
            q: [u16a(8), u16a(10), u16a(12), u16a(14)],
            block: u16a(16),
            plugin: b[18],
            scale: b[19],
            gid: u32::from_le_bytes(b[20..24].try_into().unwrap()),
        }
    }
    pub fn to_bytes(&self) -> [u8; 24] {
        let mut o = [0u8; 24];
        o[0..4].copy_from_slice(&self.y.to_le_bytes());
        o[4..6].copy_from_slice(&self.x.to_le_bytes());
        o[6..8].copy_from_slice(&self.z.to_le_bytes());
        for k in 0..4 {
            o[8 + 2 * k..10 + 2 * k].copy_from_slice(&self.q[k].to_le_bytes());
        }
        o[16..18].copy_from_slice(&self.block.to_le_bytes());
        o[18] = self.plugin;
        o[19] = self.scale;
        o[20..24].copy_from_slice(&self.gid.to_le_bytes());
        o
    }
}

/// A parameter: (name, type, raw u32), in file order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub typ: u16,
    pub raw: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ObjectBrushFile {
    pub kind: Kind,
    pub params: Vec<Param>,
    pub objects: Vec<DataUnit>,
}

impl ObjectBrushFile {
    pub fn param_raw(&self, name: &str) -> Option<u32> {
        self.params.iter().find(|p| p.name == name).map(|p| p.raw)
    }
    /// value as f64 (float params reinterpret the raw bits)
    pub fn param(&self, name: &str) -> Option<f64> {
        self.params.iter().find(|p| p.name == name)
            .map(|p| if p.typ == PARAM_FLOAT { f32::from_bits(p.raw) as f64 } else { p.raw as f64 })
    }
}

pub fn new_obr(num_blocks_w: u32, num_blocks_h: u32, block_size: f32, objects: Vec<DataUnit>) -> ObjectBrushFile {
    let p = |n: &str, t, raw| Param { name: n.into(), typ: t, raw };
    ObjectBrushFile {
        kind: Kind::Obr,
        params: vec![p("blockSizeW", PARAM_FLOAT, block_size.to_bits()), p("blockSizeH", PARAM_FLOAT, block_size.to_bits()),
                     p("numBlocksW", PARAM_UINT, num_blocks_w), p("numBlocksH", PARAM_UINT, num_blocks_h),
                     p("numObjects", PARAM_UINT, 0)],
        objects,
    }
}

pub fn new_obrb(block_id: u32, objects: Vec<DataUnit>) -> ObjectBrushFile {
    let p = |n: &str, raw| Param { name: n.into(), typ: PARAM_UINT, raw };
    ObjectBrushFile { kind: Kind::Obrb, params: vec![p("blockId", block_id), p("numObjects", 0), p("flags", 1)], objects }
}

fn rd<const N: usize>(b: &[u8], i: usize) -> Result<[u8; N], String> {
    b.get(i..i + N).map(|s| s.try_into().unwrap()).ok_or_else(|| format!("obr: truncated at {i:#x}"))
}
fn u32at(b: &[u8], i: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(rd(b, i)?))
}
fn i32at(b: &[u8], i: usize) -> Result<i32, String> {
    Ok(i32::from_le_bytes(rd(b, i)?))
}
fn cstr(b: &[u8], off: usize) -> Result<String, String> {
    let s = b.get(off..).ok_or("obr: string out of range")?;
    let n = s.iter().position(|&c| c == 0).ok_or("obr: unterminated string")?;
    Ok(s[..n].iter().map(|&c| c as char).collect())
}
fn latin1(s: &str) -> Vec<u8> {
    s.chars().map(|c| c as u32 as u8).collect()
}

pub fn read(b: &[u8]) -> Result<ObjectBrushFile, String> {
    let version = u32at(b, 0)?;
    let nodes = u32at(b, 4)?;
    if !(version == 2 || version == 3) || nodes != 0x20 {
        return Err(format!("not an obr/obrb (version {version}, nodes at {nodes:#x})"));
    }
    let kind = if version == 3 { Kind::Obr } else { Kind::Obrb };
    let n = 0x20usize;
    let doff = i32at(b, n + 12)?;
    let dsize = u32at(b, n + 16)? as usize;
    for k in 0..4 {
        if i32at(b, n + 20 + 4 * k)? != 0 {
            return Err("obr: unexpected node links".into());
        }
    }
    let prm = i32at(b, n + 36)?;
    let mut params = Vec::new();
    let mut po = (n as i64 + prm as i64) as usize;
    loop {
        let typ = u16::from_le_bytes(rd(b, po)?);
        let pnext = i16::from_le_bytes(rd(b, po + 2)?);
        let h = u32at(b, po + 4)?;
        let rel = i32at(b, po + 8)?;
        let name = cstr(b, (po as i64 + 4 + rel as i64) as usize)?;
        if strcode32(&latin1(&name)) != h {
            return Err(format!("obr: param name hash mismatch {name}"));
        }
        params.push(Param { name, typ, raw: u32at(b, po + 12)? });
        if pnext == 0 {
            break;
        }
        po = (po as i64 + pnext as i64) as usize;
    }
    let mut o = ObjectBrushFile { kind, params, objects: Vec::new() };
    let nobj = o.param_raw("numObjects").ok_or("obr: no numObjects")? as usize;
    if dsize != 24 * nobj {
        return Err(format!("obr: data size {dsize} != 24 * {nobj}"));
    }
    let at = (n as i64 + doff as i64) as usize;
    let data = b.get(at..at + dsize).ok_or("obr: data out of range")?;
    o.objects = data.chunks_exact(24).map(DataUnit::from_bytes).collect();
    Ok(o)
}

fn align(v: usize, a: usize) -> usize {
    v.div_ceil(a) * a
}

pub fn write(o: &ObjectBrushFile) -> Vec<u8> {
    let params: Vec<Param> = o.params.iter().map(|p| Param {
        raw: if p.name == "numObjects" { o.objects.len() as u32 } else { p.raw },
        ..p.clone()
    }).collect();
    let np = params.len();
    let dlen = 24 * o.objects.len();
    let (hdr_name, hdr_str, params_at, strs, data_at, end, node_name_at, node_flags);
    match o.kind {
        Kind::Obr => {
            hdr_name = "ObjectBrush";
            hdr_str = 0x50usize;
            params_at = align(hdr_str + hdr_name.len() + 1, 16);
            let mut s = params_at + 0x10 * np;
            let mut v = Vec::with_capacity(np);
            for p in &params {
                s = align(s, 16);
                v.push(s);
                s += p.name.len() + 1;
            }
            strs = v;
            data_at = align(s, 16);
            end = align(data_at + dlen, 16);
            node_name_at = None;
            node_flags = 1u32;
        }
        Kind::Obrb => {
            hdr_name = "ObjectBrushBlock";
            params_at = 0x50;
            data_at = params_at + 0x10 * np;
            let mut s = align(data_at + dlen, 16);
            let mut pos = Vec::with_capacity(np + 2);
            let names: Vec<&str> = [hdr_name, ""].into_iter().chain(params.iter().map(|p| p.name.as_str())).collect();
            for nm in &names {
                s = align(s, 4);
                pos.push(s);
                s += nm.len() + 1;
            }
            hdr_str = pos[0];
            node_name_at = Some(pos[1]);
            strs = pos[2..].to_vec();
            end = align(s, 4);
            node_flags = 0;
        }
    }
    let mut b = vec![0u8; end];
    let put = |b: &mut Vec<u8>, at: usize, v: &[u8]| b[at..at + v.len()].copy_from_slice(v);
    put(&mut b, 0, &(if o.kind == Kind::Obr { 3u32 } else { 2 }).to_le_bytes());
    put(&mut b, 4, &0x20u32.to_le_bytes());
    put(&mut b, 8, &(end as u32).to_le_bytes());
    put(&mut b, 12, &strcode32(hdr_name.as_bytes()).to_le_bytes());
    let hrel = if o.kind == Kind::Obr { hdr_str as i32 } else { hdr_str as i32 - 12 };
    put(&mut b, 16, &hrel.to_le_bytes());
    put(&mut b, hdr_str, hdr_name.as_bytes());
    let n = 0x20usize;
    if let Some(at) = node_name_at {
        put(&mut b, n, &strcode32(b"").to_le_bytes());
        put(&mut b, n + 4, &((at - n) as i32).to_le_bytes());
    }
    put(&mut b, n + 8, &node_flags.to_le_bytes());
    put(&mut b, n + 12, &((data_at - n) as i32).to_le_bytes());
    put(&mut b, n + 16, &(dlen as u32).to_le_bytes());
    put(&mut b, n + 36, &((params_at - n) as i32).to_le_bytes());
    for (i, p) in params.iter().enumerate() {
        let po = params_at + 0x10 * i;
        put(&mut b, po, &p.typ.to_le_bytes());
        put(&mut b, po + 2, &(if i + 1 < np { 0x10i16 } else { 0 }).to_le_bytes());
        let nb = latin1(&p.name);
        put(&mut b, po + 4, &strcode32(&nb).to_le_bytes());
        put(&mut b, po + 8, &((strs[i] as i64 - (po as i64 + 4)) as i32).to_le_bytes());
        put(&mut b, po + 12, &p.raw.to_le_bytes());
        put(&mut b, strs[i], &nb);
    }
    for (k, u) in o.objects.iter().enumerate() {
        put(&mut b, data_at + 24 * k, &u.to_bytes());
    }
    b
}

/// metres from the block centre -> i16 units (x 255, round half to even, clamped), as obr.encode_local
pub fn encode_local(v: f64) -> i16 {
    let r = (v * 255.0).round_ties_even();
    r.clamp(-32768.0, 32767.0) as i16
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn synthetic_roundtrip() {
        let u = DataUnit { y: 12.5, x: -300, z: 77, q: [0, 0x3800, 0, 0x3BFF], block: 5, plugin: 3, scale: 200, gid: 9 };
        for o in [new_obr(64, 64, 128.0, vec![]), new_obr(32, 32, 128.0, vec![u; 3]), new_obrb(2078, vec![u]),
                  new_obrb(1, vec![u; 2])] {
            let b = write(&o);
            let r = read(&b).unwrap();
            assert_eq!(r.objects, o.objects);
            assert_eq!(write(&r), b);
        }
        let b = write(&new_obrb(1, vec![u]));
        assert_eq!(b.len() % 4, 0);
        assert_eq!(&b[0..4], &2u32.to_le_bytes());
    }
}
