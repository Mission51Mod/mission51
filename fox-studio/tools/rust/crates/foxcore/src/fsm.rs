//! Demo streams (.fsm): a sequence of timed chunks (SYS / DEMO / SND / END). Read + write (chunk level), and the
//! TrackStream packet header of DEMO chunks.
//!
//! Port of the container part of tools/demo/stream.py (read_stream, Packet); segment-data decoding (QUAT18 etc.)
//! stays there. Notes: docs/formats/sand.md. Credit: kapuragu/FoxEngineTemplates anim_common.bt (documentation).
//!
//!   chunk: char[4] tag, u32 chunk size (header included), f64 time, payload (size - 16 bytes); the stream ends at
//!          the "END " chunk (or a chunk of size 0); bytes after it are kept.
//!   DEMO payload = one TrackStream packet: u32 type (0), u32 flags (2 segment flags, 4 base offset, 8 events),
//!          u32 start frame, u32 frame count, u32 segment count, u32 packet size, u32 events offset,
//!          u32 segment offset[n] (from the packet start, 0 = no data), [f32 base offset x3 if flags & 4],
//!          [u16 segment flags[n] if flags & 2], segment data.

#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    pub tag: [u8; 4],
    /// the f64 time, kept as bits
    pub time_bits: u64,
    pub payload: Vec<u8>,
    /// the size field when it is not 16 + payload length (the END chunk may store 0); None = 16 + payload
    pub size_override: Option<u32>,
}

impl Chunk {
    pub fn time(&self) -> f64 {
        f64::from_bits(self.time_bits)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fsm {
    pub chunks: Vec<Chunk>,
    pub tail: Vec<u8>,
}

fn u32at(d: &[u8], i: usize) -> Result<u32, String> {
    d.get(i..i + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("fsm: truncated at {i:#x}"))
}

pub fn read(d: &[u8]) -> Result<Fsm, String> {
    let mut off = 0usize;
    let mut chunks = Vec::new();
    while off + 16 <= d.len() {
        let tag: [u8; 4] = d[off..off + 4].try_into().unwrap();
        let size = u32at(d, off + 4)? as usize;
        let time_bits = u64::from_le_bytes(d[off + 8..off + 16].try_into().unwrap());
        let end = if size >= 16 { off + size } else { off + 16 };
        let payload = d.get(off + 16..end).ok_or_else(|| format!("fsm: chunk at {off:#x} runs past the end"))?.to_vec();
        chunks.push(Chunk { tag, time_bits, payload, size_override: if size >= 16 { None } else { Some(size as u32) } });
        off = end;
        if &tag == b"END " || size == 0 {
            break;
        }
    }
    Ok(Fsm { chunks, tail: d[off.min(d.len())..].to_vec() })
}

pub fn write(f: &Fsm) -> Vec<u8> {
    let mut o = Vec::new();
    for c in &f.chunks {
        o.extend_from_slice(&c.tag);
        let size = c.size_override.unwrap_or(16 + c.payload.len() as u32);
        o.extend_from_slice(&size.to_le_bytes());
        o.extend_from_slice(&c.time_bits.to_le_bytes());
        o.extend_from_slice(&c.payload);
    }
    o.extend_from_slice(&f.tail);
    o
}

/// The TrackStream header of a DEMO chunk's payload (stream.Packet).
#[derive(Clone, Debug, PartialEq)]
pub struct Packet {
    pub typ: u32,
    pub flags: u32,
    pub start: u32,
    pub frames: u32,
    pub size: u32,
    pub events: u32,
    pub offsets: Vec<u32>,
    pub base: [f32; 3],
    pub seg_flags: Vec<u16>,
    /// offset of the segment data in the payload
    pub data_start: usize,
}

pub fn packet(p: &[u8]) -> Result<Packet, String> {
    let g = |i: usize| u32at(p, 4 * i);
    let n = g(4)? as usize;
    let offsets = (0..n).map(|i| u32at(p, 28 + 4 * i)).collect::<Result<Vec<_>, _>>()?;
    let flags = g(1)?;
    let mut q = 28 + 4 * n;
    let mut base = [0.0f32; 3];
    if flags & 4 != 0 {
        for (k, b) in base.iter_mut().enumerate() {
            *b = f32::from_bits(u32at(p, q + 4 * k)?);
        }
        q += 12;
    }
    let seg_flags = if flags & 2 != 0 {
        (0..n).map(|i| p.get(q + 2 * i..q + 2 * i + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
            .ok_or_else(|| "fsm: truncated segment flags".to_string())).collect::<Result<Vec<_>, _>>()?
    } else {
        vec![0; n]
    };
    Ok(Packet { typ: g(0)?, flags, start: g(2)?, frames: g(3)?, size: g(5)?, events: g(6)?, offsets, base, seg_flags,
                data_start: q + 2 * n })
}

/// stream.Packet.seg_bytes: raw bytes of segment i, up to the next segment with data, or the events / the end
pub fn seg_bytes<'a>(payload: &'a [u8], pk: &Packet, i: usize) -> &'a [u8] {
    let o = pk.offsets[i] as usize;
    if o == 0 {
        return &[];
    }
    let end = pk.offsets[i + 1..].iter().find(|&&x| x != 0).map(|&x| x as usize)
        .unwrap_or(if pk.events != 0 { pk.events as usize } else { pk.size as usize });
    payload.get(o..end).unwrap_or(&[])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_small() {
        let f = Fsm { chunks: vec![
            Chunk { tag: *b"SYS ", time_bits: 0f64.to_bits(), payload: vec![1, 2, 3, 4], size_override: None },
            Chunk { tag: *b"END ", time_bits: 2.5f64.to_bits(), payload: vec![], size_override: Some(0) }],
            tail: vec![] };
        let b = write(&f);
        assert_eq!(read(&b).unwrap(), f);
    }
}
