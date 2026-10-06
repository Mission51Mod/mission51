//! Locator files (.lba): instance placements of gimmicks and other located objects. Read + write.
//!
//! Port of tools/location/flyk_dressing_probes.py `lba_type2` / `lba_parse` (wrapped by tools/breakables/lba.py),
//! which handle type 2; this module also covers the two other layouts found in the vanilla corpus (types 0 and 3).
//! Notes: docs/formats/lba.md and docs/formats/breakables.md 2.7.
//!
//!   header  u32 count, u32 type, u32 0, u32 0
//!   records count x (structure of arrays, all records first):
//!           type 0 / 2: f32 pos[3], u32 (uninitialised in vanilla), f32 quat xyzw                         (32 B)
//!           type 3:     f32 pos[3], u32 (uninitialised), f32 quat xyzw, f32 scale[3], u32 (uninitialised) (48 B)
//!   hashes  types 2 and 3 only, count x {u32 StrCode32(locator name), u32 PathCode64(fox2 path) & 0xffffffff}
use crate::hash::{pathcode64, strcode32};

#[derive(Clone, Debug, PartialEq)]
pub struct Locator {
    pub pos: [f32; 3],
    /// the u32 after pos (vanilla leaves it uninitialised; the writer keeps whatever is given, 0 for new files)
    pub junk: u32,
    pub quat: [f32; 4],
    /// type 3 only: scale and the u32 after it
    pub scale: Option<([f32; 3], u32)>,
    /// types 2 and 3: (StrCode32(name), PathCode64(fox2) & 0xffffffff)
    pub hashes: Option<(u32, u32)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lba {
    pub typ: u32,
    pub locators: Vec<Locator>,
}

fn rec_size(typ: u32) -> Option<usize> {
    match typ {
        0 | 2 => Some(32),
        3 => Some(48),
        _ => None,
    }
}

fn has_hashes(typ: u32) -> bool {
    typ == 2 || typ == 3
}

fn f(b: &[u8], i: usize) -> f32 {
    f32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}
fn u(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}

pub fn read(b: &[u8]) -> Result<Lba, String> {
    if b.len() < 16 {
        return Err("lba: truncated header".into());
    }
    let (n, typ) = (u(b, 0) as usize, u(b, 4));
    if u(b, 8) != 0 || u(b, 12) != 0 {
        return Err("lba: non-zero header padding".into());
    }
    let rs = rec_size(typ).ok_or_else(|| format!("lba: unknown type {typ}"))?;
    let want = 16 + n * (rs + if has_hashes(typ) { 8 } else { 0 });
    if b.len() != want {
        return Err(format!("lba: {} bytes, expected {want} for {n} type-{typ} locators", b.len()));
    }
    let locators = (0..n).map(|i| {
        let o = 16 + rs * i;
        let h = 16 + rs * n + 8 * i;
        Locator {
            pos: [f(b, o), f(b, o + 4), f(b, o + 8)],
            junk: u(b, o + 12),
            quat: [f(b, o + 16), f(b, o + 20), f(b, o + 24), f(b, o + 28)],
            scale: (typ == 3).then(|| ([f(b, o + 32), f(b, o + 36), f(b, o + 40)], u(b, o + 44))),
            hashes: has_hashes(typ).then(|| (u(b, h), u(b, h + 4))),
        }
    }).collect();
    Ok(Lba { typ, locators })
}

pub fn write(l: &Lba) -> Result<Vec<u8>, String> {
    rec_size(l.typ).ok_or_else(|| format!("lba: unknown type {}", l.typ))?;
    let mut o = Vec::new();
    for v in [l.locators.len() as u32, l.typ, 0, 0] {
        o.extend_from_slice(&v.to_le_bytes());
    }
    for x in &l.locators {
        for v in x.pos {
            o.extend_from_slice(&v.to_le_bytes());
        }
        o.extend_from_slice(&x.junk.to_le_bytes());
        for v in x.quat {
            o.extend_from_slice(&v.to_le_bytes());
        }
        if l.typ == 3 {
            let (s, j) = x.scale.ok_or("lba: type 3 locator without scale")?;
            for v in s {
                o.extend_from_slice(&v.to_le_bytes());
            }
            o.extend_from_slice(&j.to_le_bytes());
        }
    }
    if has_hashes(l.typ) {
        for x in &l.locators {
            let (a, b) = x.hashes.ok_or("lba: locator without name/path hashes")?;
            o.extend_from_slice(&a.to_le_bytes());
            o.extend_from_slice(&b.to_le_bytes());
        }
    }
    Ok(o)
}

/// lba_type2(locs): [(pos, quat, locator name, fox2 path)] -> type-2 .lba bytes (junk 0)
pub fn type2(locs: &[([f32; 3], [f32; 4], &str, &str)]) -> Vec<u8> {
    let l = Lba {
        typ: 2,
        locators: locs.iter().map(|&(pos, quat, name, fox2)| Locator {
            pos,
            junk: 0,
            quat,
            scale: None,
            hashes: Some((strcode32(name.as_bytes()), (pathcode64(fox2, None) & 0xFFFF_FFFF) as u32)),
        }).collect(),
    };
    write(&l).unwrap()
}

/// lba.locator_name(model, i): "<model>_gim_nNNNN|srt_<model>"
pub fn locator_name(model: &str, i: usize) -> String {
    format!("{model}_gim_n{i:04}|srt_{model}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_types() {
        let loc = |s: Option<([f32; 3], u32)>, h| Locator { pos: [1.0, 2.0, 3.0], junk: 0x2E, quat: [0.0, 0.5, 0.0, 0.75],
                                                            scale: s, hashes: h };
        for l in [Lba { typ: 0, locators: vec![loc(None, None); 2] },
                  Lba { typ: 2, locators: vec![loc(None, Some((1, 2))), loc(None, Some((3, 4)))] },
                  Lba { typ: 3, locators: vec![loc(Some(([1.0; 3], 7)), Some((5, 6)))] }] {
            let b = write(&l).unwrap();
            assert_eq!(read(&b).unwrap(), l);
        }
        let b = type2(&[([0.0; 3], [0.0, 0.0, 0.0, 1.0], &locator_name("m", 3), "/Assets/x.fox2")]);
        assert_eq!(b.len(), 16 + 40);
        assert_eq!(locator_name("afgh_x", 3), "afgh_x_gim_n0003|srt_afgh_x");
    }
}
