use napi::{Env, Task, bindgen_prelude::*};
use napi_derive::napi;
use recern_vector as rv;
use serde_json::{Value, json};
use std::sync::{Arc, RwLock};
fn error(e: impl std::fmt::Display) -> Error {
    Error::from_reason(e.to_string())
}
type Shared = Arc<RwLock<rv::Database>>;
#[napi]
pub struct Database {
    inner: Shared,
}
#[napi(object)]
pub struct CollectionOptions {
    pub dim: u32,
    pub metric: Option<String>,
    pub quantization: Option<String>,
    pub m: Option<u32>,
    pub ef_construction: Option<u32>,
    pub ef_search: Option<u32>,
}
#[napi(object)]
pub struct SearchOptions {
    pub ef: Option<u32>,
    pub exact: Option<bool>,
    pub filter: Option<Value>,
}
fn options(value: Option<SearchOptions>) -> Result<rv::SearchOptions> {
    let Some(value) = value else {
        return Ok(rv::SearchOptions::default());
    };
    Ok(rv::SearchOptions {
        ef: value.ef.map(|v| v as usize),
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
        let config = rv::CollectionConfig::new(opts.dim as usize, metric)
            .with_quantization(quantization)
            .with_hnsw(rv::HnswParams {
                m: opts.m.unwrap_or(16) as usize,
                ef_construction: opts.ef_construction.unwrap_or(200) as usize,
                ef_search: opts.ef_search.unwrap_or(64) as usize,
            });
        self.inner
            .write()
            .map_err(error)?
            .create_collection(&name, config)
            .map_err(error)?;
        Ok(Collection {
            inner: self.inner.clone(),
            name,
        })
    }
    #[napi]
    pub fn collection(&self, name: String) -> Result<Collection> {
        self.inner
            .read()
            .map_err(error)?
            .collection(&name)
            .map_err(error)?;
        Ok(Collection {
            inner: self.inner.clone(),
            name,
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
        self.inner
            .write()
            .map_err(error)?
            .drop_collection(&name)
            .map_err(error)
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
    #[napi(getter)]
    pub fn read_only(&self) -> Result<bool> {
        Ok(self.inner.read().map_err(error)?.is_read_only())
    }
}
impl Database {
    fn wrap(db: rv::Database) -> Self {
        Self {
            inner: Arc::new(RwLock::new(db)),
        }
    }
}
#[napi]
pub struct Collection {
    inner: Shared,
    name: String,
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
            .collection_mut(&self.name)
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
            .collection_mut(&self.name)
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
            .collection_mut(&self.name)
            .map_err(error)?
            .delete(&id))
    }
    #[napi]
    pub fn get(&self, id: String) -> Result<Option<Value>> {
        Ok(self
            .inner
            .read()
            .map_err(error)?
            .collection(&self.name)
            .map_err(error)?
            .get(&id)
            .map(|r| json!({"id":r.id,"vector":r.vector,"metadata":r.metadata})))
    }
    #[napi]
    pub fn explain(
        &self,
        query: Float32Array,
        k: u32,
        opts: Option<SearchOptions>,
    ) -> Result<Value> {
        Ok(report(
            self.inner
                .read()
                .map_err(error)?
                .collection(&self.name)
                .map_err(error)?
                .explain(&query, k as usize, &options(opts)?)
                .map_err(error)?,
        ))
    }
    #[napi]
    pub fn search(
        &self,
        query: Float32Array,
        k: u32,
        opts: Option<SearchOptions>,
    ) -> Result<Value> {
        Ok(self.explain(query, k, opts)?["hits"].take())
    }
    /// Copies the query before dispatch, so JS may safely reuse its buffer.
    #[napi]
    pub fn search_async(
        &self,
        query: Float32Array,
        k: u32,
        opts: Option<SearchOptions>,
    ) -> Result<AsyncTask<SearchTask>> {
        Ok(AsyncTask::new(SearchTask {
            inner: self.inner.clone(),
            name: self.name.clone(),
            query: query.to_vec(),
            k: k as usize,
            options: options(opts)?,
        }))
    }
    #[napi]
    pub fn compact(&self) -> Result<u32> {
        Ok(self
            .inner
            .write()
            .map_err(error)?
            .collection_mut(&self.name)
            .map_err(error)?
            .compact() as u32)
    }
    #[napi]
    pub fn stats(&self) -> Result<Value> {
        let db = self.inner.read().map_err(error)?;
        let s = db.collection(&self.name).map_err(error)?.stats();
        Ok(
            json!({"name":s.name,"dim":s.config.dim,"metric":s.config.metric.as_str(),"quantization":s.config.quantization.as_str(),"live":s.live,"deleted":s.deleted,"vectorBytes":s.vector_bytes,"graphBytes":s.graph_bytes,"metadataBytes":s.metadata_bytes,"unreachable":s.unreachable,"nodesPerLayer":s.nodes_per_layer}),
        )
    }
}
pub struct SearchTask {
    inner: Shared,
    name: String,
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
            .collection(&self.name)
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
