//! Light probe arrays (.grxla, TppLightProbeArray.lightArrayFile, "FGxL"). Read + write.
//!
//! Port of tools/location/flyk_look_probes.py parse_grxla / write_grxla (template patch). Notes:
//! docs/formats/grxla.md. Credit: kapuragu/FoxEngineTemplates grxla.bt (documentation).
//!
//!   "FGxL", u32 0, u32 data offset (16), u32 version (1); then entries {char[4] type, u32 entry size, payload}:
//!   CM00 (u64 StrCode64 (48 bits used) + u32 string offset from the field -> the owning fox2 path),
//!   EP00 (light probe: name hash + string offset, u32 flags, u16 shape type, half inner scale x6, transform
//!         scale / quat / translation, u32 inner-area offset (from the field), s16 priority, u16 related light count,
//!         u16 related lights start, u16 draw rejection levels | SH data index, f32 occlusion open rate, the name,
//!         the inner-area transform), and "\0\0\0\0" (end, size 8). Entries are kept raw; `ep00` decodes one.
use crate::hash::strcode64;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub typ: [u8; 4],
    /// the whole entry, including its 8-byte type + size header
    pub raw: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Grxla {
    pub zero: u32,
    pub data_offset: u32,
    pub version: u32,
    /// bytes between the 16-byte header and the data offset (none in vanilla)
    pub gap: Vec<u8>,
    pub entries: Vec<Entry>,
    /// bytes after the end entry (none in vanilla)
    pub tail: Vec<u8>,
}

fn u32at(b: &[u8], i: usize) -> Result<u32, String> {
    b.get(i..i + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("grxla: truncated at {i:#x}"))
}
fn f32s<const N: usize>(b: &[u8], i: usize) -> Result<[f32; N], String> {
    let s = b.get(i..i + 4 * N).ok_or_else(|| format!("grxla: truncated at {i:#x}"))?;
    Ok(std::array::from_fn(|k| f32::from_le_bytes(s[4 * k..4 * k + 4].try_into().unwrap())))
}
fn cstr(b: &[u8], off: usize) -> Result<String, String> {
    let s = b.get(off..).ok_or("grxla: string out of range")?;
    let n = s.iter().position(|&c| c == 0).ok_or("grxla: unterminated string")?;
    Ok(s[..n].iter().map(|&c| c as char).collect())
}

pub fn read(b: &[u8]) -> Result<Grxla, String> {
    if b.get(0..4) != Some(b"FGxL") {
        return Err("not FGxL".into());
    }
    let (zero, data_offset, version) = (u32at(b, 4)?, u32at(b, 8)?, u32at(b, 12)?);
    let mut p = data_offset as usize;
    let gap = b.get(16..p).ok_or("grxla: data offset out of range")?.to_vec();
    let mut entries = Vec::new();
    while p < b.len() {
        let typ: [u8; 4] = b[p..p + 4].try_into().map_err(|_| "grxla: truncated entry")?;
        let size = u32at(b, p + 4)? as usize;
        let end = if &typ == b"\0\0\0\0" { p + 8 } else { p + size }; // the end entry: 8 bytes
        if size < 8 && &typ != b"\0\0\0\0" {
            return Err(format!("grxla: entry size {size} at {p:#x}"));
        }
        entries.push(Entry { typ, raw: b.get(p..end).ok_or("grxla: entry out of range")?.to_vec() });
        p = end;
        if &typ == b"\0\0\0\0" {
            break;
        }
    }
    Ok(Grxla { zero, data_offset, version, gap, entries, tail: b[p.min(b.len())..].to_vec() })
}

pub fn write(g: &Grxla) -> Vec<u8> {
    let mut o = b"FGxL".to_vec();
    for v in [g.zero, g.data_offset, g.version] {
        o.extend_from_slice(&v.to_le_bytes());
    }
    o.extend_from_slice(&g.gap);
    for e in &g.entries {
        o.extend_from_slice(&e.raw);
    }
    o.extend_from_slice(&g.tail);
    o
}

/// CM00: (48-bit hash, the owning fox2 path)
pub fn cm00(e: &Entry) -> Result<(u64, Option<String>), String> {
    let r = &e.raw;
    let h = u64::from_le_bytes(r.get(8..16).ok_or("grxla: CM00 too short")?.try_into().unwrap()) & 0xFFFF_FFFF_FFFF;
    let so = u32at(r, 16)? as usize;
    Ok((h, if so != 0 { Some(cstr(r, 16 + so)?) } else { None }))
}

/// A decoded EP00 (light probe box).
#[derive(Clone, Debug, PartialEq)]
pub struct Ep00 {
    pub hash: u64,
    pub name: Option<String>,
    pub flags: u32,
    pub shape: u16,
    pub inner_scale: [u16; 6],
    pub scale: [f32; 3],
    pub quat: [f32; 4],
    pub translation: [f32; 3],
    pub inner_area_offset: u32,
    /// the inner-area transform: scale 3, quat 4, translation 3
    pub inner_area: Option<[f32; 10]>,
    pub priority: i16,
    pub related_count: u16,
    pub related_start: u16,
    pub drawrej_shdata: u16,
    pub occlusion_open_rate: f32,
}

pub fn ep00(e: &Entry) -> Result<Ep00, String> {
    let r = &e.raw;
    let q = 8;
    let h16 = |i: usize| -> Result<u16, String> { r.get(i..i + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or("grxla: EP00 too short".into()) };
    let so = u32at(r, q + 8)? as usize;
    let ia_off = u32at(r, q + 76)?;
    Ok(Ep00 {
        hash: u64::from_le_bytes(r.get(q..q + 8).ok_or("grxla: EP00 too short")?.try_into().unwrap()) & 0xFFFF_FFFF_FFFF,
        name: if so != 0 { Some(cstr(r, q + 8 + so)?) } else { None },
        flags: u32at(r, q + 16)?,
        shape: h16(q + 20)?,
        inner_scale: [h16(q + 24)?, h16(q + 26)?, h16(q + 28)?, h16(q + 30)?, h16(q + 32)?, h16(q + 34)?],
        scale: f32s(r, q + 36)?,
        quat: f32s(r, q + 48)?,
        translation: f32s(r, q + 64)?,
        inner_area_offset: ia_off,
        inner_area: if ia_off != 0 { Some(f32s(r, q + 76 + ia_off as usize)?) } else { None },
        priority: h16(q + 80)? as i16,
        related_count: h16(q + 82)?,
        related_start: h16(q + 84)?,
        drawrej_shdata: h16(q + 86)?,
        occlusion_open_rate: f32s::<1>(r, q + 88)?[0],
    })
}

/// write_grxla(template, fox2_path, centre, size_y, flags) with the box constants passed in (BOX xz, inner_xz,
/// inner_y_less in flyk_look_probes.py). The template holds CM00 + exactly one EP00 + the end entry; the fox2 path
/// must keep the template path's length. Values are computed in f64 and stored as f32, as struct.pack('<3f').
#[allow(clippy::too_many_arguments)]
pub fn write_grxla(template: &[u8], fox2_path: &str, centre: [f64; 3], size_y: f64, flags: Option<u32>,
                   box_xz: f64, inner_xz: f64, inner_y_less: f64) -> Result<Vec<u8>, String> {
    let g = read(template)?;
    let mut b = template.to_vec();
    let mut p = g.data_offset as usize;
    let (mut cm_at, mut ep_at) = (None, Vec::new());
    for e in &g.entries {
        match &e.typ {
            b"CM00" if cm_at.is_none() => cm_at = Some(p),
            b"EP00" => ep_at.push(p),
            _ => {}
        }
        p += e.raw.len();
    }
    let cm = cm_at.ok_or("template grxla has no CM00")?;
    if ep_at.len() != 1 {
        return Err(format!("template grxla has {} EP00", ep_at.len()));
    }
    let so = u32at(&b, cm + 16)? as usize;
    let s0 = cm + 16 + so;
    let old = cstr(&b, s0)?;
    let new: Vec<u8> = fox2_path.chars().map(|c| c as u32 as u8).collect();
    if new.len() != old.len() {
        return Err(format!("fox2 path length {} != template {}", new.len(), old.len()));
    }
    b[s0..s0 + new.len()].copy_from_slice(&new);
    let h = u64::from_le_bytes(b[cm + 8..cm + 16].try_into().unwrap());
    let h = (h & !0xFFFF_FFFF_FFFF) | (strcode64(&new) & 0xFFFF_FFFF_FFFF);
    b[cm + 8..cm + 16].copy_from_slice(&h.to_le_bytes());
    let q = ep_at[0] + 8;
    if let Some(f) = flags {
        b[q + 16..q + 20].copy_from_slice(&f.to_le_bytes());
    }
    let put3 = |b: &mut Vec<u8>, at: usize, v: [f64; 3]| {
        for k in 0..3 {
            b[at + 4 * k..at + 4 * k + 4].copy_from_slice(&(v[k] as f32).to_le_bytes());
        }
    };
    put3(&mut b, q + 36, [box_xz, size_y, box_xz]);
    put3(&mut b, q + 64, centre);
    let ia = q + 76 + u32at(&b, q + 76)? as usize;
    put3(&mut b, ia, [inner_xz, size_y - inner_y_less, inner_xz]);
    put3(&mut b, ia + 28, centre);
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_entries() {
        let mut b = b"FGxL".to_vec();
        for v in [0u32, 16, 1] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        let mut cm = b"CM00".to_vec();
        cm.extend_from_slice(&28u32.to_le_bytes());
        cm.extend_from_slice(&0u64.to_le_bytes());
        cm.extend_from_slice(&4u32.to_le_bytes()); // string at field + 4
        cm.extend_from_slice(b"/a.fox2\0");
        b.extend_from_slice(&cm);
        b.extend_from_slice(b"\0\0\0\0");
        b.extend_from_slice(&8u32.to_le_bytes());
        let g = read(&b).unwrap();
        assert_eq!(g.entries.len(), 2);
        assert_eq!(cm00(&g.entries[0]).unwrap().1.as_deref(), Some("/a.fox2"));
        assert_eq!(write(&g), b);
    }
}
