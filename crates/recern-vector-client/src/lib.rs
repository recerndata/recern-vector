//! Portable profile v1. Blocking Rust API; Node runs it on libuv workers.
//! Source-only development package. Existing native APIs keep their save semantics.
mod embedded;
mod error;
mod http;
mod model;
mod qdrant;
mod remote;
pub use error::{Code, Error, Result};
pub use http::HttpOptions;
pub use model::{Capabilities, CollectionSpec, Hit, PROFILE, Page, Record, metadata, strict_json};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Descriptor {
    #[serde(flatten)]
    pub spec: CollectionSpec,
    pub identity: String,
    pub creation_token: Option<String>,
}
pub(crate) trait Backend: Send + Sync {
    fn capabilities(&self) -> Capabilities;
    fn identity(&self) -> String;
    fn call(&self, route: &str, body: Value) -> Result<Value>;
    fn snapshot(&self) -> Result<Arc<dyn Backend>>;
    fn close(&self) -> Result<()> {
        Ok(())
    }
}
#[derive(Clone)]
pub struct Client {
    backend: Arc<dyn Backend>,
    closed: Arc<AtomicBool>,
}
impl Client {
    fn wrap(backend: Arc<dyn Backend>) -> Self {
        Self {
            backend,
            closed: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn embedded(path: impl AsRef<Path>, read_only: bool) -> Result<Self> {
        Ok(Self::wrap(Arc::new(embedded::Embedded::open(
            path.as_ref(),
            read_only,
        )?)))
    }
    pub fn remote(options: HttpOptions) -> Result<Self> {
        Ok(Self::wrap(Arc::new(remote::Remote::connect(options)?)))
    }
    pub fn qdrant(options: HttpOptions, namespace: &str) -> Result<Self> {
        Ok(Self::wrap(Arc::new(qdrant::Qdrant::connect(
            options, namespace,
        )?)))
    }
    pub fn capabilities(&self) -> Capabilities {
        self.backend.capabilities()
    }
    pub fn identity(&self) -> String {
        self.backend.identity()
    }
    fn call(&self, route: &str, body: Value) -> Result<Value> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::unavailable());
        }
        self.backend.call(route, body)
    }
    pub fn close(&self) -> Result<()> {
        if !self.closed.swap(true, Ordering::AcqRel) {
            self.backend.close()?;
        }
        Ok(())
    }
    pub fn list_collections(&self) -> Result<Vec<String>> {
        let value = self.call("/v1/collections/list", json!({}))?;
        let list = value["collections"].as_array().ok_or_else(Error::corrupt)?;
        if list.len() > 10000 {
            return Err(Error::corrupt());
        }
        let mut names = Vec::new();
        for item in list {
            names.push(descriptor(item.clone())?.spec.name);
        }
        names.sort();
        if names.windows(2).any(|v| v[0] == v[1]) {
            return Err(Error::corrupt());
        }
        Ok(names)
    }
    pub fn collection(&self, name: &str) -> Result<Collection> {
        if name.is_empty() || name.len() > 255 {
            return Err(Error::invalid());
        }
        let value = self.call("/v1/collections/describe", json!({"name":name}))?;
        let d = descriptor(value["collection"].clone())?;
        if d.spec.name != name {
            return Err(Error::corrupt());
        }
        Ok(Collection {
            client: self.clone(),
            descriptor: d,
        })
    }
    pub fn create_collection(&self, spec: CollectionSpec) -> Result<Collection> {
        self.create_with_token(spec, None)
    }
    /// Recern HTTP supports explicit creation-token replay; transports never
    /// retry writes. Native files do not persist creation tokens.
    pub fn create_with_token(
        &self,
        spec: CollectionSpec,
        token: Option<&str>,
    ) -> Result<Collection> {
        spec.config()?;
        if !self
            .capabilities()
            .quantization
            .contains(&spec.quantization)
        {
            return Err(Error::new(Code::Unsupported));
        }
        if let Some(token) = token
            && uuid::Uuid::parse_str(token)
                .map(|v| v.to_string())
                .ok()
                .as_deref()
                != Some(token)
        {
            return Err(Error::invalid());
        }
        let value = self.call(
            "/v1/collections/create",
            json!({"spec":spec,"creation_token":token}),
        )?;
        ack(self, &value)?;
        let d = descriptor(value["collection"].clone()).map_err(Error::write)?;
        if d.spec != spec {
            return Err(Error::corrupt().write());
        }
        Ok(Collection {
            client: self.clone(),
            descriptor: d,
        })
    }
    pub fn drop_collection(&self, name: &str) -> Result<()> {
        self.collection(name)?.drop()
    }
    /// Owns an immutable native view or a bounded remote lease. Close remote
    /// snapshots explicitly to observe release errors; dropping is best effort.
    pub fn snapshot(&self) -> Result<Self> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::unavailable());
        }
        if !self.capabilities().consistent_export {
            return Err(Error::new(Code::Unsupported));
        }
        Ok(Self::wrap(self.backend.snapshot()?))
    }
    /// Rebind a serialized handle (used by the Node bridge). Backends verify its
    /// generation on every operation; this does not refresh a stale handle.
    pub fn bind(&self, value: Descriptor) -> Result<Collection> {
        value.spec.config()?;
        model::token(&value.identity)?;
        Ok(Collection {
            client: self.clone(),
            descriptor: value,
        })
    }
}
#[derive(Clone)]
pub struct Collection {
    client: Client,
    descriptor: Descriptor,
}
impl Collection {
    pub fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }
    pub fn spec(&self) -> &CollectionSpec {
        &self.descriptor.spec
    }
    fn body(&self) -> Value {
        json!({"collection":{"name":self.spec().name,"identity":self.descriptor.identity}})
    }
    pub fn drop(&self) -> Result<()> {
        ack(
            &self.client,
            &self.client.call("/v1/collections/drop", self.body())?,
        )
    }
    pub fn upsert(&self, record: Record) -> Result<()> {
        self.upsert_many(&[record]).map(|_| ())
    }
    pub fn upsert_many(&self, records: &[Record]) -> Result<usize> {
        model::records(records, self.spec())?;
        if records.len() > self.client.capabilities().max_batch {
            return Err(Error::invalid());
        }
        let mut body = self.body();
        body["records"] = json!(records);
        let value = self.client.call("/v1/records/upsert", body)?;
        ack(&self.client, &value)?;
        if value["written"].as_u64() != Some(records.len() as u64) {
            return Err(Error::corrupt().write());
        }
        Ok(records.len())
    }
    pub fn get(&self, id: &str) -> Result<Option<Record>> {
        model::id(id)?;
        let mut body = self.body();
        body["id"] = json!(id);
        let value = self.client.call("/v1/records/get", body)?;
        if value.get("record").is_none() {
            return Err(Error::corrupt());
        }
        if value["record"].is_null() {
            return Ok(None);
        }
        let r = response_record(value["record"].clone(), self.spec())?;
        if r.id != id {
            return Err(Error::corrupt());
        }
        Ok(Some(r))
    }
    pub fn delete(&self, id: &str) -> Result<bool> {
        model::id(id)?;
        let mut body = self.body();
        body["id"] = json!(id);
        let value = self.client.call("/v1/records/delete", body)?;
        ack(&self.client, &value)?;
        value["deleted"]
            .as_bool()
            .ok_or_else(|| Error::corrupt().write())
    }
    pub fn count(&self) -> Result<u64> {
        model::uint64(&self.client.call("/v1/records/count", self.body())?["count"])
    }
    pub fn page(&self, limit: usize, cursor: Option<&str>) -> Result<Page> {
        if !(1..=256).contains(&limit) || cursor.is_some_and(|v| v.is_empty() || v.len() > 4096) {
            return Err(Error::invalid());
        }
        let mut body = self.body();
        body["limit"] = json!(limit);
        body["cursor"] = json!(cursor);
        let value = self.client.call("/v1/records/page", body)?;
        if value.get("next_cursor").is_none() {
            return Err(Error::corrupt());
        }
        if value["records"]
            .as_array()
            .ok_or_else(Error::corrupt)?
            .iter()
            .any(|r| r.get("metadata_json").is_none())
        {
            return Err(Error::corrupt());
        }
        let page: Page = model::decode(value)?;
        if page.records.len() > limit
            || page.next_cursor.as_ref().is_some_and(|c| {
                c.is_empty()
                    || c.len() > 4096
                    || Some(c.as_str()) == cursor
                    || page.records.is_empty()
            })
        {
            return Err(Error::corrupt());
        }
        model::records(&page.records, self.spec()).map_err(|_| Error::corrupt())?;
        Ok(page)
    }
    pub fn search(
        &self,
        query: &[f32],
        k: usize,
        filter_json: Option<&str>,
        exact: bool,
    ) -> Result<Vec<Hit>> {
        model::vector(query, self.spec())?;
        model::filter(filter_json)?;
        if !(1..=10000).contains(&k) {
            return Err(Error::invalid());
        }
        if exact && !self.client.capabilities().exact_search {
            return Err(Error::new(Code::Unsupported));
        }
        let mut body = self.body();
        body["query"] = json!(query);
        body["k"] = json!(k);
        body["filter_json"] = json!(filter_json);
        body["exact"] = json!(exact);
        let value = self.client.call("/v1/search", body)?;
        let mut hits: Vec<Hit> = model::decode(value["hits"].clone())?;
        let mut ids = std::collections::BTreeSet::new();
        if hits.len() > k {
            return Err(Error::corrupt());
        }
        for h in &hits {
            if !h.distance.is_finite() || !ids.insert(&h.id) {
                return Err(Error::corrupt());
            }
            model::id(&h.id)
                .and_then(|_| model::metadata(&h.metadata_json).map(|_| ()))
                .map_err(|_| Error::corrupt())?;
        }
        hits.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap()
                .then(a.id.cmp(&b.id))
        });
        Ok(hits)
    }
}
fn descriptor(value: Value) -> Result<Descriptor> {
    let d: Descriptor = model::decode(value)?;
    d.spec.config().map_err(|_| Error::corrupt())?;
    model::token(&d.identity)?;
    Ok(d)
}
fn response_record(value: Value, spec: &CollectionSpec) -> Result<Record> {
    if value.get("metadata_json").is_none() {
        return Err(Error::corrupt());
    }
    let r: Record = model::decode(value)?;
    model::records(std::slice::from_ref(&r), spec).map_err(|_| Error::corrupt())?;
    Ok(r)
}
fn ack(client: &Client, value: &Value) -> Result<()> {
    if client.capabilities().backend == "qdrant" {
        return if value["completed"] == true {
            Ok(())
        } else {
            Err(Error::unavailable().write())
        };
    }
    if value["commit"]["durable"].as_bool() != Some(true) {
        return Err(Error::corrupt().write());
    }
    model::uint64(&value["commit"]["sequence"]).map_err(Error::write)?;
    Ok(())
}
