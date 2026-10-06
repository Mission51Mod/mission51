//! Rig driver files (.frdv, "FRDV"): the per-model helper-bone drivers (thigh / piston HLP bones follow other bones).
//! Read + write. Our own implementation from the vanilla files (no Python codec existed; the pipeline copied them).
//! Notes: docs/formats/frdv.md.
//!
//!   "FRDV", u32 version (0x0BFFB0A8), u32 record count, u32 0, u32 record offset[count] (absolute), zero pad to 16,
//!   the records back to back, 128 bytes each. A record starts with u16 type, u16 driven bone, then u16 fields
//!   (counts / source bones, 0xFFFF = none) and float parameters; the fields are kept as bytes.
//! Checked on all 215 distinct vanilla files: one version, 4,105 records, all 128 bytes, contiguous, sorted offsets.

pub const MAGIC: &[u8; 4] = b"FRDV";
pub const RECORD: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frdv {
    pub version: u32,
    pub records: Vec<[u8; RECORD]>,
}

impl Frdv {
    /// the leading u16 fields of record i: (type, driven bone, then six more u16)
    pub fn record_header(&self, i: usize) -> [u16; 8] {
        let r = &self.records[i];
        std::array::from_fn(|k| u16::from_le_bytes([r[2 * k], r[2 * k + 1]]))
    }
}

fn u32at(b: &[u8], i: usize) -> Result<u32, String> {
    b.get(i..i + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("frdv: truncated at {i:#x}"))
}

pub fn read(b: &[u8]) -> Result<Frdv, String> {
    if b.get(0..4) != Some(MAGIC) {
        return Err("not an frdv (FRDV)".into());
    }
    let version = u32at(b, 4)?;
    let n = u32at(b, 8)? as usize;
    if u32at(b, 12)? != 0 {
        return Err("frdv: non-zero header field".into());
    }
    let first = (16 + 4 * n).next_multiple_of(16);
    let mut records = Vec::with_capacity(n);
    for i in 0..n {
        let o = u32at(b, 16 + 4 * i)? as usize;
        if o != first + RECORD * i {
            return Err(format!("frdv: record {i} at {o:#x}, expected {:#x} (records not contiguous)", first + RECORD * i));
        }
        records.push(b.get(o..o + RECORD).ok_or("frdv: record out of range")?.try_into().unwrap());
    }
    if b.len() != first + RECORD * n {
        return Err(format!("frdv: {} bytes after the records", b.len() as i64 - (first + RECORD * n) as i64));
    }
    if b[16 + 4 * n..first].iter().any(|&c| c != 0) {
        return Err("frdv: non-zero table padding".into());
    }
    Ok(Frdv { version, records })
}

pub fn write(f: &Frdv) -> Vec<u8> {
    let n = f.records.len();
    let first = (16 + 4 * n).next_multiple_of(16);
    let mut o = Vec::with_capacity(first + RECORD * n);
    o.extend_from_slice(MAGIC);
    for v in [f.version, n as u32, 0] {
        o.extend_from_slice(&v.to_le_bytes());
    }
    for i in 0..n {
        o.extend_from_slice(&((first + RECORD * i) as u32).to_le_bytes());
    }
    o.resize(first, 0);
    for r in &f.records {
        o.extend_from_slice(r);
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let mut r = [0u8; RECORD];
        r[0] = 3;
        r[2] = 45;
        let f = Frdv { version: 0x0BFF_B0A8, records: vec![r, [7; RECORD], [9; RECORD]] };
        let b = write(&f);
        assert_eq!(b.len(), 32 + 3 * RECORD);
        let g = read(&b).unwrap();
        assert_eq!(g, f);
        assert_eq!(g.record_header(0)[..2], [3, 45]);
    }
}
