//! Fox Engine animation archives (.mtar) and their motions (.gani): structural read / write.
//!
//! A port of tools/anim/gani.py (our Python reference; format notes in docs/formats/gani.md). `write()` places every
//! structure at its recorded position, so vanilla archives come back byte-identical (gani.py check A); the regression
//! test tools/rust/tests/regress_mtar.py compares this port with gani.py and with the original files.
//! Keys are kept raw (u128: up to 4 x 32-bit components); value decoding lives in the anim tools.

pub const QUAT: u8 = 0;
pub const QUAT_DIFF: u8 = 5;
pub const UNIT_STATIC: u8 = 4;
pub const MTAR_VERSION: u32 = 201403250;
pub const MTAR_USE_MINI: u16 = 0x1000;
pub const NODE_LAYOUT: u32 = 0x4FBDAAEF;
pub const NODE_SKL_LIST: u32 = 0x91E4534B;
pub const NODE_MTP_LIST: u32 = 0x3B9A7784;

fn align(n: usize, a: usize) -> usize {
    n.div_ceil(a) * a
}

type R<T> = Result<T, String>;

fn get<const N: usize>(d: &[u8], p: usize) -> R<[u8; N]> {
    d.get(p..p + N).map(|s| s.try_into().unwrap()).ok_or_else(|| format!("truncated at {p}"))
}
fn u8_(d: &[u8], p: usize) -> R<u8> {
    Ok(get::<1>(d, p)?[0])
}
fn u16_(d: &[u8], p: usize) -> R<u16> {
    Ok(u16::from_le_bytes(get(d, p)?))
}
fn i16_(d: &[u8], p: usize) -> R<i16> {
    Ok(i16::from_le_bytes(get(d, p)?))
}
fn u32_(d: &[u8], p: usize) -> R<u32> {
    Ok(u32::from_le_bytes(get(d, p)?))
}
fn i32_(d: &[u8], p: usize) -> R<i32> {
    Ok(i32::from_le_bytes(get(d, p)?))
}
fn u64_(d: &[u8], p: usize) -> R<u64> {
    Ok(u64::from_le_bytes(get(d, p)?))
}

fn put(buf: &mut Vec<u8>, p: usize, b: &[u8]) {
    if buf.len() < p + b.len() {
        buf.resize(p + b.len(), 0);
    }
    buf[p..p + b.len()].copy_from_slice(b);
}

// ------------------------------------------------------------------------------------------------ bit streams

struct BitReader<'a> {
    d: &'a [u8],
    base: usize,
    bit: usize,
}

impl BitReader<'_> {
    fn read(&mut self, n: usize) -> R<u128> {
        if n == 0 {
            return Ok(0);
        }
        let start = self.base + (self.bit >> 3);
        let end = self.base + ((self.bit + n + 7) >> 3);
        if end > self.d.len() {
            return Err("bit read past end".into());
        }
        let mut v: u128 = 0;
        for (i, b) in self.d[start..end].iter().enumerate() {
            v |= (*b as u128) << (8 * i);
        }
        v >>= self.bit & 7;
        self.bit += n;
        Ok(if n >= 128 { v } else { v & ((1u128 << n) - 1) })
    }
    fn nbytes(&self) -> usize {
        (self.bit + 7) >> 3
    }
}

struct BitWriter {
    out: Vec<u8>,
    bit: usize,
}

impl BitWriter {
    fn write(&mut self, x: u128, n: usize) -> R<()> {
        if n == 0 {
            return Ok(());
        }
        if n < 128 && x >> n != 0 {
            return Err(format!("value does not fit {n} bits"));
        }
        for i in 0..n {
            let b = self.bit + i;
            if b >> 3 >= self.out.len() {
                self.out.push(0);
            }
            if (x >> i) & 1 == 1 {
                self.out[b >> 3] |= 1 << (b & 7);
            }
        }
        self.bit += n;
        Ok(())
    }
}

pub fn key_bits(typ: u8, bits: u8) -> usize {
    match typ {
        0 | 5 => 3 * bits as usize + 3,
        1 => bits as usize,
        2 => 2 * bits as usize,
        3 | 6 => 3 * bits as usize,
        4 => 4 * bits as usize,
        _ => 0,
    }
}

#[derive(Clone, Debug, Default)]
pub struct Seg {
    pub typ: u8,
    pub bits: u8,
    pub keys: Vec<(u32, u128)>,
    pub static_: bool,
    pub present: bool,
}

fn read_keys(d: &[u8], pos: usize, typ: u8, bits: u8, static_: bool, frames: u32) -> R<(Vec<(u32, u128)>, usize)> {
    let mut br = BitReader { d, base: pos, bit: 0 };
    let kb = key_bits(typ, bits);
    let mut keys = vec![(0u32, br.read(kb)?)];
    if !static_ {
        let mut f = 0u32;
        while f < frames {
            let fc = br.read(8)? as u32;
            if fc == 0 {
                return Err("zero frame delta".into());
            }
            f += fc;
            keys.push((f, br.read(kb)?));
        }
    }
    let n = br.nbytes();
    Ok((keys, n + (n & 1)))
}

pub fn write_keys(s: &Seg) -> R<Vec<u8>> {
    let mut bw = BitWriter { out: vec![], bit: 0 };
    let kb = key_bits(s.typ, s.bits);
    bw.write(s.keys[0].1, kb)?;
    if !s.static_ {
        let mut prev = 0u32;
        for (f, r) in &s.keys[1..] {
            let df = f - prev;
            if !(1..256).contains(&df) {
                return Err(format!("frame delta {df} out of range"));
            }
            bw.write(df as u128, 8)?;
            bw.write(*r, kb)?;
            prev = *f;
        }
    }
    let mut b = bw.out;
    if b.len() & 1 == 1 {
        b.push(0);
    }
    Ok(b)
}

// ------------------------------------------------------------------------------------------------ TrackHeader

#[derive(Clone, Debug, Default)]
pub struct Unit {
    pub name: u32,
    pub flags: u8,
    pub segs: Vec<Seg>,
    pub seg_ids: Vec<i16>,
    pub nexts: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct Track {
    pub units: Vec<Unit>,
    pub id: u16,
    pub kind: u8,
    pub frames: u32,
    pub rate: u8,
    pub rate_pad: [u8; 3],
    pub no_data: bool,
    /// recorded layout: unit offsets, stream positions per segment index, end
    unit_offs: Vec<u32>,
    data: std::collections::BTreeMap<usize, (usize, usize)>,
    end: usize,
}

impl Track {
    pub fn read(d: &[u8], base: usize, no_data: bool) -> R<Track> {
        let n_units = i32_(d, base)? as usize;
        let n_segs = u32_(d, base + 4)? as usize;
        let mut t = Track {
            id: u16_(d, base + 8)?,
            kind: u8_(d, base + 11)?,
            frames: u32_(d, base + 12)?,
            rate: u8_(d, base + 16)?,
            rate_pad: get(d, base + 17)?,
            no_data,
            ..Default::default()
        };
        if u8_(d, base + 10)? != 0 {
            return Err("track byte 10 != 0".into());
        }
        let mut end = base + 20 + 4 * n_units;
        let mut sid_total = 0usize;
        for i in 0..n_units {
            let uo = u32_(d, base + 20 + 4 * i)?;
            t.unit_offs.push(uo);
            let p = base + uo as usize;
            let nseg = u8_(d, p + 4)? as usize;
            let mut u = Unit { name: u32_(d, p)?, flags: u8_(d, p + 5)?, ..Default::default() };
            if u16_(d, p + 6)? != 0 {
                return Err("unit pad != 0".into());
            }
            for k in 0..nseg {
                let q = p + 8 + 8 * k;
                let doff = i32_(d, q)?;
                let sid = i16_(d, q + 4)?;
                let tb = u8_(d, q + 6)?;
                let bits = u8_(d, q + 7)?;
                let (typ, nxt) = (tb & 0xF, tb >> 4);
                let mut seg = Seg { typ, bits, keys: vec![], static_: u.flags & UNIT_STATIC != 0, present: doff != 0 };
                u.seg_ids.push(sid);
                u.nexts.push(nxt);
                if doff != 0 {
                    let sp = (q as i64 + doff as i64) as usize;
                    if !no_data {
                        let (keys, ln) = read_keys(d, sp, typ, bits, seg.static_, t.frames)?;
                        seg.keys = keys;
                        t.data.insert(sid_total, (sp - base, ln));
                        end = end.max(sp + ln);
                    } else {
                        t.data.insert(sid_total, (sp - base, 0));
                    }
                }
                u.segs.push(seg);
                sid_total += 1;
            }
            end = end.max(p + 8 + 8 * nseg);
            t.units.push(u);
        }
        t.end = end - base;
        if sid_total != n_segs {
            return Err(format!("segment count {sid_total} != {n_segs}"));
        }
        Ok(t)
    }

    pub fn seg_count(&self) -> usize {
        self.units.iter().map(|u| u.segs.len()).sum()
    }

    /// recorded-layout write (byte-identical rewrite of a read track)
    pub fn write(&self) -> R<Vec<u8>> {
        let n = self.units.len();
        let unit_offs = &self.unit_offs;
        let p_units_end =
            if n > 0 { unit_offs.iter().zip(&self.units).map(|(uo, u)| *uo as usize + 8 + 8 * u.segs.len()).max().unwrap() } else { 20 };
        let mut buf = vec![0u8; self.end.max(p_units_end)];
        put(&mut buf, 0, &(n as i32).to_le_bytes());
        put(&mut buf, 4, &(self.seg_count() as u32).to_le_bytes());
        put(&mut buf, 8, &self.id.to_le_bytes());
        put(&mut buf, 10, &[0, self.kind]);
        put(&mut buf, 12, &self.frames.to_le_bytes());
        put(&mut buf, 16, &[self.rate]);
        put(&mut buf, 17, &self.rate_pad);
        for (i, uo) in unit_offs.iter().enumerate() {
            put(&mut buf, 20 + 4 * i, &uo.to_le_bytes());
        }
        let mut k = 0usize;
        for (uo, u) in unit_offs.iter().zip(&self.units) {
            let uo = *uo as usize;
            put(&mut buf, uo, &u.name.to_le_bytes());
            put(&mut buf, uo + 4, &[u.segs.len() as u8, u.flags, 0, 0]);
            for (j, s) in u.segs.iter().enumerate() {
                let q = uo + 8 + 8 * j;
                let doff: i32 = if s.present { self.data.get(&k).map(|(o, _)| *o as i64 - q as i64).unwrap_or(0) as i32 } else { 0 };
                put(&mut buf, q, &doff.to_le_bytes());
                put(&mut buf, q + 4, &u.seg_ids[j].to_le_bytes());
                put(&mut buf, q + 6, &[s.typ | (u.nexts[j] << 4), s.bits]);
                if s.present && !s.keys.is_empty() && !self.no_data {
                    let b = write_keys(s)?;
                    let at = self.data[&k].0;
                    put(&mut buf, at, &b);
                }
                k += 1;
            }
        }
        Ok(buf)
    }
}

// ------------------------------------------------------------------------------------------------ events (EVP)

#[derive(Clone, Debug)]
pub struct Event {
    pub name: u32,
    pub fmt: u8,
    pub sections: Vec<(i32, i32)>,
    pub ints: Vec<u32>,
    pub floats: Vec<u32>,
    pub strings: Vec<u64>,
}

fn sec_size(fmt: u8) -> usize {
    match fmt {
        0 => 8,
        1 => 4,
        2 => 2,
        _ => 0,
    }
}

fn read_event(d: &[u8], p: usize) -> R<(Event, usize)> {
    let name = u32_(d, p)?;
    let info = u8_(d, p + 4)?;
    let (ni, nf, ns) = (u8_(d, p + 5)? as usize, u8_(d, p + 6)? as usize, u8_(d, p + 7)? as usize);
    let (nsec, fmt) = ((info & 0x3F) as usize, info >> 6);
    let mut q = p + 8;
    let mut secs = vec![];
    for _ in 0..nsec {
        let s = match fmt {
            0 => (i32_(d, q)?, i32_(d, q + 4)?),
            1 => (i16_(d, q)? as i32, i16_(d, q + 2)? as i32),
            2 => (u8_(d, q)? as i8 as i32, u8_(d, q + 1)? as i8 as i32),
            _ => (-1, -1),
        };
        secs.push(s);
        q += sec_size(fmt);
    }
    q = align(q, 4);
    let mut ints = vec![];
    for i in 0..ni {
        ints.push(u32_(d, q + 4 * i)?);
    }
    q += 4 * ni;
    let mut floats = vec![];
    for i in 0..nf {
        floats.push(u32_(d, q + 4 * i)?);
    }
    q += 4 * nf;
    let mut strings = vec![];
    for i in 0..ns {
        strings.push(u64_(d, q + 8 * i)?);
    }
    q += 8 * ns;
    Ok((Event { name, fmt, sections: secs, ints, floats, strings }, q - p))
}

fn write_event(e: &Event, start_pos: usize) -> Vec<u8> {
    let mut o = vec![];
    o.extend_from_slice(&e.name.to_le_bytes());
    o.push(e.sections.len() as u8 | (e.fmt << 6));
    o.extend_from_slice(&[e.ints.len() as u8, e.floats.len() as u8, e.strings.len() as u8]);
    for s in &e.sections {
        match e.fmt {
            0 => {
                o.extend_from_slice(&s.0.to_le_bytes());
                o.extend_from_slice(&s.1.to_le_bytes());
            }
            1 => {
                o.extend_from_slice(&(s.0 as i16).to_le_bytes());
                o.extend_from_slice(&(s.1 as i16).to_le_bytes());
            }
            2 => {
                o.push(s.0 as i8 as u8);
                o.push(s.1 as i8 as u8);
            }
            _ => {}
        }
    }
    while !(start_pos + o.len()).is_multiple_of(4) {
        o.push(0);
    }
    for v in &e.ints {
        o.extend_from_slice(&v.to_le_bytes());
    }
    for v in &e.floats {
        o.extend_from_slice(&v.to_le_bytes());
    }
    for v in &e.strings {
        o.extend_from_slice(&v.to_le_bytes());
    }
    o
}

#[derive(Clone, Debug)]
pub struct EvpGroup {
    pub category: u32,
    pub events: Vec<Event>,
    pub cache: Option<Vec<u8>>,
    event_offs: Vec<u32>,
    cache_off: u16,
}

#[derive(Clone, Debug)]
pub struct Evp {
    pub version: u32,
    pub groups: Vec<EvpGroup>,
    pub size: usize,
    group_offs: Vec<u32>,
}

fn ag_cache_size(d: &[u8], p: usize) -> R<usize> {
    let (fo, fc) = (u32_(d, p)? as usize, u32_(d, p + 4)? as usize);
    let (to, tc) = (u32_(d, p + 16)? as usize, u32_(d, p + 20)? as usize);
    let mut end = p + 24;
    if fo != 0 {
        end = end.max(p + fo + 4 * fc);
    }
    if to != 0 {
        end = end.max(p + 16 + to + 8 * tc);
    }
    Ok(end - p)
}

impl Evp {
    pub fn read(d: &[u8], base: usize) -> R<Evp> {
        let version = u32_(d, base)?;
        let cnt = i16_(d, base + 4)? as usize;
        if u16_(d, base + 6)? != 0 {
            return Err("evp pad != 0".into());
        }
        let mut ev = Evp { version, groups: vec![], size: 0, group_offs: vec![] };
        let mut end = base + 8 + 4 * cnt;
        for i in 0..cnt {
            let go = u32_(d, base + 8 + 4 * i)?;
            ev.group_offs.push(go);
            let g0 = base + go as usize;
            let cat = u32_(d, g0)?;
            let nu = u16_(d, g0 + 4)? as usize;
            let co = u16_(d, g0 + 6)?;
            let mut g = EvpGroup { category: cat, events: vec![], cache: None, event_offs: vec![], cache_off: co };
            let mut gend = g0 + 8 + 4 * nu;
            for k in 0..nu {
                let uo = u32_(d, g0 + 8 + 4 * k)?;
                g.event_offs.push(uo);
                let (e, ln) = read_event(d, g0 + uo as usize)?;
                g.events.push(e);
                gend = gend.max(g0 + uo as usize + ln);
            }
            if co != 0 {
                let cs = ag_cache_size(d, g0 + co as usize)?;
                g.cache = Some(d[g0 + co as usize..g0 + co as usize + cs].to_vec());
                gend = gend.max(g0 + co as usize + cs);
            }
            ev.groups.push(g);
            end = end.max(gend);
        }
        ev.size = end - base;
        Ok(ev)
    }

    pub fn write(&self) -> Vec<u8> {
        let n = self.groups.len();
        let mut out = vec![];
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend_from_slice(&(n as i16).to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out.resize(8 + 4 * n, 0);
        for (gi, g) in self.groups.iter().enumerate() {
            let g0 = self.group_offs[gi] as usize;
            if out.len() < g0 {
                out.resize(g0, 0);
            }
            let mut blk = vec![0u8; 8 + 4 * g.events.len()];
            for (ei, e) in g.events.iter().enumerate() {
                let eo = g.event_offs[ei] as usize;
                let b = write_event(e, g0 + eo);
                put(&mut blk, eo, &b);
            }
            if let Some(c) = &g.cache {
                put(&mut blk, g.cache_off as usize, c);
            }
            put(&mut blk, 0, &g.category.to_le_bytes());
            put(&mut blk, 4, &(g.events.len() as u16).to_le_bytes());
            put(&mut blk, 6, &g.cache_off.to_le_bytes());
            for (k, eo) in g.event_offs.iter().enumerate() {
                put(&mut blk, 8 + 4 * k, &eo.to_le_bytes());
            }
            put(&mut out, g0, &blk);
            put(&mut out, 8 + 4 * gi, &(g0 as u32).to_le_bytes());
        }
        out
    }
}

// ------------------------------------------------------------------------------------------------ mini gani

#[derive(Clone, Debug, Default)]
pub struct Gani {
    pub hash: u64,
    pub frames: u32,
    pub params: Vec<(u32, u32)>,
    pub unit_flags: Vec<u8>,
    pub segs: Vec<Seg>,
    pub mtp: Option<Track>,
    pub events: Option<Evp>,
    pub foxdata: Option<FoxData>,
    // recorded layout
    gap: Vec<u8>,
    streams: Vec<Option<usize>>,
    table: (u32, u16, u16, u16, u32),
    ev_pad: Vec<u8>,
    fd_table: (u32, u32),
    fd_pad: Vec<u8>,
}

fn read_gani2(d: &[u8], base: usize, layout: &Track) -> R<Gani> {
    let mut g = Gani { frames: u32_(d, base)?, ..Default::default() };
    if u8_(d, base + 4)? != 0 || u16_(d, base + 6)? != 0 {
        return Err("gani header pad".into());
    }
    let npar = u8_(d, base + 5)? as usize;
    let mut q = base + 8;
    for _ in 0..npar {
        g.params.push((u32_(d, q)?, u32_(d, q + 4)?));
        q += 8;
    }
    let nu = layout.units.len();
    g.unit_flags = d.get(q..q + nu).ok_or("truncated unit flags")?.to_vec();
    q = base + align(q - base + nu, 4);
    let seg_types: Vec<(u8, usize)> =
        layout.units.iter().enumerate().flat_map(|(ui, u)| u.segs.iter().map(move |s| (s.typ, ui))).collect();
    let ns = seg_types.len();
    g.gap = d.get(q + 4 * ns..q + 4 * ns + 16).ok_or("truncated gap")?.to_vec();
    for (k, (typ, ui)) in seg_types.iter().enumerate() {
        let h = u32_(d, q + 4 * k)?;
        let (bits, off) = ((h & 0xFF) as u8, (h >> 8) as usize);
        let st = g.unit_flags[*ui] & UNIT_STATIC != 0;
        if off == 0 {
            g.segs.push(Seg { typ: *typ, bits, keys: vec![], static_: st, present: false });
            g.streams.push(None);
            continue;
        }
        let sp = q + 4 * k + off;
        let (keys, _ln) = read_keys(d, sp, *typ, bits, st, g.frames)?;
        g.segs.push(Seg { typ: *typ, bits, keys, static_: st, present: true });
        g.streams.push(Some(sp - base));
    }
    Ok(g)
}

fn write_gani2(g: &Gani) -> R<Vec<u8>> {
    let mut out = vec![];
    out.extend_from_slice(&g.frames.to_le_bytes());
    out.extend_from_slice(&[0, g.params.len() as u8, 0, 0]);
    for (nm, v) in &g.params {
        out.extend_from_slice(&nm.to_le_bytes());
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&g.unit_flags);
    out.resize(align(out.len(), 4), 0);
    let hp = out.len();
    out.resize(hp + 4 * g.segs.len(), 0);
    out.extend_from_slice(&g.gap);
    for (k, s) in g.segs.iter().enumerate() {
        if !s.present {
            continue;
        }
        let b = write_keys(s)?;
        let sp = g.streams[k].ok_or("stream position missing")?;
        put(&mut out, sp, &b);
        let off = sp - (hp + 4 * k);
        put(&mut out, hp + 4 * k, &((s.bits as u32) | ((off as u32) << 8)).to_le_bytes());
    }
    let size = g.table.1 as usize * 16;
    out.resize(size.max(out.len()), 0);
    if out.len() != size {
        return Err("gani block larger than its recorded size".into());
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------------------ FoxData

#[derive(Clone, Debug)]
enum Payload {
    Track(usize, Track),
    Evp(usize, Evp),
    Strings(usize, Vec<(u32, u32)>),
    Raw(usize, Vec<u8>),
}

#[derive(Clone, Debug)]
struct FdParam {
    pos: usize,
    typ: u16,
    next: i16,
    name: (u32, u32),
    value: (bool, u32, u32), // (is name, a, b)
}

#[derive(Clone, Debug)]
struct FdNode {
    pos: usize,
    hash: u32,
    stroff: u32,
    fields: (u32, i32, u32, i32, i32, i32, i32, i32),
    params: Vec<FdParam>,
    payload: Option<Payload>,
}

#[derive(Clone, Debug)]
pub struct FoxData {
    size: usize,
    header: [u32; 6],
    strings: Vec<(usize, Vec<u8>)>,
    nodes: Vec<FdNode>,
}

fn cstr(b: &[u8], p: usize) -> R<Vec<u8>> {
    let e = b[p..].iter().position(|&c| c == 0).ok_or("unterminated string")?;
    Ok(b[p..p + e].to_vec())
}

impl FoxData {
    pub fn read(d: &[u8], base: usize, size: usize) -> R<FoxData> {
        let b = d.get(base..base + size).ok_or("foxdata out of range")?;
        let mut header = [0u32; 6];
        for (i, h) in header.iter_mut().enumerate() {
            *h = u32_(b, 4 * i)?;
        }
        let mut f = FoxData { size, header, strings: vec![], nodes: vec![] };
        if header[4] != 0 {
            let p = 12 + header[4] as usize;
            f.strings.push((p, cstr(b, p)?));
        }
        let mut seen = std::collections::HashSet::new();
        f.walk(b, header[1] as usize, &mut seen)?;
        Ok(f)
    }

    fn name(&mut self, b: &[u8], p: usize) -> R<(u32, u32)> {
        let (h, so) = (u32_(b, p)?, u32_(b, p + 4)?);
        if so != 0 {
            let sp = p + so as usize;
            let s = cstr(b, sp)?;
            if !self.strings.iter().any(|(q, _)| *q == sp) {
                self.strings.push((sp, s));
            }
        }
        Ok((h, so))
    }

    fn walk(&mut self, b: &[u8], mut p: usize, seen: &mut std::collections::HashSet<usize>) -> R<()> {
        while seen.insert(p) {
            let (h, so) = self.name(b, p)?;
            let fl = u32_(b, p + 8)?;
            let fields = (fl, i32_(b, p + 12)?, u32_(b, p + 16)?, i32_(b, p + 20)?, i32_(b, p + 24)?, i32_(b, p + 28)?,
                          i32_(b, p + 32)?, i32_(b, p + 36)?);
            let (doff, dsz, chi, nxt, prm) = (fields.1, fields.2, fields.4, fields.6, fields.7);
            let mut node = FdNode { pos: p, hash: h, stroff: so, fields, params: vec![], payload: None };
            if prm != 0 {
                let mut q = (p as i64 + prm as i64) as usize;
                loop {
                    let typ = u16_(b, q)?;
                    let nx = i16_(b, q + 2)?;
                    let name = self.name(b, q + 4)?;
                    let value = if typ == 1 {
                        let (vh, vso) = self.name(b, q + 12)?;
                        (true, vh, vso)
                    } else {
                        (false, u32_(b, q + 12)?, 0)
                    };
                    node.params.push(FdParam { pos: q, typ, next: nx, name, value });
                    if nx == 0 {
                        break;
                    }
                    q = (q as i64 + nx as i64) as usize;
                }
            }
            if doff != 0 && (fl & 1 != 0 || dsz != 0) {
                let dp = (p as i64 + doff as i64) as usize;
                node.payload = Some(match fl {
                    1 => Payload::Track(dp, Track::read(b, dp, false)?),
                    3 => Payload::Evp(dp, Evp::read(b, dp)?),
                    0 => {
                        let cnt = u32_(b, dp)? as usize;
                        let mut ents = vec![];
                        for i in 0..cnt {
                            ents.push(self.name(b, dp + 4 + 8 * i)?);
                        }
                        Payload::Strings(dp, ents)
                    }
                    _ => Payload::Raw(dp, b[dp..dp + dsz as usize].to_vec()),
                });
            }
            self.nodes.push(node);
            if chi != 0 {
                self.walk(b, (p as i64 + chi as i64) as usize, seen)?;
            }
            if nxt == 0 {
                break;
            }
            p = (p as i64 + nxt as i64) as usize;
        }
        Ok(())
    }

    pub fn first_track_frames(&self) -> Option<u32> {
        self.nodes.iter().find_map(|n| match &n.payload {
            Some(Payload::Track(_, t)) => Some(t.frames),
            _ => None,
        })
    }

    pub fn write(&self) -> R<Vec<u8>> {
        let mut b = vec![0u8; self.size];
        for (i, h) in self.header.iter().enumerate() {
            put(&mut b, 4 * i, &h.to_le_bytes());
        }
        for (p, s) in &self.strings {
            put(&mut b, *p, s);
            put(&mut b, *p + s.len(), &[0]);
        }
        for n in &self.nodes {
            let p = n.pos;
            put(&mut b, p, &n.hash.to_le_bytes());
            put(&mut b, p + 4, &n.stroff.to_le_bytes());
            let f = n.fields;
            put(&mut b, p + 8, &f.0.to_le_bytes());
            put(&mut b, p + 12, &f.1.to_le_bytes());
            put(&mut b, p + 16, &f.2.to_le_bytes());
            put(&mut b, p + 20, &f.3.to_le_bytes());
            put(&mut b, p + 24, &f.4.to_le_bytes());
            put(&mut b, p + 28, &f.5.to_le_bytes());
            put(&mut b, p + 32, &f.6.to_le_bytes());
            put(&mut b, p + 36, &f.7.to_le_bytes());
            for pr in &n.params {
                let q = pr.pos;
                put(&mut b, q, &pr.typ.to_le_bytes());
                put(&mut b, q + 2, &pr.next.to_le_bytes());
                put(&mut b, q + 4, &pr.name.0.to_le_bytes());
                put(&mut b, q + 8, &pr.name.1.to_le_bytes());
                if pr.value.0 {
                    put(&mut b, q + 12, &pr.value.1.to_le_bytes());
                    put(&mut b, q + 16, &pr.value.2.to_le_bytes());
                } else {
                    put(&mut b, q + 12, &pr.value.1.to_le_bytes());
                }
            }
            match &n.payload {
                Some(Payload::Track(dp, t)) => {
                    let blob = t.write()?;
                    put(&mut b, *dp, &blob);
                }
                Some(Payload::Evp(dp, e)) => {
                    let blob = e.write();
                    put(&mut b, *dp, &blob);
                }
                Some(Payload::Strings(dp, ents)) => {
                    let mut blob = (ents.len() as u32).to_le_bytes().to_vec();
                    for (h, so) in ents {
                        blob.extend_from_slice(&h.to_le_bytes());
                        blob.extend_from_slice(&so.to_le_bytes());
                    }
                    put(&mut b, *dp, &blob);
                }
                Some(Payload::Raw(dp, raw)) => put(&mut b, *dp, raw),
                None => {}
            }
        }
        b.truncate(self.size);
        Ok(b)
    }
}

// ------------------------------------------------------------------------------------------------ archive

#[derive(Clone, Debug)]
pub struct Mtar {
    pub version: u32,
    pub unit_count: u16,
    pub seg_count: u16,
    pub shader_nodes: u16,
    pub shader_units: u16,
    pub mtp_units: u16,
    pub flags: u16,
    pub layout_track: Option<Track>,
    pub ganis: Vec<Gani>,
    size: usize,
    cio: u32,
    /// common-info chain as read: (name, dsize, next, pz, raw body)
    common: Vec<(u32, u32, u32, u32, Vec<u8>)>,
    layout_track_raw_size: usize,
}

impl Mtar {
    pub fn mini(&self) -> bool {
        self.flags & MTAR_USE_MINI != 0
    }

    pub fn read(d: &[u8]) -> R<Mtar> {
        let version = u32_(d, 0)?;
        if version != MTAR_VERSION {
            return Err(format!("mtar version {version}"));
        }
        let n = u32_(d, 4)? as usize;
        let mut m = Mtar {
            version,
            unit_count: u16_(d, 8)?,
            seg_count: u16_(d, 10)?,
            shader_nodes: u16_(d, 12)?,
            shader_units: u16_(d, 14)?,
            mtp_units: u16_(d, 16)?,
            flags: u16_(d, 18)?,
            layout_track: None,
            ganis: vec![],
            size: d.len(),
            cio: u32_(d, 20)?,
            common: vec![],
            layout_track_raw_size: 0,
        };
        if m.mini() {
            let mut p = m.cio as usize;
            loop {
                let (name, dsize, nxt, pz) = (u32_(d, p)?, u32_(d, p + 4)?, u32_(d, p + 8)?, u32_(d, p + 12)?);
                let body = p + 16;
                if name == NODE_LAYOUT {
                    m.layout_track = Some(Track::read(d, body, true)?);
                    m.layout_track_raw_size = dsize as usize;
                }
                m.common.push((name, dsize, nxt, pz, d.get(body..body + dsize as usize).ok_or("common out of range")?.to_vec()));
                if nxt == 0 {
                    break;
                }
                p += nxt as usize;
            }
            let lt = m.layout_track.clone().ok_or("no layout track")?;
            for i in 0..n {
                let e = 0x20 + 0x20 * i;
                let h = u64_(d, e)?;
                let (uto, uts, mpo, mps) = (u32_(d, e + 8)?, u16_(d, e + 12)?, u16_(d, e + 14)?, u16_(d, e + 16)?);
                let (sto, sts, p0, meo, p1) = (u16_(d, e + 18)?, u16_(d, e + 20)?, u16_(d, e + 22)?, u32_(d, e + 24)?, u32_(d, e + 28)?);
                if sto != 0 || sts != 0 || p0 != 0 || p1 != 0 {
                    return Err("shader tracks in a mini gani".into());
                }
                let mut g = read_gani2(d, uto as usize, &lt)?;
                g.hash = h;
                g.table = (uto, uts, mpo, mps, meo);
                if mpo != 0 {
                    g.mtp = Some(Track::read(d, uto as usize + mpo as usize * 16, false)?);
                }
                if meo != 0 {
                    g.events = Some(Evp::read(d, meo as usize)?);
                }
                m.ganis.push(g);
            }
            let mut evs: Vec<(u32, usize)> =
                m.ganis.iter().enumerate().filter(|(_, g)| g.events.is_some()).map(|(i, g)| (g.table.4, i)).collect();
            evs.sort();
            for k in 0..evs.len() {
                let (meo, i) = evs[k];
                let nxt = evs.get(k + 1).map(|x| x.0 as usize).unwrap_or(d.len());
                let end = meo as usize + m.ganis[i].events.as_ref().unwrap().size;
                m.ganis[i].ev_pad = d.get(end..nxt).unwrap_or(&[]).to_vec();
            }
        } else {
            for i in 0..n {
                let e = 0x20 + 0x10 * i;
                let (h, off, sz) = (u64_(d, e)?, u32_(d, e + 8)?, u32_(d, e + 12)?);
                let fd = FoxData::read(d, off as usize, sz as usize)?;
                let g = Gani { hash: h, frames: fd.first_track_frames().unwrap_or(0), foxdata: Some(fd), fd_table: (off, sz),
                               ..Default::default() };
                m.ganis.push(g);
            }
            let mut spans: Vec<(u32, u32, usize)> = m.ganis.iter().enumerate().map(|(i, g)| (g.fd_table.0, g.fd_table.1, i)).collect();
            spans.sort();
            for k in 0..spans.len() {
                let (off, sz, i) = spans[k];
                let nxt = spans.get(k + 1).map(|x| x.0 as usize).unwrap_or(d.len());
                m.ganis[i].fd_pad = d.get(off as usize + sz as usize..nxt).unwrap_or(&[]).to_vec();
            }
        }
        Ok(m)
    }

    /// byte-identical rewrite of a read archive (recorded layout)
    pub fn write(&self) -> R<Vec<u8>> {
        let n = self.ganis.len();
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| self.ganis[i].hash);
        if !self.mini() {
            let mut out = vec![0u8; align(0x20 + 0x10 * n, 16)];
            let mut data_order: Vec<usize> = (0..n).collect();
            data_order.sort_by_key(|&i| self.ganis[i].fd_table.0);
            let mut table = vec![(0u32, 0u32); n];
            for &i in &data_order {
                let g = &self.ganis[i];
                let fd = g.foxdata.as_ref().unwrap();
                let blob = fd.write()?;
                if (g.fd_table.0 as usize) > out.len() {
                    out.resize(g.fd_table.0 as usize, 0);
                }
                let off = out.len();
                let padn = align(blob.len(), 16) - blob.len();
                out.extend_from_slice(&blob);
                if g.fd_pad.len() >= padn {
                    out.extend_from_slice(&g.fd_pad);
                } else {
                    out.resize(out.len() + padn, 0);
                }
                table[i] = (off as u32, fd.size as u32);
            }
            for (slot, &i) in order.iter().enumerate() {
                let e = 0x20 + 0x10 * slot;
                put(&mut out, e, &self.ganis[i].hash.to_le_bytes());
                put(&mut out, e + 8, &table[i].0.to_le_bytes());
                put(&mut out, e + 12, &table[i].1.to_le_bytes());
            }
            self.write_header(&mut out, n, 0);
            if self.size > out.len() {
                out.resize(self.size, 0);
            }
            return Ok(out);
        }
        let mut out = vec![0u8; 0x20 + 0x20 * n];
        let cio = out.len();
        // common info, recorded chain
        let nc = self.common.len();
        for (k, (name, _dsize, nxt, _pz, raw)) in self.common.iter().enumerate() {
            let body: Vec<u8> = if *name == NODE_LAYOUT {
                let lt = self.layout_track.as_ref().unwrap().write()?;
                lt[..self.layout_track_raw_size.min(lt.len())].to_vec()
            } else {
                raw.clone()
            };
            let node_pos = out.len();
            let nx = if k + 1 < nc { if *nxt == 0 { align(16 + body.len(), 16) as u32 } else { *nxt } } else { 0 };
            out.extend_from_slice(&name.to_le_bytes());
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(&nx.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&body);
            if nx != 0 && out.len() < node_pos + nx as usize {
                out.resize(node_pos + nx as usize, 0);
            }
        }
        out.resize(align(out.len(), 16), 0);
        let mut data_order: Vec<usize> = (0..n).collect();
        data_order.sort_by_key(|&i| self.ganis[i].table.0);
        let mut table = vec![[0u32; 5]; n];
        for &i in &data_order {
            let g = &self.ganis[i];
            let blk = write_gani2(g)?;
            let uto = out.len();
            out.extend_from_slice(&blk);
            let (mut mpo, mut mps) = (0, 0);
            if let Some(t) = &g.mtp {
                let mut mt = t.write()?;
                mpo = blk.len() / 16;
                mt.resize(align(mt.len(), 16), 0);
                mps = mt.len() / 16;
                out.extend_from_slice(&mt);
            }
            table[i] = [uto as u32, (blk.len() / 16) as u32, mpo as u32, mps as u32, 0];
        }
        for &i in &data_order {
            let g = &self.ganis[i];
            let Some(ev) = &g.events else { continue };
            table[i][4] = out.len() as u32;
            let e = ev.write();
            let padn = align(e.len(), 16) - e.len();
            out.extend_from_slice(&e);
            if g.ev_pad.len() >= padn {
                out.extend_from_slice(&g.ev_pad);
            } else {
                out.resize(out.len() + padn, 0);
            }
        }
        for (slot, &i) in order.iter().enumerate() {
            let e = 0x20 + 0x20 * slot;
            let t = table[i];
            put(&mut out, e, &self.ganis[i].hash.to_le_bytes());
            put(&mut out, e + 8, &t[0].to_le_bytes());
            put(&mut out, e + 12, &(t[1] as u16).to_le_bytes());
            put(&mut out, e + 14, &(t[2] as u16).to_le_bytes());
            put(&mut out, e + 16, &(t[3] as u16).to_le_bytes());
            put(&mut out, e + 18, &[0u8; 6]);
            put(&mut out, e + 24, &t[4].to_le_bytes());
            put(&mut out, e + 28, &0u32.to_le_bytes());
        }
        self.write_header(&mut out, n, cio as u32);
        Ok(out)
    }

    fn write_header(&self, out: &mut Vec<u8>, n: usize, cio: u32) {
        put(out, 0, &self.version.to_le_bytes());
        put(out, 4, &(n as u32).to_le_bytes());
        for (i, v) in [self.unit_count, self.seg_count, self.shader_nodes, self.shader_units, self.mtp_units, self.flags]
            .iter()
            .enumerate()
        {
            put(out, 8 + 2 * i, &v.to_le_bytes());
        }
        put(out, 20, &cio.to_le_bytes());
        put(out, 24, &0u64.to_le_bytes());
    }
}
