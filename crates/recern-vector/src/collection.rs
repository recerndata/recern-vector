use std::collections::{BinaryHeap, HashMap};
use std::fmt;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{Error, Result};
use crate::filter::Filter;
use crate::hnsw::{Candidate, Counters, Graph, HnswParams, Vectors};
use crate::metric::{self, Metric};
use crate::rng::SplitMix64;

pub(crate) const DEFAULT_SEED: u64 = 0x5245_4345_524E_5645;
const MAX_DIM: usize = 65_536;

const SELECTIVITY_SAMPLE: usize = 512;

/// Storage precision. Int8 uses a symmetric scale per vector; original floats
/// are not retained. Exact searches are exact over the stored approximation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Quantization {
    #[default]
    F32,
    Int8,
}
impl Quantization {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::F32 => "f32",
            Self::Int8 => "int8",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollectionConfig {
    pub dim: usize,
    pub metric: Metric,
    pub hnsw: HnswParams,
    pub quantization: Quantization,
}

impl CollectionConfig {
    pub fn new(dim: usize, metric: Metric) -> Self {
        Self {
            dim,
            metric,
            hnsw: HnswParams::default(),
            quantization: Quantization::F32,
        }
    }

    pub fn with_hnsw(mut self, hnsw: HnswParams) -> Self {
        self.hnsw = hnsw;
        self
    }

    pub fn with_quantization(mut self, quantization: Quantization) -> Self {
        self.quantization = quantization;
        self
    }

    fn validate(&self) -> Result<()> {
        if self.dim == 0 || self.dim > MAX_DIM {
            return Err(Error::InvalidArgument(format!(
                "dim must be between 1 and {MAX_DIM}"
            )));
        }
        self.hnsw.validate()
    }
}

/// A stored record. For cosine collections `vector` is the normalized vector.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub id: String,
    pub vector: Vec<f32>,
    pub metadata: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    pub id: String,
    pub distance: f32,
    pub metadata: Option<Value>,
}

#[derive(Clone, Debug, Default)]
pub struct SearchOptions {
    /// Candidate list size; defaults to the collection's `ef_search`.
    pub ef: Option<usize>,
    /// Scan every record instead of using the index.
    pub exact: bool,
    pub filter: Option<Filter>,
}

impl SearchOptions {
    pub fn ef(mut self, ef: usize) -> Self {
        self.ef = Some(ef);
        self
    }

    pub fn exact(mut self) -> Self {
        self.exact = true;
        self
    }

    pub fn filter(mut self, filter: Filter) -> Self {
        self.filter = Some(filter);
        self
    }
}

/// How a query was answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// HNSW graph search (filtered results are collected while traversing).
    Hnsw,
    /// Full scan, requested explicitly.
    Exact,
    /// Full scan chosen automatically because the filter is very selective.
    FilteredExact,
}

impl fmt::Display for Strategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Strategy::Hnsw => "hnsw",
            Strategy::Exact => "exact",
            Strategy::FilteredExact => "exact (selective filter)",
        })
    }
}

/// Search results together with the work it took to produce them.
#[derive(Clone, Debug)]
pub struct SearchReport {
    pub hits: Vec<SearchHit>,
    pub strategy: Strategy,
    /// Candidate list size used by an HNSW search.
    pub ef: Option<usize>,
    pub visited: usize,
    pub distance_computations: usize,
    /// Estimated share of live records matching the filter.
    pub filter_selectivity: Option<f64>,
    pub elapsed: Duration,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CollectionStats {
    pub name: String,
    pub config: CollectionConfig,
    pub live: usize,
    /// Deleted or replaced records still occupying space until `compact`.
    pub deleted: usize,
    /// Nodes on each HNSW layer, bottom first.
    pub nodes_per_layer: Vec<usize>,
    pub avg_degree_layer0: f64,
    /// Live records that graph search can never reach from the entry point.
    pub unreachable: usize,
    pub vector_bytes: usize,
    pub graph_bytes: usize,
    pub metadata_bytes: usize,
}

#[derive(Clone, Debug)]
pub struct RecallOptions {
    /// Number of stored vectors used as queries.
    pub sample: usize,
    pub k: usize,
    pub ef_values: Vec<usize>,
    pub seed: u64,
}

impl Default for RecallOptions {
    fn default() -> Self {
        Self {
            sample: 100,
            k: 10,
            ef_values: vec![16, 32, 64, 128, 256],
            seed: 42,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RecallPoint {
    pub ef: usize,
    /// Share of the exact top-k found by the index, averaged over queries.
    pub recall: f64,
    pub p50: Duration,
    pub p95: Duration,
}

#[derive(Clone, Debug)]
pub struct RecallReport {
    pub k: usize,
    pub sample: usize,
    pub exact_p50: Duration,
    pub points: Vec<RecallPoint>,
}

pub struct Collection {
    pub(crate) name: String,
    pub(crate) config: CollectionConfig,
    pub(crate) vectors: Vectors,
    pub(crate) ids: Vec<String>,
    pub(crate) metadata: Vec<Option<Value>>,
    pub(crate) deleted: Vec<bool>,
    pub(crate) graph: Graph,
    index: HashMap<String, u32>,
}

impl Collection {
    pub(crate) fn new(name: &str, config: CollectionConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            name: name.to_owned(),
            config,
            vectors: Vectors::with_encoding(config.dim, config.quantization, config.metric),
            ids: Vec::new(),
            metadata: Vec::new(),
            deleted: Vec::new(),
            graph: Graph::new(config.hnsw, DEFAULT_SEED),
            index: HashMap::new(),
        })
    }

    /// Reassembles a collection read from disk, rebuilding the id index.
    pub(crate) fn from_parts(
        name: String,
        config: CollectionConfig,
        vectors: Vectors,
        ids: Vec<String>,
        metadata: Vec<Option<Value>>,
        deleted: Vec<bool>,
        graph: Graph,
    ) -> Result<Self> {
        config
            .validate()
            .map_err(|e| Error::Corrupt(format!("collection '{name}': {e}")))?;
        let mut index = HashMap::with_capacity(ids.len());
        for (node, id) in ids.iter().enumerate() {
            if !deleted[node] && index.insert(id.clone(), node as u32).is_some() {
                return Err(Error::Corrupt(format!(
                    "collection '{name}': duplicate id '{id}'"
                )));
            }
        }
        Ok(Self {
            name,
            config,
            vectors,
            ids,
            metadata,
            deleted,
            graph,
            index,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn config(&self) -> &CollectionConfig {
        &self.config
    }

    /// Number of live records.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.index.contains_key(id)
    }

    /// Inserts a record, replacing any record with the same id.
    pub fn upsert(&mut self, id: &str, vector: &[f32], metadata: Option<Value>) -> Result<()> {
        let vector = self.prepare(vector)?;
        if self.ids.len() >= u32::MAX as usize {
            return Err(Error::InvalidArgument("collection is full".into()));
        }
        if let Some(&old) = self.index.get(id) {
            self.deleted[old as usize] = true;
        }
        let node = self.append(id.to_owned(), &vector, metadata);
        self.graph.insert(node, &self.vectors, self.config.metric);
        Ok(())
    }

    /// Inserts many records, replacing records with the same ids, and links
    /// them into the index using all available cores. The batch is atomic: if
    /// any vector is invalid, the collection is left unchanged. Returns the
    /// number of records written.
    pub fn upsert_many<I, S, V>(&mut self, records: I) -> Result<usize>
    where
        I: IntoIterator<Item = (S, V, Option<Value>)>,
        S: Into<String>,
        V: AsRef<[f32]>,
    {
        self.upsert_many_with_threads(records, default_threads())
    }

    /// [`Collection::upsert_many`] with an explicit thread count. With one
    /// thread the graph is identical to upserting the records one by one;
    /// with more, link order (and so the exact graph) depends on scheduling.
    pub fn upsert_many_with_threads<I, S, V>(&mut self, records: I, threads: usize) -> Result<usize>
    where
        I: IntoIterator<Item = (S, V, Option<Value>)>,
        S: Into<String>,
        V: AsRef<[f32]>,
    {
        let start = self.ids.len();
        let mut replaced = Vec::new();
        for (id, vector, metadata) in records {
            let id = id.into();
            let vector = match self.prepare(vector.as_ref()) {
                Ok(vector) if self.ids.len() < u32::MAX as usize => vector,
                Ok(_) => {
                    self.rollback(start, replaced);
                    return Err(Error::InvalidArgument("collection is full".into()));
                }
                Err(err) => {
                    self.rollback(start, replaced);
                    return Err(err);
                }
            };
            if let Some(&old) = self.index.get(&id) {
                self.deleted[old as usize] = true;
                replaced.push((id.clone(), old));
            }
            self.append(id, &vector, metadata);
        }
        let end = self.ids.len();
        self.graph.insert_batch(
            start as u32..end as u32,
            &self.vectors,
            self.config.metric,
            threads.max(1),
        );
        Ok(end - start)
    }

    /// Removes a record. Returns whether it existed.
    ///
    /// The node stays in the graph as a waypoint until [`Collection::compact`].
    pub fn delete(&mut self, id: &str) -> bool {
        match self.index.remove(id) {
            Some(node) => {
                self.deleted[node as usize] = true;
                true
            }
            None => false,
        }
    }

    pub fn get(&self, id: &str) -> Option<Record> {
        let node = *self.index.get(id)?;
        Some(Record {
            id: id.to_owned(),
            vector: self.vectors.get(node).to_vec(),
            metadata: self.metadata[node as usize].clone(),
        })
    }

    pub fn search(
        &self,
        query: &[f32],
        k: usize,
        options: &SearchOptions,
    ) -> Result<Vec<SearchHit>> {
        Ok(self.explain(query, k, options)?.hits)
    }

    /// Runs a search and reports how it was executed.
    pub fn explain(
        &self,
        query: &[f32],
        k: usize,
        options: &SearchOptions,
    ) -> Result<SearchReport> {
        let start = Instant::now();
        if k == 0 {
            return Err(Error::InvalidArgument("k must be greater than zero".into()));
        }
        let query = self.prepare(query)?;
        let filter = options.filter.as_ref();
        let selectivity = filter.map(|f| self.estimate_selectivity(f));
        let base_ef = options.ef.unwrap_or(self.config.hnsw.ef_search).max(k);
        let adaptive_ef = selectivity.map_or(base_ef, |s| {
            ((base_ef as f64 / s.max(1.0 / self.len().max(1) as f64)).ceil() as usize)
                .min(self.len())
                .max(k)
        });
        let exact_cost = selectivity.map(|s| {
            self.graph.len() as f64 / self.config.dim.max(1) as f64 + self.len() as f64 * s
        });
        let graph_cost = adaptive_ef as f64 * self.config.hnsw.m as f64 * 0.5;
        let strategy = if options.exact {
            Strategy::Exact
        } else if exact_cost.is_some_and(|cost| cost <= graph_cost) {
            Strategy::FilteredExact
        } else {
            Strategy::Hnsw
        };

        let accept = |node: u32| {
            !self.deleted[node as usize]
                && filter.is_none_or(|f| f.matches(self.metadata[node as usize].as_ref()))
        };
        let mut counters = Counters::default();
        let (found, ef) = match strategy {
            Strategy::Hnsw => {
                let ef = adaptive_ef;
                let found = self.graph.search(
                    &query,
                    k,
                    ef,
                    &self.vectors,
                    self.config.metric,
                    &accept,
                    &mut counters,
                );
                (found, Some(ef))
            }
            Strategy::Exact | Strategy::FilteredExact => {
                (self.scan(&query, k, &accept, &mut counters), None)
            }
        };

        Ok(SearchReport {
            hits: found.into_iter().map(|c| self.hit(c)).collect(),
            strategy,
            ef,
            visited: counters.visited,
            distance_computations: counters.distance_computations,
            filter_selectivity: selectivity,
            elapsed: start.elapsed(),
        })
    }

    pub fn stats(&self) -> CollectionStats {
        let nodes = self.graph.len();
        let reachable = self.graph.reachable();
        let unreachable = (0..nodes)
            .filter(|&n| !self.deleted[n] && !reachable[n])
            .count();
        let layer0_links: usize = self.graph.links.iter().map(|layers| layers[0].len()).sum();
        let graph_bytes: usize = self
            .graph
            .links
            .iter()
            .map(|layers| {
                size_of::<Vec<Vec<u32>>>()
                    + layers
                        .iter()
                        .map(|l| size_of::<Vec<u32>>() + l.len() * 4)
                        .sum::<usize>()
            })
            .sum();
        let metadata_bytes = self
            .metadata
            .iter()
            .flatten()
            .map(|m| serde_json::to_vec(m).map_or(0, |b| b.len()))
            .sum();

        CollectionStats {
            name: self.name.clone(),
            config: self.config,
            live: self.len(),
            deleted: nodes - self.len(),
            nodes_per_layer: self.graph.nodes_per_layer(),
            avg_degree_layer0: if nodes == 0 {
                0.0
            } else {
                layer0_links as f64 / nodes as f64
            },
            unreachable,
            vector_bytes: self.vectors.bytes(),
            graph_bytes,
            metadata_bytes,
        }
    }

    /// Measures how many of the exact nearest neighbors the index finds.
    ///
    /// Stored vectors are sampled as queries; each query's own record is
    /// excluded from both the exact and the approximate results.
    pub fn estimate_recall(&self, options: &RecallOptions) -> Result<RecallReport> {
        if options.k == 0 || options.sample == 0 || options.ef_values.is_empty() {
            return Err(Error::InvalidArgument(
                "k, sample and ef values must be non-empty and greater than zero".into(),
            ));
        }
        let mut live: Vec<u32> = (0..self.graph.len() as u32)
            .filter(|&n| !self.deleted[n as usize])
            .collect();
        if live.len() < 2 {
            return Err(Error::InvalidArgument(
                "recall needs at least two records".into(),
            ));
        }

        let mut rng = SplitMix64::new(options.seed);
        let sample = options.sample.min(live.len());
        for i in 0..sample {
            let j = i + rng.below(live.len() - i);
            live.swap(i, j);
        }
        let queries = &live[..sample];
        let k = options.k.min(live.len() - 1);
        let metric = self.config.metric;

        let mut exact_times = Vec::with_capacity(sample);
        let truths: Vec<Vec<u32>> = queries
            .iter()
            .map(|&q| {
                let accept = |n: u32| n != q && !self.deleted[n as usize];
                let start = Instant::now();
                let found = self.scan(&self.vectors.get(q), k, &accept, &mut Counters::default());
                exact_times.push(start.elapsed());
                found.into_iter().map(|c| c.id).collect()
            })
            .collect();

        let mut points = Vec::with_capacity(options.ef_values.len());
        for &ef in &options.ef_values {
            let mut times = Vec::with_capacity(sample);
            let mut found_total = 0;
            for (&q, truth) in queries.iter().zip(&truths) {
                let accept = |n: u32| n != q && !self.deleted[n as usize];
                let start = Instant::now();
                let found = self.graph.search(
                    &self.vectors.get(q),
                    k,
                    ef.max(k),
                    &self.vectors,
                    metric,
                    &accept,
                    &mut Counters::default(),
                );
                times.push(start.elapsed());
                found_total += found.iter().filter(|c| truth.contains(&c.id)).count();
            }
            points.push(RecallPoint {
                ef,
                recall: found_total as f64 / (k * sample) as f64,
                p50: percentile(&mut times, 0.50),
                p95: percentile(&mut times, 0.95),
            });
        }

        Ok(RecallReport {
            k,
            sample,
            exact_p50: percentile(&mut exact_times, 0.50),
            points,
        })
    }

    /// Rebuilds the collection without deleted records. Returns how many
    /// were removed.
    pub fn compact(&mut self) -> usize {
        let removed = self.graph.len() - self.len();
        if removed == 0 {
            return 0;
        }
        let mut fresh = Collection::new(&self.name, self.config)
            .expect("configuration was validated when the collection was created");
        for node in 0..self.graph.len() {
            if !self.deleted[node] {
                let vector = self.vectors.get(node as u32);
                fresh.append(self.ids[node].clone(), &vector, self.metadata[node].clone());
            }
        }
        let live = fresh.ids.len() as u32;
        fresh.graph.insert_batch(
            0..live,
            &fresh.vectors,
            fresh.config.metric,
            default_threads(),
        );
        *self = fresh;
        removed
    }

    fn prepare(&self, vector: &[f32]) -> Result<Vec<f32>> {
        if vector.len() != self.config.dim {
            return Err(Error::DimensionMismatch {
                expected: self.config.dim,
                actual: vector.len(),
            });
        }
        if vector.iter().any(|x| !x.is_finite()) {
            return Err(Error::InvalidVector(
                "contains NaN or infinite values".into(),
            ));
        }
        let mut vector = vector.to_vec();
        if self.config.metric == Metric::Cosine && !metric::normalize(&mut vector) {
            return Err(Error::InvalidVector(
                "zero vector has no direction for cosine".into(),
            ));
        }
        Ok(vector)
    }

    /// Appends an already prepared record without linking it into the graph.
    fn append(&mut self, id: String, vector: &[f32], metadata: Option<Value>) -> u32 {
        let node = self.ids.len() as u32;
        self.vectors.push(vector);
        self.ids.push(id.clone());
        self.metadata.push(metadata);
        self.deleted.push(false);
        self.index.insert(id, node);
        node
    }

    /// Undoes the appends of a failed batch that started at node `start`.
    fn rollback(&mut self, start: usize, replaced: Vec<(String, u32)>) {
        for node in start..self.ids.len() {
            let id = &self.ids[node];
            if self.index.get(id) == Some(&(node as u32)) {
                self.index.remove(id);
            }
        }
        self.vectors.truncate(start);
        self.ids.truncate(start);
        self.metadata.truncate(start);
        self.deleted.truncate(start);
        for (id, old) in replaced.into_iter().rev() {
            if (old as usize) < start {
                self.deleted[old as usize] = false;
                self.index.insert(id, old);
            }
        }
    }

    fn scan(
        &self,
        query: &[f32],
        k: usize,
        accept: &dyn Fn(u32) -> bool,
        counters: &mut Counters,
    ) -> Vec<Candidate> {
        let mut heap = BinaryHeap::with_capacity(k.min(self.len()) + 1);
        for node in 0..self.graph.len() as u32 {
            if !accept(node) {
                continue;
            }
            counters.visited += 1;
            counters.distance_computations += 1;
            let dist = self.vectors.distance(query, node, self.config.metric);
            if heap.len() < k {
                heap.push(Candidate { dist, id: node });
            } else if heap.peek().is_some_and(|w: &Candidate| dist < w.dist) {
                heap.pop();
                heap.push(Candidate { dist, id: node });
            }
        }
        let mut out = heap.into_vec();
        out.sort();
        out
    }

    /// Estimates the share of live records matching `filter`. Small
    /// collections are checked fully; larger ones from a random sample, since
    /// an evenly spaced one aliases with periodic insertion patterns.
    fn estimate_selectivity(&self, filter: &Filter) -> f64 {
        let nodes = self.graph.len();
        let mut rng = SplitMix64::new(DEFAULT_SEED);
        let sample: Box<dyn Iterator<Item = usize>> = if nodes <= SELECTIVITY_SAMPLE {
            Box::new(0..nodes)
        } else {
            Box::new((0..SELECTIVITY_SAMPLE).map(move |_| rng.below(nodes)))
        };
        let (mut checked, mut matched) = (0usize, 0usize);
        for node in sample.filter(|&n| !self.deleted[n]) {
            checked += 1;
            if filter.matches(self.metadata[node].as_ref()) {
                matched += 1;
            }
        }
        if checked == 0 {
            1.0
        } else {
            matched as f64 / checked as f64
        }
    }

    fn hit(&self, candidate: Candidate) -> SearchHit {
        let node = candidate.id as usize;
        // Rounding can push the cosine distance of identical vectors just
        // below zero; report it as 0.
        let distance = match self.config.metric {
            Metric::Cosine => candidate.dist.max(0.0),
            _ => candidate.dist,
        };
        SearchHit {
            id: self.ids[node].clone(),
            distance,
            metadata: self.metadata[node].clone(),
        }
    }
}

fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

fn percentile(times: &mut [Duration], p: f64) -> Duration {
    if times.is_empty() {
        return Duration::ZERO;
    }
    times.sort_unstable();
    times[((times.len() - 1) as f64 * p).round() as usize]
}
