//! Texture packs (.pftxs) and sound packs (.sbp): read and write.
//! Layouts documented by the community (GzsTool, MIT); our own implementation, proven by round trips over every
//! vanilla file (tools/rust/tests/regress_containers.py).
//!
//! pftxs: "PFTX", u32 0x40000000, u32 0x10, u32 1 | "TEXL", u32 size (from TEXL to the end), u32 count, u32 0 |
//!        count x FTEX block: "FTEX", u32 block size, u64 hash, u32 n, 3 x u32 0, n x (u64 hash, i32 offset from the
//!        block start, i32 size), then the n data blobs in order.
//! sbp:   "SBPL", u8 count, u16 header size (8 + 12 count), u8 0, count x (4-byte type tag "bnk\0"/"stp\0"/"sab\0",
//!        u32 absolute offset, i32 size), zero pad to 16, each blob zero padded to 16.

fn u32le(b: &[u8], i: usize) -> Result<u32, String> {
    b.get(i..i + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("truncated at {i}"))
}

fn u64le(b: &[u8], i: usize) -> Result<u64, String> {
    b.get(i..i + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("truncated at {i}"))
}

#[derive(Clone, Debug, PartialEq)]
pub struct FtexBlock {
    pub hash: u64,
    pub entries: Vec<(u64, Vec<u8>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pftxs {
    pub head: [u32; 3],
    pub texl_unknown: u32,
    pub blocks: Vec<FtexBlock>,
}

pub fn pftxs_read(b: &[u8]) -> Result<Pftxs, String> {
    if b.get(0..4) != Some(b"PFTX") {
        return Err("not a pftxs (PFTX)".into());
    }
    let head = [u32le(b, 4)?, u32le(b, 8)?, u32le(b, 12)?];
    if b.get(16..20) != Some(b"TEXL") {
        return Err("pftxs: no TEXL".into());
    }
    let texl_size = u32le(b, 20)? as usize;
    let count = u32le(b, 24)? as usize;
    let texl_unknown = u32le(b, 28)?;
    if texl_size != b.len() - 16 {
        return Err("pftxs: TEXL size does not match the file extent".into());
    }
    if count > (b.len() - 32) / 32 {
        return Err("pftxs: block count exceeds available block headers".into());
    }
    let mut position = 32usize;
    let mut blocks = Vec::with_capacity(count);
    for _ in 0..count {
        let header_end = position.checked_add(32).filter(|&end| end <= b.len())
            .ok_or("pftxs: truncated FTEX block header")?;
        let header = &b[position..header_end];
        if header.get(..4) != Some(b"FTEX") {
            return Err(format!("pftxs: no FTEX at {position}"));
        }
        let block_size = u32le(header, 4)? as usize;
        if block_size < 32 {
            return Err("pftxs: FTEX block size is smaller than its header".into());
        }
        let block_end = position.checked_add(block_size).filter(|&end| end <= b.len())
            .ok_or("pftxs: FTEX block exceeds the file extent")?;
        let hash = u64le(header, 8)?;
        let entry_count = u32le(header, 16)? as usize;
        if entry_count > (block_size - 32) / 16 {
            return Err("pftxs: entry table exceeds its FTEX block".into());
        }
        let table_size = entry_count.checked_mul(16).and_then(|size| size.checked_add(32))
            .ok_or("pftxs: entry table size overflow")?;
        let block = &b[position..block_end];
        let mut entries = Vec::with_capacity(entry_count);
        for index in 0..entry_count {
            let record = 32 + 16 * index;
            let entry_hash = u64le(block, record)?;
            let offset = u32le(block, record + 8)? as usize;
            let size = u32le(block, record + 12)? as usize;
            let end = offset.checked_add(size).filter(|&end| end <= block.len())
                .ok_or("pftxs: entry exceeds its FTEX block")?;
            if size != 0 && offset < table_size {
                return Err("pftxs: entry overlaps its FTEX table".into());
            }
            entries.push((entry_hash, block[offset..end].to_vec()));
        }
        blocks.push(FtexBlock { hash, entries });
        position = block_end;
    }
    if position != b.len() {
        return Err(format!("pftxs: {} trailing bytes", b.len() - position));
    }
    Ok(Pftxs { head, texl_unknown, blocks })
}

pub fn pftxs_write(x: &Pftxs) -> Vec<u8> {
    let mut o = Vec::new();
    o.extend_from_slice(b"PFTX");
    for v in x.head {
        o.extend_from_slice(&v.to_le_bytes());
    }
    o.extend_from_slice(b"TEXL");
    o.extend_from_slice(&[0u8; 4]); // size, below
    o.extend_from_slice(&(x.blocks.len() as u32).to_le_bytes());
    o.extend_from_slice(&x.texl_unknown.to_le_bytes());
    for blk in &x.blocks {
        let start = o.len();
        let table = 32 + 16 * blk.entries.len();
        o.extend_from_slice(b"FTEX");
        o.extend_from_slice(&[0u8; 4]);
        o.extend_from_slice(&blk.hash.to_le_bytes());
        o.extend_from_slice(&(blk.entries.len() as u32).to_le_bytes());
        o.extend_from_slice(&[0u8; 12]);
        let mut off = table;
        for (h, d) in &blk.entries {
            o.extend_from_slice(&h.to_le_bytes());
            o.extend_from_slice(&(off as u32).to_le_bytes());
            o.extend_from_slice(&(d.len() as u32).to_le_bytes());
            off += d.len();
        }
        for (_, d) in &blk.entries {
            o.extend_from_slice(d);
        }
        let size = (o.len() - start) as u32;
        o[start + 4..start + 8].copy_from_slice(&size.to_le_bytes());
    }
    let texl = (o.len() - 16) as u32;
    o[20..24].copy_from_slice(&texl.to_le_bytes());
    o
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sbp {
    pub header_pad: u8,
    /// (4-byte type tag, data)
    pub entries: Vec<([u8; 4], Vec<u8>)>,
}

pub fn sbp_read(b: &[u8]) -> Result<Sbp, String> {
    if b.get(0..4) != Some(b"SBPL") {
        return Err("not an sbp (SBPL)".into());
    }
    let n = *b.get(4).ok_or("truncated")? as usize;
    let header_pad = *b.get(7).ok_or("truncated")?;
    let mut entries = Vec::with_capacity(n);
    for k in 0..n {
        let e = 8 + 12 * k;
        let tag: [u8; 4] = b.get(e..e + 4).ok_or("truncated")?.try_into().unwrap();
        let off = u32le(b, e + 4)? as usize;
        let sz = u32le(b, e + 8)? as usize;
        entries.push((tag, b.get(off..off + sz).ok_or("sbp: entry out of range")?.to_vec()));
    }
    Ok(Sbp { header_pad, entries })
}

pub fn sbp_write(x: &Sbp) -> Result<Vec<u8>, String> {
    let count = u8::try_from(x.entries.len()).map_err(|_| "sbp: entry count exceeds 255")?;
    let hs = 8 + 12 * x.entries.len();
    let mut total_size = hs.next_multiple_of(16);
    for (_, data) in &x.entries {
        i32::try_from(data.len()).map_err(|_| "sbp: payload size exceeds signed 32-bit field")?;
        total_size = total_size.checked_add(data.len()).and_then(|size| size.checked_add(15))
            .map(|size| size / 16 * 16).ok_or("sbp: container size overflow")?;
        u32::try_from(total_size).map_err(|_| "sbp: container exceeds 4 GiB")?;
    }
    let mut o = vec![0u8; hs];
    o[0..4].copy_from_slice(b"SBPL");
    o[4] = count;
    o[5..7].copy_from_slice(&(hs as u16).to_le_bytes());
    o[7] = x.header_pad;
    while !o.len().is_multiple_of(16) {
        o.push(0);
    }
    for (k, (tag, d)) in x.entries.iter().enumerate() {
        let off = o.len() as u32;
        let e = 8 + 12 * k;
        o[e..e + 4].copy_from_slice(tag);
        o[e + 4..e + 8].copy_from_slice(&off.to_le_bytes());
        o[e + 8..e + 12].copy_from_slice(&(d.len() as u32).to_le_bytes());
        o.extend_from_slice(d);
        while !o.len().is_multiple_of(16) {
            o.push(0);
        }
    }
    Ok(o)
}
