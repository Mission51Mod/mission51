//! Subtitle packs (.subp). Read + write (container level; the text bytes are kept as stored).
//!
//! Port of tools/audio/voice_bank.py read_subp / write_subp (tools/audio belongs to the debug agent; this is a
//! separate copy of the container codec only). Checked on all 529 distinct vanilla files: the units sit back to back
//! in the order of the index table, each text NUL-terminated once. Notes: docs/formats/subp.md.
//!
//!   u8 flags (0x13 in most), u8 language (0 jpn, 1 eng, 2 fre, 3 ita, 4 ger, 5 spa, 6 por, 7 rus), u16 count,
//!   count x {u32 subtitle id, u32 unit offset}, then the units:
//!   unit: u16 0x4C01, u8 timing count, u8 type, u16 text size (incl. NUL), u16 second size (write_subp: UTF-8
//!         length + 1), i16 speaker, u16 flags, timing count x {u16 start, u16 end}, the text bytes + NUL

pub const UNIT_MAGIC: u16 = 0x4C01;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: u32,
    pub typ: u8,
    pub size2: u16,
    pub speaker: i16,
    pub flags: u16,
    pub timings: Vec<(u16, u16)>,
    /// text bytes without the NUL (Latin-1 in the files write_subp makes)
    pub text: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subp {
    pub flags: u8,
    pub lang: u8,
    pub entries: Vec<Entry>,
}

fn u16at(d: &[u8], i: usize) -> Result<u16, String> {
    d.get(i..i + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or_else(|| format!("subp: truncated at {i:#x}"))
}
fn u32at(d: &[u8], i: usize) -> Result<u32, String> {
    d.get(i..i + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("subp: truncated at {i:#x}"))
}

pub fn read(d: &[u8]) -> Result<Subp, String> {
    if d.len() < 4 {
        return Err("subp: truncated".into());
    }
    let (flags, lang, count) = (d[0], d[1], u16at(d, 2)? as usize);
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let (id, off) = (u32at(d, 4 + 8 * i)?, u32at(d, 8 + 8 * i)? as usize);
        if u16at(d, off)? != UNIT_MAGIC {
            return Err("bad subtitle unit".into());
        }
        let nt = *d.get(off + 2).ok_or("subp: truncated")? as usize;
        let typ = d[off + 3];
        let size = u16at(d, off + 4)? as usize;
        let size2 = u16at(d, off + 6)?;
        let speaker = u16at(d, off + 8)? as i16;
        let fl = u16at(d, off + 10)?;
        let timings = (0..nt).map(|k| Ok((u16at(d, off + 12 + 4 * k)?, u16at(d, off + 14 + 4 * k)?)))
            .collect::<Result<Vec<_>, String>>()?;
        let t0 = off + 12 + 4 * nt;
        let raw = d.get(t0..t0 + size).ok_or("subp: text out of range")?;
        if raw.last() != Some(&0) {
            return Err(format!("subp: text of {id} not NUL-terminated"));
        }
        entries.push(Entry { id, typ, size2, speaker, flags: fl, timings, text: raw[..size - 1].to_vec() });
    }
    Ok(Subp { flags, lang, entries })
}

fn checked_text_size(length: usize, encoding: &str) -> Result<u16, String> {
    length.checked_add(1).and_then(|size| u16::try_from(size).ok())
        .ok_or_else(|| format!("subp: {encoding} text size including NUL exceeds 65535 bytes"))
}

/// Units back to back in entry order (the vanilla layout). Invalid format counts
/// and sizes are rejected before a completed container is returned.
pub fn write(s: &Subp) -> Result<Vec<u8>, String> {
    let entry_count = u16::try_from(s.entries.len())
        .map_err(|_| "subp: entry count exceeds 65535")?;
    let mut head = vec![s.flags, s.lang];
    head.extend_from_slice(&entry_count.to_le_bytes());
    let base = 4 + 8 * s.entries.len();
    let mut body = Vec::new();
    for entry in &s.entries {
        let timing_count = u8::try_from(entry.timings.len())
            .map_err(|_| format!("subp: subtitle {} has more than 255 timings", entry.id))?;
        let text_size = checked_text_size(entry.text.len(), "stored")?;
        let offset = base.checked_add(body.len()).ok_or("subp: unit offset overflow")?;
        let unit_size = 12 + 4 * entry.timings.len() + text_size as usize;
        let end = offset.checked_add(unit_size).ok_or("subp: container size overflow")?;
        u32::try_from(end).map_err(|_| "subp: container exceeds 4 GiB")?;
        let offset = u32::try_from(offset).map_err(|_| "subp: unit offset exceeds 4 GiB")?;
        head.extend_from_slice(&entry.id.to_le_bytes());
        head.extend_from_slice(&offset.to_le_bytes());
        body.extend_from_slice(&UNIT_MAGIC.to_le_bytes());
        body.push(timing_count);
        body.push(entry.typ);
        body.extend_from_slice(&text_size.to_le_bytes());
        body.extend_from_slice(&entry.size2.to_le_bytes());
        body.extend_from_slice(&entry.speaker.to_le_bytes());
        body.extend_from_slice(&entry.flags.to_le_bytes());
        for &(start, end) in &entry.timings {
            body.extend_from_slice(&start.to_le_bytes());
            body.extend_from_slice(&end.to_le_bytes());
        }
        body.extend_from_slice(&entry.text);
        body.push(0);
    }
    head.extend_from_slice(&body);
    Ok(head)
}

/// Subtitle id, speaker, text, type, flags and start/end timing pairs.
pub type SubtitleInput<'a> = (u32, i16, &'a str, u8, u16, Vec<(u16, u16)>);

/// voice_bank.write_subp(entries, lang_byte): entries sorted by id, text encoded
/// Latin-1 (error otherwise), second size = UTF-8 length + 1, header flags 0x13.
pub fn write_subp(entries: &[SubtitleInput<'_>], lang: u8) -> Result<Vec<u8>, String> {
    u16::try_from(entries.len()).map_err(|_| "subp: entry count exceeds 65535")?;
    let mut ordered: Vec<_> = entries.iter().collect();
    ordered.sort_by_key(|entry| entry.0);
    if ordered.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err("duplicate subtitle id".into());
    }
    let mut output = Vec::with_capacity(ordered.len());
    for (id, speaker, text, typ, flags, timings) in ordered {
        let size2 = checked_text_size(text.len(), "UTF-8")?;
        u8::try_from(timings.len())
            .map_err(|_| format!("subp: subtitle {id} has more than 255 timings"))?;
        let raw = text.chars().map(|character| {
            u8::try_from(character as u32).map_err(|_| {
                format!("subtitle text must be Latin-1 (vanilla .subp encoding): {text:?}")
            })
        }).collect::<Result<Vec<_>, _>>()?;
        output.push(Entry {
            id: *id, typ: *typ, size2, speaker: *speaker, flags: *flags,
            timings: timings.clone(), text: raw,
        });
    }
    write(&Subp { flags: 0x13, lang, entries: output })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_and_sort() {
        let b = write_subp(&[(9, 1, "b\u{e9}", 0, 0, vec![(0, 10)]), (3, -1, "a", 1, 2, vec![])], 1).unwrap();
        let s = read(&b).unwrap();
        assert_eq!(s.entries[0].id, 3);
        assert_eq!(s.entries[1].text, vec![b'b', 0xE9]);
        assert_eq!(s.entries[1].size2, 4); // UTF-8 length 3 + 1
        assert_eq!(write(&s).unwrap(), b);
        assert!(write_subp(&[(1, 0, "\u{3042}", 0, 0, vec![])], 0).is_err());
    }
}
