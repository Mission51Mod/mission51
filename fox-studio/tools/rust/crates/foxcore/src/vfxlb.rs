//! Effect locator arrays (.vfxlb, loaded by FxLocatorArrayData.vfxlbFile). Read + write.
//!
//! Port of tools/location/vfxlb.py (parse / build / encode_locator / decode_locator / vfx_code). Notes:
//! docs/formats/vfxlb.md.
//!
//!   "VFXLB", u8 version (1), u16 locator count, u16 group count, u32 0
//!   u32 end offset[locators + groups - 1]   (record i ends where record i + 1 starts; offsets from the data start)
//!   locator records, then group records (groups kept raw)
//!   locator: u64 vfx path code, u16 flags, u16 0, u32 0, [f32 scale x3 unless flags & 0x100],
//!            [f32 quat x4 unless flags & 0x200], f32 position x3, [u32 StrCode32(effect id) if flags & 0x10], other
//!            flag bits add more fields: such records are kept raw.
use crate::hash::{pathcode64, strcode32};

pub const VFX_EXT: u64 = 0x1982;
const MASK50: u64 = (1 << 50) - 1;

/// vfx_code(path): PathCode64 (50 bits) with the .vfx extension id in bits 50+
pub fn vfx_code(path: &str) -> u64 {
    (pathcode64(path, None) & MASK50) | (VFX_EXT << 50)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Vfxlb {
    pub version: u8,
    /// the header u32 as read (vfxlb.py's build writes 0)
    pub zero: u32,
    /// locator records, raw (decode with `decode_locator`)
    pub locators: Vec<Vec<u8>>,
    pub groups: Vec<Vec<u8>>,
}

/// A decoded locator (decode_locator). The optional fields are filled for the standard layouts only.
#[derive(Clone, Debug, PartialEq)]
pub struct Locator {
    pub code: u64,
    pub flags: u16,
    pub z1: u16,
    pub z2: u32,
    pub scale: Option<[f32; 3]>,
    pub rot: Option<[f32; 4]>,
    pub pos: Option<[f32; 3]>,
    pub id: Option<u32>,
    pub auto: bool,
}

fn fv<const N: usize>(r: &[u8], p: usize) -> Result<[f32; N], String> {
    let s = r.get(p..p + 4 * N).ok_or("vfxlb: record too short")?;
    Ok(std::array::from_fn(|k| f32::from_le_bytes(s[4 * k..4 * k + 4].try_into().unwrap())))
}

pub fn parse(b: &[u8]) -> Result<Vfxlb, String> {
    if b.get(0..5) != Some(b"VFXLB") || b.len() < 14 {
        return Err("not a vfxlb".into());
    }
    let version = b[5];
    let n = u16::from_le_bytes([b[6], b[7]]) as usize;
    let ng = u16::from_le_bytes([b[8], b[9]]) as usize;
    let zero = u32::from_le_bytes(b[10..14].try_into().unwrap());
    let nt = (n + ng).saturating_sub(1);
    let base = 14 + 4 * nt;
    let mut offs = Vec::with_capacity(nt);
    for i in 0..nt {
        let s = b.get(14 + 4 * i..18 + 4 * i).ok_or("vfxlb: truncated offsets")?;
        offs.push(u32::from_le_bytes(s.try_into().unwrap()) as usize);
    }
    let total = b.len().checked_sub(base).ok_or("vfxlb: truncated")?;
    let starts: Vec<usize> = std::iter::once(0).chain(offs.iter().cloned()).collect();
    let ends: Vec<usize> = offs.iter().cloned().chain(std::iter::once(total)).collect();
    let mut recs = Vec::with_capacity(starts.len());
    for (&s, &e) in starts.iter().zip(&ends) {
        recs.push(b.get(base + s..base + e).ok_or("vfxlb: record out of range")?.to_vec());
    }
    if n + ng == 0 {
        recs.clear();
    }
    let groups = recs.split_off(n.min(recs.len()));
    for r in &recs {
        decode_locator(r)?;
    }
    Ok(Vfxlb { version, zero, locators: recs, groups })
}

/// decode_locator: header fields always; scale / rot / pos / id for the standard flag layouts (vfxlb.py's list:
/// 0x230a 0x210a 0x220a 0x200a 0x2308 0x108 0x308, each optionally | 0x10). Other records stay undecoded, as in
/// the reference, including encode_locator's own script-started id layouts with rotation or scale (e.g. 0x2118).
pub fn decode_locator(r: &[u8]) -> Result<Locator, String> {
    if r.len() < 16 {
        return Err("vfxlb: locator record shorter than 16 bytes".into());
    }
    let mut l = Locator {
        code: u64::from_le_bytes(r[0..8].try_into().unwrap()),
        flags: u16::from_le_bytes([r[8], r[9]]),
        z1: u16::from_le_bytes([r[10], r[11]]),
        z2: u32::from_le_bytes(r[12..16].try_into().unwrap()),
        scale: None,
        rot: None,
        pos: None,
        id: None,
        auto: false,
    };
    let f = l.flags;
    if [0x230a, 0x210a, 0x220a, 0x200a, 0x2308, 0x108, 0x308].contains(&(f & !0x10)) {
        let mut p = 16;
        if f & 0x100 == 0 {
            l.scale = Some(fv(r, p)?);
            p += 12;
        }
        if f & 0x200 == 0 {
            l.rot = Some(fv(r, p)?);
            p += 16;
        }
        l.pos = Some(fv(r, p)?);
        p += 12;
        if f & 0x10 != 0 {
            let s = r.get(p..p + 4).ok_or("vfxlb: record too short")?;
            l.id = Some(u32::from_le_bytes(s.try_into().unwrap()));
            l.auto = f & 0x2 != 0;
            p += 4;
        }
        if p != r.len() {
            return Err(format!("vfxlb: record size {} does not match flags {f:#x}", r.len()));
        }
    }
    Ok(l)
}

/// An effect id for a new locator: a name (hashed with StrCode32) or the hash itself.
#[derive(Clone, Debug, PartialEq)]
pub enum EffectId {
    Name(String),
    Hash(u32),
}

/// A new standard locator (encode_locator's dict without 'raw').
#[derive(Clone, Debug, PartialEq)]
pub struct NewLocator {
    /// the vfx path code, or None to compute it from `vfx`
    pub code: Option<u64>,
    pub vfx: Option<String>,
    pub pos: [f32; 3],
    pub rot: Option<[f32; 4]>,
    pub scale: Option<[f32; 3]>,
    pub id: Option<EffectId>,
    /// with an id: start at load (keep flag 0x2) instead of waiting for CreateEffectFromId
    pub auto: bool,
}

/// encode_locator for a new locator (records read from a file are written back raw by `build`)
pub fn encode_locator(l: &NewLocator) -> Result<Vec<u8>, String> {
    let code = match (l.code, &l.vfx) {
        (Some(c), _) => c,
        (None, Some(v)) => vfx_code(v),
        (None, None) => return Err("vfxlb: locator needs a code or a vfx path".into()),
    };
    let mut flags: u16 = 0x200a;
    if l.scale.is_none() {
        flags |= 0x0100;
    }
    if l.rot.is_none() {
        flags |= 0x0200;
    }
    if l.id.is_some() {
        flags |= 0x0010;
        if !l.auto {
            flags &= !0x0002;
        }
    }
    let mut o = Vec::with_capacity(48);
    o.extend_from_slice(&code.to_le_bytes());
    o.extend_from_slice(&flags.to_le_bytes());
    o.extend_from_slice(&[0u8; 6]);
    for v in l.scale.iter().flatten().chain(l.rot.iter().flatten()).chain(l.pos.iter()) {
        o.extend_from_slice(&v.to_le_bytes());
    }
    if let Some(id) = &l.id {
        let h = match id {
            EffectId::Name(s) => strcode32(s.as_bytes()),
            EffectId::Hash(h) => *h,
        };
        o.extend_from_slice(&h.to_le_bytes());
    }
    Ok(o)
}

/// build(locators, groups, version): records back to back after the end-offset table; the header u32 is 0.
pub fn build(locators: &[Vec<u8>], groups: &[Vec<u8>], version: u8) -> Result<Vec<u8>, String> {
    let recs: Vec<&Vec<u8>> = locators.iter().chain(groups.iter()).collect();
    if recs.is_empty() {
        return Err("a vfxlb needs at least one locator".into());
    }
    let mut out = b"VFXLB".to_vec();
    out.push(version);
    out.extend_from_slice(&(locators.len() as u16).to_le_bytes());
    out.extend_from_slice(&(groups.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    let mut acc = 0usize;
    for r in &recs[..recs.len() - 1] {
        acc += r.len();
        out.extend_from_slice(&(acc as u32).to_le_bytes());
    }
    for r in recs {
        out.extend_from_slice(r);
    }
    Ok(out)
}

pub fn write(v: &Vfxlb) -> Result<Vec<u8>, String> {
    build(&v.locators, &v.groups, v.version)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn new_locators_roundtrip() {
        let a = NewLocator { code: None, vfx: Some("/Assets/tpp/fx/x.vfx".into()), pos: [1.0, 2.0, 3.0], rot: None,
                             scale: None, id: None, auto: false };
        // auto: flags 0x211a (0x2118, a script-started id with rotation, is outside vfxlb.py's decoded layouts)
        let b = NewLocator { rot: Some([0.0, 0.0, 0.0, 1.0]), id: Some(EffectId::Name("BrkFx".into())), auto: true, ..a.clone() };
        let recs = vec![encode_locator(&a).unwrap(), encode_locator(&b).unwrap()];
        let f = build(&recs, &[], 1).unwrap();
        let v = parse(&f).unwrap();
        assert_eq!(v.locators, recs);
        let l = decode_locator(&v.locators[1]).unwrap();
        assert_eq!(l.flags, 0x211a);
        assert_eq!(l.pos, Some([1.0, 2.0, 3.0]));
        assert_eq!(l.id, Some(strcode32(b"BrkFx")));
        assert_eq!(decode_locator(&v.locators[0]).unwrap().flags, 0x230a);
        assert_eq!(write(&v).unwrap(), f);
    }
}
