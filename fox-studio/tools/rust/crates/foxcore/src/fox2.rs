//! Fox Engine data files (.fox2): binary read/write and the datfpk XML dialect (parse/write).
//!
//! Binary layout (Atvaark's FoxTool, MIT, as reference; the string-table rules are datfpk's, proven against its
//! output by tools/rust/tests/regress_fox2.py):
//!   header (32): u32 0x786f62f2, u32 0x35, i32 entity count, i32 string table offset, i32 32, 12 zero bytes
//!   entity (64-byte header + properties): i16 64, u16 class id, 2 zero, "ent\0", u32 address, 4 zero, u32 id,
//!     4 zero, u16 class version, u64 class name hash, u16 static count, u16 dynamic count, i32 64,
//!     u32 static data size (header + static properties), u32 data size; zero pad to 64
//!   property (32-byte header + values, padded to 16): u64 name hash, u8 type, u8 container, u16 count, u16 32,
//!     u16 size, 16 zero bytes; StringMap entries are (u64 key hash, value) each padded to 16
//!   string table: (u64 StrCode64, u32 length, bytes) for every distinct (hash, literal) in first-use order
//!     (entity: class name, then each property: name, then its keys / values in order; an EntityLink registers
//!     nameInArchive, packagePath, archivePath in that order); hash-only strings are written with an empty literal; then u64 0, pad 16, "\0\0end", pad 16.
//!   A second exporter (vanilla .des / .vfxlf) sorts the table by literal (byte order) and leaves hash-only strings
//!   out: read() records which order a file uses (`TableOrder`) and write() follows it.
//! Strings are StrCode64 hashes of their literal; a literal the table does not know (or knows as "") prints as
//! hash="0x...".
use crate::hash::strcode64;
use std::collections::{HashMap, HashSet};

pub const TYPE_NAMES: [&str; 25] = [
    "Int8", "UInt8", "Int16", "UInt16", "Int32", "UInt32", "Int64", "UInt64", "Float", "Double", "Bool", "String",
    "Path", "EntityPtr", "Vector3", "Vector4", "Quat", "Matrix3", "Matrix4", "Color", "FilePtr", "EntityHandle",
    "EntityLink", "PropertyInfo", "WideVector3",
];
pub const CONTAINER_NAMES: [&str; 4] = ["StaticArray", "DynamicArray", "StringMap", "List"];

#[derive(Clone, Debug, PartialEq)]
pub enum Str {
    /// a non-empty literal (hash = StrCode64(literal))
    Lit(String),
    /// only the hash is known (or the literal is empty)
    Hash(u64),
}

impl Str {
    pub fn from_text(s: &str) -> Str {
        if s.is_empty() { Str::Hash(strcode64(b"")) } else { Str::Lit(s.to_string()) }
    }
    pub fn hash(&self) -> u64 {
        match self {
            Str::Lit(s) => strcode64(s.as_bytes()),
            Str::Hash(h) => *h,
        }
    }
    fn table_entry(&self) -> (u64, &[u8]) {
        match self {
            Str::Lit(s) => (strcode64(s.as_bytes()), s.as_bytes()),
            Str::Hash(h) => (*h, b""),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Int(i64),
    UInt64(u64),
    F32(f32),
    F64(f64),
    Bool(bool),
    Str(Str),
    /// EntityPtr / EntityHandle
    Ptr(u64),
    /// Vector3 / Vector4 / Quat (x y z w), Color (r g b a)
    V4([f32; 4]),
    M3([f32; 9]),
    M4([f32; 16]),
    Link { package: Str, archive: Str, name: Str, handle: u64 },
    WideV3 { v: [f32; 3], a: u16, b: u16 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Property {
    pub name: Str,
    pub ty: u8,
    pub container: u8,
    /// (StringMap key, value)
    pub entries: Vec<(Option<Str>, Val)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entity {
    pub class: Str,
    pub class_id: u16,
    pub version: u16,
    pub addr: u32,
    pub id: u32,
    pub statics: Vec<Property>,
    pub dynamics: Vec<Property>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Fox2 {
    pub entities: Vec<Entity>,
    /// the string-table order to write (read() detects it; new files: FirstUse)
    pub table_order: TableOrder,
}

/// String-table order of the binary file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TableOrder {
    /// datfpk's rule (and the vanilla .fox2 files): every distinct (hash, literal) in first-use order, hash-only
    /// strings with an empty literal
    #[default]
    FirstUse,
    /// vanilla .des / .vfxlf: the literals sorted by their bytes, hash-only strings left out
    Sorted,
}

// ------------------------------------------------------------------------------------------------ binary write

struct Table {
    seen: HashSet<(u64, Vec<u8>)>,
    order: Vec<(u64, Vec<u8>)>,
}

impl Table {
    fn add(&mut self, s: &Str) -> u64 {
        let (h, lit) = s.table_entry();
        let k = (h, lit.to_vec());
        if !self.seen.contains(&k) {
            self.seen.insert(k.clone());
            self.order.push(k);
        }
        h
    }
}

fn pad16(b: &mut Vec<u8>) {
    while !b.len().is_multiple_of(16) {
        b.push(0);
    }
}

fn write_val(ty: u8, v: &Val, out: &mut Vec<u8>, t: &mut Table) -> Result<(), String> {
    match (ty, v) {
        (0, Val::Int(i)) => out.push(*i as i8 as u8),
        (1, Val::Int(i)) => out.push(*i as u8),
        (2, Val::Int(i)) => out.extend_from_slice(&(*i as i16).to_le_bytes()),
        (3, Val::Int(i)) => out.extend_from_slice(&(*i as u16).to_le_bytes()),
        (4, Val::Int(i)) => out.extend_from_slice(&(*i as i32).to_le_bytes()),
        (5, Val::Int(i)) => out.extend_from_slice(&(*i as u32).to_le_bytes()),
        (6, Val::Int(i)) => out.extend_from_slice(&i.to_le_bytes()),
        (7, Val::UInt64(u)) => out.extend_from_slice(&u.to_le_bytes()),
        (8, Val::F32(f)) => out.extend_from_slice(&f.to_le_bytes()),
        (9, Val::F64(f)) => out.extend_from_slice(&f.to_le_bytes()),
        (10, Val::Bool(b)) => out.push(*b as u8),
        (11 | 12 | 20, Val::Str(s)) => out.extend_from_slice(&t.add(s).to_le_bytes()),
        (13 | 21, Val::Ptr(p)) => out.extend_from_slice(&p.to_le_bytes()),
        (14 | 15 | 16 | 19, Val::V4(a)) => a.iter().for_each(|f| out.extend_from_slice(&f.to_le_bytes())),
        (17, Val::M3(a)) => a.iter().for_each(|f| out.extend_from_slice(&f.to_le_bytes())),
        (18, Val::M4(a)) => a.iter().for_each(|f| out.extend_from_slice(&f.to_le_bytes())),
        (22, Val::Link { package, archive, name, handle }) => {
            // datfpk registers nameInArchive first, then packagePath, archivePath (string-table order)
            let hn = t.add(name);
            let hp = t.add(package);
            let ha = t.add(archive);
            for h in [hp, ha, hn] {
                out.extend_from_slice(&h.to_le_bytes());
            }
            out.extend_from_slice(&handle.to_le_bytes());
        }
        (24, Val::WideV3 { v, a, b }) => {
            v.iter().for_each(|f| out.extend_from_slice(&f.to_le_bytes()));
            out.extend_from_slice(&a.to_le_bytes());
            out.extend_from_slice(&b.to_le_bytes());
        }
        _ => return Err(format!("value {v:?} does not fit type {}", TYPE_NAMES.get(ty as usize).unwrap_or(&"?"))),
    }
    Ok(())
}

fn write_prop(class: &Str, p: &Property, out: &mut Vec<u8>, t: &mut Table) -> Result<(), String> {
    let start = out.len();
    let nh = t.add(&p.name);
    out.resize(start + 32, 0);
    for (k, v) in &p.entries {
        if p.container == 2 {
            let k = k.as_ref().ok_or("StringMap entry without a key")?;
            out.extend_from_slice(&t.add(k).to_le_bytes());
            write_val(p.ty, v, out, t)?;
            pad16(out);
        } else {
            write_val(p.ty, v, out, t)?;
        }
    }
    pad16(out);
    // the property header's size is a u16: a bigger property wraps, and the game's walk falls into the data and drops
    // the whole fox2 (work/debug/crash/20261005_082423: a StaticModelArray of 1494 transforms). datfpk writes it
    // silently; we refuse (same message as flyk_scatter_pack.Fox2Doc.write).
    let bytes = out.len() - start;
    if bytes > 0xFFFF {
        return Err(format!("{}.{}: {} x {} = {} bytes > 65,535 (fox2 u16 property size): split it", show(class), show(&p.name),
                           p.entries.len(), TYPE_NAMES.get(p.ty as usize).unwrap_or(&"?"), group3(bytes)));
    }
    let size = bytes as u16;
    out[start..start + 8].copy_from_slice(&nh.to_le_bytes());
    out[start + 8] = p.ty;
    out[start + 9] = p.container;
    out[start + 10..start + 12].copy_from_slice(&(p.entries.len() as u16).to_le_bytes());
    out[start + 12..start + 14].copy_from_slice(&32u16.to_le_bytes());
    out[start + 14..start + 16].copy_from_slice(&size.to_le_bytes());
    Ok(())
}

fn show(s: &Str) -> String {
    match s {
        Str::Lit(x) => x.clone(),
        Str::Hash(h) => format!("0x{h:x}"),
    }
}

/// 95648 -> "95,648"
fn group3(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn write(f: &Fox2) -> Result<Vec<u8>, String> {
    let mut t = Table { seen: HashSet::new(), order: Vec::new() };
    let mut out = vec![0u8; 32];
    for e in &f.entities {
        let start = out.len();
        let ch = t.add(&e.class);
        out.resize(start + 64, 0);
        for p in &e.statics {
            write_prop(&e.class, p, &mut out, &mut t)?;
        }
        let static_size = (out.len() - start) as u32;
        for p in &e.dynamics {
            write_prop(&e.class, p, &mut out, &mut t)?;
        }
        let size = (out.len() - start) as u32;
        let h = &mut out[start..start + 64];
        h[0..2].copy_from_slice(&64i16.to_le_bytes());
        h[2..4].copy_from_slice(&e.class_id.to_le_bytes());
        h[6..10].copy_from_slice(&0x746e65u32.to_le_bytes());
        h[10..14].copy_from_slice(&e.addr.to_le_bytes());
        h[18..22].copy_from_slice(&e.id.to_le_bytes());
        h[26..28].copy_from_slice(&e.version.to_le_bytes());
        h[28..36].copy_from_slice(&ch.to_le_bytes());
        h[36..38].copy_from_slice(&(e.statics.len() as u16).to_le_bytes());
        h[38..40].copy_from_slice(&(e.dynamics.len() as u16).to_le_bytes());
        h[40..44].copy_from_slice(&64i32.to_le_bytes());
        h[44..48].copy_from_slice(&static_size.to_le_bytes());
        h[48..52].copy_from_slice(&size.to_le_bytes());
    }
    let table_off = out.len() as i32;
    if f.table_order == TableOrder::Sorted {
        t.order.retain(|(_, lit)| !lit.is_empty());
        t.order.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    }
    for (h, lit) in &t.order {
        out.extend_from_slice(&h.to_le_bytes());
        out.extend_from_slice(&(lit.len() as u32).to_le_bytes());
        out.extend_from_slice(lit);
    }
    out.extend_from_slice(&[0u8; 8]);
    pad16(&mut out);
    out.extend_from_slice(b"\0\0end");
    pad16(&mut out);
    out[0..4].copy_from_slice(&0x786f62f2u32.to_le_bytes());
    out[4..8].copy_from_slice(&0x35u32.to_le_bytes());
    out[8..12].copy_from_slice(&(f.entities.len() as i32).to_le_bytes());
    out[12..16].copy_from_slice(&table_off.to_le_bytes());
    out[16..20].copy_from_slice(&32i32.to_le_bytes());
    Ok(out)
}

// ------------------------------------------------------------------------------------------------ binary read

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let s = self.b.get(self.p..self.p + n).ok_or_else(|| format!("truncated at {}", self.p))?;
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn align16(&mut self) {
        self.p = (self.p + 15) & !15;
    }
}

/// the table as a map (first non-empty literal per hash) and its (hash, literal bytes) entries in file order
type ParsedStringTable = (HashMap<u64, String>, Vec<(u64, Vec<u8>)>);

fn read_table_list(b: &[u8], off: usize) -> Result<ParsedStringTable, String> {
    let mut r = Rd { b, p: off };
    let mut m = HashMap::new();
    let mut list = Vec::new();
    loop {
        let h = r.u64()?;
        if h == 0 {
            break;
        }
        let n = r.u32()? as usize;
        let s = r.take(n)?;
        list.push((h, s.to_vec()));
        // first non-empty literal wins
        let lit: String = String::from_utf8(s.to_vec()).unwrap_or_else(|_| s.iter().map(|&c| c as char).collect());
        let e = m.entry(h).or_insert_with(String::new);
        if e.is_empty() {
            *e = lit;
        }
    }
    Ok((m, list))
}

/// Sorted when the table has no hash-only entries and its literals ascend (byte order); else FirstUse
fn detect_order(list: &[(u64, Vec<u8>)]) -> TableOrder {
    let sorted = list.len() > 1 && list.iter().all(|(_, l)| !l.is_empty())
        && list.windows(2).all(|w| (&w[0].1, w[0].0) < (&w[1].1, w[1].0));
    if sorted { TableOrder::Sorted } else { TableOrder::FirstUse }
}

fn rstr(h: u64, t: &HashMap<u64, String>) -> Str {
    match t.get(&h) {
        Some(s) if !s.is_empty() => Str::Lit(s.clone()),
        _ => Str::Hash(h),
    }
}

fn read_val(ty: u8, r: &mut Rd, t: &HashMap<u64, String>) -> Result<Val, String> {
    Ok(match ty {
        0 => Val::Int(r.u8()? as i8 as i64),
        1 => Val::Int(r.u8()? as i64),
        2 => Val::Int(r.u16()? as i16 as i64),
        3 => Val::Int(r.u16()? as i64),
        4 => Val::Int(r.u32()? as i32 as i64),
        5 => Val::Int(r.u32()? as i64),
        6 => Val::Int(r.u64()? as i64),
        7 => Val::UInt64(r.u64()?),
        8 => Val::F32(r.f32()?),
        9 => Val::F64(f64::from_le_bytes(r.take(8)?.try_into().unwrap())),
        10 => Val::Bool(r.u8()? != 0),
        11 | 12 | 20 => Val::Str(rstr(r.u64()?, t)),
        13 | 21 => Val::Ptr(r.u64()?),
        14 | 15 | 16 | 19 => Val::V4([r.f32()?, r.f32()?, r.f32()?, r.f32()?]),
        17 => {
            let mut a = [0f32; 9];
            for x in a.iter_mut() {
                *x = r.f32()?;
            }
            Val::M3(a)
        }
        18 => {
            let mut a = [0f32; 16];
            for x in a.iter_mut() {
                *x = r.f32()?;
            }
            Val::M4(a)
        }
        22 => Val::Link {
            package: rstr(r.u64()?, t),
            archive: rstr(r.u64()?, t),
            name: rstr(r.u64()?, t),
            handle: r.u64()?,
        },
        24 => Val::WideV3 { v: [r.f32()?, r.f32()?, r.f32()?], a: r.u16()?, b: r.u16()? },
        x => return Err(format!("unsupported property type {x}")),
    })
}

pub fn read(b: &[u8]) -> Result<Fox2, String> {
    let mut r = Rd { b, p: 0 };
    if r.u32()? != 0x786f62f2 {
        return Err("not a fox2 file".into());
    }
    r.u32()?;
    let n = r.u32()? as usize;
    let table_off = r.u32()? as usize;
    let first = r.u32()? as usize;
    let (t, list) = read_table_list(b, table_off)?;
    r.p = first;
    let mut f = Fox2 { table_order: detect_order(&list), ..Fox2::default() };
    for _ in 0..n {
        let start = r.p;
        let _hs = r.u16()?;
        let class_id = r.u16()?;
        r.take(2)?;
        r.u32()?; // "ent\0"
        let addr = r.u32()?;
        r.u32()?;
        let id = r.u32()?;
        r.u32()?;
        let version = r.u16()?;
        let class_hash = r.u64()?;
        let ns = r.u16()? as usize;
        let nd = r.u16()? as usize;
        let poff = r.u32()? as usize;
        r.u32()?;
        let size = r.u32()? as usize;
        r.p = start + poff;
        let mut props = Vec::with_capacity(ns + nd);
        for _ in 0..ns + nd {
            let ps = r.p;
            let nh = r.u64()?;
            let ty = r.u8()?;
            let container = r.u8()?;
            let count = r.u16()? as usize;
            let hoff = r.u16()? as usize;
            let psize = r.u16()? as usize;
            r.p = ps + hoff;
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                if container == 2 {
                    let k = rstr(r.u64()?, &t);
                    let v = read_val(ty, &mut r, &t)?;
                    r.align16();
                    entries.push((Some(k), v));
                } else {
                    entries.push((None, read_val(ty, &mut r, &t)?));
                }
            }
            r.p = ps + psize;
            props.push(Property { name: rstr(nh, &t), ty, container, entries });
        }
        let dynamics = props.split_off(ns);
        f.entities.push(Entity { class: rstr(class_hash, &t), class_id, version, addr, id, statics: props, dynamics });
        r.p = start + size;
    }
    Ok(f)
}

// ------------------------------------------------------------------------------------------------ XML (datfpk dialect)

/// Go strconv.FormatFloat(v, 'g', -1, bits) from shortest digits: `digits` = significant digits (no dot), value =
/// 0.d1d2... * 10^dp with dp = exp10 + 1. Go (ftoa.go formatDigits): %e when exp < -4 || exp >= 6 (shortest uses
/// eprec 6), mantissa with all digits, exponent sign + at least 2 digits; otherwise %f with nd - dp decimals.
fn go_g(digits: &str, exp10: i32, neg: bool) -> String {
    let nd = digits.len() as i32;
    let dp = exp10 + 1;
    let exp = dp - 1;
    let mut s = String::new();
    if neg {
        s.push('-');
    }
    if !(-4..6).contains(&exp) {
        s.push_str(&digits[..1]);
        if nd > 1 {
            s.push('.');
            s.push_str(&digits[1..]);
        }
        s.push('e');
        s.push(if exp < 0 { '-' } else { '+' });
        let a = exp.unsigned_abs();
        if a < 10 {
            s.push('0');
        }
        s.push_str(&a.to_string());
        return s;
    }
    if dp <= 0 {
        s.push_str("0.");
        for _ in 0..(-dp) {
            s.push('0');
        }
        s.push_str(digits);
    } else if dp >= nd {
        s.push_str(digits);
        for _ in 0..(dp - nd) {
            s.push('0');
        }
    } else {
        s.push_str(&digits[..dp as usize]);
        s.push('.');
        s.push_str(&digits[dp as usize..]);
    }
    s
}

fn split_sci(sci: &str) -> (String, i32, bool) {
    // Rust "{:e}" shortest: "-1.2345e-7" / "1e7"
    let neg = sci.starts_with('-');
    let s = sci.trim_start_matches('-');
    let (m, e) = s.split_once('e').unwrap();
    let digits: String = m.chars().filter(|c| *c != '.').collect();
    let digits = digits.trim_end_matches('0').to_string();
    let digits = if digits.is_empty() { "0".to_string() } else { digits };
    (digits, e.parse().unwrap(), neg)
}

pub fn fmt_f32(v: f32) -> String {
    if v == 0.0 {
        return if v.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "+Inf".into() } else { "-Inf".into() };
    }
    // shortest digit count from Rust, digits re-derived by exact rounding (ties to even, as Go picks the closest
    // shortest decimal and breaks exact ties to even: 47.1953125 -> "47.195312", Rust's shortest gives ...313)
    let (d, _, _) = split_sci(&format!("{v:e}"));
    let n = d.len().max(1);
    let (d2, e2, neg2) = split_sci(&format!("{:.*e}", n - 1, v as f64));
    go_g(&d2, e2, neg2)
}

pub fn fmt_f64(v: f64) -> String {
    if v == 0.0 {
        return if v.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "+Inf".into() } else { "-Inf".into() };
    }
    let (d, e, n) = split_sci(&format!("{v:e}"));
    go_g(&d, e, n)
}

/// Go encoding/xml EscapeText
fn esc(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\t' => out.push_str("&#x9;"),
            '\n' => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            c => out.push(c),
        }
    }
}

fn str_attr(name: &str, s: &Str, out: &mut String) {
    match s {
        Str::Lit(l) => {
            out.push(' ');
            out.push_str(name);
            out.push_str("=\"");
            esc(l, out);
            out.push('"');
        }
        Str::Hash(h) => out.push_str(&format!(" {name}Hash=\"0x{h:X}\"")),
    }
}

/// element `tag` (+ key attribute) holding one value, datfpk style
fn xml_val(tag: &str, key: Option<&Str>, v: &Val, out: &mut String) {
    out.push('<');
    out.push_str(tag);
    if let Some(k) = key {
        match k {
            Str::Lit(l) => {
                out.push_str(" key=\"");
                esc(l, out);
                out.push('"');
            }
            Str::Hash(h) => out.push_str(&format!(" key=\"0x{h:X}\"")),
        }
    }
    let mut text = String::new();
    match v {
        Val::Int(i) => text = i.to_string(),
        Val::UInt64(u) => text = u.to_string(),
        Val::F32(f) => text = fmt_f32(*f),
        Val::F64(f) => text = fmt_f64(*f),
        Val::Bool(b) => text = if *b { "true".into() } else { "false".into() },
        Val::Str(Str::Lit(l)) => esc(l, &mut text),
        Val::Str(Str::Hash(h)) => out.push_str(&format!(" hash=\"0x{h:X}\"")),
        Val::Ptr(p) => text = format!("0x{p:X}"),
        Val::V4(a) => {
            // Vector3/Vector4/Quat use x y z w; Color r g b a (the caller passes the type through the tag order)
            for (n, f) in ["x", "y", "z", "w"].iter().zip(a) {
                out.push_str(&format!(" {n}=\"{}\"", fmt_f32(*f)));
            }
        }
        Val::M3(a) => {
            for (i, f) in a.iter().enumerate() {
                out.push_str(&format!(" r{}{}=\"{}\"", i / 3 + 1, i % 3 + 1, fmt_f32(*f)));
            }
        }
        Val::M4(a) => {
            for (i, f) in a.iter().enumerate() {
                out.push_str(&format!(" r{}{}=\"{}\"", i / 4 + 1, i % 4 + 1, fmt_f32(*f)));
            }
        }
        Val::Link { package, archive, name, handle } => {
            str_attr("packagePath", package, out);
            str_attr("archivePath", archive, out);
            str_attr("nameInArchive", name, out);
            text = format!("0x{handle:X}");
        }
        Val::WideV3 { v, a, b } => {
            for (n, f) in ["x", "y", "z"].iter().zip(v) {
                out.push_str(&format!(" {n}=\"{}\"", fmt_f32(*f)));
            }
            out.push_str(&format!(" a=\"{a}\" b=\"{b}\""));
        }
    }
    out.push('>');
    out.push_str(&text);
    out.push_str("</");
    out.push_str(tag);
    out.push('>');
}

fn color_fix(s: String) -> String {
    // Color values use r g b a instead of x y z w
    s.replacen(" x=\"", " r=\"", 1).replacen(" y=\"", " g=\"", 1).replacen(" z=\"", " b=\"", 1).replacen(" w=\"", " a=\"", 1)
}

fn xml_prop(p: &Property, out: &mut String) {
    out.push_str("        <property name=\"");
    match &p.name {
        Str::Lit(l) => esc(l, out),
        Str::Hash(h) => out.push_str(&format!("0x{h:X}")),
    }
    out.push_str(&format!(
        "\" type=\"{}\" container=\"{}\"",
        TYPE_NAMES.get(p.ty as usize).unwrap_or(&"Unknown"),
        CONTAINER_NAMES.get(p.container as usize).unwrap_or(&"Unknown"),
    ));
    if !p.entries.is_empty() {
        // datfpk omits arraySize="0" (Go omitempty)
        out.push_str(&format!(" arraySize=\"{}\"", p.entries.len()));
    }
    out.push('>');
    for (k, v) in &p.entries {
        let mut one = String::new();
        if p.container == 2 {
            out.push_str("\n          <containerEntry key=\"");
            match k.as_ref().unwrap_or(&Str::Hash(0)) {
                Str::Lit(l) => esc(l, out),
                Str::Hash(h) => out.push_str(&format!("0x{h:X}")),
            }
            out.push_str("\">\n            ");
            xml_val("data", None, v, &mut one);
            out.push_str(&if p.ty == 19 { color_fix(one) } else { one });
            out.push_str("\n          </containerEntry>");
        } else {
            out.push_str("\n          ");
            xml_val("containerEntry", None, v, &mut one);
            out.push_str(&if p.ty == 19 { color_fix(one) } else { one });
        }
    }
    if !p.entries.is_empty() {
        out.push_str("\n        ");
    }
    out.push_str("</property>");
}

/// The datfpk XML text of a fox2 (what `datfpk file.fox2 file.fox2.xml` writes).
pub fn to_xml(f: &Fox2) -> String {
    let mut o = String::with_capacity(4096);
    // tableOrder is ours (not datfpk's): only written for the sorted string-table layout, so datfpk XML is unchanged
    o.push_str(if f.table_order == TableOrder::Sorted {
        "<fox formatVersion=\"2\" fileVersion=\"0\" tableOrder=\"sorted\">\n  <entities>"
    } else {
        "<fox formatVersion=\"2\" fileVersion=\"0\">\n  <entities>"
    });
    for e in &f.entities {
        o.push_str("\n    <entity class=\"");
        match &e.class {
            Str::Lit(l) => esc(l, &mut o),
            Str::Hash(h) => o.push_str(&format!("0x{h:X}")),
        }
        o.push_str(&format!(
            "\" classVersion=\"{}\" classID=\"0x{:X}\" addr=\"0x{:X}\" id=\"0x{:X}\">",
            e.version, e.class_id, e.addr, e.id
        ));
        for (tag, props) in [("staticProperties", &e.statics), ("dynamicProperties", &e.dynamics)] {
            o.push_str(&format!("\n      <{tag}>"));
            for p in props {
                o.push('\n');
                xml_prop(p, &mut o);
            }
            if !props.is_empty() {
                o.push_str("\n      ");
            }
            o.push_str(&format!("</{tag}>"));
        }
        o.push_str("\n    </entity>");
    }
    if !f.entities.is_empty() {
        o.push_str("\n  ");
    }
    o.push_str("</entities>\n</fox>");
    o
}

// ------------------------------------------------------------------------------------------------ XML parse

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

fn attrs(e: &BytesStart) -> Result<HashMap<String, String>, String> {
    let mut m = HashMap::new();
    for a in e.attributes() {
        let a = a.map_err(|x| x.to_string())?;
        let k = a.key.0.to_string();
        #[allow(deprecated)]
        let v = a.unescape_value().map_err(|x| x.to_string())?.to_string();
        m.insert(k, v);
    }
    Ok(m)
}

fn num_u64(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).map_err(|e| format!("{s}: {e}"))
    } else {
        s.parse::<u64>().map_err(|e| format!("{s}: {e}"))
    }
}

fn num_i64(s: &str) -> Result<i64, String> {
    let s = s.trim();
    if s.starts_with("0x") || s.starts_with("0X") {
        Ok(num_u64(s)? as i64)
    } else {
        s.parse::<i64>().map_err(|e| format!("{s}: {e}"))
    }
}

fn f32_of(s: &str) -> Result<f32, String> {
    let s = s.trim();
    // datfpk / Go ParseFloat(s, 32): correctly rounded straight to float32
    s.parse::<f32>().map_err(|e| format!("{s}: {e}"))
}

fn str_of_attrs(a: &HashMap<String, String>, lit: &str) -> Result<Str, String> {
    if let Some(h) = a.get(&format!("{lit}Hash")) {
        return Ok(Str::Hash(num_u64(h)?));
    }
    Ok(Str::from_text(a.get(lit).map(|s| s.as_str()).unwrap_or("")))
}

fn make_val(ty: u8, a: &HashMap<String, String>, text: &str) -> Result<Val, String> {
    let g = |k: &str| -> Result<f32, String> { f32_of(a.get(k).ok_or_else(|| format!("missing attribute {k}"))?) };
    Ok(match ty {
        0..=6 => Val::Int(num_i64(text)?),
        7 => Val::UInt64(num_u64(text)?),
        8 => Val::F32(f32_of(text)?),
        9 => Val::F64(text.trim().parse::<f64>().map_err(|e| e.to_string())?),
        10 => Val::Bool(match text.trim() {
            "true" | "True" | "1" => true,
            "false" | "False" | "0" => false,
            x => return Err(format!("bad bool {x}")),
        }),
        11 | 12 | 20 => match a.get("hash") {
            Some(h) => Val::Str(Str::Hash(num_u64(h)?)),
            None => Val::Str(Str::from_text(text)),
        },
        13 | 21 => Val::Ptr(if text.trim().is_empty() { 0 } else { num_u64(text)? }),
        14..=16 => Val::V4([g("x")?, g("y")?, g("z")?, g("w")?]),
        19 => Val::V4([g("r")?, g("g")?, g("b")?, g("a")?]),
        17 => {
            let mut m = [0f32; 9];
            for (i, x) in m.iter_mut().enumerate() {
                *x = g(&format!("r{}{}", i / 3 + 1, i % 3 + 1))?;
            }
            Val::M3(m)
        }
        18 => {
            let mut m = [0f32; 16];
            for (i, x) in m.iter_mut().enumerate() {
                *x = g(&format!("r{}{}", i / 4 + 1, i % 4 + 1))?;
            }
            Val::M4(m)
        }
        22 => Val::Link {
            package: str_of_attrs(a, "packagePath")?,
            archive: str_of_attrs(a, "archivePath")?,
            name: str_of_attrs(a, "nameInArchive")?,
            handle: if text.trim().is_empty() { 0 } else { num_u64(text)? },
        },
        24 => Val::WideV3 {
            v: [g("x")?, g("y")?, g("z")?],
            a: num_u64(a.get("a").map(|s| s.as_str()).unwrap_or("0"))? as u16,
            b: num_u64(a.get("b").map(|s| s.as_str()).unwrap_or("0"))? as u16,
        },
        x => return Err(format!("unsupported property type {x}")),
    })
}

/// Parse the datfpk XML dialect (also accepts self-closing elements and the hand/Python-written variants seen in
/// this project: hash="" attributes, lists on one line).
pub fn from_xml(text: &str) -> Result<Fox2, String> {
    let mut r = Reader::from_str(text);
    r.config_mut().trim_text(false);
    let mut f = Fox2::default();
    let mut cur_e: Option<Entity> = None;
    let mut in_dynamic = false;
    let mut cur_p: Option<Property> = None;
    // pending value element: (attrs, text, key)
    let mut pend: Option<(HashMap<String, String>, String)> = None;
    let mut key: Option<Str> = None;
    let mut depth_val = false;
    loop {
        let ev = r.read_event().map_err(|e| format!("xml error at {}: {e}", r.buffer_position()))?;
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(ev, Event::Empty(_));
                let name = e.name().0.to_string();
                let a = attrs(e)?;
                match name.as_str() {
                    "fox" => {
                        f.table_order = match a.get("tableOrder").map(|s| s.as_str()) {
                            None | Some("firstUse") => TableOrder::FirstUse,
                            Some("sorted") => TableOrder::Sorted,
                            Some(x) => return Err(format!("fox tableOrder=\"{x}\" (expected sorted or firstUse)")),
                        };
                    }
                    "entity" => {
                        cur_e = Some(Entity {
                            class: Str::from_text(a.get("class").map(|s| s.as_str()).unwrap_or("")),
                            class_id: num_u64(a.get("classID").map(|s| s.as_str()).unwrap_or("0"))? as u16,
                            version: num_u64(a.get("classVersion").map(|s| s.as_str()).unwrap_or("0"))? as u16,
                            addr: num_u64(a.get("addr").map(|s| s.as_str()).unwrap_or("0"))? as u32,
                            id: num_u64(a.get("id").map(|s| s.as_str()).unwrap_or("0"))? as u32,
                            statics: vec![],
                            dynamics: vec![],
                        });
                        if empty {
                            f.entities.push(cur_e.take().unwrap());
                        }
                    }
                    "staticProperties" => in_dynamic = false,
                    "dynamicProperties" => in_dynamic = true,
                    "property" => {
                        let ty = TYPE_NAMES.iter().position(|t| Some(*t) == a.get("type").map(|s| s.as_str()))
                            .ok_or_else(|| format!("unknown type {:?}", a.get("type")))? as u8;
                        let container = CONTAINER_NAMES.iter().position(|t| Some(*t) == a.get("container").map(|s| s.as_str()))
                            .ok_or_else(|| format!("unknown container {:?}", a.get("container")))? as u8;
                        let p = Property { name: Str::from_text(a.get("name").map(|s| s.as_str()).unwrap_or("")), ty, container, entries: vec![] };
                        if empty {
                            let e = cur_e.as_mut().ok_or("property outside an entity")?;
                            if in_dynamic { e.dynamics.push(p) } else { e.statics.push(p) }
                        } else {
                            cur_p = Some(p);
                        }
                    }
                    "containerEntry" | "data" => {
                        let p = cur_p.as_ref().ok_or("value outside a property")?;
                        if name == "containerEntry" && p.container == 2 {
                            let k = a.get("key").cloned().unwrap_or_default();
                            key = Some(if k.starts_with("0x") && k.len() > 2 && u64::from_str_radix(&k[2..], 16).is_ok() {
                                Str::Hash(num_u64(&k)?)
                            } else {
                                Str::from_text(&k)
                            });
                            continue;
                        }
                        if empty {
                            let v = make_val(p.ty, &a, "")?;
                            let k = key.take();
                            cur_p.as_mut().unwrap().entries.push((k, v));
                        } else {
                            pend = Some((a, String::new()));
                            depth_val = true;
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(text) if depth_val => {
                if let Some((_, value)) = pend.as_mut() {
                    let raw = text[..].to_string();
                    let unescaped = quick_xml::escape::unescape(&raw).map_err(|error| error.to_string())?;
                    value.push_str(&unescaped);
                }
            }
            Event::GeneralRef(reference) if depth_val => {
                if let Some((_, value)) = pend.as_mut() {
                    let name = reference[..].to_string();
                    let entity = format!("&{name};");
                    let unescaped = quick_xml::escape::unescape(&entity).map_err(|error| error.to_string())?;
                    value.push_str(&unescaped);
                }
            }
            Event::End(ref e) => {
                let name = e.name().0.to_string();
                match name.as_str() {
                    "containerEntry" | "data" => {
                        if let Some((a, s)) = pend.take() {
                            let p = cur_p.as_mut().ok_or("value outside a property")?;
                            let v = make_val(p.ty, &a, &s)?;
                            let k = if p.container == 2 { key.take() } else { None };
                            p.entries.push((k, v));
                        }
                        depth_val = false;
                    }
                    "property" => {
                        if let Some(p) = cur_p.take() {
                            let e = cur_e.as_mut().ok_or("property outside an entity")?;
                            if in_dynamic { e.dynamics.push(p) } else { e.statics.push(p) }
                        }
                    }
                    "entity" => {
                        if let Some(e) = cur_e.take() {
                            f.entities.push(e);
                        }
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(f)
}

/// datfpk XML -> fox2 bytes
pub fn compile(xml: &str) -> Result<Vec<u8>, String> {
    write(&from_xml(xml)?)
}

/// fox2 bytes -> datfpk XML
pub fn decompile(b: &[u8]) -> Result<String, String> {
    Ok(to_xml(&read(b)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_property_refused() {
        let m = Val::M4([0.0; 16]);
        let mk = |n: usize| Fox2 { entities: vec![Entity {
            class: Str::Lit("StaticModelArray".into()), class_id: 0, version: 0, addr: 0x10000000, id: 1,
            statics: vec![Property { name: Str::Lit("transforms".into()), ty: 18, container: 3,
                                     entries: (0..n).map(|_| (None, m.clone())).collect() }],
            dynamics: vec![] }], ..Fox2::default() };
        assert!(write(&mk(1000)).is_ok());
        let e = write(&mk(1494)).unwrap_err();
        assert_eq!(e, "StaticModelArray.transforms: 1494 x Matrix4 = 95,648 bytes > 65,535 (fox2 u16 property size): split it");
    }
    #[test]
    fn go_float_text() {
        assert_eq!(fmt_f32(-47.195_313), "-47.195312");
        assert_eq!(fmt_f32(1e7), "1e+07");
        assert_eq!(fmt_f32(0.99999994), "0.99999994");
        assert_eq!(fmt_f32(555.926), "555.926");
        assert_eq!(fmt_f32(4294967295.0), "4.2949673e+09");
        assert_eq!(fmt_f32(0.0001), "0.0001");
        assert_eq!(fmt_f32(0.00001), "1e-05");
        assert_eq!(fmt_f32(123456.0), "123456");
    }
}
