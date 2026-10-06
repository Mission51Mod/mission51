//! .nav2 reader / writer (owner: nav agent; moved from foxnav, which re-exports it as foxnav::nav2): port of
//! tools/location/nav2.py (docs/formats/nav2.md). The writer recomputes every offset,
//! count, size and pad from the objects; write(read(b)) == b on the vanilla corpus (`fox nav roundtrip`).
use std::collections::BTreeMap;

pub const VERSIONS: [u32; 3] = [201403240, 201403241, 201403242];
pub const CHUNK_NAVIGATION_GRAPH: u32 = 0;
pub const CHUNK_NAVMESH: u32 = 1;
pub const CHUNK_SEGMENT_GRAPH: u32 = 3;
pub const CHUNK_SEGMENT: u32 = 4;
pub const CHUNK_FILE_ORDER: [u32; 4] = [0, 1, 4, 3];
pub const CHUNK_DESC_ORDER: [u32; 3] = [0, 3, 1];
pub const NO_NEIGHBOR: u16 = 0xFFFF;

pub type V3 = [u16; 3];

pub fn align(x: usize, a: usize) -> usize {
    x.div_ceil(a) * a
}

pub fn uid_pack(index: u32, tile: u32) -> u32 {
    (index & 0x7FFF) | ((tile & 0xFFFF) << 15)
}

pub fn uid_unpack(v: u32) -> (u32, u32, u32) {
    (v & 0x7FFF, (v >> 15) & 0xFFFF, v >> 31)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NavGraphNode {
    pub segment: u16,
    pub links: Vec<(u16, u16)>,
    pub cross_segment: Option<u16>,
    pub cross_chunk: Option<(u16, u16)>,
    pub cross_tile: Option<(u16, u16, u16)>,
    pub kind: u8,
    pub flag_pad: u8,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NavGraphEdge {
    pub weight: u16,
    pub segment: u16,
    pub node0: u8,
    pub node1: u8,
    pub kind: u8,
    pub pad: u8,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NavigationGraph {
    pub positions: Vec<V3>,
    pub nodes: Vec<NavGraphNode>,
    pub edges: Vec<NavGraphEdge>,
    pub attributes: Vec<u16>,
    pub navmesh_indices: Vec<u16>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Polygon {
    pub segment: u16,
    pub vertices: Vec<u8>,
    pub neighbors: Vec<u16>,
    pub graph_nodes: Vec<u8>,
    pub neighbor_chunks: Option<Vec<u16>>,
    pub neighbor_tiles: Option<Vec<u16>>,
}

impl Polygon {
    pub fn needs_cross_arrays(&self) -> bool {
        self.neighbors.iter().any(|&n| n != NO_NEIGHBOR && matches!(n >> 14, 1 | 2))
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Navmesh {
    pub positions: Vec<V3>,
    pub polygons: Vec<Polygon>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SegmentLink {
    pub weight: u16,
    pub node: u16,
    pub link_index: u8,
    pub graph_nodes: Vec<u8>,
    pub data_chunk: Option<u16>,
    pub tile: Option<u16>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SegmentGraphNode {
    pub island_nodes: [u16; 3],
    pub inside: Vec<SegmentLink>,
    pub cross_chunk: Vec<SegmentLink>,
    pub cross_tile: Vec<SegmentLink>,
    pub unknown: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SegmentGraph {
    pub positions: Vec<V3>,
    pub nodes: Vec<SegmentGraphNode>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Segment {
    pub bounds_min: V3,
    pub bounds_max: V3,
    pub base_position: u16,
    pub base_polygon: u16,
    pub base_edge: u16,
    pub base_node: u16,
    pub position_count: u8,
    pub polygon_count: u8,
    pub edge_count: u8,
    pub node_count: u8,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChunkPads {
    pub graph: [u32; 5], // p0 p1 p2 (u32) p3 p4 (u16)
    pub mesh: (u64, u32, u32),
    pub segment_graph: (u32, u32, u32),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DataChunk {
    pub uid: u32,
    pub graph: NavigationGraph,
    pub mesh: Navmesh,
    pub segment_graph: SegmentGraph,
    pub segments: Vec<Segment>,
    pub pads: ChunkPads,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct IslandNode {
    pub position: V3,
    pub attributes: u16,
    pub low: (u32, u16, u16),
    pub segment_count: u16,
    pub cross_chunk: Vec<(u32, u16, u16)>,
    pub inside: Vec<(u16, u16)>,
    pub cross_tile: Vec<u16>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IslandGraph {
    pub nodes: Vec<IslandNode>,
    pub side_groups: Vec<(u16, u16, u16)>,
    pub side_infos: Vec<(u16, u16)>,
}

impl Default for IslandGraph {
    fn default() -> Self {
        IslandGraph { nodes: vec![], side_groups: vec![(0, 0, 0); 4], side_infos: vec![] }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchBucket {
    pub min_y: u8,
    pub max_y: u8,
    pub entries: Vec<(u32, Vec<u16>)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchSpace {
    pub origin: (f64, f64, f64), // written as f32
    pub range_y: u16,
    pub counts: (u32, u32, u32),
    pub block_size_xz: u32,
    pub block_size_y: u32,
    pub buckets: Vec<SearchBucket>,
    pub pad0: u16,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConnectTileNode {
    pub graph_node: u16,
    pub navmesh: u16,
    pub side: u16,
}

/// A connection identifier and its tile nodes.
pub type ConnectTileNodes = (u16, Vec<ConnectTileNode>);
/// A group name and its connections, preserving file order.
pub type ConnectTileGroup = (u32, Vec<ConnectTileNodes>);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConnectTileInfo {
    pub groups: Vec<ConnectTileGroup>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Nav2 {
    pub version: u32,
    pub file_uid: u32,
    /// origin kept as f64 (the generator's values); written as f32, read back as f32 values
    pub origin: (f64, f64, f64),
    pub denominator: (u16, u16, u16),
    pub weight_denominator: u16,
    pub feature_flags: u8,
    pub source_hash: [u8; 16],
    pub chunks: Vec<DataChunk>,
    pub island_graphs: Vec<IslandGraph>,
    pub search_space: Option<SearchSpace>,
    pub connect_tile_info: Option<ConnectTileInfo>,
    pub unknown_crossing_tile: u32,
    pub pre2014_index: u8,
    pub header_pads: (u32, u32, u32, u8),
    pub igc_without_graphs: u8,
    /// non-canonical chunk data offsets (type, uid) -> offset (a few vanilla files); empty = canonical
    pub chunk_data_offset: BTreeMap<(u32, u32), u32>,
}

impl Default for Nav2 {
    fn default() -> Self {
        Nav2 {
            version: 201403242,
            file_uid: 0,
            origin: (0.0, 0.0, 0.0),
            denominator: (1, 1, 1),
            weight_denominator: 1,
            feature_flags: 1,
            source_hash: [0; 16],
            chunks: vec![],
            island_graphs: vec![],
            search_space: None,
            connect_tile_info: None,
            unknown_crossing_tile: 0,
            pre2014_index: 0,
            header_pads: (0, 0, 0, 0),
            igc_without_graphs: 0,
            chunk_data_offset: BTreeMap::new(),
        }
    }
}

impl Nav2 {
    /// world position of a quantized vertex: origin + v / den (Python int / int true division)
    pub fn world(&self, v: V3) -> (f64, f64, f64) {
        let (o, d) = (self.origin, self.denominator);
        (o.0 + v[0] as f64 / d.0 as f64, o.1 + v[1] as f64 / d.1 as f64, o.2 + v[2] as f64 / d.2 as f64)
    }
}

// ---------------------------------------------------------------------------------------------------------------
// reader
// ---------------------------------------------------------------------------------------------------------------

struct R<'a>(&'a [u8]);
impl R<'_> {
    fn u8(&self, o: usize) -> u8 {
        self.0[o]
    }
    fn u16(&self, o: usize) -> u16 {
        u16::from_le_bytes([self.0[o], self.0[o + 1]])
    }
    fn u32(&self, o: usize) -> u32 {
        u32::from_le_bytes(self.0[o..o + 4].try_into().unwrap())
    }
    fn u64(&self, o: usize) -> u64 {
        u64::from_le_bytes(self.0[o..o + 8].try_into().unwrap())
    }
    fn f32(&self, o: usize) -> f32 {
        f32::from_le_bytes(self.0[o..o + 4].try_into().unwrap())
    }
    fn vec(&self, o: usize, n: usize) -> Vec<V3> {
        (0..n).map(|i| [self.u16(o + 6 * i), self.u16(o + 6 * i + 2), self.u16(o + 6 * i + 4)]).collect()
    }
    fn arr16(&self, o: usize, n: usize) -> Vec<u16> {
        (0..n).map(|i| self.u16(o + 2 * i)).collect()
    }
}

pub fn read(b: &[u8]) -> Result<Nav2, String> {
    if b.len() < 96 {
        return Err("nav2: too short".into());
    }
    let r = R(b);
    let version = r.u32(0);
    if !VERSIONS.contains(&version) {
        return Err(format!("unknown nav2 version {version}"));
    }
    let chunks_off = r.u32(8) as usize;
    let chunk_count = r.u32(12) as usize;
    let ss_off = r.u32(16) as usize;
    let file_uid = r.u32(20);
    let ig_off = r.u32(24) as usize;
    let unk = r.u32(28);
    let (ox, oy, oz) = (r.f32(32), r.f32(36), r.f32(40));
    let cti_off = r.u32(44) as usize;
    let pad0 = r.u32(48);
    let pad1 = r.u32(60);
    let pad2 = r.u32(64);
    let (dx, dy, dz) = (r.u16(68), r.u16(70), r.u16(72));
    let wd = r.u16(74);
    let ff = r.u8(76);
    let igc = r.u8(77);
    let pre = r.u8(78);
    let pad3 = r.u8(79);
    let mut nav = Nav2 {
        version,
        file_uid,
        origin: (ox as f64, oy as f64, oz as f64),
        denominator: (dx, dy, dz),
        weight_denominator: wd,
        feature_flags: ff,
        source_hash: b[80..96].try_into().unwrap(),
        unknown_crossing_tile: unk,
        pre2014_index: pre,
        header_pads: (pad0, pad1, pad2, pad3),
        ..Default::default()
    };
    let mut order: Vec<u32> = vec![];
    let mut by_uid: BTreeMap<u32, DataChunk> = BTreeMap::new();
    let mut o = chunks_off;
    for _ in 0..chunk_count {
        let ctype = r.u32(o);
        let nxt = r.u32(o + 4) as usize;
        let doff = r.u32(o + 8);
        let uid = r.u32(o + 12);
        let dc = match by_uid.entry(uid) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                order.push(uid);
                entry.insert(DataChunk { uid, ..Default::default() })
            }
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
        };
        let d = o + doff as usize;
        if doff != 16 {
            nav.chunk_data_offset.insert((ctype, uid), doff);
        }
        match ctype {
            CHUNK_NAVIGATION_GRAPH => read_graph(&r, d, dc),
            CHUNK_NAVMESH => read_mesh(&r, d, dc),
            CHUNK_SEGMENT_GRAPH => read_segment_graph(&r, d, dc),
            CHUNK_SEGMENT => read_segments(&r, d, dc),
            _ => return Err(format!("unknown chunk type {ctype}")),
        }
        o += nxt;
    }
    nav.chunks = order.iter().map(|u| by_uid.remove(u).unwrap()).collect();
    if ig_off != 0 {
        let mut o = ig_off;
        for _ in 0..igc {
            let (g, nxt) = read_island_graph(&r, o);
            nav.island_graphs.push(g);
            if nxt != 0 {
                o += nxt as usize;
            }
        }
    } else if igc != 0 {
        nav.igc_without_graphs = igc;
    }
    if ss_off != 0 {
        nav.search_space = Some(read_search_space(&r, ss_off));
    }
    if cti_off != 0 {
        nav.connect_tile_info = Some(read_cti(&r, cti_off));
    }
    Ok(nav)
}

fn read_graph(r: &R, d: usize, dc: &mut DataChunk) {
    let pos_off = r.u32(d) as usize;
    let nodes_off = r.u32(d + 4) as usize;
    let edges_off = r.u32(d + 8) as usize;
    let link_off = r.u32(d + 12) as usize;
    let p0 = r.u32(d + 16);
    let p1 = r.u32(d + 20);
    let attr_off = r.u32(d + 24) as usize;
    let p2 = r.u32(d + 28);
    let nmi_off = r.u32(d + 32) as usize;
    let ncount = r.u16(d + 36) as usize;
    let ecount = r.u16(d + 38) as usize;
    let p3 = r.u32(d + 40);
    let p4 = r.u16(d + 44) as u32;
    let acount = r.u16(d + 46) as usize;
    dc.pads.graph = [p0, p1, p2, p3, p4];
    let g = &mut dc.graph;
    g.positions = r.vec(d + pos_off, ncount);
    for i in 0..ncount {
        let no = d + nodes_off + 6 * i;
        let lo = r.u16(no) as usize;
        let packed1 = r.u16(no + 2);
        let ilc = r.u8(no + 4) as usize;
        let flags = r.u8(no + 5);
        let mut off = d + link_off + ((((packed1 >> 12) as usize) << 16) | lo) * 2;
        let mut node = NavGraphNode { segment: packed1 & 0xFFF, kind: (flags >> 3) & 3, flag_pad: flags >> 5, ..Default::default() };
        node.links = (0..ilc).map(|k| (r.u16(off + 4 * k), r.u16(off + 4 * k + 2))).collect();
        off += 4 * ilc;
        if flags & 1 != 0 {
            node.cross_segment = Some(r.u16(off));
            off += 2;
        }
        if flags & 2 != 0 {
            node.cross_chunk = Some((r.u16(off), r.u16(off + 2)));
            off += 4;
        }
        if flags & 4 != 0 {
            node.cross_tile = Some((r.u16(off), r.u16(off + 2), r.u16(off + 4)));
        }
        g.nodes.push(node);
    }
    for i in 0..ecount {
        let eo = d + edges_off + 6 * i;
        let packed = r.u16(eo + 2);
        g.edges.push(NavGraphEdge {
            weight: r.u16(eo),
            segment: packed & 0xFFF,
            kind: ((packed >> 12) & 3) as u8,
            pad: (packed >> 14) as u8,
            node0: r.u8(eo + 4),
            node1: r.u8(eo + 5),
        });
    }
    g.attributes = r.arr16(d + attr_off, acount);
    g.navmesh_indices = r.arr16(d + nmi_off, ncount);
}

fn read_mesh(r: &R, d: usize, dc: &mut DataChunk) {
    let pos_off = r.u32(d) as usize;
    let mesh_off = r.u32(d + 4) as usize;
    let md_off = r.u32(d + 8) as usize;
    let p0 = r.u64(d + 12);
    let p1 = r.u32(d + 20);
    let mcount = r.u16(d + 24) as usize;
    let pcount = r.u16(d + 26) as usize;
    let p2 = r.u32(d + 28);
    dc.pads.mesh = (p0, p1, p2);
    let m = &mut dc.mesh;
    m.positions = r.vec(d + pos_off, pcount);
    for i in 0..mcount {
        let packed = r.u32(d + mesh_off + 4 * i);
        let nv = if (packed >> 18) & 1 != 0 { 4 } else { 3 };
        let nn = if (packed >> 19) & 1 != 0 { 4 } else { 3 };
        let mut o = d + md_off + (packed & 0x3FFFF) as usize * 2;
        let nbr = r.arr16(o, nv);
        o += 2 * nv;
        let verts = r.0[o..o + nv].to_vec();
        o += nv;
        let gn = r.0[o..o + nn].to_vec();
        o += nn;
        let mut p = Polygon { segment: (packed >> 20) as u16, vertices: verts, neighbors: nbr, graph_nodes: gn, ..Default::default() };
        if p.needs_cross_arrays() {
            o = align(o, 2);
            p.neighbor_chunks = Some(r.arr16(o, nv));
            p.neighbor_tiles = Some(r.arr16(o + 2 * nv, nv));
        }
        m.polygons.push(p);
    }
}

fn read_segment_graph(r: &R, d: usize, dc: &mut DataChunk) {
    let pos_off = r.u32(d) as usize;
    let nodes_off = r.u32(d + 4) as usize;
    let link_off = r.u32(d + 8) as usize;
    let p0 = r.u32(d + 12);
    let ncount = r.u16(d + 20) as usize;
    let p1 = r.u32(d + 22);
    let p2 = r.u32(d + 28);
    dc.pads.segment_graph = (p0, p1, p2);
    let sg = &mut dc.segment_graph;
    sg.positions = r.vec(d + pos_off, ncount);
    for i in 0..ncount {
        let no = d + nodes_off + 12 * i;
        let packed = r.u32(no);
        let isl = [r.u16(no + 4), r.u16(no + 6), r.u16(no + 8)];
        let cdc = r.u8(no + 10) as usize;
        let ctc = r.u8(no + 11) as usize;
        let ilc = (packed >> 24) as usize;
        let mut node = SegmentGraphNode { island_nodes: isl, unknown: (packed >> 20) & 0xF, ..Default::default() };
        let mut o = d + link_off + (packed & 0xFFFFF) as usize * 2;
        let mut counts = vec![];
        for _ in 0..ilc {
            node.inside.push(SegmentLink { weight: r.u16(o), node: r.u16(o + 2), link_index: r.u8(o + 5), ..Default::default() });
            counts.push(r.u8(o + 4) as usize);
            o += 6;
        }
        for _ in 0..cdc {
            node.cross_chunk.push(SegmentLink {
                data_chunk: Some(r.u16(o)),
                weight: r.u16(o + 2),
                node: r.u16(o + 4),
                link_index: r.u8(o + 7),
                ..Default::default()
            });
            counts.push(r.u8(o + 6) as usize);
            o += 8;
        }
        for _ in 0..ctc {
            node.cross_tile.push(SegmentLink {
                tile: Some(r.u16(o)),
                data_chunk: Some(r.u16(o + 2)),
                weight: r.u16(o + 4),
                node: r.u16(o + 6),
                link_index: r.u8(o + 9),
                ..Default::default()
            });
            counts.push(r.u8(o + 8) as usize);
            o += 10;
        }
        for (index, link) in node.inside.iter_mut().chain(node.cross_chunk.iter_mut())
            .chain(node.cross_tile.iter_mut()).enumerate()
        {
            let count = counts[index];
            link.graph_nodes = r.0[o..o + count].to_vec();
            o += count;
        }
        sg.nodes.push(node);
    }
}

fn read_segments(r: &R, d: usize, dc: &mut DataChunk) {
    let bounds_off = r.u32(d) as usize;
    let segs_off = r.u32(d + 4) as usize;
    let scount = r.u32(d + 12) as usize;
    for i in 0..scount {
        let bo = d + bounds_off + 12 * i;
        let so = d + segs_off + 12 * i;
        dc.segments.push(Segment {
            bounds_min: [r.u16(bo), r.u16(bo + 2), r.u16(bo + 4)],
            bounds_max: [r.u16(bo + 6), r.u16(bo + 8), r.u16(bo + 10)],
            base_position: r.u16(so),
            base_polygon: r.u16(so + 2),
            base_edge: r.u16(so + 4),
            base_node: r.u16(so + 6),
            position_count: r.u8(so + 8),
            polygon_count: r.u8(so + 9),
            edge_count: r.u8(so + 10),
            node_count: r.u8(so + 11),
        });
    }
}

fn read_island_graph(r: &R, o: usize) -> (IslandGraph, u16) {
    let nxt = r.u16(o);
    let nodes_off = r.u16(o + 2) as usize;
    let ncount = r.u16(o + 4) as usize;
    let link_off = r.u16(o + 6) as usize;
    let low_off = r.u16(o + 8) as usize;
    let segc_off = r.u16(o + 10) as usize;
    let csg_off = r.u16(o + 12) as usize;
    let csi_off = r.u16(o + 14) as usize;
    let mut g = IslandGraph::default();
    for i in 0..ncount {
        let no = o + nodes_off + 14 * i;
        let pos = [r.u16(no), r.u16(no + 2), r.u16(no + 4)];
        let attrs = r.u16(no + 6);
        let lo = r.u16(no + 8) as usize;
        let ctc = r.u16(no + 10) as usize;
        let ilc = r.u8(no + 12) as usize;
        let cdc = r.u8(no + 13) as usize;
        let mut la = o + link_off + lo;
        let cross_chunk = (0..cdc).map(|k| (r.u32(la + 8 * k), r.u16(la + 8 * k + 4), r.u16(la + 8 * k + 6))).collect();
        la += 8 * cdc;
        let inside = (0..ilc).map(|k| (r.u16(la + 4 * k), r.u16(la + 4 * k + 2))).collect();
        la += 4 * ilc;
        let cross_tile = r.arr16(la, ctc);
        let lw = o + low_off + 8 * i;
        g.nodes.push(IslandNode {
            position: pos,
            attributes: attrs,
            low: (r.u32(lw), r.u16(lw + 4), r.u16(lw + 6)),
            segment_count: r.u16(o + segc_off + 2 * i),
            cross_chunk,
            inside,
            cross_tile,
        });
    }
    g.side_groups = (0..4).map(|k| {
        let so = o + csg_off + 6 * k;
        (r.u16(so), r.u16(so + 2), r.u16(so + 4))
    }).collect();
    let n_infos = g.side_groups.iter().map(|&(_, s, c)| s as usize + c as usize).max().unwrap_or(0);
    g.side_infos = (0..n_infos).map(|k| (r.u16(o + csi_off + 4 * k), r.u16(o + csi_off + 4 * k + 2))).collect();
    (g, nxt)
}

fn read_search_space(r: &R, o: usize) -> SearchSpace {
    let mut ss = SearchSpace {
        origin: (r.f32(o) as f64, r.f32(o + 4) as f64, r.f32(o + 8) as f64),
        range_y: r.u16(o + 12),
        pad0: r.u16(o + 14),
        counts: (r.u32(o + 16), r.u32(o + 20), r.u32(o + 24)),
        block_size_xz: r.u32(o + 28),
        block_size_y: r.u32(o + 32),
        buckets: vec![],
    };
    let bd_off = r.u32(o + 36) as usize;
    let n = (ss.counts.0 * ss.counts.1 * ss.counts.2) as usize;
    for i in 0..n {
        let bo = o + bd_off + 8 * i;
        let start = r.u32(bo) as usize;
        let cnt = r.u16(bo + 4) as usize;
        let mut bk = SearchBucket { min_y: r.u8(bo + 6), max_y: r.u8(bo + 7), entries: vec![] };
        let mut e = bo + start;
        for _ in 0..cnt {
            let uid = r.u32(e);
            let sc = r.u32(e + 4) as usize;
            bk.entries.push((uid, r.arr16(e + 8, sc)));
            e += 8 + 2 * sc;
        }
        ss.buckets.push(bk);
    }
    ss
}

fn read_cti(r: &R, o: usize) -> ConnectTileInfo {
    let g_off = r.u16(o) as usize;
    let i_off = r.u16(o + 2) as usize;
    let n_off = r.u16(o + 4) as usize;
    let gc = r.u16(o + 6) as usize;
    let mut cti = ConnectTileInfo::default();
    for gi in 0..gc {
        let go = o + 2 * g_off + 6 * gi;
        let name = r.u32(go);
        let packed = r.u16(go + 4) as usize;
        let mut infos = vec![];
        for ii in 0..(packed & 0x1F) {
            let io = o + 2 * i_off + 4 * ((packed >> 5) + ii);
            let dcid = r.u16(io);
            let p2 = r.u16(io + 2) as usize;
            let nodes = (0..(p2 & 0x1F)).map(|ni| {
                let no = o + 2 * n_off + 4 * ((p2 >> 5) + ni);
                let p3 = r.u16(no + 2);
                ConnectTileNode { graph_node: r.u16(no), navmesh: p3 & 0x3FFF, side: p3 >> 14 }
            }).collect();
            infos.push((dcid, nodes));
        }
        cti.groups.push((name, infos));
    }
    cti
}

// ---------------------------------------------------------------------------------------------------------------
// writer
// ---------------------------------------------------------------------------------------------------------------

struct W(Vec<u8>);
impl W {
    fn tell(&self) -> usize {
        self.0.len()
    }
    fn pad(&mut self, a: usize) {
        let n = align(self.0.len(), a);
        self.0.resize(n, 0);
    }
    fn zeros(&mut self, n: usize) {
        self.0.resize(self.0.len() + n, 0);
    }
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn at16(&mut self, o: usize, v: u16) {
        self.0[o..o + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn at32(&mut self, o: usize, v: u32) {
        self.0[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn at64(&mut self, o: usize, v: u64) {
        self.0[o..o + 8].copy_from_slice(&v.to_le_bytes());
    }
    fn atf32(&mut self, o: usize, v: f64) {
        self.0[o..o + 4].copy_from_slice(&(v as f32).to_le_bytes());
    }
    fn vecs(&mut self, vs: &[V3]) {
        for v in vs {
            self.u16(v[0]);
            self.u16(v[1]);
            self.u16(v[2]);
        }
    }
}

pub fn write(nav: &Nav2) -> Vec<u8> {
    let mut w = W(Vec::with_capacity(1 << 16));
    w.zeros(80);
    w.0.extend_from_slice(&nav.source_hash);
    let cd_off = w.tell();
    let n_dc = nav.chunks.len();
    w.u32(n_dc as u32);
    let desc_pos = w.tell();
    w.zeros(12 * 3 * n_dc);
    w.pad(16);
    let mut chunks_off = w.tell();
    let mut info: BTreeMap<(u32, u32), (usize, usize)> = BTreeMap::new();
    for ctype in CHUNK_FILE_ORDER {
        for dc in &nav.chunks {
            let start = w.tell();
            w.u32(ctype);
            w.u32(0);
            w.u32(16);
            w.u32(dc.uid);
            let d = w.tell();
            match ctype {
                CHUNK_NAVIGATION_GRAPH => write_graph(&mut w, dc),
                CHUNK_NAVMESH => write_mesh(&mut w, dc),
                CHUNK_SEGMENT => write_segments(&mut w, dc),
                _ => write_segment_graph(&mut w, dc),
            }
            w.pad(16);
            let size = w.tell() - start;
            w.at32(start + 4, size as u32);
            info.insert((ctype, dc.uid), (d, size));
        }
    }
    for (gi, ctype) in CHUNK_DESC_ORDER.iter().enumerate() {
        for (i, dc) in nav.chunks.iter().enumerate() {
            let (d, size) = info[&(*ctype, dc.uid)];
            let p = desc_pos + 12 * (gi * n_dc + i);
            w.at32(p, dc.uid);
            w.at32(p + 4, d as u32);
            w.at32(p + 8, ctype | ((size as u32) << 8));
        }
    }
    let mut ig_off = 0;
    if !nav.island_graphs.is_empty() {
        ig_off = w.tell();
        for g in &nav.island_graphs {
            let start = w.tell();
            write_island_graph(&mut w, g);
            w.pad(16);
            let sz = (w.tell() - start) as u16;
            w.at16(start, sz);
        }
    }
    let mut ss_off = 0;
    if let Some(ss) = &nav.search_space {
        ss_off = w.tell();
        write_search_space(&mut w, ss);
    }
    let mut cti_off = 0;
    if let Some(c) = &nav.connect_tile_info {
        cti_off = w.tell();
        write_cti(&mut w, c);
    }
    if nav.chunks.is_empty() && nav.island_graphs.is_empty() && nav.search_space.is_none() {
        chunks_off = w.tell();
    }
    let total = w.tell();
    let (p0, p1, p2, p3) = nav.header_pads;
    w.at32(0, nav.version);
    w.at32(4, total as u32);
    w.at32(8, chunks_off as u32);
    w.at32(12, 4 * n_dc as u32);
    w.at32(16, ss_off as u32);
    w.at32(20, nav.file_uid);
    w.at32(24, ig_off as u32);
    w.at32(28, nav.unknown_crossing_tile);
    w.atf32(32, nav.origin.0);
    w.atf32(36, nav.origin.1);
    w.atf32(40, nav.origin.2);
    w.at32(44, cti_off as u32);
    w.at32(48, p0);
    w.at32(52, cd_off as u32);
    w.at32(56, 4 + 36 * n_dc as u32);
    w.at32(60, p1);
    w.at32(64, p2);
    w.at16(68, nav.denominator.0);
    w.at16(70, nav.denominator.1);
    w.at16(72, nav.denominator.2);
    w.at16(74, nav.weight_denominator);
    w.0[76] = nav.feature_flags;
    w.0[77] = if nav.island_graphs.is_empty() { nav.igc_without_graphs } else { nav.island_graphs.len() as u8 };
    w.0[78] = nav.pre2014_index;
    w.0[79] = p3;
    w.0
}

fn write_graph(w: &mut W, dc: &DataChunk) {
    let g = &dc.graph;
    let d = w.tell();
    let [p0, p1, p2, p3, p4] = dc.pads.graph;
    w.zeros(48);
    let pos_off = w.tell() - d;
    w.vecs(&g.positions);
    w.pad(16);
    let nodes_off = w.tell() - d;
    w.zeros(6 * g.nodes.len());
    w.pad(16);
    let link_off = w.tell() - d;
    for (i, node) in g.nodes.iter().enumerate() {
        let rel = (w.tell() - d - link_off) / 2;
        let flags = (node.cross_segment.is_some() as u8)
            | ((node.cross_chunk.is_some() as u8) << 1)
            | ((node.cross_tile.is_some() as u8) << 2)
            | (node.kind << 3)
            | (node.flag_pad << 5);
        let no = d + nodes_off + 6 * i;
        w.at16(no, (rel & 0xFFFF) as u16);
        w.at16(no + 2, (node.segment & 0xFFF) | (((rel >> 16) as u16) << 12));
        w.0[no + 4] = node.links.len() as u8;
        w.0[no + 5] = flags;
        for &(a, b) in &node.links {
            w.u16(a);
            w.u16(b);
        }
        if let Some(c) = node.cross_segment {
            w.u16(c);
        }
        if let Some((a, b)) = node.cross_chunk {
            w.u16(a);
            w.u16(b);
        }
        if let Some((a, b, c)) = node.cross_tile {
            w.u16(a);
            w.u16(b);
            w.u16(c);
        }
    }
    w.pad(16);
    let edges_off = w.tell() - d;
    for e in &g.edges {
        w.u16(e.weight);
        w.u16((e.segment & 0xFFF) | ((e.kind as u16) << 12) | ((e.pad as u16) << 14));
        w.u8(e.node0);
        w.u8(e.node1);
    }
    w.pad(16);
    let attr_off = w.tell() - d;
    for &a in &g.attributes {
        w.u16(a);
    }
    w.pad(16);
    let nmi_off = w.tell() - d;
    for &a in &g.navmesh_indices {
        w.u16(a);
    }
    w.at32(d, pos_off as u32);
    w.at32(d + 4, nodes_off as u32);
    w.at32(d + 8, edges_off as u32);
    w.at32(d + 12, link_off as u32);
    w.at32(d + 16, p0);
    w.at32(d + 20, p1);
    w.at32(d + 24, attr_off as u32);
    w.at32(d + 28, p2);
    w.at32(d + 32, nmi_off as u32);
    w.at16(d + 36, g.nodes.len() as u16);
    w.at16(d + 38, g.edges.len() as u16);
    w.at32(d + 40, p3);
    w.at16(d + 44, p4 as u16);
    w.at16(d + 46, g.attributes.len() as u16);
}

fn mesh_record_size(p: &Polygon) -> usize {
    let nv = p.vertices.len();
    let mut size = 2 * nv + nv + p.graph_nodes.len();
    if p.needs_cross_arrays() {
        size = align(size, 2) + 4 * nv;
    }
    align(size, 16)
}

fn write_mesh(w: &mut W, dc: &DataChunk) {
    let m = &dc.mesh;
    let d = w.tell();
    let (p0, p1, p2) = dc.pads.mesh;
    w.zeros(32);
    let pos_off = w.tell() - d;
    w.vecs(&m.positions);
    w.pad(16);
    let mesh_off = w.tell() - d;
    let md_off = mesh_off + 4 * m.polygons.len();
    let mut rel = 0usize;
    for p in &m.polygons {
        let nv = p.vertices.len();
        w.u32(((rel / 2) as u32) | (((nv == 4) as u32) << 18) | (((p.graph_nodes.len() == 4) as u32) << 19)
              | ((p.segment as u32) << 20));
        rel += mesh_record_size(p);
    }
    for p in &m.polygons {
        let start = w.tell();
        let nv = p.vertices.len();
        for &n in &p.neighbors {
            w.u16(n);
        }
        w.0.extend_from_slice(&p.vertices);
        w.0.extend_from_slice(&p.graph_nodes);
        if p.needs_cross_arrays() {
            w.pad(2);
            match &p.neighbor_chunks {
                Some(v) => v.iter().for_each(|&x| w.u16(x)),
                None => (0..nv).for_each(|_| w.u16(0xFFFF)),
            }
            match &p.neighbor_tiles {
                Some(v) => v.iter().for_each(|&x| w.u16(x)),
                None => (0..nv).for_each(|_| w.u16(0)),
            }
        }
        let end = start + mesh_record_size(p);
        let cur = w.tell();
        w.zeros(end - cur);
    }
    w.at32(d, pos_off as u32);
    w.at32(d + 4, mesh_off as u32);
    w.at32(d + 8, md_off as u32);
    w.at64(d + 12, p0);
    w.at32(d + 20, p1);
    w.at16(d + 24, m.polygons.len() as u16);
    w.at16(d + 26, m.positions.len() as u16);
    w.at32(d + 28, p2);
}

fn write_segment_graph(w: &mut W, dc: &DataChunk) {
    let sg = &dc.segment_graph;
    let d = w.tell();
    let (p0, p1, p2) = dc.pads.segment_graph;
    w.zeros(32);
    let pos_off = w.tell() - d;
    w.vecs(&sg.positions);
    w.pad(16);
    let nodes_off = w.tell() - d;
    w.zeros(12 * sg.nodes.len());
    w.pad(16);
    let link_off = w.tell() - d;
    let mut total = 0usize;
    for (i, node) in sg.nodes.iter().enumerate() {
        let rel = (w.tell() - d - link_off) / 2;
        let no = d + nodes_off + 12 * i;
        w.at32(no, rel as u32 | (node.unknown << 20) | ((node.inside.len() as u32) << 24));
        w.at16(no + 4, node.island_nodes[0]);
        w.at16(no + 6, node.island_nodes[1]);
        w.at16(no + 8, node.island_nodes[2]);
        w.0[no + 10] = node.cross_chunk.len() as u8;
        w.0[no + 11] = node.cross_tile.len() as u8;
        for lk in &node.inside {
            w.u16(lk.weight);
            w.u16(lk.node);
            w.u8(lk.graph_nodes.len() as u8);
            w.u8(lk.link_index);
        }
        for lk in &node.cross_chunk {
            w.u16(lk.data_chunk.unwrap_or(0));
            w.u16(lk.weight);
            w.u16(lk.node);
            w.u8(lk.graph_nodes.len() as u8);
            w.u8(lk.link_index);
        }
        for lk in &node.cross_tile {
            w.u16(lk.tile.unwrap_or(0));
            w.u16(lk.data_chunk.unwrap_or(0));
            w.u16(lk.weight);
            w.u16(lk.node);
            w.u8(lk.graph_nodes.len() as u8);
            w.u8(lk.link_index);
        }
        for lk in node.inside.iter().chain(&node.cross_chunk).chain(&node.cross_tile) {
            w.0.extend_from_slice(&lk.graph_nodes);
            total += lk.graph_nodes.len();
        }
        w.pad(2);
    }
    w.pad(16);
    let self_size = w.tell() - d;
    w.at32(d, pos_off as u32);
    w.at32(d + 4, nodes_off as u32);
    w.at32(d + 8, link_off as u32);
    w.at32(d + 12, p0);
    w.at32(d + 16, self_size as u32);
    w.at16(d + 20, sg.nodes.len() as u16);
    w.at32(d + 22, p1);
    w.at16(d + 26, total as u16);
    w.at32(d + 28, p2);
}

fn write_segments(w: &mut W, dc: &DataChunk) {
    let d = w.tell();
    w.zeros(16);
    let bounds_off = w.tell() - d;
    for s in &dc.segments {
        w.vecs(&[s.bounds_min, s.bounds_max]);
    }
    w.pad(16);
    let segs_off = w.tell() - d;
    for s in &dc.segments {
        w.u16(s.base_position);
        w.u16(s.base_polygon);
        w.u16(s.base_edge);
        w.u16(s.base_node);
        w.u8(s.position_count);
        w.u8(s.polygon_count);
        w.u8(s.edge_count);
        w.u8(s.node_count);
    }
    w.pad(16);
    let size = w.tell() - d;
    w.at32(d, bounds_off as u32);
    w.at32(d + 4, segs_off as u32);
    w.at32(d + 8, size as u32);
    w.at32(d + 12, dc.segments.len() as u32);
}

fn write_island_graph(w: &mut W, g: &IslandGraph) {
    let o = w.tell();
    w.zeros(16);
    let nodes_off = w.tell() - o;
    w.zeros(14 * g.nodes.len());
    w.pad(16);
    let link_off = w.tell() - o;
    for (i, n) in g.nodes.iter().enumerate() {
        let lo = w.tell() - o - link_off;
        let no = o + nodes_off + 14 * i;
        w.at16(no, n.position[0]);
        w.at16(no + 2, n.position[1]);
        w.at16(no + 4, n.position[2]);
        w.at16(no + 6, n.attributes);
        w.at16(no + 8, lo as u16);
        w.at16(no + 10, n.cross_tile.len() as u16);
        w.0[no + 12] = n.inside.len() as u8;
        w.0[no + 13] = n.cross_chunk.len() as u8;
        for &(a, b, c) in &n.cross_chunk {
            w.u32(a);
            w.u16(b);
            w.u16(c);
        }
        for &(a, b) in &n.inside {
            w.u16(a);
            w.u16(b);
        }
        for &c in &n.cross_tile {
            w.u16(c);
        }
    }
    w.pad(16);
    let low_off = w.tell() - o;
    for n in &g.nodes {
        w.u32(n.low.0);
        w.u16(n.low.1);
        w.u16(n.low.2);
    }
    let segc_off = w.tell() - o;
    for n in &g.nodes {
        w.u16(n.segment_count);
    }
    let csg_off = w.tell() - o;
    for &(a, b, c) in &g.side_groups {
        w.u16(a);
        w.u16(b);
        w.u16(c);
    }
    let csi_off = w.tell() - o;
    for &(a, b) in &g.side_infos {
        w.u16(a);
        w.u16(b);
    }
    for (k, v) in [0usize, nodes_off, g.nodes.len(), link_off, low_off, segc_off, csg_off, csi_off].iter().enumerate() {
        w.at16(o + 2 * k, *v as u16);
    }
}

fn write_search_space(w: &mut W, ss: &SearchSpace) {
    let o = w.tell();
    w.zeros(48);
    let bd_off = w.tell() - o;
    let nb = ss.buckets.len();
    w.zeros(8 * nb);
    let be_off = w.tell() - o;
    for (i, bk) in ss.buckets.iter().enumerate() {
        let bo = o + bd_off + 8 * i;
        let start = (w.tell() - bo) as u32;
        w.at32(bo, start);
        w.at16(bo + 4, bk.entries.len() as u16);
        w.0[bo + 6] = bk.min_y;
        w.0[bo + 7] = bk.max_y;
        for (uid, segs) in &bk.entries {
            w.u32(*uid);
            w.u32(segs.len() as u32);
            for &s in segs {
                w.u16(s);
            }
        }
    }
    w.pad(16);
    let size = w.tell() - o;
    w.atf32(o, ss.origin.0);
    w.atf32(o + 4, ss.origin.1);
    w.atf32(o + 8, ss.origin.2);
    w.at16(o + 12, ss.range_y);
    w.at16(o + 14, ss.pad0);
    w.at32(o + 16, ss.counts.0);
    w.at32(o + 20, ss.counts.1);
    w.at32(o + 24, ss.counts.2);
    w.at32(o + 28, ss.block_size_xz);
    w.at32(o + 32, ss.block_size_y);
    w.at32(o + 36, bd_off as u32);
    w.at32(o + 40, be_off as u32);
    w.at32(o + 44, size as u32);
}

fn write_cti(w: &mut W, cti: &ConnectTileInfo) {
    let o = w.tell();
    w.zeros(12);
    let groups_off = (w.tell() - o) / 2;
    w.zeros(6 * cti.groups.len());
    let infos_off = (w.tell() - o) / 2;
    let n_infos: usize = cti.groups.iter().map(|(_, i)| i.len()).sum();
    w.zeros(4 * n_infos);
    let nodes_off = (w.tell() - o) / 2;
    let (mut ii, mut ni) = (0usize, 0usize);
    for (gi, (name, infos)) in cti.groups.iter().enumerate() {
        let go = o + 2 * groups_off + 6 * gi;
        w.at32(go, *name);
        w.at16(go + 4, (infos.len() | (ii << 5)) as u16);
        for (dcid, nodes) in infos {
            let io = o + 2 * infos_off + 4 * ii;
            w.at16(io, *dcid);
            w.at16(io + 2, (nodes.len() | (ni << 5)) as u16);
            for nd in nodes {
                w.u16(nd.graph_node);
                w.u16(nd.navmesh | (nd.side << 14));
            }
            ii += 1;
            ni += nodes.len();
        }
    }
    for (k, v) in [groups_off, infos_off, nodes_off, cti.groups.len(), n_infos, ni].iter().enumerate() {
        w.at16(o + 2 * k, *v as u16);
    }
}
