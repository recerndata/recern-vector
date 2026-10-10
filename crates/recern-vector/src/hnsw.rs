//! Hierarchical Navigable Small World graph (Malkov & Yashunin, 2016).
//!
//! The graph only stores links. Vectors live in [`Vectors`] and are passed in
//! by the owning collection, which also decides which nodes are acceptable
//! results (deleted and filtered-out nodes are still traversed as waypoints).
//!
//! Link access goes through [`ReadLinks`] / [`Links`] so the same insertion
//! code runs on a plain `&mut` graph (single inserts, queries) and on a graph
//! with one mutex per node (parallel batch build, as in hnswlib).

use std::cell::RefCell;
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::error::{Error, Result};
use crate::metric::Metric;
use crate::rng::SplitMix64;

/// Highest layer a node can be assigned to.
pub(crate) const MAX_LEVEL: usize = 16;

/// HNSW index parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HnswParams {
    /// Maximum links per node on upper layers. Layer 0 allows `2 * m`.
    pub m: usize,
    /// Candidate list size while building the graph. Higher builds a better
    /// graph more slowly.
    pub ef_construction: usize,
    /// Default candidate list size while searching. Higher improves recall at
    /// the cost of latency.
    pub ef_search: usize,
}

impl Default for HnswParams {
    fn default() -> Self {
        Self {
            m: 16,
            ef_construction: 200,
            ef_search: 64,
        }
    }
}

impl HnswParams {
    pub(crate) fn validate(&self) -> Result<()> {
        if !(2..=256).contains(&self.m) {
            return Err(Error::InvalidArgument("m must be between 2 and 256".into()));
        }
        if self.ef_construction == 0 || self.ef_search == 0 {
            return Err(Error::InvalidArgument(
                "ef values must be greater than zero".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn max_links(&self, layer: usize) -> usize {
        if layer == 0 { self.m * 2 } else { self.m }
    }
}

/// Contiguous encoded vectors, without retaining a duplicate f32 copy.
pub(crate) struct Vectors {
    pub(crate) dim: usize,
    pub(crate) data: Vec<f32>,
    pub(crate) codes: Vec<i8>,
    pub(crate) scales: Vec<f32>,
    pub(crate) encoding: crate::Quantization,
    metric: Metric,
}
impl Vectors {
    #[cfg(test)]
    pub(crate) fn new(dim: usize) -> Self {
        Self::with_encoding(dim, crate::Quantization::F32, Metric::L2)
    }
    pub(crate) fn with_encoding(dim: usize, encoding: crate::Quantization, metric: Metric) -> Self {
        Self {
            dim,
            data: Vec::new(),
            codes: Vec::new(),
            scales: Vec::new(),
            encoding,
            metric,
        }
    }
    pub(crate) fn len(&self) -> usize {
        if self.encoding == crate::Quantization::F32 {
            self.data.len() / self.dim.max(1)
        } else {
            self.scales.len()
        }
    }
    pub(crate) fn bytes(&self) -> usize {
        self.data.len() * 4 + self.codes.len() + self.scales.len() * 4
    }
    pub(crate) fn truncate(&mut self, nodes: usize) {
        self.data.truncate(nodes * self.dim);
        self.codes.truncate(nodes * self.dim);
        self.scales.truncate(nodes);
    }
    #[inline]
    pub(crate) fn get(&self, node: u32) -> std::borrow::Cow<'_, [f32]> {
        let start = node as usize * self.dim;
        if self.encoding == crate::Quantization::F32 {
            return (&self.data[start..start + self.dim]).into();
        }
        self.codes[start..start + self.dim]
            .iter()
            .map(|&v| v as f32 * self.scales[node as usize])
            .collect::<Vec<_>>()
            .into()
    }
    #[inline]
    pub(crate) fn distance(&self, query: &[f32], node: u32, metric: Metric) -> f32 {
        if self.encoding == crate::Quantization::F32 {
            return metric.distance(query, &self.get(node));
        }
        let start = node as usize * self.dim;
        let scale = self.scales[node as usize];
        let mut sum = 0.0;
        for (&a, &b) in query.iter().zip(&self.codes[start..start + self.dim]) {
            let b = b as f32 * scale;
            sum += if metric == Metric::L2 {
                (a - b) * (a - b)
            } else {
                a * b
            };
        }
        match metric {
            Metric::L2 => sum,
            Metric::Cosine => 1.0 - sum,
            Metric::Dot => -sum,
        }
    }
    pub(crate) fn prefetch(&self, node: u32) {
        if self.encoding == crate::Quantization::F32 {
            prefetch(&self.get(node));
        }
    }
    pub(crate) fn push(&mut self, vector: &[f32]) {
        debug_assert_eq!(vector.len(), self.dim);
        if self.encoding == crate::Quantization::F32 {
            self.data.extend_from_slice(vector);
            return;
        }
        let max = vector.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let mut scale = if max == 0.0 {
            1.0
        } else {
            (max / 127.0).max(f32::from_bits(1))
        };
        let start = self.codes.len();
        self.codes.extend(
            vector
                .iter()
                .map(|&v| (v / scale).round().clamp(-127.0, 127.0) as i8),
        );
        if self.metric == Metric::Cosine {
            let norm = self.codes[start..]
                .iter()
                .map(|&v| (v as f32).powi(2))
                .sum::<f32>()
                .sqrt();
            scale = 1.0 / norm;
        }
        self.scales.push(scale);
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Candidate {
    pub(crate) dist: f32,
    pub(crate) id: u32,
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Candidate {}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.dist
            .total_cmp(&other.dist)
            .then(self.id.cmp(&other.id))
    }
}

/// Work done by a search, reported by `explain`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Counters {
    pub(crate) visited: usize,
    pub(crate) distance_computations: usize,
}

/// Set of visited nodes: a bitset (one bit per node keeps it cache-resident
/// on large graphs) that is reset by clearing only the words a search
/// touched. Reused across searches on the same thread.
#[derive(Default)]
pub(crate) struct Visited {
    bits: Vec<u64>,
    touched: Vec<u32>,
    /// Scratch buffer for the unvisited neighbors of the node being expanded.
    fresh: Vec<u32>,
}

impl Visited {
    fn reset(&mut self, len: usize) {
        for word in self.touched.drain(..) {
            self.bits[word as usize] = 0;
        }
        let words = len.div_ceil(64);
        if self.bits.len() < words {
            self.bits.resize(words, 0);
        }
    }

    /// Marks `node` as visited and returns whether it was new.
    #[inline]
    fn insert(&mut self, node: u32) -> bool {
        let (word, bit) = (node as usize / 64, 1u64 << (node % 64));
        let bits = &mut self.bits[word];
        if *bits & bit != 0 {
            return false;
        }
        if *bits == 0 {
            self.touched.push(word as u32);
        }
        *bits |= bit;
        true
    }
}

thread_local! {
    static VISITED: RefCell<Visited> = RefCell::new(Visited::default());
}

/// Read access to neighbor lists.
pub(crate) trait ReadLinks {
    fn with_neighbors<R>(&self, node: u32, layer: usize, f: impl FnOnce(&[u32]) -> R) -> R;

    /// Whether `node` is linked on every layer. Only nodes another thread is
    /// still inserting are not.
    fn is_ready(&self, _node: u32) -> bool {
        true
    }
}

/// Write access to neighbor lists.
pub(crate) trait Links: ReadLinks {
    /// Writes the links chosen for a newly inserted node. Links other inserts
    /// added to it meanwhile are kept, pruned together to `max`.
    fn set_neighbors(
        &mut self,
        node: u32,
        layer: usize,
        neighbors: Vec<u32>,
        max: usize,
        vectors: &Vectors,
        metric: Metric,
    );

    /// Adds a link `node -> new`, pruning the list back to `max` links.
    fn add_link(
        &mut self,
        node: u32,
        layer: usize,
        new: u32,
        max: usize,
        vectors: &Vectors,
        metric: Metric,
    );
}

impl ReadLinks for [Vec<Vec<u32>>] {
    #[inline]
    fn with_neighbors<R>(&self, node: u32, layer: usize, f: impl FnOnce(&[u32]) -> R) -> R {
        f(&self[node as usize][layer])
    }
}

/// Exclusive access for sequential inserts.
struct Exclusive<'a>(&'a mut [Vec<Vec<u32>>]);

impl ReadLinks for Exclusive<'_> {
    fn with_neighbors<R>(&self, node: u32, layer: usize, f: impl FnOnce(&[u32]) -> R) -> R {
        self.0.with_neighbors(node, layer, f)
    }
}

impl Links for Exclusive<'_> {
    fn set_neighbors(
        &mut self,
        node: u32,
        layer: usize,
        neighbors: Vec<u32>,
        _: usize,
        _: &Vectors,
        _: Metric,
    ) {
        // Sequential inserts: nothing can have linked to `node` yet.
        self.0[node as usize][layer] = neighbors;
    }

    fn add_link(
        &mut self,
        node: u32,
        layer: usize,
        new: u32,
        max: usize,
        vectors: &Vectors,
        metric: Metric,
    ) {
        push_and_prune(
            &mut self.0[node as usize][layer],
            node,
            new,
            max,
            vectors,
            metric,
        );
    }
}

/// Shared access for parallel inserts: one lock per node.
#[derive(Clone, Copy)]
struct Shared<'a> {
    links: &'a [Mutex<Vec<Vec<u32>>>],
    ready: &'a [AtomicBool],
}

impl Shared<'_> {
    fn lock(&self, node: u32) -> MutexGuard<'_, Vec<Vec<u32>>> {
        self.links[node as usize]
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl ReadLinks for Shared<'_> {
    fn with_neighbors<R>(&self, node: u32, layer: usize, f: impl FnOnce(&[u32]) -> R) -> R {
        f(&self.lock(node)[layer])
    }

    fn is_ready(&self, node: u32) -> bool {
        self.ready[node as usize].load(AtomicOrdering::Acquire)
    }
}

impl Links for Shared<'_> {
    fn set_neighbors(
        &mut self,
        node: u32,
        layer: usize,
        neighbors: Vec<u32>,
        max: usize,
        vectors: &Vectors,
        metric: Metric,
    ) {
        let mut links = self.lock(node);
        let list = &mut links[layer];
        if list.is_empty() {
            *list = neighbors;
            return;
        }
        for neighbor in neighbors {
            if !list.contains(&neighbor) {
                list.push(neighbor);
            }
        }
        prune(list, node, max, vectors, metric);
    }

    fn add_link(
        &mut self,
        node: u32,
        layer: usize,
        new: u32,
        max: usize,
        vectors: &Vectors,
        metric: Metric,
    ) {
        push_and_prune(&mut self.lock(node)[layer], node, new, max, vectors, metric);
    }
}

pub(crate) struct Graph {
    pub(crate) params: HnswParams,
    /// `links[node][layer]` lists the neighbors of `node` on `layer`.
    pub(crate) links: Vec<Vec<Vec<u32>>>,
    pub(crate) entry: Option<u32>,
    pub(crate) max_level: usize,
    pub(crate) rng: SplitMix64,
}

impl Graph {
    pub(crate) fn new(params: HnswParams, seed: u64) -> Self {
        Self {
            params,
            links: Vec::new(),
            entry: None,
            max_level: 0,
            rng: SplitMix64::new(seed),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.links.len()
    }

    fn random_level(&mut self) -> usize {
        let ml = 1.0 / (self.params.m as f64).ln();
        let level = (-self.rng.next_f64().ln() * ml).floor() as usize;
        level.min(MAX_LEVEL)
    }

    /// Links node `id`, which must be the next node and whose vector must
    /// already be in `vectors`.
    pub(crate) fn insert(&mut self, id: u32, vectors: &Vectors, metric: Metric) {
        debug_assert_eq!(id as usize, self.links.len());
        self.insert_batch(id..id + 1, vectors, metric, 1);
    }

    /// Links nodes `nodes`, which must directly follow the existing nodes and
    /// whose vectors must already be in `vectors`, using up to `threads`
    /// threads. Levels are drawn in node order, so a single-threaded batch
    /// builds exactly the same graph as inserting the nodes one by one.
    pub(crate) fn insert_batch(
        &mut self,
        nodes: Range<u32>,
        vectors: &Vectors,
        metric: Metric,
        threads: usize,
    ) {
        debug_assert_eq!(nodes.start as usize, self.links.len());
        let levels: Vec<usize> = nodes.clone().map(|_| self.random_level()).collect();
        for &level in &levels {
            self.links.push(vec![Vec::new(); level + 1]);
        }
        let mut rest = nodes.clone();
        if self.entry.is_none() {
            let Some(first) = rest.next() else { return };
            self.entry = Some(first);
            self.max_level = levels[0];
        }
        let level_of = |node: u32| levels[(node - nodes.start) as usize];
        let params = self.params;

        // Small batches are not worth the lock overhead.
        if threads <= 1 || rest.len() < 1000 {
            VISITED.with_borrow_mut(|visited| {
                for node in rest {
                    let level = level_of(node);
                    let entry = self.entry.expect("entry is set above");
                    let mut links = Exclusive(&mut self.links);
                    link_node(
                        &mut links,
                        &params,
                        node,
                        level,
                        entry,
                        self.max_level,
                        vectors,
                        metric,
                        visited,
                    );
                    if level > self.max_level {
                        self.max_level = level;
                        self.entry = Some(node);
                    }
                }
            });
            return;
        }

        let locked: Vec<Mutex<Vec<Vec<u32>>>> = std::mem::take(&mut self.links)
            .into_iter()
            .map(Mutex::new)
            .collect();
        let top = Mutex::new((self.entry.expect("entry is set above"), self.max_level));
        let next = AtomicUsize::new(rest.start as usize);
        let ready: Vec<AtomicBool> = (0..locked.len())
            .map(|node| AtomicBool::new(node < rest.start as usize))
            .collect();
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(|| {
                    let mut links = Shared {
                        links: &locked,
                        ready: &ready,
                    };
                    let mut visited = Visited::default();
                    loop {
                        let node = next.fetch_add(1, AtomicOrdering::Relaxed);
                        if node >= rest.end as usize {
                            break;
                        }
                        let (node, level) = (node as u32, level_of(node as u32));
                        let mut top = top.lock().unwrap_or_else(PoisonError::into_inner);
                        let (entry, max_level) = *top;
                        if level > max_level {
                            // Rare: the new node becomes the entry point, so other
                            // inserts wait until it is fully linked (as in hnswlib).
                            link_node(
                                &mut links,
                                &params,
                                node,
                                level,
                                entry,
                                max_level,
                                vectors,
                                metric,
                                &mut visited,
                            );
                            *top = (node, level);
                        } else {
                            drop(top);
                            link_node(
                                &mut links,
                                &params,
                                node,
                                level,
                                entry,
                                max_level,
                                vectors,
                                metric,
                                &mut visited,
                            );
                        }
                        ready[node as usize].store(true, AtomicOrdering::Release);
                    }
                });
            }
        });
        self.links = locked
            .into_iter()
            .map(|m| m.into_inner().unwrap_or_else(PoisonError::into_inner))
            .collect();
        let (entry, max_level) = top.into_inner().unwrap_or_else(PoisonError::into_inner);
        self.entry = Some(entry);
        self.max_level = max_level;
        self.repair_unreachable(vectors, metric);
    }

    /// Concurrent pruning can, rarely, remove every incoming link of a node,
    /// making it invisible to search. Re-links each such node from its
    /// nearest reachable neighbors on layer 0. Returns how many were found.
    pub(crate) fn repair_unreachable(&mut self, vectors: &Vectors, metric: Metric) -> usize {
        let reachable = self.reachable();
        let orphans: Vec<u32> = (0..self.links.len() as u32)
            .filter(|&node| !reachable[node as usize])
            .collect();
        let Some(entry) = self.entry else { return 0 };
        let max = self.params.max_links(0);
        VISITED.with_borrow_mut(|visited| {
            for &orphan in &orphans {
                let query = vectors.get(orphan);
                let found = {
                    let links: &[Vec<Vec<u32>>] = &self.links;
                    let mut counters = Counters::default();
                    let mut ep = Candidate {
                        dist: vectors.distance(&query, entry, metric),
                        id: entry,
                    };
                    for layer in (1..=self.max_level).rev() {
                        ep = greedy(links, &query, ep, layer, vectors, metric, &mut counters);
                    }
                    let ef = self.params.ef_construction;
                    let accept = |n: u32| n != orphan;
                    search_layer(
                        links,
                        &query,
                        &[ep],
                        ef,
                        0,
                        vectors,
                        metric,
                        &accept,
                        &mut counters,
                        visited,
                    )
                };
                let mut linked = false;
                for candidate in found.iter().take(self.params.m) {
                    let list = &mut self.links[candidate.id as usize][0];
                    push_and_prune(list, candidate.id, orphan, max, vectors, metric);
                    linked |= list.contains(&orphan);
                }
                if !linked && let Some(nearest) = found.first() {
                    // The heuristic rejected every link; keep one anyway.
                    self.links[nearest.id as usize][0].push(orphan);
                }
            }
        });
        orphans.len()
    }

    /// Returns up to `k` accepted nodes closest to `query`, nearest first.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn search(
        &self,
        query: &[f32],
        k: usize,
        ef: usize,
        vectors: &Vectors,
        metric: Metric,
        accept: &dyn Fn(u32) -> bool,
        counters: &mut Counters,
    ) -> Vec<Candidate> {
        let Some(entry) = self.entry else {
            return Vec::new();
        };
        let links: &[Vec<Vec<u32>>] = &self.links;
        counters.distance_computations += 1;
        let mut ep = Candidate {
            dist: vectors.distance(query, entry, metric),
            id: entry,
        };
        for layer in (1..=self.max_level).rev() {
            ep = greedy(links, query, ep, layer, vectors, metric, counters);
        }
        let mut found = VISITED.with_borrow_mut(|visited| {
            search_layer(
                links,
                query,
                &[ep],
                ef.max(k),
                0,
                vectors,
                metric,
                accept,
                counters,
                visited,
            )
        });
        found.truncate(k);
        found
    }

    /// Number of nodes present on each layer, bottom first.
    pub(crate) fn nodes_per_layer(&self) -> Vec<usize> {
        if self.links.is_empty() {
            return Vec::new();
        }
        let mut counts = vec![0; self.max_level + 1];
        for layers in &self.links {
            for count in counts.iter_mut().take(layers.len()) {
                *count += 1;
            }
        }
        counts
    }

    /// Marks every node reachable from the entry point on layer 0.
    pub(crate) fn reachable(&self) -> Vec<bool> {
        let mut seen = vec![false; self.links.len()];
        let Some(entry) = self.entry else { return seen };
        let mut stack = vec![entry];
        seen[entry as usize] = true;
        while let Some(node) = stack.pop() {
            for &neighbor in &self.links[node as usize][0] {
                if !seen[neighbor as usize] {
                    seen[neighbor as usize] = true;
                    stack.push(neighbor);
                }
            }
        }
        seen
    }
}

/// Inserts `node` into the graph starting from a snapshot of the entry point.
/// The node's own links are written before any reverse link, so concurrent
/// searches never reach a node without neighbors on a fully linked layer.
#[allow(clippy::too_many_arguments)]
fn link_node<L: Links>(
    links: &mut L,
    params: &HnswParams,
    node: u32,
    level: usize,
    entry: u32,
    max_level: usize,
    vectors: &Vectors,
    metric: Metric,
    visited: &mut Visited,
) {
    let query = vectors.get(node);
    let mut counters = Counters::default();
    let mut ep = Candidate {
        dist: vectors.distance(&query, entry, metric),
        id: entry,
    };
    for layer in (level + 1..=max_level).rev() {
        ep = greedy(&*links, &query, ep, layer, vectors, metric, &mut counters);
    }

    let mut entry_points = vec![ep];
    for layer in (0..=level.min(max_level)).rev() {
        let found = search_layer(
            &*links,
            &query,
            &entry_points,
            params.ef_construction,
            layer,
            vectors,
            metric,
            &|n| n != node,
            &mut counters,
            visited,
        );
        let max = params.max_links(layer);
        let neighbors = select_neighbors(&found, max, vectors, metric);
        links.set_neighbors(node, layer, neighbors.clone(), max, vectors, metric);
        for neighbor in neighbors {
            links.add_link(neighbor, layer, node, max, vectors, metric);
        }
        entry_points = found;
    }
}

/// Hints the CPU to start loading `vector` into cache, one prefetch per
/// 64-byte line.
#[inline(always)]
fn prefetch(vector: &[f32]) {
    for line in vector.chunks(16) {
        let ptr = line.as_ptr();
        #[cfg(target_arch = "aarch64")]
        // SAFETY: a prefetch is only a hint; it never faults and has no
        // architectural side effects.
        unsafe {
            std::arch::asm!("prfm pldl1keep, [{0}]", in(reg) ptr, options(nostack, preserves_flags, readonly));
        }
        #[cfg(target_arch = "x86_64")]
        // SAFETY: as above; `_mm_prefetch` is available on every x86_64 CPU.
        unsafe {
            std::arch::x86_64::_mm_prefetch(ptr.cast::<i8>(), std::arch::x86_64::_MM_HINT_T0);
        }
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
        let _ = ptr;
    }
}

fn greedy<R: ReadLinks + ?Sized>(
    links: &R,
    query: &[f32],
    mut best: Candidate,
    layer: usize,
    vectors: &Vectors,
    metric: Metric,
    counters: &mut Counters,
) -> Candidate {
    loop {
        let current = best.id;
        links.with_neighbors(current, layer, |neighbors| {
            for &neighbor in neighbors {
                vectors.prefetch(neighbor);
            }
            for &neighbor in neighbors {
                counters.distance_computations += 1;
                let dist = vectors.distance(query, neighbor, metric);
                // A node still being inserted may have empty lower layers;
                // descending into it would leave the search stranded.
                if dist < best.dist && links.is_ready(neighbor) {
                    best = Candidate { dist, id: neighbor };
                }
            }
        });
        if best.id == current {
            return best;
        }
    }
}

/// Beam search on one layer. Every reachable node is traversed, but only
/// nodes passing `accept` enter the result set, so a restrictive filter
/// widens the search instead of returning fewer results.
#[allow(clippy::too_many_arguments)]
fn search_layer<R: ReadLinks + ?Sized>(
    links: &R,
    query: &[f32],
    entry_points: &[Candidate],
    ef: usize,
    layer: usize,
    vectors: &Vectors,
    metric: Metric,
    accept: &dyn Fn(u32) -> bool,
    counters: &mut Counters,
    visited: &mut Visited,
) -> Vec<Candidate> {
    visited.reset(vectors.len());
    let mut candidates = BinaryHeap::new();
    let mut results: BinaryHeap<Candidate> = BinaryHeap::new();

    for &ep in entry_points {
        if !visited.insert(ep.id) {
            continue;
        }
        counters.visited += 1;
        candidates.push(Reverse(ep));
        if accept(ep.id) {
            results.push(ep);
            if results.len() > ef {
                results.pop();
            }
        }
    }

    while let Some(Reverse(current)) = candidates.pop() {
        if results.len() >= ef && results.peek().is_some_and(|w| current.dist > w.dist) {
            break;
        }
        // Collect unvisited neighbors first and prefetch their vectors, so
        // the memory loads overlap instead of stalling one distance at a time.
        let mut fresh = std::mem::take(&mut visited.fresh);
        fresh.clear();
        links.with_neighbors(current.id, layer, |neighbors| {
            for &neighbor in neighbors {
                if visited.insert(neighbor) {
                    fresh.push(neighbor);
                }
            }
        });
        for &neighbor in &fresh {
            vectors.prefetch(neighbor);
        }
        for &neighbor in &fresh {
            counters.visited += 1;
            counters.distance_computations += 1;
            let dist = vectors.distance(query, neighbor, metric);
            let worst = results.peek().map_or(f32::INFINITY, |w| w.dist);
            if results.len() < ef || dist < worst {
                let candidate = Candidate { dist, id: neighbor };
                candidates.push(Reverse(candidate));
                if accept(neighbor) {
                    results.push(candidate);
                    if results.len() > ef {
                        results.pop();
                    }
                }
            }
        }
        visited.fresh = fresh;
    }

    let mut out = results.into_vec();
    out.sort();
    out
}

/// Neighbor selection heuristic: prefer candidates that are closer to the
/// base node than to any already selected neighbor, which keeps links spread
/// across directions. Remaining slots are filled with the nearest pruned
/// candidates. `candidates` must be sorted nearest first.
fn select_neighbors(
    candidates: &[Candidate],
    max: usize,
    vectors: &Vectors,
    metric: Metric,
) -> Vec<u32> {
    let mut selected: Vec<Candidate> = Vec::with_capacity(max);
    let mut pruned = Vec::new();
    for &candidate in candidates {
        if selected.len() >= max {
            break;
        }
        let vector = vectors.get(candidate.id);
        let diverse = selected
            .iter()
            .all(|s| vectors.distance(&vector, s.id, metric) > candidate.dist);
        if diverse {
            selected.push(candidate);
        } else {
            pruned.push(candidate);
        }
    }
    for candidate in pruned {
        if selected.len() >= max {
            break;
        }
        selected.push(candidate);
    }
    selected.into_iter().map(|c| c.id).collect()
}

fn push_and_prune(
    list: &mut Vec<u32>,
    node: u32,
    new: u32,
    max: usize,
    vectors: &Vectors,
    metric: Metric,
) {
    list.push(new);
    prune(list, node, max, vectors, metric);
}

/// Shrinks `list` back to `max` links with the selection heuristic.
fn prune(list: &mut Vec<u32>, node: u32, max: usize, vectors: &Vectors, metric: Metric) {
    if list.len() <= max {
        return;
    }
    let base = vectors.get(node);
    let mut candidates: Vec<Candidate> = list
        .iter()
        .map(|&n| Candidate {
            dist: vectors.distance(&base, n, metric),
            id: n,
        })
        .collect();
    candidates.sort();
    *list = select_neighbors(&candidates, max, vectors, metric);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random_graph(n: usize, dim: usize) -> (Graph, Vectors) {
        let mut rng = SplitMix64::new(9);
        let mut vectors = Vectors::new(dim);
        for _ in 0..n {
            let v: Vec<f32> = (0..dim).map(|_| rng.next_f64() as f32).collect();
            vectors.push(&v);
        }
        let mut graph = Graph::new(HnswParams::default(), 1);
        graph.insert_batch(0..n as u32, &vectors, Metric::L2, 1);
        (graph, vectors)
    }

    #[test]
    fn repair_relinks_orphaned_nodes() {
        let (mut graph, vectors) = random_graph(500, 8);
        let orphan = (0..500u32).find(|&n| Some(n) != graph.entry).unwrap();
        for layers in &mut graph.links {
            for list in layers.iter_mut() {
                list.retain(|&n| n != orphan);
            }
        }
        assert!(!graph.reachable()[orphan as usize]);
        assert_eq!(graph.repair_unreachable(&vectors, Metric::L2), 1);
        assert!(graph.reachable().iter().all(|&r| r));
        assert_eq!(graph.repair_unreachable(&vectors, Metric::L2), 0);
    }

    #[test]
    fn visited_set_resets_between_searches() {
        let mut visited = Visited::default();
        visited.reset(200);
        assert!(visited.insert(3) && visited.insert(130));
        assert!(!visited.insert(3));
        visited.reset(200);
        assert!(visited.insert(3) && visited.insert(130));
    }
}
