//! FoxPackage (.fpk / .fpkd) read and write.
//!
//! Layout (GzsTool FpkFile, MIT; identical bytes to datfpk v0.3.0 output, proven by
//! tools/rust/tests/regress_fpk.py over every pack of the FLYK build):
//!   header (48): "foxfpk", type (0x00 fpk | 'd' fpkd), "win", u32 file size, 18 zero bytes, u32 2,
//!                u32 entry count, u32 reference count, 4 zero bytes
//!   entries (48 each): u32 data offset, 0, u32 data size, 0, u32 path offset, 0, u32 path length, 0,
//!                md5(path text) [16]
//!   references (16 each): u32 path offset, 0, u32 path length, 0
//!   strings: every entry path then every reference path, each NUL-terminated; zero pad to 16
//!   data: each entry's bytes, zero padded to 16 after each
use md5::{Digest, Md5};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Fpk,
    Fpkd,
}

impl Kind {
    pub fn byte(self) -> u8 {
        match self {
            Kind::Fpk => 0,
            Kind::Fpkd => b'd',
        }
    }
    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "fpk" => Some(Kind::Fpk),
            "fpkd" => Some(Kind::Fpkd),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: String,
    pub offset: u32,
    pub size: u32,
    pub md5: [u8; 16],
}

#[derive(Clone, Debug)]
pub struct Package {
    pub kind: Kind,
    pub entries: Vec<Entry>,
    pub references: Vec<String>,
}

fn pad16(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(16) {
        out.push(0);
    }
}

fn put_u32(out: &mut [u8], at: usize, v: u32) {
    out[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// Entry type as the vanilla order rules see it: everything after the FIRST dot of the basename, lowercased
/// ("/x/a.1.nav2" -> "1.nav2", "/x/b.eng.lng" -> "eng.lng").
pub fn entry_type(path: &str) -> String {
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    base.find('.')
        .map(|i| base[i + 1..].to_ascii_lowercase())
        .unwrap_or_default()
}

/// The vanilla type-order rules of a pack kind (crate::packorder_rules, learned from every vanilla pack by
/// tools/debug/packorder.py): (A, B) = every A entry precedes every B entry.
#[cfg(feature = "internal-game-data")]
pub fn order_rules(kind: Kind) -> &'static [(&'static str, &'static str)] {
    match kind {
        Kind::Fpk => crate::packorder_rules::FPK,
        Kind::Fpkd => crate::packorder_rules::FPKD,
    }
}

/// Rule violations of an entry order: (index of the last A, index of the first B, A, B) for every rule broken.
#[cfg(feature = "internal-game-data")]
pub fn order_violations(
    kind: Kind,
    paths: &[&str],
) -> Vec<(usize, usize, &'static str, &'static str)> {
    let types: Vec<String> = paths.iter().map(|p| entry_type(p)).collect();
    let mut out = vec![];
    for &(a, b) in order_rules(kind) {
        let last_a = types.iter().rposition(|t| t == a);
        let first_b = types.iter().position(|t| t == b);
        if let (Some(la), Some(fb)) = (last_a, first_b) {
            if la > fb {
                out.push((la, fb, a, b));
            }
        }
    }
    out
}

/// The canonical type rank of a pack kind (crate::packorder_rules::*_RANK).
#[cfg(feature = "internal-game-data")]
pub fn order_rank(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Fpk => crate::packorder_rules::FPK_RANK,
        Kind::Fpkd => crate::packorder_rules::FPKD_RANK,
    }
}

/// tools/debug/packorder.py sort_entries, exactly: a stable sort by (rank of the entry's type, 0); a type without a
/// rank gets (rank of the previous known entry, 1), i.e. it travels with what precedes it (-1 before any known
/// entry). Returns the permutation (indices into `paths`); Err when the result still breaks a rule (the Python
/// raises RuntimeError there). Unlike vanilla_order this GROUPS types, so it can move entries that break no rule:
/// it is what flyk_m2.datfpk_pack applies before packing; fpk::write's vanilla_order then leaves the result alone.
#[cfg(feature = "internal-game-data")]
pub fn sort_entries(kind: Kind, paths: &[&str]) -> Result<Vec<usize>, String> {
    let rank: std::collections::HashMap<&str, i64> = order_rank(kind)
        .iter()
        .enumerate()
        .map(|(i, t)| (*t, i as i64))
        .collect();
    let mut cur = -1i64;
    let keys: Vec<(i64, u8)> = paths
        .iter()
        .map(|p| match rank.get(entry_type(p).as_str()) {
            Some(&r) => {
                cur = r;
                (r, 0)
            }
            None => (cur, 1),
        })
        .collect();
    let mut idx: Vec<usize> = (0..paths.len()).collect();
    idx.sort_by_key(|&i| keys[i]); // stable
    let sorted: Vec<&str> = idx.iter().map(|&i| paths[i]).collect();
    let v = order_violations(kind, &sorted);
    if let Some((a, b, ta, tb)) = v.first() {
        return Err(format!(
            "sort_entries left {} violation(s), e.g. .{ta} ({}) after .{tb} ({})",
            v.len(),
            sorted[*a],
            sorted[*b]
        ));
    }
    Ok(idx)
}

/// Entry order the game needs. Vanilla packs list entry TYPES in a fixed order (every .fpkd puts all .fox2 first;
/// every .fpk puts .mtar before .fmdl, ...: 472 fpk + 136 fpkd rules, no vanilla exception). Breaking one crashes the
/// game (work/debug/crash/20261005_063455: hornbill .parts before its fox2; 07:28: stork .mtar after the .fmdls).
/// Returns a STABLE topological order: repeatedly the earliest remaining entry whose type has no remaining entry of
/// a type that must precede it. An order that breaks no rule (every vanilla pack, packorder.sort_entries output)
/// comes back unchanged; types without rules keep their place.
#[cfg(feature = "internal-game-data")]
pub fn vanilla_order(kind: Kind, paths: &[&str]) -> Vec<usize> {
    let n = paths.len();
    let types: Vec<String> = paths.iter().map(|p| entry_type(p)).collect();
    let rules = order_rules(kind);
    // per entry: the rule types that must come before it
    let preds: Vec<Vec<&str>> = types
        .iter()
        .map(|t| rules.iter().filter(|r| r.1 == t).map(|r| r.0).collect())
        .collect();
    let mut remaining: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for t in &types {
        *remaining.entry(t.as_str()).or_default() += 1;
    }
    let mut done = vec![false; n];
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        let pick = (0..n)
            .find(|&i| {
                !done[i]
                    && preds[i]
                        .iter()
                        .all(|a| remaining.get(a).copied().unwrap_or(0) == 0)
            })
            // the rules are acyclic (learned as a DAG); fall back to input order rather than loop
            .unwrap_or_else(|| (0..n).find(|&i| !done[i]).unwrap());
        done[pick] = true;
        *remaining.get_mut(types[pick].as_str()).unwrap() -= 1;
        out.push(pick);
    }
    out
}

/// GzsTool's view of an entry (FpkEntry.ReadData, GzsTool v0.6): Fox's obfuscated scripts (first byte 0x1B or 0x1C)
/// come out decrypted. key = !StrCode64(lowercase file name with extension) as 8 little-endian bytes; for every byte
/// after the first: key[i % 8] ^= data[i + 1], out[i] = key[i % 8]; valid when the last output byte is 0 (dropped).
/// Everything that passes through GzsTool unpack -> repack (MakeBite's ContentHash of pack entries, SnakeBite's merged
/// packs) carries this plain form; the game reads both. None = not an obfuscated entry (use the bytes as they are).
pub fn gzs_decrypt(path: &str, data: &[u8]) -> Option<Vec<u8>> {
    if data.is_empty() || (data[0] != 0x1B && data[0] != 0x1C) {
        return None;
    }
    let p = path.replace('\\', "/");
    let name = p.rsplit('/').next().unwrap_or(&p).to_lowercase();
    let mut key = (!crate::hash::strcode64(name.as_bytes())).to_le_bytes();
    let mut out = Vec::with_capacity(data.len() - 1);
    for (i, &b) in data[1..].iter().enumerate() {
        key[i % 8] ^= b;
        out.push(key[i % 8]);
    }
    if out.last() != Some(&0) {
        return None;
    }
    out.pop();
    Some(out)
}

/// gzs_decrypt or the bytes unchanged
pub fn gzs_view<'a>(path: &str, data: &'a [u8]) -> std::borrow::Cow<'a, [u8]> {
    match gzs_decrypt(path, data) {
        Some(v) => std::borrow::Cow::Owned(v),
        None => std::borrow::Cow::Borrowed(data),
    }
}

/// Build a package from (entry path, data) pairs and reference paths. Entries keep the given order unless it breaks a
/// vanilla type-order rule, then they are reordered minimally (vanilla_order; byte-identical to datfpk whenever the
/// input already follows the rules).
#[cfg(feature = "internal-game-data")]
pub fn write(kind: Kind, entries: &[(&str, &[u8])], references: &[&str]) -> Vec<u8> {
    let paths: Vec<&str> = entries.iter().map(|(p, _)| *p).collect();
    let order = vanilla_order(kind, &paths);
    if order.iter().enumerate().any(|(i, &j)| i != j) {
        let sorted: Vec<(&str, &[u8])> = order.iter().map(|&i| entries[i]).collect();
        return write_in_order(kind, &sorted, references);
    }
    write_in_order(kind, entries, references)
}

/// Build a package with the entries exactly in the given order (no vanilla ordering; for tests and round trips of
/// existing packages).
pub fn write_in_order(kind: Kind, entries: &[(&str, &[u8])], references: &[&str]) -> Vec<u8> {
    let n = entries.len();
    let r = references.len();
    let table = 48 + 48 * n + 16 * r;
    let total_data: usize = entries.iter().map(|(_, d)| d.len() + 15).sum();
    let mut out = vec![0u8; table];
    out.reserve(total_data + 256 * (n + r));
    let mut spos = Vec::with_capacity(n + r);
    for p in entries
        .iter()
        .map(|(p, _)| *p)
        .chain(references.iter().copied())
    {
        spos.push((out.len() as u32, p.len() as u32));
        out.extend_from_slice(p.as_bytes());
        out.push(0);
    }
    pad16(&mut out);
    let mut dpos = Vec::with_capacity(n);
    for (_, d) in entries {
        dpos.push((out.len() as u32, d.len() as u32));
        out.extend_from_slice(d);
        pad16(&mut out);
    }
    let size = out.len() as u32;
    out[0..6].copy_from_slice(b"foxfpk");
    out[6] = kind.byte();
    out[7..10].copy_from_slice(b"win");
    put_u32(&mut out, 10, size);
    put_u32(&mut out, 32, 2);
    put_u32(&mut out, 36, n as u32);
    put_u32(&mut out, 40, r as u32);
    for (i, (p, _)) in entries.iter().enumerate() {
        let at = 48 + 48 * i;
        put_u32(&mut out, at, dpos[i].0);
        put_u32(&mut out, at + 8, dpos[i].1);
        put_u32(&mut out, at + 16, spos[i].0);
        put_u32(&mut out, at + 24, spos[i].1);
        let h = Md5::digest(p.as_bytes());
        out[at + 32..at + 48].copy_from_slice(&h);
    }
    for i in 0..r {
        let at = 48 + 48 * n + 16 * i;
        put_u32(&mut out, at, spos[n + i].0);
        put_u32(&mut out, at + 8, spos[n + i].1);
    }
    out
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, String> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
        .ok_or_else(|| format!("truncated at {at}"))
}

fn str_at(b: &[u8], off: u32, len: u32) -> Result<String, String> {
    let start = off as usize;
    let end = start
        .checked_add(len as usize)
        .ok_or("string range overflow")?;
    let s = b.get(start..end).ok_or("string out of range")?;
    Ok(s.iter().map(|&c| c as char).collect()) // latin-1, as the Python readers
}

/// Parse the header and tables (entry data stays in `bytes`: use offset/size).
pub fn read(bytes: &[u8]) -> Result<Package, String> {
    read_metadata(bytes, bytes.len())
}

/// Parse a header/table/string region read from a file whose full length is known.
/// Every payload range is checked against that length before it is returned.
pub fn read_metadata(bytes: &[u8], total_size: usize) -> Result<Package, String> {
    if bytes.len() < 48 || &bytes[0..6] != b"foxfpk" {
        return Err("not a FoxPackage".into());
    }
    let kind = match bytes[6] {
        0 | b' ' => Kind::Fpk,
        b'd' => Kind::Fpkd,
        x => return Err(format!("unknown package type byte {x:#x}")),
    };
    let n = u32_at(bytes, 36)? as usize;
    let r = u32_at(bytes, 40)? as usize;
    let entries_end = n
        .checked_mul(48)
        .and_then(|s| s.checked_add(48))
        .ok_or("entry table size overflow")?;
    let table_end = r
        .checked_mul(16)
        .and_then(|s| entries_end.checked_add(s))
        .ok_or("reference table size overflow")?;
    if table_end > bytes.len() || table_end > total_size {
        return Err("package tables exceed the available file".into());
    }
    let mut entries = Vec::with_capacity(n);
    for i in 0..n {
        let at = 48 + 48 * i;
        let md5: [u8; 16] = bytes
            .get(at + 32..at + 48)
            .ok_or("truncated entry")?
            .try_into()
            .unwrap();
        let offset = u32_at(bytes, at)?;
        let size = u32_at(bytes, at + 8)?;
        if (offset as usize)
            .checked_add(size as usize)
            .is_none_or(|end| end > total_size)
        {
            return Err(format!("package entry {i} payload exceeds the file"));
        }
        entries.push(Entry {
            offset,
            size,
            path: str_at(bytes, u32_at(bytes, at + 16)?, u32_at(bytes, at + 24)?)?,
            md5,
        });
    }
    let mut references = Vec::with_capacity(r);
    for i in 0..r {
        let at = 48 + 48 * n + 16 * i;
        references.push(str_at(bytes, u32_at(bytes, at)?, u32_at(bytes, at + 8)?)?);
    }
    Ok(Package {
        kind,
        entries,
        references,
    })
}

/// Write with ordering learned during local setup.
pub fn write_with_order(
    order: &crate::runtime_data::PackOrder,
    kind: Kind,
    entries: &[(&str, &[u8])],
    references: &[&str],
) -> Result<Vec<u8>, String> {
    order.write(kind, entries, references)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(feature = "internal-game-data")]
    fn fpkd_fox2_first() {
        let e: [(&str, &[u8]); 4] = [
            ("/Assets/a.parts", b"p"),
            ("/Assets/b.fox2", b"f"),
            ("/Assets/c.tgt", b"t"),
            ("/Assets/d.fox2", b"g"),
        ];
        let p = read(&write(Kind::Fpkd, &e, &[])).unwrap();
        let names: Vec<&str> = p.entries.iter().map(|x| x.path.as_str()).collect();
        assert_eq!(
            names,
            [
                "/Assets/b.fox2",
                "/Assets/d.fox2",
                "/Assets/a.parts",
                "/Assets/c.tgt"
            ]
        );
        // already ordered input: identical bytes to the plain writer; fpk: order untouched
        let ok: [(&str, &[u8]); 2] = [("/Assets/b.fox2", b"f"), ("/Assets/a.parts", b"p")];
        assert_eq!(
            write(Kind::Fpkd, &ok, &[]),
            write_in_order(Kind::Fpkd, &ok, &[])
        );
    }
    #[test]
    #[cfg(feature = "internal-game-data")]
    fn fpk_mtar_before_fmdl() {
        let e: [(&str, &[u8]); 4] = [
            ("/Assets/a.fmdl", b"m"),
            ("/Assets/b.mtar", b"t"),
            ("/Assets/c.frdv", b"f"),
            ("/Assets/z.unknownx", b"u"),
        ];
        let p = read(&write(Kind::Fpk, &e, &[])).unwrap();
        let names: Vec<&str> = p.entries.iter().map(|x| x.path.as_str()).collect();
        assert_eq!(
            names,
            [
                "/Assets/b.mtar",
                "/Assets/a.fmdl",
                "/Assets/c.frdv",
                "/Assets/z.unknownx"
            ]
        );
        assert!(order_violations(Kind::Fpk, &names).is_empty());
        let ok: [(&str, &[u8]); 3] = [
            ("/Assets/b.mtar", b"t"),
            ("/Assets/a.fmdl", b"m"),
            ("/Assets/q.unknownx", b"u"),
        ];
        assert_eq!(
            write(Kind::Fpk, &ok, &[]),
            write_in_order(Kind::Fpk, &ok, &[])
        );
        assert_eq!(entry_type("/a/b/x.1.nav2"), "1.nav2");
    }
    #[test]
    #[cfg(feature = "internal-game-data")]
    fn sort_entries_like_packorder_py() {
        // fmdl before mtar in the input: grouped by rank (mtar < fmdl), the unknown type travels with its predecessor
        let p = ["/a/x.fmdl", "/a/q.unknownx", "/a/y.mtar", "/a/z.fmdl"];
        let o = sort_entries(Kind::Fpk, &p).unwrap();
        let got: Vec<&str> = o.iter().map(|&i| p[i]).collect();
        assert_eq!(
            got,
            ["/a/y.mtar", "/a/x.fmdl", "/a/z.fmdl", "/a/q.unknownx"]
        );
        // an unknown type first stays first (rank -1)
        let p = ["/a/q.unknownx", "/a/x.fmdl", "/a/y.mtar"];
        let o = sort_entries(Kind::Fpk, &p).unwrap();
        assert_eq!(o, [0, 2, 1]);
    }
    #[test]
    fn roundtrip_small() {
        let b = write_in_order(
            Kind::Fpk,
            &[("/Assets/a.bin", b"hello")],
            &["/Assets/r.fpk"],
        );
        assert_eq!(b.len() % 16, 0);
        let p = read(&b).unwrap();
        assert_eq!(p.entries[0].path, "/Assets/a.bin");
        assert_eq!(&b[p.entries[0].offset as usize..][..5], b"hello");
        assert_eq!(p.references, vec!["/Assets/r.fpk".to_string()]);
    }
    #[test]
    fn empty_is_header_only() {
        assert_eq!(write_in_order(Kind::Fpkd, &[], &[]).len(), 48);
    }
}
