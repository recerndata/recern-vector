//! Format 3: fixed pages with self-describing object-directory entries.
//! The last word is a CRC32 Merkle root over page checksums (not format-2 CRC).
use crate::{
    Collection, Error, Result,
    storage::{self, Reader, Writer},
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

pub(crate) const PAGE: usize = 4096;
const END: usize = PAGE - 4;
const PREFIX: usize = 32;
const PAYLOAD: usize = END - PREFIX;
type Block = Box<[u8; PAGE]>;
type Key = (u64, u8, u32); // collection, kind (1=header, 2=node), node
#[derive(Clone, Copy)]
struct Info {
    id: u64,
    epoch: u64,
}
pub(crate) struct Image {
    pages: Vec<Block>,
    directory: BTreeMap<Key, Vec<usize>>,
    free: BTreeSet<usize>,
    infos: BTreeMap<String, Info>,
    tree: Vec<u32>,
    pub id: u128,
    pub sequence: u64,
    next_id: u64,
}
enum TreePatch {
    Sparse(BTreeMap<usize, u32>),
    Full(Vec<u32>),
}
pub(crate) struct Patch {
    pub pages: BTreeMap<usize, Block>,
    directory: BTreeMap<Key, Option<Vec<usize>>>,
    free: BTreeMap<usize, bool>,
    infos: BTreeMap<String, Option<Info>>,
    tree: TreePatch,
    pub root: u32,
    pub sequence: u64,
    next_id: u64,
    pub count: usize,
    pub encoded: u64,
}
fn corrupt(s: &str) -> Error {
    Error::Corrupt(format!("paged snapshot: {s}"))
}
fn word(p: &[u8]) -> u32 {
    u32::from_le_bytes(p.try_into().unwrap())
}
fn wide(p: &[u8]) -> u64 {
    u64::from_le_bytes(p.try_into().unwrap())
}
fn seal(mut p: Block) -> Block {
    let crc = storage::crc32(&p[..END]);
    p[END..].copy_from_slice(&crc.to_le_bytes());
    p
}
fn join(a: u32, b: u32) -> u32 {
    let mut bts = [0; 12];
    bts[..4].copy_from_slice(b"RV3T");
    bts[4..8].copy_from_slice(&a.to_le_bytes());
    bts[8..].copy_from_slice(&b.to_le_bytes());
    storage::crc32(&bts)
}
fn tree(pages: &[Block]) -> Vec<u32> {
    let cap = pages.len().next_power_of_two();
    let mut t = vec![0; cap * 2];
    for (i, p) in pages.iter().enumerate() {
        t[cap + i] = word(&p[END..]);
    }
    for i in (1..cap).rev() {
        t[i] = join(t[i * 2], t[i * 2 + 1]);
    }
    t
}
fn header(id: u128, sequence: u64, count: usize, next_id: u64) -> Block {
    let mut p = Box::new([0; PAGE]);
    p[..4].copy_from_slice(b"RVEC");
    p[4..8].copy_from_slice(&3u32.to_le_bytes());
    p[8..24].copy_from_slice(&id.to_le_bytes());
    p[24..32].copy_from_slice(&sequence.to_le_bytes());
    p[32..40].copy_from_slice(&(count as u64).to_le_bytes());
    p[40..48].copy_from_slice(&next_id.to_le_bytes());
    seal(p)
}
impl Image {
    pub fn empty(id: u128) -> Self {
        Self::empty_at(id, 0)
    }
    pub fn empty_at(id: u128, sequence: u64) -> Self {
        let pages = vec![header(id, sequence, 1, 1)];
        let tree = tree(&pages);
        Self {
            pages,
            tree,
            directory: BTreeMap::new(),
            free: BTreeSet::new(),
            infos: BTreeMap::new(),
            id,
            sequence,
            next_id: 1,
        }
    }
    pub fn len(&self) -> usize {
        self.pages.len() * PAGE + 4
    }
    pub fn root(&self) -> u32 {
        self.tree[1]
    }
    pub fn write(&self, w: &mut impl Write) -> std::io::Result<()> {
        for p in &self.pages {
            w.write_all(&p[..])?;
        }
        w.write_all(&self.root().to_le_bytes())
    }
    pub fn bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(self.len());
        self.write(&mut b).unwrap();
        b
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < PAGE + 4 || !(bytes.len() - 4).is_multiple_of(PAGE) {
            return Err(corrupt("invalid page count"));
        }
        let count = (bytes.len() - 4) / PAGE;
        let mut pages = Vec::with_capacity(count);
        let mut chunks: BTreeMap<Key, BTreeMap<usize, (usize, usize)>> = BTreeMap::new();
        let mut free = BTreeSet::new();
        for (i, b) in bytes[..bytes.len() - 4]
            .as_chunks::<PAGE>()
            .0
            .iter()
            .enumerate()
        {
            if storage::crc32(&b[..END]) != word(&b[END..]) {
                return Err(corrupt("page checksum mismatch"));
            }
            let p: Block = Box::new(*b);
            if i > 0 {
                if p[0] == 0 {
                    if p[..END].iter().any(|&x| x != 0) {
                        return Err(corrupt("invalid free page"));
                    }
                    free.insert(i);
                } else {
                    let kind = p[0];
                    let cid = wide(&p[4..12]);
                    let node = word(&p[12..16]);
                    let chunk = word(&p[16..20]) as usize;
                    let len = word(&p[20..24]) as usize;
                    let total = word(&p[24..28]) as usize;
                    if !(1..=2).contains(&kind)
                        || cid == 0
                        || (kind == 1 && node != 0)
                        || p[1..4] != [0; 3]
                        || p[28..32] != [0; 4]
                        || len == 0
                        || len > PAYLOAD
                        || total == 0
                        || total > count
                        || chunk >= total
                        || (chunk + 1 < total && len != PAYLOAD)
                        || p[PREFIX + len..END].iter().any(|&x| x != 0)
                    {
                        return Err(corrupt("invalid directory entry"));
                    }
                    if chunks
                        .entry((cid, kind, node))
                        .or_default()
                        .insert(chunk, (i, total))
                        .is_some()
                    {
                        return Err(corrupt("duplicate object chunk"));
                    }
                }
            }
            pages.push(p);
        }
        let p = &pages[0];
        if &p[..4] != b"RVEC"
            || word(&p[4..8]) != 3
            || wide(&p[32..40]) != count as u64
            || wide(&p[40..48]) == 0
            || p[48..END].iter().any(|&x| x != 0)
        {
            return Err(corrupt("invalid header"));
        }
        let id = u128::from_le_bytes(p[8..24].try_into().unwrap());
        if id == 0 {
            return Err(corrupt("zero identity"));
        }
        let sequence = wide(&p[24..32]);
        let next_id = wide(&p[40..48]);
        let tree = tree(&pages);
        if tree[1] != word(&bytes[bytes.len() - 4..]) {
            return Err(corrupt("directory root checksum mismatch"));
        }
        let mut directory = BTreeMap::new();
        for (key, parts) in chunks {
            if key.0 >= next_id {
                return Err(corrupt("collection ID out of range"));
            }
            let total = parts.first_key_value().unwrap().1.1;
            if parts.len() != total
                || parts
                    .iter()
                    .enumerate()
                    .any(|(i, (&chunk, &(_, n)))| i != chunk || n != total)
            {
                return Err(corrupt("missing object chunk"));
            }
            directory.insert(key, parts.values().map(|&(page, _)| page).collect());
        }
        let mut image = Self {
            pages,
            directory,
            free,
            infos: BTreeMap::new(),
            tree,
            id,
            sequence,
            next_id,
        };
        let mut cids = BTreeSet::new();
        for &(cid, kind, node) in image.directory.keys() {
            if kind == 1 {
                let blob = image.object((cid, kind, node));
                let mut r = Reader { buf: &blob, pos: 0 };
                let name = r.str()?;
                if name.is_empty()
                    || name.len() > 255
                    || image
                        .infos
                        .insert(name, Info { id: cid, epoch: 0 })
                        .is_some()
                {
                    return Err(corrupt("duplicate or invalid collection"));
                }
                cids.insert(cid);
            }
        }
        if image.directory.keys().any(|k| !cids.contains(&k.0)) {
            return Err(corrupt("orphan node"));
        }
        Ok(image)
    }
    fn object(&self, key: Key) -> Vec<u8> {
        let mut out = Vec::new();
        for &page in &self.directory[&key] {
            let p = &self.pages[page];
            out.extend_from_slice(&p[PREFIX..PREFIX + word(&p[20..24]) as usize]);
        }
        out
    }
    pub fn bind(&mut self, collections: &BTreeMap<String, Collection>) {
        for (name, info) in &mut self.infos {
            info.epoch = collections[name].changes.epoch;
        }
        for c in collections.values() {
            c.changes.saved();
            c.graph.changes.saved();
        }
    }
    pub fn plan(&self, collections: &BTreeMap<String, Collection>) -> Result<Patch> {
        let mut p = Planner {
            base: self,
            pages: BTreeMap::new(),
            directory: BTreeMap::new(),
            free: BTreeMap::new(),
            released: BTreeSet::new(),
            free_cursor: 0,
            infos: BTreeMap::new(),
            count: self.pages.len(),
            next_id: self.next_id,
            encoded: 0,
        };
        for (name, info) in &self.infos {
            if !collections.contains_key(name) {
                p.remove_collection(info.id);
                p.infos.insert(name.clone(), None);
            }
        }
        for (name, c) in collections {
            let old = self.infos.get(name);
            let full = old.is_none_or(|i| i.epoch != c.changes.epoch);
            let id = if let Some(i) = old {
                i.id
            } else {
                let id = p.next_id;
                p.next_id = p
                    .next_id
                    .checked_add(1)
                    .ok_or_else(|| Error::InvalidArgument("collection IDs exhausted".into()))?;
                id
            };
            if full && old.is_some() {
                p.remove_collection(id);
            }
            let mut changed = c.changes.pending();
            changed.extend(c.graph.changes.pending());
            if full || !changed.is_empty() {
                let mut w = Writer::default();
                storage::encode_collection_header(&mut w, c);
                p.object((id, 1, 0), &w.buf)?;
                let nodes: Box<dyn Iterator<Item = u32>> = if full {
                    Box::new(0..c.ids.len() as u32)
                } else {
                    Box::new(changed.into_iter().filter(|&i| (i as usize) < c.ids.len()))
                };
                for node in nodes {
                    let n = node as usize;
                    let mut w = Writer::default();
                    match c.config.quantization {
                        crate::Quantization::F32 => {
                            for x in &c.vectors.data[n * c.config.dim..(n + 1) * c.config.dim] {
                                w.bytes(&x.to_le_bytes());
                            }
                        }
                        crate::Quantization::Int8 => {
                            w.bytes(&c.vectors.scales[n].to_le_bytes());
                            for &v in &c.vectors.codes[n * c.config.dim..(n + 1) * c.config.dim] {
                                w.u8(v as u8);
                            }
                        }
                    }
                    storage::encode_node(&mut w, c, n);
                    p.object((id, 2, node), &w.buf)?;
                    p.encoded += 1;
                }
            }
            p.infos.insert(
                name.clone(),
                Some(Info {
                    id,
                    epoch: c.changes.epoch,
                }),
            );
        }
        let sequence = if p.pages.is_empty() {
            self.sequence
        } else {
            self.sequence
                .checked_add(1)
                .ok_or_else(|| Error::InvalidArgument("commit sequence exhausted".into()))?
        };
        if !p.pages.is_empty() {
            p.put(0, header(self.id, sequence, p.count, p.next_id));
        }
        let cap = p.count.next_power_of_two();
        let old_cap = self.tree.len() / 2;
        let tree = if cap == old_cap {
            let mut changed = BTreeMap::new();
            for (&page, b) in &p.pages {
                let mut i = cap + page;
                changed.insert(i, word(&b[END..]));
                while i > 1 {
                    i /= 2;
                    let left = *changed.get(&(i * 2)).unwrap_or(&self.tree[i * 2]);
                    let right = *changed.get(&(i * 2 + 1)).unwrap_or(&self.tree[i * 2 + 1]);
                    changed.insert(i, join(left, right));
                }
            }
            TreePatch::Sparse(changed)
        } else {
            let mut t = vec![0; cap * 2];
            t[cap..cap + self.pages.len()]
                .copy_from_slice(&self.tree[old_cap..old_cap + self.pages.len()]);
            for (&i, b) in &p.pages {
                t[cap + i] = word(&b[END..]);
            }
            for i in (1..cap).rev() {
                t[i] = join(t[i * 2], t[i * 2 + 1]);
            }
            TreePatch::Full(t)
        };
        let root = match &tree {
            TreePatch::Sparse(t) => *t.get(&1).unwrap_or(&self.root()),
            TreePatch::Full(t) => t[1],
        };
        Ok(Patch {
            pages: p.pages,
            directory: p.directory,
            free: p.free,
            infos: p.infos,
            tree,
            root,
            sequence,
            next_id: p.next_id,
            count: p.count,
            encoded: p.encoded,
        })
    }
    pub fn apply(&mut self, patch: Patch) {
        for (i, b) in patch.pages {
            if i < self.pages.len() {
                self.pages[i] = b;
            } else {
                assert_eq!(i, self.pages.len());
                self.pages.push(b);
            }
        }
        for (key, value) in patch.directory {
            if let Some(v) = value {
                self.directory.insert(key, v);
            } else {
                self.directory.remove(&key);
            }
        }
        for (page, free) in patch.free {
            if free {
                self.free.insert(page);
            } else {
                self.free.remove(&page);
            }
        }
        for (name, info) in patch.infos {
            if let Some(i) = info {
                self.infos.insert(name, i);
            } else {
                self.infos.remove(&name);
            }
        }
        match patch.tree {
            TreePatch::Sparse(t) => {
                for (i, v) in t {
                    self.tree[i] = v;
                }
            }
            TreePatch::Full(t) => self.tree = t,
        }
        self.sequence = patch.sequence;
        self.next_id = patch.next_id;
    }
}
struct Planner<'a> {
    base: &'a Image,
    pages: BTreeMap<usize, Block>,
    directory: BTreeMap<Key, Option<Vec<usize>>>,
    free: BTreeMap<usize, bool>,
    released: BTreeSet<usize>,
    free_cursor: usize,
    infos: BTreeMap<String, Option<Info>>,
    count: usize,
    next_id: u64,
    encoded: u64,
}
impl Planner<'_> {
    fn put(&mut self, i: usize, b: Block) {
        if self.base.pages.get(i).is_some_and(|old| old[..] == b[..]) {
            self.pages.remove(&i);
        } else {
            self.pages.insert(i, b);
        }
    }
    fn release(&mut self, i: usize) {
        self.put(i, seal(Box::new([0; PAGE])));
        self.released.insert(i);
        self.free.insert(i, true);
    }
    fn allocate(&mut self) -> usize {
        let page = if let Some(i) = self.released.pop_first() {
            i
        } else if let Some(&i) = self.base.free.range(self.free_cursor..).next() {
            self.free_cursor = i + 1;
            i
        } else {
            let i = self.count;
            self.count += 1;
            i
        };
        self.free.insert(page, false);
        page
    }
    fn remove_collection(&mut self, id: u64) {
        let keys: Vec<_> = self
            .base
            .directory
            .range((id, 0, 0)..=(id, u8::MAX, u32::MAX))
            .map(|(&k, _)| k)
            .collect();
        for key in keys {
            for &i in &self.base.directory[&key] {
                self.release(i);
            }
            self.directory.insert(key, None);
        }
    }
    fn object(&mut self, key: Key, bytes: &[u8]) -> Result<()> {
        let n = bytes.len().div_ceil(PAYLOAD);
        let total =
            u32::try_from(n).map_err(|_| Error::InvalidArgument("object too large".into()))?;
        let mut old = match self.directory.get(&key) {
            Some(v) => v.clone().unwrap_or_default(),
            None => self.base.directory.get(&key).cloned().unwrap_or_default(),
        };
        for i in old.drain(n.min(old.len())..) {
            self.release(i);
        }
        while old.len() < n {
            let i = self.allocate();
            old.push(i);
        }
        for (chunk, data) in bytes.chunks(PAYLOAD).enumerate() {
            let mut p = Box::new([0; PAGE]);
            p[0] = key.1;
            p[4..12].copy_from_slice(&key.0.to_le_bytes());
            p[12..16].copy_from_slice(&key.2.to_le_bytes());
            p[16..20].copy_from_slice(&(chunk as u32).to_le_bytes());
            p[20..24].copy_from_slice(&(data.len() as u32).to_le_bytes());
            p[24..28].copy_from_slice(&total.to_le_bytes());
            p[PREFIX..PREFIX + data.len()].copy_from_slice(data);
            self.put(old[chunk], seal(p));
        }
        self.directory.insert(key, Some(old));
        Ok(())
    }
}
pub(crate) fn decode(bytes: &[u8]) -> Result<BTreeMap<String, Collection>> {
    let image = Image::from_bytes(bytes)?;
    let mut out = BTreeMap::new();
    for (name, info) in &image.infos {
        let h = image.object((info.id, 1, 0));
        let mut r = Reader { buf: &h, pos: 0 };
        r.str()?;
        let dim = r.u32()? as usize;
        r.u8()?;
        let quant = r.u8()?;
        r.take(12 + 8)?;
        let nodes = r.u32()? as usize;
        r.take(8)?;
        if r.pos != h.len()
            || dim == 0
            || dim > 65_536
            || quant > 1
            || nodes > image.directory.len()
        {
            return Err(corrupt("invalid collection header"));
        }
        let mut blobs = Vec::with_capacity(nodes);
        for node in 0..nodes {
            let key = (info.id, 2, node as u32);
            if !image.directory.contains_key(&key) {
                return Err(corrupt("missing node"));
            }
            blobs.push(image.object(key));
        }
        if image
            .directory
            .range((info.id, 2, 0)..=(info.id, 2, u32::MAX))
            .count()
            != nodes
        {
            return Err(corrupt("extra node"));
        }
        let vector = if quant == 0 { dim * 4 } else { dim + 4 };
        if blobs.iter().any(|b| b.len() < vector) {
            return Err(corrupt("truncated vector"));
        }
        let mut w = Writer { buf: h };
        if quant == 0 {
            for b in &blobs {
                w.bytes(&b[..vector]);
            }
        } else {
            for b in &blobs {
                w.bytes(&b[..4]);
            }
            for b in &blobs {
                w.bytes(&b[4..vector]);
            }
        }
        for b in &blobs {
            w.bytes(&b[vector..]);
        }
        let mut r = Reader {
            buf: &w.buf,
            pos: 0,
        };
        let collection = storage::decode_collection(&mut r, 2)?;
        if r.pos != w.buf.len() {
            return Err(corrupt("trailing node bytes"));
        }
        out.insert(name.clone(), collection);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CollectionConfig, Metric};
    fn fixture() -> Vec<u8> {
        let mut collections = BTreeMap::new();
        let mut c = Collection::new("c", CollectionConfig::new(2, Metric::L2)).unwrap();
        c.upsert("a", &[1., 2.], None).unwrap();
        collections.insert("c".into(), c);
        let mut image = Image::empty(123);
        image.apply(image.plan(&collections).unwrap());
        image.bytes()
    }
    fn checksums(bytes: &mut [u8]) {
        let end = bytes.len() - 4;
        let count = end / PAGE;
        bytes[32..40].copy_from_slice(&(count as u64).to_le_bytes());
        let mut pages = Vec::new();
        for p in bytes[..end].as_chunks_mut::<PAGE>().0 {
            let crc = storage::crc32(&p[..END]);
            p[END..].copy_from_slice(&crc.to_le_bytes());
            pages.push(Box::new(<[u8; PAGE]>::try_from(&p[..]).unwrap()));
        }
        bytes[end..].copy_from_slice(&tree(&pages)[1].to_le_bytes());
    }
    #[test]
    fn malformed_directory_and_nodes_are_rejected_even_with_valid_checksums() {
        let original = fixture();
        // One header page and one node page. Reseal after each mutation so this
        // exercises structural validation rather than just the checksum guard.
        for case in 0..12 {
            let mut b = original.clone();
            match case {
                0 => b[PAGE] = 9, // unknown object kind
                1 => b[PAGE + 4..PAGE + 12].copy_from_slice(&0u64.to_le_bytes()),
                2 => b[2 * PAGE + 12..2 * PAGE + 16].copy_from_slice(&99u32.to_le_bytes()), // missing node
                3 => b[2 * PAGE + 16..2 * PAGE + 20].copy_from_slice(&1u32.to_le_bytes()), // missing chunk
                4 => b[2 * PAGE + 20..2 * PAGE + 24].copy_from_slice(&u32::MAX.to_le_bytes()),
                5 => b[2 * PAGE + 4..2 * PAGE + 12].copy_from_slice(&99u64.to_le_bytes()), // orphan
                6 => b[2 * PAGE + PREFIX..2 * PAGE + PREFIX + 4]
                    .copy_from_slice(&f32::NAN.to_le_bytes()),
                7 => b[PAGE + PREFIX + 4] = 0xff, // invalid collection UTF-8
                8 => b[2 * PAGE + PREFIX + 8..2 * PAGE + PREFIX + 12]
                    .copy_from_slice(&u32::MAX.to_le_bytes()), // ID length
                9 => b[48] = 1,                   // reserved header bytes
                10 => b[PAGE + 1] = 1,            // reserved directory bytes
                11 => {
                    let page = b[2 * PAGE..3 * PAGE].to_vec();
                    b.splice(3 * PAGE..3 * PAGE, page); // duplicate node chunk
                }
                _ => unreachable!(),
            }
            checksums(&mut b);
            assert!(decode(&b).is_err(), "accepted case {case}");
        }
    }
    #[test]
    fn truncation_page_crc_and_root_crc_are_checked() {
        let good = fixture();
        for cut in [0, 7, PAGE, PAGE + 4, good.len() - 1] {
            assert!(decode(&good[..cut]).is_err());
        }
        let mut broken = good.clone();
        broken[2 * PAGE + PREFIX] ^= 1;
        assert!(decode(&broken).is_err());
        let mut broken = good.clone();
        let end = broken.len() - 1;
        broken[end] ^= 1;
        assert!(decode(&broken).is_err());
        assert_eq!(decode(&good).unwrap()["c"].len(), 1);
    }
}
