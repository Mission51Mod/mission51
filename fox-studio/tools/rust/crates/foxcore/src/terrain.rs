//! Height previews from Fox Engine HTRE tiles (versions 3 and 4).
//!
//! The layout and cluster mapping come from our terrain-format investigation
//! and `foxterrain::tre`, documented in `docs/formats/terrain.md`. This module
//! only reads the height-preview subset; editing/writing and terrain generation
//! remain separate. It has no numeric-library or Python dependency.
//!
//! FoxData strings and links use signed offsets relative to their records.
//! Every referenced range is checked before indexing, and node/parameter cycles
//! are rejected. Parsing does not recurse on an untrusted tree.

use std::collections::BTreeSet;

pub const HTRE_N: usize = 64;
pub const CLUSTER: usize = 32;
const SAMPLES: usize = HTRE_N * HTRE_N;
const NODE_SIZE: usize = 0x30;

#[derive(Clone, Debug, PartialEq)]
pub struct Htre {
    pub version: u32,
    /// 4096 float32 heights, stored as four row-major 32x32 clusters.
    pub heights: Vec<f32>,
    /// The source heightMap's pitch parameter, normally 2.
    pub pitch: u32,
}

enum Visit {
    Enter(usize),
    Finish(usize),
}

/// Parse one complete HTRE file; convert `heights` with `file_to_grid` to view it.
pub fn read_htre(bytes: &[u8]) -> Result<Htre, String> {
    let reader = Reader { bytes };
    reader.range(0, 0x20, "FoxData header")?;
    let version = reader.u32(0)?;
    if !matches!(version, 3 | 4) {
        return Err(format!("HTRE: unsupported version {version}"));
    }
    let declared_size = reader.u32(8)? as usize;
    if declared_size != bytes.len() {
        return Err(format!(
            "HTRE: header declares {declared_size} bytes, got {}",
            bytes.len()
        ));
    }
    if reader.string(12)?.as_deref() != Some("terrainHighBlock") {
        return Err("HTRE: not a terrainHighBlock container".into());
    }
    let first_node = reader.u32(4)? as usize;
    if first_node < 0x20 {
        return Err("HTRE: missing/invalid root node offset".into());
    }
    let mut pending = vec![Visit::Enter(first_node)];
    let mut seen = BTreeSet::new();
    let mut active = BTreeSet::new();
    let mut heights = None;
    while let Some(visit) = pending.pop() {
        let offset = match visit {
            Visit::Enter(offset) => offset,
            Visit::Finish(offset) => {
                active.remove(&offset);
                continue;
            }
        };
        if active.contains(&offset) {
            return Err(format!("HTRE: node cycle at {offset:#x}"));
        }
        if !seen.insert(offset) {
            // Valid files can reach a finished node through both child and next.
            continue;
        }
        if seen.len() > bytes.len() / NODE_SIZE {
            return Err("HTRE: too many overlapping node records".into());
        }
        let node = reader.node(offset)?;
        active.insert(offset);
        pending.push(Visit::Finish(offset));
        // Stack order reproduces depth-first child-before-sibling discovery.
        if let Some(next) = node.next {
            pending.push(Visit::Enter(next));
        }
        if let Some(child) = node.child {
            pending.push(Visit::Enter(child));
        }
        if node.name.as_deref() == Some("heightMap") && heights.is_none() {
            if node.data.len() != SAMPLES * 4 {
                return Err(format!(
                    "HTRE: heightMap needs {} bytes for 64x64 float32 heights, got {}",
                    SAMPLES * 4,
                    node.data.len()
                ));
            }
            let pitch = node.uint_parameter("pitch")?;
            if version == 4 && node.uint_parameter("heightFormat")? != 1 {
                return Err("HTRE: only float32 heightFormat 1 is supported".into());
            }
            let values = node
                .data
                .chunks_exact(4)
                .map(|value| f32::from_le_bytes(value.try_into().unwrap()))
                .collect();
            heights = Some((values, pitch));
        }
    }
    let (heights, pitch) = heights.ok_or("HTRE: missing heightMap node")?;
    Ok(Htre {
        version,
        heights,
        pitch,
    })
}

/// Four cluster-major 32x32 blocks -> one row-major 64x64 grid (+z rows, +x cols).
pub fn file_to_grid<T: Copy + Default>(file_values: &[T]) -> Result<Vec<T>, String> {
    if file_values.len() != SAMPLES {
        return Err(format!(
            "HTRE: file_to_grid needs {SAMPLES} values, got {}",
            file_values.len()
        ));
    }
    let mut grid = vec![T::default(); SAMPLES];
    for cluster in 0..4 {
        let top = (cluster / 2) * CLUSTER;
        let left = (cluster % 2) * CLUSTER;
        for row in 0..CLUSTER {
            let source = cluster * CLUSTER * CLUSTER + row * CLUSTER;
            let target = (top + row) * HTRE_N + left;
            grid[target..target + CLUSTER].copy_from_slice(&file_values[source..source + CLUSTER]);
        }
    }
    Ok(grid)
}

struct Parameter {
    name: Option<String>,
    kind: u16,
    value: u32,
}

struct Node<'a> {
    name: Option<String>,
    data: &'a [u8],
    parameters: Vec<Parameter>,
    child: Option<usize>,
    next: Option<usize>,
}

impl Node<'_> {
    fn uint_parameter(&self, name: &str) -> Result<u32, String> {
        let mut matches = self
            .parameters
            .iter()
            .filter(|parameter| parameter.name.as_deref() == Some(name));
        let parameter = matches
            .next()
            .ok_or_else(|| format!("HTRE: missing heightMap parameter {name:?}"))?;
        if matches.next().is_some() {
            return Err(format!("HTRE: duplicate heightMap parameter {name:?}"));
        }
        if parameter.kind != 0 {
            return Err(format!(
                "HTRE: parameter {name:?} must be an unsigned integer"
            ));
        }
        Ok(parameter.value)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn range(&self, offset: usize, size: usize, what: &str) -> Result<&'a [u8], String> {
        let end = offset
            .checked_add(size)
            .ok_or_else(|| format!("HTRE: {what} range overflows"))?;
        self.bytes.get(offset..end).ok_or_else(|| {
            format!(
                "HTRE: {what} at {offset:#x} needs {size} bytes; file has {}",
                self.bytes.len()
            )
        })
    }

    fn u16(&self, offset: usize) -> Result<u16, String> {
        Ok(u16::from_le_bytes(
            self.range(offset, 2, "u16")?.try_into().unwrap(),
        ))
    }

    fn u32(&self, offset: usize) -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            self.range(offset, 4, "u32")?.try_into().unwrap(),
        ))
    }

    fn relative(&self, record: usize, delta: i32) -> Result<Option<usize>, String> {
        if delta == 0 {
            return Ok(None);
        }
        let target = if delta > 0 {
            record.checked_add(delta as usize)
        } else {
            record.checked_sub(delta.unsigned_abs() as usize)
        }
        .ok_or_else(|| format!("HTRE: relative offset {delta} from {record:#x} overflows"))?;
        if target >= self.bytes.len() {
            return Err(format!(
                "HTRE: relative offset from {record:#x} points outside the file ({target:#x})"
            ));
        }
        Ok(Some(target))
    }

    fn string(&self, record: usize) -> Result<Option<String>, String> {
        self.range(record, 8, "FoxData string record")?;
        let Some(start) = self.relative(record, self.u32(record + 4)? as i32)? else {
            return Ok(None);
        };
        let remaining = &self.bytes[start..];
        let length = remaining
            .iter()
            .position(|&byte| byte == 0)
            .ok_or_else(|| format!("HTRE: unterminated string at {start:#x}"))?;
        // Match our terrain reader's byte-preserving Latin-1 name conversion.
        Ok(Some(
            remaining[..length]
                .iter()
                .map(|&byte| byte as char)
                .collect(),
        ))
    }

    fn link(&self, record: usize, field: usize) -> Result<Option<usize>, String> {
        let target = self.relative(record, self.u32(record + field)? as i32)?;
        if let Some(offset) = target {
            if offset < 0x20 {
                return Err("HTRE: node link points into the FoxData header".into());
            }
            self.range(offset, NODE_SIZE, "linked node")?;
        }
        Ok(target)
    }

    fn node(&self, offset: usize) -> Result<Node<'a>, String> {
        self.range(offset, NODE_SIZE, "node header")?;
        let name = self.string(offset)?;
        let data_size = self.u32(offset + 16)? as usize;
        let data_start = self.relative(offset, self.u32(offset + 12)? as i32)?;
        let data = match data_start {
            Some(start) => self.range(start, data_size, "node data")?,
            None if data_size == 0 => &[],
            None => {
                return Err(format!(
                    "HTRE: node at {offset:#x} has a data size but no data pointer"
                ));
            }
        };
        self.link(offset, 20)?; // parent (not traversed)
        self.link(offset, 28)?; // previous sibling (not traversed)
        let child = self.link(offset, 24)?;
        let next = self.link(offset, 32)?;
        let parameters = match self.relative(offset, self.u32(offset + 36)? as i32)? {
            Some(first) => self.parameters(first)?,
            None => Vec::new(),
        };
        Ok(Node {
            name,
            data,
            parameters,
            child,
            next,
        })
    }

    fn parameters(&self, first: usize) -> Result<Vec<Parameter>, String> {
        let mut parameters = Vec::new();
        let mut seen = BTreeSet::new();
        let mut offset = first;
        loop {
            if !seen.insert(offset) {
                return Err(format!("HTRE: parameter cycle at {offset:#x}"));
            }
            if seen.len() > self.bytes.len() / 16 {
                return Err("HTRE: too many overlapping parameter records".into());
            }
            self.range(offset, 16, "parameter")?;
            let kind = self.u16(offset)?;
            if kind > 2 {
                return Err(format!(
                    "HTRE: unknown parameter type {kind} at {offset:#x}"
                ));
            }
            let name = self.string(offset + 4)?;
            let value = self.u32(offset + 12)?;
            if kind == 1 {
                self.string(offset + 12)?;
            }
            parameters.push(Parameter { name, kind, value });
            let delta = self.u16(offset + 2)? as i16 as i32;
            match self.relative(offset, delta)? {
                Some(next) => offset = next,
                None => return Ok(parameters),
            }
        }
    }
}
