//! Versioned snapshot codec. See docs/file-format.md for the compatibility contract.

use std::collections::BTreeMap;

use crate::collection::{Collection, CollectionConfig, Quantization};
use crate::error::{Error, Result};
use crate::hnsw::{Graph, HnswParams, MAX_LEVEL, Vectors};
use crate::metric::Metric;
use crate::rng::SplitMix64;

const MAGIC: &[u8; 4] = b"RVEC";
/// Highest supported snapshot version and the layout used for new databases.
/// Existing format-2 files retain their layout until an explicit storage upgrade.
pub const FORMAT_VERSION: u32 = 3;
const NO_ENTRY: u32 = u32::MAX;

pub(crate) fn encode(
    collections: &BTreeMap<String, Collection>,
    identity: u128,
    sequence: u64,
) -> Vec<u8> {
    let mut w = Writer::default();
    w.bytes(MAGIC);
    w.u32(2);
    w.bytes(&identity.to_le_bytes());
    w.u64(sequence);
    w.u32(collections.len() as u32);
    for collection in collections.values() {
        encode_collection(&mut w, collection);
    }
    let crc = crc32(&w.buf);
    w.u32(crc);
    w.buf
}

/// Advances an encoded format-2 image without encoding collections twice.
pub(crate) fn set_sequence(bytes: &mut [u8], sequence: u64) {
    debug_assert_eq!(&bytes[4..8], &2u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&sequence.to_le_bytes());
    let end = bytes.len() - 4;
    let crc = crc32(&bytes[..end]);
    bytes[end..].copy_from_slice(&crc.to_le_bytes());
}

fn encode_collection(w: &mut Writer, c: &Collection) {
    encode_collection_header(w, c);
    let graph = &c.graph;
    for &x in &c.vectors.data {
        w.bytes(&x.to_le_bytes());
    }
    if c.config.quantization == Quantization::Int8 {
        for &scale in &c.vectors.scales {
            w.bytes(&scale.to_le_bytes());
        }
        for &code in &c.vectors.codes {
            w.u8(code as u8);
        }
    }
    for node in 0..graph.len() {
        encode_node(w, c, node);
    }
}

pub(crate) fn encode_collection_header(w: &mut Writer, c: &Collection) {
    let graph = &c.graph;
    w.str(&c.name);
    w.u32(c.config.dim as u32);
    w.u8(c.config.metric.code());
    w.u8(if c.config.quantization == Quantization::Int8 {
        1
    } else {
        0
    });
    w.u32(c.config.hnsw.m as u32);
    w.u32(c.config.hnsw.ef_construction as u32);
    w.u32(c.config.hnsw.ef_search as u32);
    w.u64(graph.rng.state);
    w.u32(graph.len() as u32);
    w.u32(graph.entry.unwrap_or(NO_ENTRY));
    w.u32(graph.max_level as u32);
}

pub(crate) fn encode_node(w: &mut Writer, c: &Collection, node: usize) {
    let graph = &c.graph;
    w.str(&c.ids[node]);
    w.u8(c.deleted[node] as u8);
    match &c.metadata[node] {
        Some(value) => {
            w.u8(1);
            w.str(&value.to_string());
        }
        None => w.u8(0),
    }
    let layers = &graph.links[node];
    w.u8(layers.len() as u8);
    for links in layers {
        w.u32(links.len() as u32);
        for &link in links {
            w.u32(link);
        }
    }
}

pub(crate) fn decode(bytes: &[u8]) -> Result<BTreeMap<String, Collection>> {
    if bytes.len() < 4 || &bytes[..4] != MAGIC {
        return Err(Error::Corrupt("not a Recern Vector file".into()));
    }
    if bytes.len() < 16 {
        return Err(Error::Corrupt("file is truncated".into()));
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    if version == 3 {
        return crate::paged::decode(bytes);
    }
    if version != 1 && version != 2 {
        return Err(Error::UnsupportedVersion(version));
    }
    let (body, tail) = bytes.split_at(bytes.len() - 4);
    if crc32(body) != u32::from_le_bytes(tail.try_into().unwrap()) {
        return Err(Error::Corrupt("checksum mismatch".into()));
    }

    let mut r = Reader { buf: body, pos: 8 };
    if version >= 2 {
        r.take(24)?;
    }
    let count = r.u32()?;
    let mut collections = BTreeMap::new();
    for _ in 0..count {
        let collection = decode_collection(&mut r, version)?;
        if collections
            .insert(collection.name.clone(), collection)
            .is_some()
        {
            return Err(Error::Corrupt("duplicate collection name".into()));
        }
    }
    if r.pos != body.len() {
        return Err(Error::Corrupt("unexpected trailing data".into()));
    }
    Ok(collections)
}

pub(crate) fn decode_collection(r: &mut Reader, version: u32) -> Result<Collection> {
    let name = r.str()?;
    let dim = r.u32()? as usize;
    let metric = Metric::from_code(r.u8()?).ok_or_else(|| corrupt(&name, "unknown metric"))?;
    let quantization = if version == 1 {
        Quantization::F32
    } else {
        match r.u8()? {
            0 => Quantization::F32,
            1 => Quantization::Int8,
            _ => return Err(corrupt(&name, "unknown vector encoding")),
        }
    };
    let hnsw = HnswParams {
        m: r.u32()? as usize,
        ef_construction: r.u32()? as usize,
        ef_search: r.u32()? as usize,
    };
    let config = CollectionConfig {
        dim,
        metric,
        hnsw,
        quantization,
    };
    let rng_state = r.u64()?;
    let nodes = r.u32()? as usize;
    let entry = match r.u32()? {
        NO_ENTRY => None,
        e if (e as usize) < nodes => Some(e),
        _ => return Err(corrupt(&name, "entry point out of range")),
    };
    let max_level = r.u32()? as usize;
    if max_level > MAX_LEVEL || (entry.is_none() && nodes > 0) {
        return Err(corrupt(&name, "invalid graph header"));
    }

    let vector_len = nodes
        .checked_mul(dim)
        .ok_or_else(|| corrupt(&name, "vector data too large"))?;
    let mut vectors = Vectors::with_encoding(dim, quantization, metric);
    if version >= 2 {
        match quantization {
            Quantization::F32 => vectors.data = r.f32s(vector_len)?,
            Quantization::Int8 => {
                vectors.scales = r.f32s(nodes)?;
                if vectors.scales.iter().any(|v| !v.is_finite() || *v <= 0.0) {
                    return Err(corrupt(&name, "invalid quantization scale"));
                }
                vectors.codes = r.take(vector_len)?.iter().map(|&v| v as i8).collect();
            }
        }
    }
    // Every node takes at least 10 bytes, which bounds allocations driven by
    // a corrupt node count.
    let capacity = nodes.min(r.remaining() / 10);
    let mut ids = Vec::with_capacity(capacity);
    let mut deleted = Vec::with_capacity(capacity);
    let mut metadata = Vec::with_capacity(capacity);
    let mut links: Vec<Vec<Vec<u32>>> = Vec::with_capacity(capacity);
    for _ in 0..nodes {
        ids.push(r.str()?);
        deleted.push(match r.u8()? {
            0 => false,
            1 => true,
            _ => return Err(corrupt(&name, "invalid deletion flag")),
        });
        metadata.push(match r.u8()? {
            0 => None,
            1 => Some(
                serde_json::from_str(&r.str()?).map_err(|_| corrupt(&name, "invalid metadata"))?,
            ),
            _ => return Err(corrupt(&name, "invalid metadata flag")),
        });
        let layer_count = r.u8()? as usize;
        if layer_count == 0 || layer_count > max_level + 1 {
            return Err(corrupt(&name, "invalid node level"));
        }
        let mut layers = Vec::with_capacity(layer_count);
        for _ in 0..layer_count {
            let len = r.u32()? as usize;
            let mut layer = Vec::with_capacity(len.min(r.remaining() / 4));
            for _ in 0..len {
                let link = r.u32()?;
                if link as usize >= nodes {
                    return Err(corrupt(&name, "link out of range"));
                }
                layer.push(link);
            }
            layers.push(layer);
        }
        links.push(layers);
    }
    // Search indexes `links[neighbor][layer]`, so every link must point to a
    // node that exists on that layer.
    for layers in &links {
        for (layer, neighbors) in layers.iter().enumerate() {
            if neighbors.iter().any(|&n| links[n as usize].len() <= layer) {
                return Err(corrupt(&name, "link to a node missing from its layer"));
            }
        }
    }
    if entry.is_some_and(|e| links[e as usize].len() != max_level + 1) {
        return Err(corrupt(&name, "entry point is not on the top layer"));
    }
    if version == 1 {
        vectors.data = r.f32s(vector_len)?;
    }
    if vectors.data.iter().any(|v| !v.is_finite()) {
        return Err(corrupt(&name, "non-finite vector"));
    }

    let graph = Graph {
        changes: crate::changes::Changes::new(),
        params: hnsw,
        links,
        entry,
        max_level,
        rng: SplitMix64::new(rng_state),
    };
    Collection::from_parts(name, config, vectors, ids, metadata, deleted, graph)
}

fn corrupt(collection: &str, reason: &str) -> Error {
    Error::Corrupt(format!("collection '{collection}': {reason}"))
}

#[derive(Default)]
pub(crate) struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub(crate) fn bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }
    pub(crate) fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub(crate) fn u32(&mut self, v: u32) {
        self.bytes(&v.to_le_bytes());
    }
    pub(crate) fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }
    pub(crate) fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.bytes(s.as_bytes());
    }
}

pub(crate) struct Reader<'a> {
    pub buf: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&end| end <= self.buf.len());
        let end = end.ok_or_else(|| Error::Corrupt("unexpected end of file".into()))?;
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }
    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub(crate) fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub(crate) fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub(crate) fn str(&mut self) -> Result<String> {
        let len = self.u32()? as usize;
        String::from_utf8(self.take(len)?.to_vec())
            .map_err(|_| Error::Corrupt("invalid UTF-8 string".into()))
    }
    pub(crate) fn f32s(&mut self, n: usize) -> Result<Vec<f32>> {
        let bytes = self.take(
            n.checked_mul(4)
                .ok_or_else(|| Error::Corrupt("overflow".into()))?,
        )?;
        Ok(bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect())
    }
}

pub(crate) struct Crc32(u32);
impl Crc32 {
    pub fn new() -> Self {
        Self(!0)
    }
    pub fn update(&mut self, data: &[u8]) {
        const TABLE: [u32; 256] = {
            let mut table = [0u32; 256];
            let mut i = 0;
            while i < 256 {
                let mut c = i as u32;
                let mut k = 0;
                while k < 8 {
                    c = if c & 1 != 0 {
                        0xEDB8_8320 ^ (c >> 1)
                    } else {
                        c >> 1
                    };
                    k += 1;
                }
                table[i] = c;
                i += 1;
            }
            table
        };
        for &byte in data {
            self.0 = TABLE[((self.0 ^ byte as u32) & 0xFF) as usize] ^ (self.0 >> 8);
        }
    }
    pub fn finish(self) -> u32 {
        !self.0
    }
}
pub(crate) fn crc32(data: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(data);
    c.finish()
}

/// Reads only a header previously validated by `decode`.
pub(crate) fn header(bytes: &[u8]) -> (u128, u64) {
    if matches!(bytes.get(4..8), Some(v) if v == 2u32.to_le_bytes() || v == 3u32.to_le_bytes())
        && bytes.len() >= 36
    {
        (
            u128::from_le_bytes(bytes[8..24].try_into().unwrap()),
            u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
        )
    } else {
        (0, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_reference_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
