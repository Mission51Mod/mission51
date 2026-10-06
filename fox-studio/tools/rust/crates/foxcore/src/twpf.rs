//! Weather parameter files (.twpf, TppLocationData.weatherParametersFile). Read + write.
//!
//! tools/location/twpf.py parses these and edits key values in place; this module parses them into a tree and
//! writes the tree back. The vanilla files are laid out as a contiguous depth-first pre-order walk (section, its
//! properties, each property's areas, each area's curves, each curve's keys), which the writer reproduces; a key's
//! value is everything up to the next record (twpf.py's rule). An edit that keeps the value sizes therefore gives
//! the same bytes as twpf.py's in-place Editor. Notes: docs/formats/twpf.md.
//!
//!   "TWPF" "win" u8 version, u32 unk (12), u32 section count, u32 section offset[n]   (absolute offsets)
//!   section   u16 property count, u16 class id, u32 property offset[n]
//!   property  u8 area count, u8 value type (1 float, 2 vec3, 4 path pair), u8 property id, u8 class id,
//!             u32 area offset[n]
//!   area      u16 area key, u16 (1), u8 curve count, u8 area index, u16 (0), u32 curve offset[n]
//!   curve     u16 weather (0 SUNNY, 1 CLOUDY, 2 RAINY, 3 SANDSTORM, 4 FOGGY, 5 POURING), u16 key count,
//!             u32 key offset[n]
//!   key       u32 minute of the day, value bytes

#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub minute: u32,
    pub value: Vec<u8>,
}

impl Key {
    /// the value as little-endian f32 components (types 1 and 2)
    pub fn floats(&self) -> Vec<f32> {
        self.value.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect()
    }
    pub fn set_floats(&mut self, v: &[f32]) -> Result<(), String> {
        if v.len() * 4 != self.value.len() {
            return Err("twpf: component count changed".into());
        }
        self.value = v.iter().flat_map(|x| x.to_le_bytes()).collect();
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Curve {
    pub weather: u16,
    pub keys: Vec<Key>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Area {
    pub key: u16,
    pub one: u16,
    pub index: u8,
    pub pad: u16,
    pub curves: Vec<Curve>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Property {
    pub vtype: u8,
    pub id: u8,
    pub cls: u8,
    pub areas: Vec<Area>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub cls: u16,
    pub props: Vec<Property>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Twpf {
    /// bytes 4..8 ("win" + version)
    pub tag: [u8; 4],
    pub unk: u32,
    pub sections: Vec<Section>,
}

pub const WEATHER: [&str; 6] = ["SUNNY", "CLOUDY", "RAINY", "SANDSTORM", "FOGGY", "POURING"];

fn u32s(b: &[u8], at: usize, n: usize) -> Result<Vec<usize>, String> {
    (0..n).map(|i| b.get(at + 4 * i..at + 4 * i + 4)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()) as usize)
        .ok_or_else(|| format!("twpf: truncated at {:#x}", at + 4 * i))).collect()
}
fn u16at(b: &[u8], i: usize) -> Result<u16, String> {
    b.get(i..i + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or_else(|| format!("twpf: truncated at {i:#x}"))
}
fn u8at(b: &[u8], i: usize) -> Result<u8, String> {
    b.get(i).copied().ok_or_else(|| format!("twpf: truncated at {i:#x}"))
}

pub fn parse(b: &[u8]) -> Result<Twpf, String> {
    if b.get(0..4) != Some(b"TWPF") || b.len() < 16 {
        return Err("not a TWPF".into());
    }
    let unk = u32s(b, 8, 1)?[0] as u32;
    let nsec = u32s(b, 12, 1)?[0];
    // pass 1: the tree with record offsets; pass 2: key value sizes up to the next record start
    struct KeyOffset { offset: usize, minute: u32 }
    type CurveOffsets = (u16, Vec<KeyOffset>);
    type AreaOffsets = (u16, u16, u8, u16, Vec<CurveOffsets>);
    type PropertyOffsets = (u8, u8, u8, Vec<AreaOffsets>);
    type SectionOffsets = (u16, Vec<PropertyOffsets>);
    let mut starts = vec![b.len()];
    let mut key_starts = Vec::new();
    let mut tree: Vec<SectionOffsets> = Vec::new();
    for s in u32s(b, 16, nsec)? {
        starts.push(s);
        let (np, cls) = (u16at(b, s)? as usize, u16at(b, s + 2)?);
        let mut props = Vec::new();
        for p in u32s(b, s + 4, np)? {
            starts.push(p);
            let (na, vt, pid, pcls) = (u8at(b, p)? as usize, u8at(b, p + 1)?, u8at(b, p + 2)?, u8at(b, p + 3)?);
            let mut areas = Vec::new();
            for a in u32s(b, p + 4, na)? {
                starts.push(a);
                let (akey, one, nc, ai, pad) = (u16at(b, a)?, u16at(b, a + 2)?, u8at(b, a + 4)? as usize, u8at(b, a + 5)?, u16at(b, a + 6)?);
                let mut curves = Vec::new();
                for c in u32s(b, a + 8, nc)? {
                    starts.push(c);
                    let (w, nk) = (u16at(b, c)?, u16at(b, c + 2)? as usize);
                    let mut keys = Vec::new();
                    for k in u32s(b, c + 4, nk)? {
                        starts.push(k);
                        keys.push(KeyOffset { offset: k, minute: u32s(b, k, 1)?[0] as u32 });
                        key_starts.push(k);
                    }
                    curves.push((w, keys));
                }
                areas.push((akey, one, ai, pad, curves));
            }
            props.push((vt, pid, pcls, areas));
        }
        tree.push((cls, props));
    }
    starts.sort();
    starts.dedup();
    // Every key's minute was checked above, so the file-end sentinel follows it.
    let next = |offset: usize| starts[starts.partition_point(|&start| start <= offset)];
    for offset in key_starts {
        let value_start = offset.checked_add(4).ok_or("twpf: key offset overflow")?;
        let value_end = next(offset);
        b.get(value_start..value_end).ok_or_else(|| {
            format!("twpf: key at {offset:#x} overlaps the next record at {value_end:#x}")
        })?;
    }
    let sections = tree.into_iter().map(|(cls, props)| Section {
        cls,
        props: props.into_iter().map(|(vtype, id, pcls, areas)| Property {
            vtype, id, cls: pcls,
            areas: areas.into_iter().map(|(key, one, index, pad, curves)| Area {
                key, one, index, pad,
                curves: curves.into_iter().map(|(weather, keys)| Curve {
                    weather,
                    keys: keys.into_iter().map(|KeyOffset { offset, minute }| Key {
                        minute, value: b[offset + 4..next(offset)].to_vec(),
                    }).collect(),
                }).collect(),
            }).collect(),
        }).collect(),
    }).collect();
    Ok(Twpf { tag: b[4..8].try_into().unwrap(), unk, sections })
}

pub fn write(t: &Twpf) -> Vec<u8> {
    fn put_u32(o: &mut [u8], at: usize, v: usize) {
        o[at..at + 4].copy_from_slice(&(v as u32).to_le_bytes());
    }
    let mut o = Vec::new();
    o.extend_from_slice(b"TWPF");
    o.extend_from_slice(&t.tag);
    o.extend_from_slice(&t.unk.to_le_bytes());
    o.extend_from_slice(&(t.sections.len() as u32).to_le_bytes());
    let sec_tab = o.len();
    o.resize(o.len() + 4 * t.sections.len(), 0);
    for (si, s) in t.sections.iter().enumerate() {
        let at = o.len();
        put_u32(&mut o, sec_tab + 4 * si, at);
        o.extend_from_slice(&(s.props.len() as u16).to_le_bytes());
        o.extend_from_slice(&s.cls.to_le_bytes());
        o.resize(o.len() + 4 * s.props.len(), 0);
        for (pi, p) in s.props.iter().enumerate() {
            let pat = o.len();
            put_u32(&mut o, at + 4 + 4 * pi, pat);
            o.extend_from_slice(&[p.areas.len() as u8, p.vtype, p.id, p.cls]);
            o.resize(o.len() + 4 * p.areas.len(), 0);
            for (ai, a) in p.areas.iter().enumerate() {
                let aat = o.len();
                put_u32(&mut o, pat + 4 + 4 * ai, aat);
                o.extend_from_slice(&a.key.to_le_bytes());
                o.extend_from_slice(&a.one.to_le_bytes());
                o.extend_from_slice(&[a.curves.len() as u8, a.index]);
                o.extend_from_slice(&a.pad.to_le_bytes());
                o.resize(o.len() + 4 * a.curves.len(), 0);
                for (ci, c) in a.curves.iter().enumerate() {
                    let cat = o.len();
                    put_u32(&mut o, aat + 8 + 4 * ci, cat);
                    o.extend_from_slice(&c.weather.to_le_bytes());
                    o.extend_from_slice(&(c.keys.len() as u16).to_le_bytes());
                    o.resize(o.len() + 4 * c.keys.len(), 0);
                    for (ki, k) in c.keys.iter().enumerate() {
                        let kat = o.len();
                        put_u32(&mut o, cat + 4 + 4 * ki, kat);
                        o.extend_from_slice(&k.minute.to_le_bytes());
                        o.extend_from_slice(&k.value);
                    }
                }
            }
        }
    }
    o
}

impl Twpf {
    /// every curve of (class, property id), optionally limited to weathers / area indices, with its area index
    pub fn curves_mut(&mut self, cls: u16, pid: u8) -> impl Iterator<Item = (u8, &mut Curve)> {
        self.sections.iter_mut().filter(move |s| s.cls == cls).flat_map(|s| s.props.iter_mut())
            .filter(move |p| p.id == pid).flat_map(|p| p.areas.iter_mut())
            .flat_map(|a| { let i = a.index; a.curves.iter_mut().map(move |c| (i, c)) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_small() {
        let key = |m, v: &[f32]| Key { minute: m, value: v.iter().flat_map(|x| x.to_le_bytes()).collect() };
        let t = Twpf {
            tag: *b"win\x01",
            unk: 12,
            sections: vec![Section { cls: 1, props: vec![Property { vtype: 2, id: 2, cls: 1, areas: vec![Area {
                key: 0, one: 1, index: 0, pad: 0,
                curves: vec![Curve { weather: 0, keys: vec![key(0, &[0.3, 0.6, 1.0]), key(720, &[1.0, 1.0, 1.0])] }],
            }] }] }],
        };
        let b = write(&t);
        let mut r = parse(&b).unwrap();
        assert_eq!(r, t);
        for (_, c) in r.curves_mut(1, 2) {
            c.keys[0].set_floats(&[0.5, 0.5, 0.5]).unwrap();
        }
        assert_eq!(write(&r).len(), b.len());
    }
}
