use napi::{Env, Task, bindgen_prelude::*};
use napi_derive::napi;
use recern_vector as rv;
use serde_json::{Value, json};
use std::sync::{Arc, RwLock};
mod portable;
fn error(e: impl std::fmt::Display) -> Error {
    Error::from_reason(e.to_string())
}

// Tokens belong to this open database, not the name or the on-disk format.
// Keep the token check and the operation under the same database lock.
struct BindingDatabase {
    db: rv::Database,
    generations: std::collections::BTreeMap<String, Arc<()>>,
}
impl BindingDatabase {
    fn new(db: rv::Database) -> Self {
        let generations = db
            .collections()
            .map(|c| (c.name().to_owned(), Arc::new(())))
            .collect();
        Self { db, generations }
    }
    fn resolve(&self, name: &str, generation: &Arc<()>) -> rv::Result<&rv::Collection> {
        self.check(name, generation)?;
        self.db.collection(name)
    }
    fn resolve_mut(&mut self, name: &str, generation: &Arc<()>) -> rv::Result<&mut rv::Collection> {
        self.check(name, generation)?;
        self.db.collection_mut(name)
    }
    fn check(&self, name: &str, generation: &Arc<()>) -> rv::Result<()> {
        if self
            .generations
            .get(name)
            .is_some_and(|current| Arc::ptr_eq(current, generation))
        {
            Ok(())
        } else {
            Err(rv::Error::CollectionNotFound(name.to_owned()))
        }
    }
}
impl std::ops::Deref for BindingDatabase {
    type Target = rv::Database;
    fn deref(&self) -> &Self::Target {
        &self.db
    }
}
impl std::ops::DerefMut for BindingDatabase {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.db
    }
}

type Shared = Arc<RwLock<BindingDatabase>>;
#[napi]
pub struct Database {
    inner: Shared,
}
#[napi(object)]
pub struct CheckpointPolicyOptions {
    pub max_wal_bytes: Option<f64>,
    pub max_commits: Option<f64>,
}
fn checkpoint_threshold(value: Option<f64>) -> Result<Option<u64>> {
    value
        .map(|n| {
            if !n.is_finite() || n.fract() != 0.0 || !(1.0..=9_007_199_254_740_991.0).contains(&n) {
                return Err(error(
                    "checkpoint thresholds must be positive safe integers or omitted",
                ));
            }
            Ok(n as u64)
        })
        .transpose()
}
#[napi(object)]
pub struct CollectionOptions {
    pub dim: f64,
    pub metric: Option<String>,
    pub quantization: Option<String>,
    pub m: Option<f64>,
    pub ef_construction: Option<f64>,
    pub ef_search: Option<f64>,
}
#[napi(object)]
pub struct SearchOptions {
    pub ef: Option<f64>,
    pub exact: Option<bool>,
    pub filter: Option<Value>,
}
fn positive_u32(value: f64, name: &str) -> Result<usize> {
    if !value.is_finite() || value.fract() != 0.0 || !(1.0..=u32::MAX as f64).contains(&value) {
        return Err(error(format!(
            "{name} must be an integer between 1 and {}",
            u32::MAX
        )));
    }
    Ok(value as usize)
}
fn options(value: Option<SearchOptions>) -> Result<rv::SearchOptions> {
    let Some(value) = value else {
        return Ok(rv::SearchOptions::default());
    };
    Ok(rv::SearchOptions {
        ef: value.ef.map(|v| positive_u32(v, "ef")).transpose()?,
        exact: value.exact.unwrap_or(false),
        filter: value
            .filter
            .as_ref()
            .map(rv::Filter::from_json)
            .transpose()
            .map_err(error)?,
    })
}
#[napi]
impl Database {
    #[napi(constructor)]
    pub fn new(path: String) -> Result<Self> {
        rv::Database::open(path).map(Self::wrap).map_err(error)
    }
    #[napi(factory)]
    pub fn create(path: String) -> Result<Self> {
        rv::Database::create(path).map(Self::wrap).map_err(error)
    }
    #[napi(factory)]
    pub fn open_or_create(path: String) -> Result<Self> {
        rv::Database::open_or_create(path)
            .map(Self::wrap)
            .map_err(error)
    }
    #[napi(factory)]
    pub fn open_read_only(path: String) -> Result<Self> {
        rv::Database::open_read_only(path)
            .map(Self::wrap)
            .map_err(error)
    }
    #[napi]
    pub fn create_collection(&self, name: String, opts: CollectionOptions) -> Result<Collection> {
        let metric = opts
            .metric
            .as_deref()
            .unwrap_or("cosine")
            .parse::<rv::Metric>()
            .map_err(error)?;
        let quantization = match opts.quantization.as_deref().unwrap_or("f32") {
            "f32" => rv::Quantization::F32,
            "int8" => rv::Quantization::Int8,
            _ => return Err(error("quantization must be f32 or int8")),
        };
        let config = rv::CollectionConfig::new(positive_u32(opts.dim, "dim")?, metric)
            .with_quantization(quantization)
            .with_hnsw(rv::HnswParams {
                m: positive_u32(opts.m.unwrap_or(16.0), "m")?,
                ef_construction: positive_u32(
                    opts.ef_construction.unwrap_or(200.0),
                    "efConstruction",
                )?,
                ef_search: positive_u32(opts.ef_search.unwrap_or(64.0), "efSearch")?,
            });
        let mut db = self.inner.write().map_err(error)?;
        db.create_collection(&name, config).map_err(error)?;
        let generation = Arc::new(());
        db.generations.insert(name.clone(), generation.clone());
        Ok(Collection {
            inner: self.inner.clone(),
            name,
            generation,
        })
    }
    #[napi]
    pub fn collection(&self, name: String) -> Result<Collection> {
        let db = self.inner.read().map_err(error)?;
        db.collection(&name).map_err(error)?;
        let generation = db.generations[&name].clone();
        Ok(Collection {
            inner: self.inner.clone(),
            name,
            generation,
        })
    }
    #[napi]
    pub fn collection_names(&self) -> Result<Vec<String>> {
        Ok(self
            .inner
            .read()
            .map_err(error)?
            .collections()
            .map(|c| c.name().into())
            .collect())
    }
    #[napi]
    pub fn drop_collection(&self, name: String) -> Result<()> {
        let mut db = self.inner.write().map_err(error)?;
        db.drop_collection(&name).map_err(error)?;
        db.generations.remove(&name);
        Ok(())
    }
    #[napi]
    pub fn save(&self) -> Result<()> {
        self.inner.read().map_err(error)?.save().map_err(error)
    }
    #[napi]
    pub fn checkpoint(&self) -> Result<()> {
        self.inner
            .read()
            .map_err(error)?
            .checkpoint()
            .map_err(error)
    }
    #[napi]
    pub fn upgrade_storage(&self) -> Result<Option<String>> {
        self.inner
            .read()
            .map_err(error)?
            .upgrade_storage()
            .map(|backup| backup.map(|p| p.to_string_lossy().into_owned()))
            .map_err(error)
    }
    #[napi(getter)]
    pub fn format_version(&self) -> Result<u32> {
        Ok(self.inner.read().map_err(error)?.format_version())
    }
    #[napi]
    pub fn set_checkpoint_policy(&self, options: CheckpointPolicyOptions) -> Result<()> {
        let policy = rv::CheckpointPolicy {
            max_wal_bytes: checkpoint_threshold(options.max_wal_bytes)?,
            max_commits: checkpoint_threshold(options.max_commits)?,
        };
        self.inner
            .write()
            .map_err(error)?
            .set_checkpoint_policy(policy)
            .map_err(error)
    }
    #[napi(getter)]
    pub fn checkpoint_policy(&self) -> Result<Value> {
        let policy = self.inner.read().map_err(error)?.checkpoint_policy();
        Ok(json!({"maxWalBytes":policy.max_wal_bytes,"maxCommits":policy.max_commits}))
    }
    #[napi]
    pub fn persistence_stats(&self) -> Result<Value> {
        let stats = self.inner.read().map_err(error)?.persistence_stats();
        Ok(
            json!({"walBytes":stats.wal_bytes,"walCommits":stats.wal_commits,
            "automaticCheckpoints":stats.automatic_checkpoints,"lastCheckpointError":stats.last_checkpoint_error,
            "lastEncodedRecords":stats.last_encoded_records,"lastWrittenPages":stats.last_written_pages,
            "lastValidatedBytes":stats.last_validated_bytes}),
        )
    }
    #[napi(getter)]
    pub fn read_only(&self) -> Result<bool> {
        Ok(self.inner.read().map_err(error)?.is_read_only())
    }
}
impl Database {
    fn wrap(db: rv::Database) -> Self {
        Self {
            inner: Arc::new(RwLock::new(BindingDatabase::new(db))),
        }
    }
}
#[napi]
pub struct Collection {
    inner: Shared,
    name: String,
    generation: Arc<()>,
}
#[napi(object)]
pub struct InputRecord {
    pub id: String,
    pub vector: Float32Array,
    pub metadata: Option<Value>,
}
fn report(report: rv::SearchReport) -> Value {
    json!({"hits": report.hits.into_iter().map(|h|json!({"id":h.id,"distance":h.distance,"metadata":h.metadata})).collect::<Vec<_>>(), "strategy":match report.strategy {rv::Strategy::Hnsw=>"hnsw",rv::Strategy::Exact=>"exact",rv::Strategy::FilteredExact=>"filtered_exact"}, "ef":report.ef,"visited":report.visited,"distanceComputations":report.distance_computations,"filterSelectivity":report.filter_selectivity,"elapsedMs":report.elapsed.as_secs_f64()*1000.0})
}
#[napi]
impl Collection {
    #[napi]
    pub fn upsert(&self, id: String, vector: Float32Array, metadata: Option<Value>) -> Result<()> {
        self.inner
            .write()
            .map_err(error)?
            .resolve_mut(&self.name, &self.generation)
            .map_err(error)?
            .upsert(&id, &vector, metadata)
            .map_err(error)
    }
    #[napi]
    pub fn upsert_many(&self, records: Vec<InputRecord>) -> Result<u32> {
        Ok(self
            .inner
            .write()
            .map_err(error)?
            .resolve_mut(&self.name, &self.generation)
            .map_err(error)?
            .upsert_many(
                records
                    .into_iter()
                    .map(|r| (r.id, r.vector.to_vec(), r.metadata)),
            )
            .map_err(error)? as u32)
    }
    #[napi]
    pub fn delete(&self, id: String) -> Result<bool> {
        Ok(self
            .inner
            .write()
            .map_err(error)?
            .resolve_mut(&self.name, &self.generation)
            .map_err(error)?
            .delete(&id))
    }
    #[napi]
    pub fn get(&self, id: String) -> Result<Option<Value>> {
        Ok(self
            .inner
            .read()
            .map_err(error)?
            .resolve(&self.name, &self.generation)
            .map_err(error)?
            .get(&id)
            .map(|r| json!({"id":r.id,"vector":r.vector,"metadata":r.metadata})))
    }
    #[napi]
    pub fn explain(
        &self,
        query: Float32Array,
        k: f64,
        opts: Option<SearchOptions>,
    ) -> Result<Value> {
        Ok(report(
            self.inner
                .read()
                .map_err(error)?
                .resolve(&self.name, &self.generation)
                .map_err(error)?
                .explain(&query, positive_u32(k, "k")?, &options(opts)?)
                .map_err(error)?,
        ))
    }
    #[napi]
    pub fn search(
        &self,
        query: Float32Array,
        k: f64,
        opts: Option<SearchOptions>,
    ) -> Result<Value> {
        Ok(self.explain(query, k, opts)?["hits"].take())
    }
    /// Copies the query before dispatch, so JS may safely reuse its buffer.
    #[napi]
    pub fn search_async(
        &self,
        query: Float32Array,
        k: f64,
        opts: Option<SearchOptions>,
    ) -> Result<AsyncTask<SearchTask>> {
        Ok(AsyncTask::new(SearchTask {
            inner: self.inner.clone(),
            name: self.name.clone(),
            generation: self.generation.clone(),
            query: query.to_vec(),
            k: positive_u32(k, "k")?,
            options: options(opts)?,
        }))
    }
    #[napi]
    pub fn compact(&self) -> Result<u32> {
        Ok(self
            .inner
            .write()
            .map_err(error)?
            .resolve_mut(&self.name, &self.generation)
            .map_err(error)?
            .compact() as u32)
    }
    #[napi]
    pub fn stats(&self) -> Result<Value> {
        let db = self.inner.read().map_err(error)?;
        let s = db
            .resolve(&self.name, &self.generation)
            .map_err(error)?
            .stats();
        Ok(
            json!({"name":s.name,"dim":s.config.dim,"metric":s.config.metric.as_str(),"quantization":s.config.quantization.as_str(),"live":s.live,"deleted":s.deleted,"vectorBytes":s.vector_bytes,"graphBytes":s.graph_bytes,"metadataBytes":s.metadata_bytes,"unreachable":s.unreachable,"nodesPerLayer":s.nodes_per_layer}),
        )
    }
}
pub struct SearchTask {
    inner: Shared,
    name: String,
    generation: Arc<()>,
    query: Vec<f32>,
    k: usize,
    options: rv::SearchOptions,
}
#[napi(object)]
pub struct Hit {
    pub id: String,
    pub distance: f64,
    pub metadata: Value,
}
impl Task for SearchTask {
    type Output = Vec<rv::SearchHit>;
    type JsValue = Vec<Hit>;
    fn compute(&mut self) -> Result<Self::Output> {
        self.inner
            .read()
            .map_err(error)?
            .resolve(&self.name, &self.generation)
            .map_err(error)?
            .search(&self.query, self.k, &self.options)
            .map_err(error)
    }
    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output
            .into_iter()
            .map(|h| Hit {
                id: h.id,
                distance: h.distance as f64,
                metadata: h.metadata.unwrap_or(Value::Null),
            })
            .collect())
    }
}
#[napi]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
#[napi]
pub fn format_version() -> u32 {
    rv::FORMAT_VERSION
}
