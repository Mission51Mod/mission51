//! Wwise containers, read-only: .wem (RIFF/WAVE) chunk headers and the Wwise Vorbis fmt fields, and SoundBank
//! (.bnk, the "bnk" blob of an .sbp) sections with the BKHD header, DIDX media index and HIRC object list.
//!
//! Ports of the parsing in tools/audio/wwise_vorbis.py (parse_wem) and tools/audio/voice_bank.py (bank_sections,
//! read_hirc). tools/audio belongs to the debug agent; this is a separate read-only copy, no encoder. Notes:
//! docs/formats/wwise.md. `reassemble_*` rebuild the bytes from the parsed chunks: equal to the input proves that the
//! parse accounts for every byte (examples/codec_corpus.rs runs it over the vanilla .wem and .sbp banks).

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub id: [u8; 4],
    /// payload offset in the file and its size (the size field)
    pub offset: usize,
    pub size: usize,
}

fn u16at(b: &[u8], i: usize) -> Result<u16, String> {
    b.get(i..i + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or_else(|| format!("wwise: truncated at {i:#x}"))
}
fn u32at(b: &[u8], i: usize) -> Result<u32, String> {
    b.get(i..i + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("wwise: truncated at {i:#x}"))
}

/// RIFF/WAVE chunks after the 12-byte header (word-aligned: an odd size is followed by one pad byte)
pub fn riff_chunks(w: &[u8]) -> Result<Vec<Chunk>, String> {
    if w.get(0..4) != Some(b"RIFF") || w.get(8..12) != Some(b"WAVE") {
        return Err("not RIFF/WAVE".into());
    }
    let mut p = 12;
    let mut v = Vec::new();
    while p + 8 <= w.len() {
        let size = u32at(w, p + 4)? as usize;
        v.push(Chunk { id: w[p..p + 4].try_into().unwrap(), offset: p + 8, size });
        p += 8 + size + (size & 1);
    }
    Ok(v)
}

/// The RIFF header + every chunk (header, payload, pad byte as stored) + whatever follows the last chunk.
pub fn reassemble_riff(w: &[u8]) -> Result<Vec<u8>, String> {
    let ch = riff_chunks(w)?;
    let mut o = w[..12].to_vec();
    let mut end = 12;
    for c in &ch {
        let stop = (c.offset + c.size + (c.size & 1)).min(w.len());
        o.extend_from_slice(&w[c.offset - 8..c.offset]);
        o.extend_from_slice(w.get(c.offset..stop).ok_or("wwise: chunk past the end")?);
        end = stop;
    }
    o.extend_from_slice(&w[end..]);
    Ok(o)
}

/// parse_wem: the fmt fields of a .wem; the Vorbis fields when the format tag is 0xFFFF with a 0x42-byte fmt.
#[derive(Clone, Debug, PartialEq)]
pub struct WemInfo {
    pub format_tag: u16,
    pub channels: u16,
    pub rate: u32,
    pub avg_bytes: u32,
    pub fmt_size: usize,
    pub data_offset: usize,
    pub data_size: usize,
    pub vorbis: Option<VorbisInfo>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VorbisInfo {
    pub sample_count: u32,
    pub mod_signal: u32,
    pub setup_offset: u32,
    pub audio_offset: u32,
    pub uid: u32,
    pub blocksize0: u8,
    pub blocksize1: u8,
    /// parse_wem's mod_packets: mod_signal not in (0x4A, 0x4B, 0x69, 0x70)
    pub mod_packets: bool,
}

pub fn parse_wem(w: &[u8]) -> Result<WemInfo, String> {
    let ch = riff_chunks(w)?;
    // a later chunk with the same id wins, as in the Python dict
    let find = |id: &[u8; 4]| ch.iter().rev().find(|c| &c.id == id).cloned();
    let f = find(b"fmt ").ok_or("wem: no fmt chunk")?;
    let d = find(b"data").ok_or("wem: no data chunk")?;
    let fo = f.offset;
    let format_tag = u16at(w, fo)?;
    let vorbis = if format_tag == 0xFFFF && f.size == 0x42 {
        let vo = fo + 0x18;
        let mod_signal = u32at(w, vo + 4)?;
        Some(VorbisInfo {
            sample_count: u32at(w, vo)?,
            mod_signal,
            setup_offset: u32at(w, vo + 0x10)?,
            audio_offset: u32at(w, vo + 0x14)?,
            uid: u32at(w, vo + 0x24)?,
            blocksize0: *w.get(vo + 0x28).ok_or("wem: truncated fmt")?,
            blocksize1: *w.get(vo + 0x29).ok_or("wem: truncated fmt")?,
            mod_packets: ![0x4A, 0x4B, 0x69, 0x70].contains(&mod_signal),
        })
    } else {
        None
    };
    Ok(WemInfo { format_tag, channels: u16at(w, fo + 2)?, rate: u32at(w, fo + 4)?, avg_bytes: u32at(w, fo + 8)?,
                 fmt_size: f.size, data_offset: d.offset, data_size: d.size, vorbis })
}

/// bank_sections: (tag, payload offset, size) in file order
pub fn bank_sections(b: &[u8]) -> Result<Vec<Chunk>, String> {
    let mut p = 0;
    let mut v = Vec::new();
    while p + 8 <= b.len() {
        let size = u32at(b, p + 4)? as usize;
        if p + 8 + size > b.len() {
            return Err(format!("bnk: section at {p:#x} runs past the end"));
        }
        v.push(Chunk { id: b[p..p + 4].try_into().unwrap(), offset: p + 8, size });
        p += 8 + size;
    }
    Ok(v)
}

pub fn reassemble_bank(b: &[u8]) -> Result<Vec<u8>, String> {
    let s = bank_sections(b)?;
    let mut o = Vec::with_capacity(b.len());
    let mut end = 0;
    for c in &s {
        o.extend_from_slice(&b[c.offset - 8..c.offset + c.size]);
        end = c.offset + c.size;
    }
    o.extend_from_slice(&b[end..]);
    Ok(o)
}

/// BKHD: (bank version, bank id)
pub fn bkhd(payload: &[u8]) -> Result<(u32, u32), String> {
    Ok((u32at(payload, 0)?, u32at(payload, 4)?))
}

/// DIDX: (media id, offset in DATA, size) entries
pub fn didx(payload: &[u8]) -> Vec<(u32, u32, u32)> {
    payload.chunks_exact(12).map(|e| {
        let u = |i: usize| u32::from_le_bytes(e[i..i + 4].try_into().unwrap());
        (u(0), u(4), u(8))
    }).collect()
}

/// read_hirc: (type, object id, payload range in the HIRC payload) per object; the objects must fill it exactly
pub fn read_hirc(h: &[u8]) -> Result<Vec<(u8, u32, std::ops::Range<usize>)>, String> {
    let n = u32at(h, 0)? as usize;
    let mut p = 4;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let t = *h.get(p).ok_or("HIRC truncated")?;
        let size = u32at(h, p + 1)? as usize;
        let oid = u32at(h, p + 5)?;
        if size < 4 || p + 5 + size > h.len() {
            return Err("HIRC object out of range".into());
        }
        out.push((t, oid, p + 9..p + 5 + size));
        p += 5 + size;
    }
    if p != h.len() {
        return Err("HIRC size mismatch".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn riff_and_bank() {
        let mut w = b"RIFF\0\0\0\0WAVE".to_vec();
        w.extend_from_slice(b"fmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&[1, 0, 2, 0, 0x44, 0xAC, 0, 0, 0x10, 0xB1, 2, 0, 4, 0, 16, 0]);
        w.extend_from_slice(b"data");
        w.extend_from_slice(&3u32.to_le_bytes());
        w.extend_from_slice(&[1, 2, 3, 0]); // odd size + pad byte
        assert_eq!(reassemble_riff(&w).unwrap(), w);
        let i = parse_wem(&w).unwrap();
        assert_eq!((i.channels, i.rate, i.data_size), (2, 44100, 3));
        let mut b = b"BKHD".to_vec();
        b.extend_from_slice(&8u32.to_le_bytes());
        b.extend_from_slice(&[0x58, 0, 0, 0, 1, 2, 3, 4]);
        b.extend_from_slice(b"HIRC");
        let hirc = [1u8, 0, 0, 0, 2, 4, 0, 0, 0, 9, 0, 0, 0];
        b.extend_from_slice(&(hirc.len() as u32).to_le_bytes());
        b.extend_from_slice(&hirc);
        let s = bank_sections(&b).unwrap();
        assert_eq!(bkhd(&b[s[0].offset..]).unwrap(), (0x58, 0x0403_0201));
        assert_eq!(read_hirc(&b[s[1].offset..s[1].offset + s[1].size]).unwrap()[0].1, 9);
        assert_eq!(reassemble_bank(&b).unwrap(), b);
    }
}
