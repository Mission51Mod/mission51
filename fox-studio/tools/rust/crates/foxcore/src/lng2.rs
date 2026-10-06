//! Localised text (.lng2, "LANG" big-endian). Read + write.
//!
//! Our own implementation from the vanilla files (the pipeline used datfpk.exe for these; nothing of it is used
//! here). Layout, checked on all 248 distinct vanilla .lng2 (docs/formats/lng2.md):
//!
//!   "LANG", u32 BE version (3), 4 bytes "BE\0\0", u32 BE entry count, u32 BE strings offset (24),
//!   u32 BE key table offset
//!   strings  per entry, back to back in file order: u16 BE (per-entry value, kept), the UTF-8 text, NUL;
//!            then zero padding to the next multiple of 4 (always 1-4 bytes)
//!   keys     per entry {u32 BE key hash, u32 BE string offset (from the strings offset)}, sorted by hash
//! Every entry has its own string (no sharing in vanilla), so the entry order is the strings' order.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub key: u32,
    /// the u16 before the text
    pub value: u16,
    /// the text bytes (UTF-8 in vanilla), without the NUL
    pub text: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lng2 {
    pub version: u32,
    pub tag: [u8; 4],
    /// in string (file) order
    pub entries: Vec<Entry>,
    /// the key table's order as read: indices into `entries`. Keys are sorted by hash, but 14 vanilla files hold
    /// different strings under equal hashes, listed in no derivable order. Empty = stable sort by hash (new files);
    /// ignored unless it is a permutation of the entries whose hashes ascend.
    pub key_order: Vec<usize>,
}

fn be32(b: &[u8], i: usize) -> Result<u32, String> {
    b.get(i..i + 4).map(|s| u32::from_be_bytes(s.try_into().unwrap())).ok_or_else(|| format!("lng2: truncated at {i:#x}"))
}

pub fn read(b: &[u8]) -> Result<Lng2, String> {
    if b.get(0..4) != Some(b"LANG") {
        return Err("not an lng2 (LANG)".into());
    }
    let version = be32(b, 4)?;
    let tag: [u8; 4] = b[8..12].try_into().unwrap();
    let (n, so, ko) = (be32(b, 12)? as usize, be32(b, 16)? as usize, be32(b, 20)? as usize);
    let mut keys = Vec::with_capacity(n);
    for i in 0..n {
        keys.push((be32(b, ko + 8 * i)?, be32(b, ko + 8 * i + 4)? as usize));
    }
    let mut by_off: Vec<(u32, usize, usize)> = keys.iter().enumerate().map(|(i, &(h, o))| (h, o, i)).collect();
    by_off.sort_by_key(|&(_, o, _)| o);
    let mut key_order = vec![0usize; n];
    let mut entries = Vec::with_capacity(n);
    let mut expect = 0usize;
    for (ei, (key, o, ki)) in by_off.into_iter().enumerate() {
        key_order[ki] = ei;
        if o != expect {
            return Err(format!("lng2: string at {o:#x} is not where the previous one ends ({expect:#x})"));
        }
        let p = so + o;
        let value = b.get(p..p + 2).map(|s| u16::from_be_bytes([s[0], s[1]])).ok_or("lng2: string out of range")?;
        let rest = b.get(p + 2..ko).ok_or("lng2: string out of range")?;
        let len = rest.iter().position(|&c| c == 0).ok_or("lng2: unterminated string")?;
        entries.push(Entry { key, value, text: rest[..len].to_vec() });
        expect = o + 2 + len + 1;
    }
    Ok(Lng2 { version, tag, entries, key_order })
}

pub fn write(l: &Lng2) -> Vec<u8> {
    let n = l.entries.len();
    let so = 24usize;
    let mut strings = Vec::new();
    let mut keys: Vec<(u32, u32)> = Vec::with_capacity(n);
    for e in &l.entries {
        keys.push((e.key, strings.len() as u32));
        strings.extend_from_slice(&e.value.to_be_bytes());
        strings.extend_from_slice(&e.text);
        strings.push(0);
    }
    let ko = (so + strings.len() + 4) & !3; // 1-4 zero bytes
    let valid = l.key_order.len() == n && {
        let mut seen = vec![false; n];
        l.key_order.iter().all(|&i| i < n && !std::mem::replace(&mut seen[i], true))
            && l.key_order.windows(2).all(|w| l.entries[w[0]].key <= l.entries[w[1]].key)
    };
    if valid {
        keys = l.key_order.iter().map(|&i| keys[i]).collect();
    } else {
        keys.sort_by_key(|&(h, _)| h); // stable
    }
    let mut o = b"LANG".to_vec();
    o.extend_from_slice(&l.version.to_be_bytes());
    o.extend_from_slice(&l.tag);
    for v in [n as u32, so as u32, ko as u32] {
        o.extend_from_slice(&v.to_be_bytes());
    }
    o.extend_from_slice(&strings);
    o.resize(ko, 0);
    for (h, off) in keys {
        o.extend_from_slice(&h.to_be_bytes());
        o.extend_from_slice(&off.to_be_bytes());
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_small() {
        for texts in [vec!["a"], vec!["ab", "c"], vec!["", "xyz1"]] {
            let l = Lng2 { version: 3, tag: *b"BE\0\0", entries: texts.iter().enumerate().map(|(i, t)| Entry {
                key: 0xFFFF_FFF0 - i as u32 * 7, value: 1 << 8, text: t.as_bytes().to_vec() }).collect(), key_order: vec![] };
            let b = write(&l);
            assert_eq!(b.len() % 4, 0);
            let r = read(&b).unwrap();
            assert_eq!(r.entries, l.entries);
            assert_eq!(write(&r), b);
        }
    }
}
