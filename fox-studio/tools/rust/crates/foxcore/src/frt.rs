//! Route sets (.frt, "ROUT"): the AI routes soldiers, helicopters and Sahelanthropus walk. Read + write.
//!
//! Port of tools/frt.py (RouteSet.parse / build, event_fields / make_event); byte-identical to it and to every vanilla
//! file. Layout after kapuragu/FoxEngineTemplates frt.bt (by youarebritish); notes in docs/formats/frt.md.
//!
//!   header  "ROUT", u16 version, u16 routeCount, [v2: 8 zero bytes, f32 x3 origin, 4 zero bytes],
//!           u32 offsets: routeIds, routeDefinitions, routeNodes, routeEventTables, routeEvents
//!   ids     u32 StrCode32(route name) per route
//!   defs    16 B per route: u32 nodes / eventTable / events offsets (relative to the def), u16 nodeCount, u16 eventCount
//!   nodes   f32 x3 per node
//!   tables  per node: u16 eventCount (edge event + node events), u16 eventStartIndex (route-local)
//!   events  48 B each: u32 type hash, u8 isNodeEvent, u8 aimType, u8 bodySection, u8 isLoop, u16 time, i16 dir,
//!           16 B aim params, 16 B type-specific params, 4 B snippet
//! The writer lays the sections out as the vanilla files do: ids, defs, all nodes, all tables, all events.

pub const EVENT_SIZE: usize = 0x30;
pub type EventBytes = [u8; EVENT_SIZE];

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub pos: [f32; 3],
    /// event table start index as read (informational; the writer recomputes it)
    pub start: u16,
    pub events: Vec<EventBytes>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    pub id: u32,
    pub nodes: Vec<Node>,
    /// as read (informational)
    pub event_count: u16,
    /// (nodes, tables, events) offsets as read, relative to the def (informational)
    pub offsets: (u32, u32, u32),
}

impl Route {
    pub fn new(id: u32, nodes: Vec<(f32, f32, f32, Vec<EventBytes>)>) -> Route {
        Route {
            id,
            nodes: nodes.into_iter().map(|(x, y, z, events)| Node { pos: [x, y, z], start: 0, events }).collect(),
            event_count: 0,
            offsets: (0, 0, 0),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RouteSet {
    pub version: u16,
    pub origin: Option<[f32; 3]>,
    pub routes: Vec<Route>,
    pub tail: Vec<u8>,
}

impl RouteSet {
    pub fn new(version: u16, origin: Option<[f32; 3]>, routes: Vec<Route>) -> RouteSet {
        RouteSet { version, origin, routes, tail: Vec::new() }
    }
}

fn rd<const N: usize>(b: &[u8], i: usize) -> Result<[u8; N], String> {
    b.get(i..i + N).map(|s| s.try_into().unwrap()).ok_or_else(|| format!("frt: truncated at {i:#x}"))
}
fn u16at(b: &[u8], i: usize) -> Result<u16, String> {
    Ok(u16::from_le_bytes(rd(b, i)?))
}
fn u32at(b: &[u8], i: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(rd(b, i)?))
}
fn f32at(b: &[u8], i: usize) -> Result<f32, String> {
    Ok(f32::from_le_bytes(rd(b, i)?))
}

pub fn parse(b: &[u8]) -> Result<RouteSet, String> {
    if b.get(0..4) != Some(b"ROUT") {
        return Err("not a route set (ROUT)".into());
    }
    let version = u16at(b, 4)?;
    let count = u16at(b, 6)? as usize;
    let (mut p, mut origin) = (8usize, None);
    if version == 2 {
        origin = Some([f32at(b, 16)?, f32at(b, 20)?, f32at(b, 24)?]);
        p = 32;
    }
    let ids_off = u32at(b, p)? as usize;
    let defs_off = u32at(b, p + 4)? as usize;
    let events_off = u32at(b, p + 16)? as usize;
    let mut routes = Vec::with_capacity(count);
    for r in 0..count {
        let id = u32at(b, ids_off + 4 * r)?;
        let dpos = defs_off + 16 * r;
        let (n_off, t_off, e_off) = (u32at(b, dpos)?, u32at(b, dpos + 4)?, u32at(b, dpos + 8)?);
        let n_count = u16at(b, dpos + 12)? as usize;
        let e_count = u16at(b, dpos + 14)?;
        let mut nodes = Vec::with_capacity(n_count);
        for n in 0..n_count {
            let at = dpos + n_off as usize + 12 * n;
            let pos = [f32at(b, at)?, f32at(b, at + 4)?, f32at(b, at + 8)?];
            let tt = dpos + t_off as usize + 4 * n;
            let (ev_count, ev_start) = (u16at(b, tt)? as usize, u16at(b, tt + 2)?);
            let mut events = Vec::with_capacity(ev_count);
            for i in 0..ev_count {
                let e = dpos + e_off as usize + EVENT_SIZE * (ev_start as usize + i);
                events.push(rd::<EVENT_SIZE>(b, e)?);
            }
            nodes.push(Node { pos, start: ev_start, events });
        }
        routes.push(Route { id, nodes, event_count: e_count, offsets: (n_off, t_off, e_off) });
    }
    // frt.py keeps data[max(eventsOffset, len):] as the tail, which is always empty
    let end = events_off.max(b.len());
    Ok(RouteSet { version, origin, routes, tail: b[end.min(b.len())..].to_vec() })
}

/// Lay the sections out the way the vanilla files do: ids, defs, all nodes, all tables, all events. The route records
/// are written in ascending u32 route-ID order, as in every vanilla .frt: the game finds a route by a binary search
/// over the ID table (FindRoute, 0x140514080), so an unsorted table makes most routes unfindable. Duplicate IDs are
/// refused for the same reason. Mirrors tools/frt.py RouteSet.build (stable sort, whole records).
pub fn build(rs: &RouteSet) -> Result<Vec<u8>, String> {
    let mut routes: Vec<&Route> = rs.routes.iter().collect();
    routes.sort_by_key(|r| r.id);
    let dup: Vec<String> = routes.windows(2).filter(|w| w[0].id == w[1].id).map(|w| format!("0x{:08x}", w[1].id)).collect();
    if !dup.is_empty() {
        return Err(format!("duplicate route IDs in the route set: {}", dup.join(", ")));
    }
    let count = routes.len();
    let head = if rs.version == 2 { 32 } else { 8 };
    let ids_off = head + 20;
    let defs_off = ids_off + 4 * count;
    let nodes_off = defs_off + 16 * count;
    let n_total: usize = routes.iter().map(|r| r.nodes.len()).sum();
    let tables_off = nodes_off + 12 * n_total;
    let events_off = tables_off + 4 * n_total;
    let mut out = Vec::new();
    out.extend_from_slice(b"ROUT");
    out.extend_from_slice(&rs.version.to_le_bytes());
    out.extend_from_slice(&(count as u16).to_le_bytes());
    if rs.version == 2 {
        out.extend_from_slice(&[0u8; 8]);
        for v in rs.origin.unwrap_or([0.0; 3]) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&[0u8; 4]);
    }
    for v in [ids_off, defs_off, nodes_off, tables_off, events_off] {
        out.extend_from_slice(&(v as u32).to_le_bytes());
    }
    let (mut ids, mut defs, mut nodes, mut tables, mut events) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut n_seen, mut ev_seen) = (0usize, 0usize);
    for (r, route) in routes.iter().enumerate() {
        let dpos = defs_off + 16 * r;
        ids.extend_from_slice(&route.id.to_le_bytes());
        let r_nodes = nodes_off + 12 * n_seen;
        let r_tables = tables_off + 4 * n_seen;
        let r_events = events_off + EVENT_SIZE * ev_seen;
        let mut local = 0usize;
        for node in &route.nodes {
            for v in node.pos {
                nodes.extend_from_slice(&v.to_le_bytes());
            }
            tables.extend_from_slice(&(node.events.len() as u16).to_le_bytes());
            tables.extend_from_slice(&(local as u16).to_le_bytes());
            for ev in &node.events {
                events.extend_from_slice(ev);
            }
            local += node.events.len();
        }
        for v in [r_nodes - dpos, r_tables - dpos, r_events - dpos] {
            defs.extend_from_slice(&(v as u32).to_le_bytes());
        }
        defs.extend_from_slice(&(route.nodes.len() as u16).to_le_bytes());
        defs.extend_from_slice(&(local as u16).to_le_bytes());
        n_seen += route.nodes.len();
        ev_seen += local;
    }
    for s in [ids, defs, nodes, tables, events] {
        out.extend_from_slice(&s);
    }
    out.extend_from_slice(&rs.tail);
    Ok(out)
}

/// The decoded fields of one event (frt.event_fields; the three blobs stay raw).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub typ: u32,
    pub is_node_event: u8,
    pub aim_type: u8,
    pub body_section: u8,
    pub is_loop: u8,
    pub time: u16,
    pub dir: i16,
    pub aim: [u8; 16],
    pub params: [u8; 16],
    pub snippet: [u8; 4],
}

pub fn event_fields(ev: &EventBytes) -> Event {
    Event {
        typ: u32::from_le_bytes(ev[0..4].try_into().unwrap()),
        is_node_event: ev[4],
        aim_type: ev[5],
        body_section: ev[6],
        is_loop: ev[7],
        time: u16::from_le_bytes([ev[8], ev[9]]),
        dir: i16::from_le_bytes([ev[10], ev[11]]),
        aim: ev[12..28].try_into().unwrap(),
        params: ev[28..44].try_into().unwrap(),
        snippet: ev[44..48].try_into().unwrap(),
    }
}

pub fn make_event(f: &Event) -> EventBytes {
    let mut o = [0u8; EVENT_SIZE];
    o[0..4].copy_from_slice(&f.typ.to_le_bytes());
    o[4] = f.is_node_event;
    o[5] = f.aim_type;
    o[6] = f.body_section;
    o[7] = f.is_loop;
    o[8..10].copy_from_slice(&f.time.to_le_bytes());
    o[10..12].copy_from_slice(&f.dir.to_le_bytes());
    o[12..28].copy_from_slice(&f.aim);
    o[28..44].copy_from_slice(&f.params);
    o[44..48].copy_from_slice(&f.snippet);
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn synthetic_roundtrip() {
        let mut ev = [0u8; EVENT_SIZE];
        for (i, x) in ev.iter_mut().enumerate() {
            *x = i as u8;
        }
        assert_eq!(make_event(&event_fields(&ev)), ev);
        for (v, o) in [(3u16, None), (2, Some([1.5f32, -2.0, 1e6]))] {
            let rs = RouteSet::new(v, o, vec![
                Route::new(0xDEADBEEF, vec![(1.0, 2.0, 3.0, vec![ev, ev]), (4.0, 5.0, 6.0, vec![ev])]),
                Route::new(7, vec![(0.25, 0.5, 0.75, vec![])]),
            ]);
            let b = build(&rs).unwrap();
            let back = parse(&b).unwrap();
            assert_eq!(back.origin, o);
            // written in ascending ID order: 7 first
            assert_eq!(back.routes.iter().map(|r| r.id).collect::<Vec<_>>(), vec![7, 0xDEADBEEF]);
            assert_eq!(back.routes[1].nodes[1].start, 2);
            assert_eq!(back.routes[1].event_count, 3);
            assert_eq!(build(&back).unwrap(), b);
        }
    }

    #[test]
    fn duplicate_ids_refused() {
        let rs = RouteSet::new(3, None, vec![Route::new(5, vec![]), Route::new(9, vec![]), Route::new(5, vec![])]);
        assert_eq!(build(&rs).unwrap_err(), "duplicate route IDs in the route set: 0x00000005");
    }
}
