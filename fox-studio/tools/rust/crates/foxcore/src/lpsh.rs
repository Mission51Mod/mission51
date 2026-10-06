//! Light probe SH coefficients (.lpsh, TppLightProbeSHCoefficients.lpshFile). Read + write.
//!
//! tools/location/flyk_look_probes.py parses these (parse_lpsh) and patches a template's SH payload (write_lpsh);
//! this module adds a structural writer that reproduces the vanilla layout. Notes: docs/formats/lpsh.md.
//! Credit: kapuragu/FoxEngineTemplates lpsh.bt (documentation).
//!
//!   FoxData v4 header (0x20): u32 4, u32 0x20, u32 file size, {StrCode32, u32 ABSOLUTE offset 0x50}
//!                             "LightProbeSHCoefficients", u32 0, 8 zero bytes
//!   node (0x30) at 0x20: name {0, 0}, flags 1, data offset, data size (to the end of the file), links 0,
//!                        parameters offset (relative to the node), 8 zero bytes
//!   header name at 0x50, NUL, padded to 16; parameters (0x10 each: u16 type 0, i16 next, {StrCode32, rel offset},
//!   u32 value) numLightProbes, numDiv, formatType; parameter names 16-aligned; data 16-aligned:
//!     u32 times[numDiv] (seconds of the day), zero pad to 16,
//!     per probe {i32 name offset, i32 data offset (both absolute), u32 flags (bit0 24 h SH, bit1 weather SH,
//!                bit2 related light, bit3 occlusion)},
//!     zero pad to 16, the probe names as C strings back to back (vanilla names may carry stray bytes before their NUL; they are
//!     part of the name), zero pad to 16,
//!     per probe its coefficient sets back to back: sets x 9 x half4 (r, g, b, sky visibility), where
//!     sets = (numDiv if bit0 else 1) * (2 if bit1 else 1) + (2 if bit2) + (1 if bit3);
//!   the file padded to 16.
use crate::hash::strcode32;

pub const CLASS: &str = "LightProbeSHCoefficients";

#[derive(Clone, Debug, PartialEq)]
pub struct Probe {
    /// raw name bytes (latin-1), without the NUL
    pub name: Vec<u8>,
    pub flags: u32,
    /// the coefficient sets as stored: sets x 9 x 4 half floats (raw bits)
    pub sh: Vec<u16>,
}

impl Probe {
    pub fn set_count(flags: u32, num_div: u32) -> usize {
        let (h24, hw, rel, occ) = (flags & 1, (flags >> 1) & 1, (flags >> 2) & 1, (flags >> 3) & 1);
        ((if h24 != 0 { num_div } else { 1 }) * (if hw != 0 { 2 } else { 1 }) + 2 * rel + occ) as usize
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lpsh {
    pub num_div: u32,
    pub format_type: u32,
    pub times: Vec<u32>,
    pub probes: Vec<Probe>,
}

fn u32at(b: &[u8], i: usize) -> Result<u32, String> {
    b.get(i..i + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("lpsh: truncated at {i:#x}"))
}
fn i32at(b: &[u8], i: usize) -> Result<i32, String> {
    Ok(u32at(b, i)? as i32)
}
fn cbytes(b: &[u8], off: usize) -> Result<Vec<u8>, String> {
    let s = b.get(off..).ok_or("lpsh: string out of range")?;
    let n = s.iter().position(|&c| c == 0).ok_or("lpsh: unterminated string")?;
    Ok(s[..n].to_vec())
}

pub fn read(b: &[u8]) -> Result<Lpsh, String> {
    if u32at(b, 0)? != 4 || u32at(b, 4)? != 0x20 {
        return Err("not an lpsh (FoxData v4)".into());
    }
    let n = 0x20usize;
    let doff = i32at(b, n + 12)? as usize;
    let mut po = n + i32at(b, n + 36)? as usize;
    let (mut nprobe, mut num_div, mut format_type) = (None, None, None);
    loop {
        let typ = u16::from_le_bytes([b[po], b[po + 1]]);
        let next = i16::from_le_bytes([b[po + 2], b[po + 3]]);
        let name = cbytes(b, (po as i64 + 4 + i32at(b, po + 8)? as i64) as usize)?;
        if typ != 0 {
            return Err(format!("lpsh: parameter type {typ}"));
        }
        let v = u32at(b, po + 12)?;
        match name.as_slice() {
            b"numLightProbes" => nprobe = Some(v),
            b"numDiv" => num_div = Some(v),
            b"formatType" => format_type = Some(v),
            _ => return Err(format!("lpsh: unexpected parameter {}", String::from_utf8_lossy(&name))),
        }
        if next == 0 {
            break;
        }
        po = (po as i64 + next as i64) as usize;
    }
    let (np, nd) = (nprobe.ok_or("lpsh: no numLightProbes")? as usize, num_div.ok_or("lpsh: no numDiv")?);
    let d = n + doff;
    let times = (0..nd as usize).map(|i| u32at(b, d + 4 * i)).collect::<Result<Vec<_>, _>>()?;
    let q = (d + 4 * nd as usize).next_multiple_of(16);
    let mut probes = Vec::with_capacity(np);
    for k in 0..np {
        let (nof, dof, fl) = (i32at(b, q + 12 * k)? as usize, i32at(b, q + 12 * k + 4)? as usize, u32at(b, q + 12 * k + 8)?);
        let nh = 36 * Probe::set_count(fl, nd);
        let raw = b.get(dof..dof + 2 * nh).ok_or("lpsh: SH data out of range")?;
        probes.push(Probe { name: cbytes(b, nof)?, flags: fl, sh: raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect() });
    }
    Ok(Lpsh { num_div: nd, format_type: format_type.ok_or("lpsh: no formatType")?, times, probes })
}

pub fn write(l: &Lpsh) -> Result<Vec<u8>, String> {
    if l.times.len() != l.num_div as usize {
        return Err("lpsh: times != numDiv".into());
    }
    for p in &l.probes {
        if p.sh.len() != 36 * Probe::set_count(p.flags, l.num_div) {
            return Err("lpsh: SH data size does not match the probe flags".into());
        }
    }
    let params: [(&str, u32); 3] = [("numLightProbes", l.probes.len() as u32), ("numDiv", l.num_div), ("formatType", l.format_type)];
    let hdr_str = 0x50usize;
    let params_at = (hdr_str + CLASS.len() + 1).next_multiple_of(16);
    let mut s = params_at + 0x10 * params.len();
    let mut strs = Vec::new();
    for (nm, _) in &params {
        s = s.next_multiple_of(16);
        strs.push(s);
        s += nm.len() + 1;
    }
    let data_at = s.next_multiple_of(16);
    // data
    let mut d = Vec::new();
    for t in &l.times {
        d.extend_from_slice(&t.to_le_bytes());
    }
    d.resize((data_at + d.len()).next_multiple_of(16) - data_at, 0);
    let table_at = data_at + d.len();
    let names_at = (table_at + 12 * l.probes.len()).next_multiple_of(16);
    let mut name_offs = Vec::new();
    let mut p = names_at;
    for pr in &l.probes {
        name_offs.push(p);
        p += pr.name.len() + 1;
    }
    let mut sh_at = p.next_multiple_of(16);
    for (k, pr) in l.probes.iter().enumerate() {
        d.extend_from_slice(&(name_offs[k] as i32).to_le_bytes());
        d.extend_from_slice(&(sh_at as i32).to_le_bytes());
        d.extend_from_slice(&pr.flags.to_le_bytes());
        sh_at += 2 * pr.sh.len();
    }
    d.resize(names_at - data_at, 0);
    for pr in &l.probes {
        d.extend_from_slice(&pr.name);
        d.push(0);
    }
    d.resize((data_at + d.len()).next_multiple_of(16) - data_at, 0);
    for pr in &l.probes {
        for h in &pr.sh {
            d.extend_from_slice(&h.to_le_bytes());
        }
    }
    d.resize((data_at + d.len()).next_multiple_of(16) - data_at, 0);
    let end = data_at + d.len();
    let mut o = vec![0u8; data_at];
    let put = |o: &mut Vec<u8>, at: usize, v: &[u8]| o[at..at + v.len()].copy_from_slice(v);
    put(&mut o, 0, &4u32.to_le_bytes());
    put(&mut o, 4, &0x20u32.to_le_bytes());
    put(&mut o, 8, &(end as u32).to_le_bytes());
    put(&mut o, 12, &strcode32(CLASS.as_bytes()).to_le_bytes());
    put(&mut o, 16, &(hdr_str as u32).to_le_bytes());
    put(&mut o, hdr_str, CLASS.as_bytes());
    let n = 0x20usize;
    put(&mut o, n + 8, &1u32.to_le_bytes());
    put(&mut o, n + 12, &((data_at - n) as i32).to_le_bytes());
    put(&mut o, n + 16, &(d.len() as u32).to_le_bytes());
    put(&mut o, n + 36, &((params_at - n) as i32).to_le_bytes());
    for (i, (nm, v)) in params.iter().enumerate() {
        let po = params_at + 0x10 * i;
        put(&mut o, po + 2, &(if i + 1 < params.len() { 0x10i16 } else { 0 }).to_le_bytes());
        put(&mut o, po + 4, &strcode32(nm.as_bytes()).to_le_bytes());
        put(&mut o, po + 8, &((strs[i] - (po + 4)) as i32).to_le_bytes());
        put(&mut o, po + 12, &v.to_le_bytes());
        put(&mut o, strs[i], nm.as_bytes());
    }
    o.extend_from_slice(&d);
    Ok(o)
}

/// IEEE half -> f32 (exact)
pub fn f16_to_f32(h: u16) -> f32 {
    let s = ((h >> 15) as u32) << 31;
    let e = ((h >> 10) & 0x1F) as u32;
    let m = (h & 0x3FF) as u32;
    let bits = if e == 0 {
        if m == 0 {
            s
        } else {
            // subnormal: m * 2^-24
            return f32::from_bits(s) + (if s != 0 { -1.0 } else { 1.0 }) * (m as f32) * f32::powi(2.0, -24);
        }
    } else if e == 31 {
        s | 0x7F80_0000 | (m << 13)
    } else {
        s | ((e + 112) << 23) | (m << 13)
    };
    f32::from_bits(bits)
}

/// f32 -> IEEE half, round to nearest even (numpy's float32 -> float16 cast)
pub fn f32_to_f16(x: f32) -> u16 {
    let b = x.to_bits();
    let s = ((b >> 16) & 0x8000) as u16;
    let e = ((b >> 23) & 0xFF) as i32;
    let m = b & 0x7F_FFFF;
    if e == 0xFF {
        return s | 0x7C00 | if m != 0 { 0x200 | (m >> 13) as u16 } else { 0 };
    }
    let e16 = e - 127 + 15;
    if e16 >= 31 {
        return s | 0x7C00;
    }
    if e16 <= 0 {
        if e16 < -10 {
            return s; // rounds to zero (|x| < 2^-25)
        }
        let mm = m | 0x80_0000;
        let shift = (14 - e16) as u32;
        let half = 1u32 << (shift - 1);
        let mut r = mm >> shift;
        let rem = mm & ((1 << shift) - 1);
        if rem > half || (rem == half && r & 1 == 1) {
            r += 1;
        }
        return s | r as u16;
    }
    let mut r = ((e16 as u32) << 10) | (m >> 13);
    let rem = m & 0x1FFF;
    if rem > 0x1000 || (rem == 0x1000 && r & 1 == 1) {
        r += 1; // may carry into the exponent (and to infinity), as IEEE rounding does
    }
    s | r as u16
}

/// write_lpsh(template, sets): a single-probe template (28 sets) with its SH payload replaced; `sets` are
/// 28 x 9 x 4 values, cast f64 -> f32 -> f16 as `np.asarray(sets, np.float32).astype('<f2')`.
pub fn write_lpsh(template: &[u8], sets: &[f64]) -> Result<Vec<u8>, String> {
    let l = read(template)?;
    if l.probes.len() != 1 || l.probes[0].sh.len() != 28 * 36 {
        return Err("template is not a single LP_auto probe".into());
    }
    if sets.len() != 28 * 36 {
        return Err("sets must be 28 x 9 x 4".into());
    }
    let halfs: Vec<u16> = sets.iter().map(|&v| f32_to_f16(v as f32)).collect();
    if halfs.iter().any(|h| h & 0x7C00 == 0x7C00) {
        return Err("SH outside the half-float range".into());
    }
    // the probe's data offset, as parse_lpsh finds it
    let n = 0x20usize;
    let d = n + i32at(template, n + 12)? as usize;
    let q = (d + 4 * l.num_div as usize).next_multiple_of(16);
    let off = i32at(template, q + 4)? as usize;
    let mut out = template.to_vec();
    for (k, h) in halfs.iter().enumerate() {
        out[off + 2 * k..off + 2 * k + 2].copy_from_slice(&h.to_le_bytes());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn half_roundtrip_and_rounding() {
        for h in 0..=0xFFFFu16 {
            if h & 0x7C00 == 0x7C00 && h & 0x3FF != 0 {
                continue; // NaN payloads
            }
            assert_eq!(f32_to_f16(f16_to_f32(h)), h, "{h:#x}");
        }
        assert_eq!(f32_to_f16(1.0 + 1.0 / 2048.0), 0x3C00); // tie -> even
        assert_eq!(f32_to_f16(1.0 + 3.0 / 2048.0), 0x3C02);
        assert_eq!(f32_to_f16(70000.0), 0x7C00);
    }
    #[test]
    fn roundtrip_small() {
        let l = Lpsh { num_div: 3, format_type: 1, times: vec![0, 3600, 7200], probes: vec![
            Probe { name: b"LP_0000".to_vec(), flags: 3, sh: vec![0x3C00; 36 * 6] },
            Probe { name: b"LP_0001".to_vec(), flags: 0, sh: vec![0x3800; 36] }] };
        let b = write(&l).unwrap();
        assert_eq!(b.len() % 16, 0);
        assert_eq!(read(&b).unwrap(), l);
    }
}
