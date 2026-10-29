use crate::{
    error::{ApiError, Result},
    wire::{self, Spec},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use recern_vector::{Collection, CollectionConfig, Database, Metric, SearchOptions};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::{File, OpenOptions},
    path::Path,
    sync::{Arc, Weak},
    time::{Duration, Instant, SystemTime},
};
use uuid::Uuid;

const CATALOG: &str = "\0recern-http/v1";
const ROOT: &str = "catalog";
const MAGIC: &str = "recern-server-catalog/1";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    spec: Spec,
    identity: String,
    creation_token: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    format: String,
    cursor_key: String,
    collections: BTreeMap<String, Entry>,
}

pub struct View {
    db: Database,
    catalog: Catalog,
}
pub struct Snapshot {
    pub view: View,
    expires: Instant,
}
pub struct Store {
    live: View,
    boot: String,
    key: Vec<u8>,
    _owner: File,
    snapshots: HashMap<String, Arc<Snapshot>>,
    retired: Vec<Weak<Snapshot>>,
    pub ready: bool,
    #[cfg(test)]
    fail_after_save: bool,
    #[cfg(test)]
    pub(crate) before_save_delay: Option<Duration>,
}

#[derive(Clone)]
pub struct Limits {
    pub request: usize,
    pub response: usize,
    pub collections: usize,
    pub active: usize,
    pub queued: usize,
    pub snapshots: usize,
    pub ttl_ms: u64,
    pub default_timeout_ms: u64,
    pub max_timeout_ms: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            request: 64 * 1024 * 1024,
            response: 64 * 1024 * 1024,
            collections: 10000,
            active: 8,
            queued: 32,
            snapshots: 1,
            ttl_ms: 300000,
            default_timeout_ms: 10000,
            max_timeout_ms: 30000,
        }
    }
}
impl Limits {
    pub fn validate(&self) -> Result<()> {
        if !(4096..=64 * 1024 * 1024).contains(&self.request)
            || !(4096..=64 * 1024 * 1024).contains(&self.response)
            || !(1..=10000).contains(&self.collections)
            || !(1..=64).contains(&self.active)
            || self.queued > 1024
            || !(1..=16).contains(&self.snapshots)
            || !(1000..=3600000).contains(&self.ttl_ms)
            || self.default_timeout_ms == 0
            || self.default_timeout_ms > self.max_timeout_ms
            || self.max_timeout_ms > 30000
        {
            return Err(ApiError::invalid());
        }
        Ok(())
    }
    pub fn json(&self) -> Value {
        json!({"max_request_bytes":self.request,"max_response_bytes":self.response,
        "max_batch":256,"max_page_size":256,"max_k":10000,"max_dim":65536,"max_metadata_bytes":1048576,
        "max_filter_bytes":1048576,"max_json_depth":64,"max_collections":self.collections,
        "default_timeout_ms":self.default_timeout_ms,"max_timeout_ms":self.max_timeout_ms,
        "max_active_operations":self.active,"max_queued_operations":self.queued,"max_snapshots":self.snapshots,"snapshot_ttl_ms":self.ttl_ms})
    }
}
pub fn capabilities(snapshot: bool) -> Value {
    json!({"backend":"recern","profile":"recern-vector/1","atomic_batches":true,
    "consistent_export":true,"quantization":["f32","int8"],"max_batch":256,"exact_search":true,"snapshot_pages":snapshot})
}

impl View {
    fn load(path: &Path, read_only: bool) -> Result<Self> {
        let db = if read_only {
            Database::open_read_only(path)?
        } else {
            Database::open(path)?
        };
        let record = db
            .collection(CATALOG)?
            .get(ROOT)
            .ok_or_else(ApiError::unavailable)?;
        let catalog: Catalog =
            serde_json::from_value(record.metadata.ok_or_else(ApiError::unavailable)?)
                .map_err(|_| ApiError::new(500, "CorruptData", "invalid_catalog"))?;
        if catalog.format != MAGIC || db.collections().count() != catalog.collections.len() + 1 {
            return Err(ApiError::new(500, "CorruptData", "invalid_catalog"));
        }
        let mut physical = BTreeSet::new();
        for (name, entry) in &catalog.collections {
            let config = entry.spec.config()?;
            if name != &entry.spec.name || !physical.insert(&entry.identity) {
                return Err(ApiError::new(500, "CorruptData", "invalid_catalog"));
            }
            let actual = db.collection(&entry.identity)?.config();
            if actual.dim != config.dim
                || actual.metric != config.metric
                || actual.quantization != config.quantization
            {
                return Err(ApiError::new(500, "CorruptData", "invalid_catalog"));
            }
        }
        Ok(Self { db, catalog })
    }
    fn entry(&self, reference: &wire::Ref) -> Result<&Entry> {
        if reference.name.is_empty() || reference.name.len() > 255 {
            return Err(ApiError::invalid());
        }
        wire::token(&reference.identity, 1)?;
        let entry = self
            .catalog
            .collections
            .get(&reference.name)
            .ok_or_else(ApiError::missing)?;
        if entry.identity != reference.identity {
            return Err(ApiError::conflict("stale_generation"));
        }
        Ok(entry)
    }
    fn collection(&self, reference: &wire::Ref) -> Result<(&Entry, &Collection)> {
        let entry = self.entry(reference)?;
        Ok((entry, self.db.collection(&entry.identity)?))
    }
    fn sequence(&self) -> String {
        self.db.sequence().to_string()
    }
}
fn descriptor(entry: &Entry) -> Value {
    json!({"name":entry.spec.name,"dim":entry.spec.dim,"metric":entry.spec.metric,
    "quantization":entry.spec.quantization,"identity":entry.identity,"creation_token":entry.creation_token})
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        // Canonicalize the parent before creating the ownership sidecar, so
        // symlink aliases cannot acquire a second writer lock for the same file.
        let path = if path.exists() {
            std::fs::canonicalize(path)
        } else {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            std::fs::canonicalize(parent).map(|p| p.join(path.file_name().unwrap_or_default()))
        }
        .map_err(|_| ApiError::unavailable())?;
        let mut owner_path = path.as_os_str().to_owned();
        owner_path.push(".server-owner");
        let owner = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(owner_path)
            .map_err(|_| ApiError::unavailable())?;
        owner
            .try_lock()
            .map_err(|_| ApiError::conflict("database_owned"))?;
        let mut db = Database::open_or_create(&path)?;
        if db.collections().count() == 0 {
            let key = [
                Uuid::new_v4().as_bytes().as_slice(),
                Uuid::new_v4().as_bytes().as_slice(),
            ]
            .concat();
            let catalog = Catalog {
                format: MAGIC.into(),
                cursor_key: URL_SAFE_NO_PAD.encode(key),
                collections: BTreeMap::new(),
            };
            db.create_collection(CATALOG, CollectionConfig::new(1, Metric::Dot))?
                .upsert(
                    ROOT,
                    &[1.0],
                    Some(serde_json::to_value(catalog).map_err(|_| ApiError::unavailable())?),
                )?;
            db.save()?;
        }
        drop(db);
        let live = View::load(&path, false)?;
        // A recovered complete WAL frame may have been written before a
        // previous fsync error. Establish a fresh durability barrier before
        // advertising readiness or durable no-op acknowledgements.
        live.db.checkpoint()?;
        let key = URL_SAFE_NO_PAD
            .decode(&live.catalog.cursor_key)
            .map_err(|_| ApiError::unavailable())?;
        if key.len() != 32 {
            return Err(ApiError::unavailable());
        }
        Ok(Self {
            live,
            boot: Uuid::new_v4().simple().to_string(),
            key,
            _owner: owner,
            snapshots: HashMap::new(),
            retired: Vec::new(),
            ready: true,
            #[cfg(test)]
            fail_after_save: false,
            #[cfg(test)]
            before_save_delay: None,
        })
    }

    fn commit<T>(&mut self, operation: impl FnOnce(&mut View) -> Result<T>) -> Result<T> {
        let result = (|| {
            let result = operation(&mut self.live)?;
            let catalog =
                serde_json::to_value(&self.live.catalog).map_err(|_| ApiError::unavailable())?;
            // Replace the small catalog only if it changed. Physical IDs and
            // data mutations share this same durable native save.
            let old = self
                .live
                .db
                .collection(CATALOG)?
                .get(ROOT)
                .and_then(|r| r.metadata);
            if old.as_ref() != Some(&catalog) {
                self.live
                    .db
                    .collection_mut(CATALOG)?
                    .upsert(ROOT, &[1.0], Some(catalog))?;
            }
            #[cfg(test)]
            if let Some(delay) = self.before_save_delay.take() {
                std::thread::sleep(delay);
            }
            self.live
                .db
                .save()
                .map_err(|e| ApiError::from(e).unknown())?;
            #[cfg(test)]
            if self.fail_after_save {
                self.fail_after_save = false;
                return Err(ApiError::unavailable().unknown());
            }
            Ok(result)
        })();
        if result.is_err() {
            match View::load(self.live.db.path(), false).and_then(|view| {
                view.db.checkpoint().map_err(ApiError::from)?;
                Ok(view)
            }) {
                Ok(view) => self.live = view,
                Err(_) => self.ready = false,
            }
        }
        result
    }
    fn cleanup(&mut self) {
        let now = Instant::now();
        self.snapshots.retain(|_, s| {
            if now >= s.expires {
                self.retired.push(Arc::downgrade(s));
                false
            } else {
                true
            }
        });
        self.retired.retain(|w| w.strong_count() > 0);
    }
    fn snapshot(&mut self, id: Option<&str>) -> Result<Option<Arc<Snapshot>>> {
        self.cleanup();
        if let Some(id) = id {
            wire::token(id, 16)?;
        }
        id.map(|id| {
            self.snapshots
                .get(id)
                .cloned()
                .ok_or_else(ApiError::missing)
        })
        .transpose()
    }

    pub fn dispatch(&mut self, path: &str, body: Value, limits: &Limits) -> Result<(u16, Vec<u8>)> {
        if !self.ready {
            return Err(ApiError::unavailable());
        }
        let status = 200;
        let value = match path {
            "/v1/info" => {
                json!({"api_version":1,"profile":"recern-vector/1","server_version":env!("CARGO_PKG_VERSION"),
                "database_id":format!("db_{:032x}",self.live.db.identity()),"sequence":self.live.sequence(),"capabilities":capabilities(false),"limits":limits.json()})
            }
            "/v1/collections/create" => {
                let request: wire::Create = wire::typed(body)?;
                let config = request.spec.config()?;
                if let Some(token) = &request.creation_token {
                    wire::uuid(token)?;
                }
                if let Some(old) = self.live.catalog.collections.get(&request.spec.name) {
                    if request.creation_token.is_some()
                        && old.creation_token == request.creation_token
                    {
                        if old.spec != request.spec {
                            return Err(ApiError::conflict("creation_token_conflict"));
                        }
                        return Ok((
                            200,
                            wire::encode(
                                &json!({"collection":descriptor(old),"commit":self.ack()}),
                                limits.response,
                            )?,
                        ));
                    }
                    return Err(ApiError::new(409, "AlreadyExists", "already_exists"));
                }
                if self.live.catalog.collections.len() >= limits.collections {
                    return Err(ApiError::new(429, "Unavailable", "collection_capacity"));
                }
                let entry = Entry {
                    spec: request.spec,
                    identity: format!("c_{}", Uuid::new_v4().simple()),
                    creation_token: request.creation_token,
                };
                let response = json!({"collection":descriptor(&entry),"commit":{"durable":true,"sequence":u64::MAX.to_string()}});
                wire::encode(&response, limits.response)?; // No successful write can have an unencodable acknowledgement.
                let created = self.commit(|view| {
                    view.db.create_collection(&entry.identity, config)?;
                    view.catalog
                        .collections
                        .insert(entry.spec.name.clone(), entry.clone());
                    Ok(descriptor(&entry))
                })?;
                return Ok((
                    201,
                    wire::encode(
                        &json!({"collection":created,"commit":self.ack()}),
                        limits.response,
                    )?,
                ));
            }
            "/v1/collections/drop" => {
                let request: wire::DropCollection = wire::typed(body)?;
                let identity = self.live.entry(&request.collection)?.identity.clone();
                self.commit(|view| {
                    view.db.drop_collection(&identity)?;
                    view.catalog.collections.remove(&request.collection.name);
                    Ok(())
                })?;
                json!({"commit":self.ack()})
            }
            "/v1/records/upsert" => {
                let request: wire::Upsert = wire::typed(body)?;
                let entry = self.live.entry(&request.collection)?.clone();
                if request.records.len() > 256 {
                    return Err(ApiError::invalid());
                }
                let mut ids = Vec::new();
                let mut vectors = Vec::new();
                let mut metadata = Vec::new();
                let mut seen = BTreeSet::new();
                for record in request.records {
                    wire::id(&record.id)?;
                    if !seen.insert(record.id.clone()) {
                        return Err(ApiError::invalid());
                    }
                    vectors.push(wire::vector(&record.vector, &entry.spec)?);
                    metadata.push(Some(wire::metadata(&record.metadata_json)?));
                    ids.push(record.id);
                }
                let written = ids.len();
                if written > 0 {
                    self.commit(|view| {
                        view.db.collection_mut(&entry.identity)?.upsert_many(
                            ids.into_iter()
                                .zip(vectors)
                                .zip(metadata)
                                .map(|((id, vector), metadata)| (id, vector, metadata)),
                        )?;
                        Ok(())
                    })?;
                }
                json!({"written":written,"commit":self.ack()})
            }
            "/v1/records/delete" => {
                let request: wire::Delete = wire::typed(body)?;
                wire::id(&request.id)?;
                let identity = self.live.entry(&request.collection)?.identity.clone();
                let deleted =
                    self.commit(|view| Ok(view.db.collection_mut(&identity)?.delete(&request.id)))?;
                json!({"deleted":deleted,"commit":self.ack()})
            }
            "/v1/snapshots/create" => {
                let _: wire::Scope = wire::typed(body.clone())?;
                if body.as_object().is_none_or(|b| !b.is_empty()) {
                    return Err(ApiError::invalid());
                }
                self.cleanup();
                if self.snapshots.len() + self.retired.len() >= limits.snapshots {
                    return Err(ApiError::new(429, "Unavailable", "snapshot_capacity"));
                }
                let view = View::load(self.live.db.path(), true)?;
                let id = format!("s_{}", Uuid::new_v4().simple());
                let duration = Duration::from_millis(limits.ttl_ms);
                let expires_at = time::OffsetDateTime::from(SystemTime::now() + duration)
                    .format(&time::format_description::well_known::Rfc3339)
                    .map_err(|_| ApiError::unavailable())?;
                let result = json!({"snapshot_id":id,"database_id":format!("db_{:032x}",view.db.identity()),"sequence":view.sequence(),"expires_at":expires_at,"capabilities":capabilities(true)});
                let encoded = wire::encode(&result, limits.response)?;
                self.snapshots.insert(
                    id,
                    Arc::new(Snapshot {
                        view,
                        expires: Instant::now() + duration,
                    }),
                );
                return Ok((201, encoded));
            }
            "/v1/snapshots/release" => {
                let request: wire::Release = wire::typed(body)?;
                wire::token(&request.snapshot_id, 16)?;
                self.cleanup();
                let released = if let Some(snapshot) = self.snapshots.remove(&request.snapshot_id) {
                    self.retired.push(Arc::downgrade(&snapshot));
                    true
                } else {
                    false
                };
                json!({"released":released})
            }
            _ => return self.read(path, body, limits),
        };
        Ok((status, wire::encode(&value, limits.response)?))
    }
    fn ack(&self) -> Value {
        json!({"durable":true,"sequence":self.live.sequence()})
    }

    fn read(&mut self, path: &str, body: Value, limits: &Limits) -> Result<(u16, Vec<u8>)> {
        let snapshot_id = body.get("snapshot_id").and_then(Value::as_str);
        let snapshot = self.snapshot(snapshot_id)?;
        let view = snapshot.as_ref().map_or(&self.live, |s| &s.view);
        let value = match path {
            "/v1/collections/list" => {
                let _: wire::Scope = wire::typed(body)?;
                json!({"collections":view.catalog.collections.values().map(descriptor).collect::<Vec<_>>(),"sequence":view.sequence()})
            }
            "/v1/collections/describe" => {
                let request: wire::Describe = wire::typed(body)?;
                if request.name.is_empty() || request.name.len() > 255 {
                    return Err(ApiError::invalid());
                }
                json!({"collection":descriptor(view.catalog.collections.get(&request.name).ok_or_else(ApiError::missing)?),"sequence":view.sequence()})
            }
            "/v1/records/get" => {
                let request: wire::Lookup = wire::typed(body)?;
                wire::id(&request.id)?;
                let (_, col) = view.collection(&request.collection)?;
                json!({"record":col.get(&request.id).map(record),"sequence":view.sequence()})
            }
            "/v1/records/count" => {
                let request: wire::Count = wire::typed(body)?;
                let (_, col) = view.collection(&request.collection)?;
                json!({"count":col.len().to_string(),"sequence":view.sequence()})
            }
            "/v1/records/page" => {
                let request: wire::Page = wire::typed(body.clone())?;
                if !(1..=256).contains(&request.limit) {
                    return Err(ApiError::invalid());
                }
                let (_, col) = view.collection(&request.collection)?;
                let mut offset = if let Some(cursor) = &request.cursor {
                    self.decode_cursor(cursor, view, &request.collection, snapshot_id)?
                } else {
                    0
                };
                let mut records = Vec::new();
                let mut total = 256;
                let cursor_reserve = self
                    .encode_cursor(view, &request.collection, snapshot_id, u32::MAX as usize)?
                    .len();
                let mut next = None;
                for _ in 0..request.limit {
                    let page = col.records_page(offset, 1)?;
                    if let Some(row) = page.records.into_iter().next() {
                        let encoded = wire::encode(&record(row), limits.response)?;
                        // Reserve enough for the authenticated context cursor,
                        // even for the longest UTF-8 collection name.
                        if total + encoded.len() + cursor_reserve > limits.response {
                            if records.is_empty() {
                                return Err(ApiError::too_large());
                            }
                            next = Some(offset);
                            break;
                        }
                        total += encoded.len() + 1;
                        records.push(encoded);
                    }
                    next = page.next_offset;
                    if let Some(position) = next {
                        offset = position;
                    } else {
                        break;
                    }
                }
                let cursor = next
                    .map(|offset| {
                        self.encode_cursor(view, &request.collection, snapshot_id, offset)
                    })
                    .transpose()?;
                return Ok((
                    200,
                    array_response(
                        "records",
                        records,
                        json!({"next_cursor":cursor,"sequence":view.sequence()}),
                        limits.response,
                    )?,
                ));
            }
            "/v1/search" => {
                let request: wire::Search = wire::typed(body)?;
                if !(1..=10000).contains(&request.k) {
                    return Err(ApiError::invalid());
                }
                let (entry, col) = view.collection(&request.collection)?;
                let query = wire::vector(&request.query, &entry.spec)?;
                let options = SearchOptions {
                    exact: request.exact,
                    filter: wire::filter(request.filter_json.as_deref())?,
                    ef: None,
                };
                let mut found = Vec::new();
                col.visit_search(&query, request.k, &options, |id, distance, metadata| {
                    found.push((id, distance, metadata));
                    Ok(())
                })?;
                found.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(b.0)));
                let mut rows = Vec::new();
                let mut total = 128;
                for (id, distance, metadata) in found {
                    let metadata_json = serde_json::to_string(metadata.unwrap_or(&Value::Null))
                        .map_err(|_| ApiError::unavailable())?;
                    let row = wire::encode(
                        &json!({"id":id,"distance":distance,"metadata_json":metadata_json}),
                        limits.response.saturating_sub(total),
                    )?;
                    total += row.len() + 1;
                    rows.push(row);
                }
                return Ok((
                    200,
                    array_response(
                        "hits",
                        rows,
                        json!({"sequence":view.sequence()}),
                        limits.response,
                    )?,
                ));
            }
            _ => return Err(ApiError::missing()),
        };
        Ok((200, wire::encode(&value, limits.response)?))
    }
    fn encode_cursor(
        &self,
        view: &View,
        collection: &wire::Ref,
        snapshot: Option<&str>,
        offset: usize,
    ) -> Result<String> {
        let value = json!([
            view.db.identity().to_string(),
            self.boot,
            collection.identity,
            snapshot,
            view.sequence(),
            offset
        ]);
        let mut data = serde_json::to_vec(&value).map_err(|_| ApiError::unavailable())?;
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.key).map_err(|_| ApiError::unavailable())?;
        mac.update(&data);
        data.extend_from_slice(&mac.finalize().into_bytes());
        Ok(URL_SAFE_NO_PAD.encode(data))
    }
    fn decode_cursor(
        &self,
        cursor: &str,
        view: &View,
        collection: &wire::Ref,
        snapshot: Option<&str>,
    ) -> Result<usize> {
        if cursor.len() > 4096 {
            return Err(ApiError::invalid());
        }
        let data = URL_SAFE_NO_PAD
            .decode(cursor)
            .map_err(|_| ApiError::invalid())?;
        if data.len() < 32 {
            return Err(ApiError::invalid());
        }
        let (payload, tag) = data.split_at(data.len() - 32);
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.key).map_err(|_| ApiError::unavailable())?;
        mac.update(payload);
        mac.verify_slice(tag).map_err(|_| ApiError::invalid())?;
        let value: Value = serde_json::from_slice(payload).map_err(|_| ApiError::invalid())?;
        let expected = json!([
            view.db.identity().to_string(),
            self.boot,
            collection.identity,
            snapshot,
            view.sequence()
        ]);
        let fields = value.as_array().ok_or_else(ApiError::invalid)?;
        if fields.len() != 6 || fields[..5] != expected.as_array().unwrap()[..] {
            return Err(ApiError::conflict("stale_cursor"));
        }
        fields[5]
            .as_u64()
            .filter(|v| *v <= u32::MAX.into())
            .map(|n| n as usize)
            .ok_or_else(ApiError::invalid)
    }
}
fn record(row: recern_vector::Record) -> Value {
    json!({"id":row.id,"vector":row.vector,"metadata_json":serde_json::to_string(row.metadata.as_ref().unwrap_or(&Value::Null)).expect("stored JSON")})
}
fn array_response(key: &str, rows: Vec<Vec<u8>>, tail: Value, limit: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(format!("{{\"{key}\":[").as_bytes());
    for (i, row) in rows.into_iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        out.extend_from_slice(&row);
    }
    out.push(b']');
    let tail = wire::encode(&tail, limit)?;
    if tail.len() > 2 {
        out.push(b',');
        out.extend_from_slice(&tail[1..tail.len() - 1]);
    }
    out.push(b'}');
    if out.len() > limit {
        return Err(ApiError::too_large());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(store: &mut Store, path: &str, body: Value) -> Value {
        serde_json::from_slice(&store.dispatch(path, body, &Limits::default()).unwrap().1).unwrap()
    }
    fn collection(store: &mut Store) -> Value {
        let value = call(
            store,
            "/v1/collections/create",
            json!({"spec":{"name":"docs","dim":2}}),
        );
        json!({"name":"docs","identity":value["collection"]["identity"]})
    }
    #[test]
    fn failed_commit_reloads_authoritative_state_and_lost_ack_preserves_new_state() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data.rvec");
        let mut store = Store::open(&path).unwrap();
        let reference = collection(&mut store);
        let id = reference["identity"].as_str().unwrap().to_string();
        let sequence = store.live.db.sequence();
        let error: Result<()> = store.commit(|view| {
            view.db
                .collection_mut(&id)?
                .upsert("uncommitted", &[1.0, 0.0], None)?;
            Err(ApiError::unavailable())
        });
        assert!(error.is_err());
        assert_eq!(store.live.db.sequence(), sequence);
        assert_eq!(
            call(
                &mut store,
                "/v1/records/count",
                json!({"collection":reference})
            )["count"],
            "0"
        );
        store.fail_after_save = true;
        let error = store
            .dispatch(
                "/v1/records/upsert",
                json!({"collection":reference,"records":[{"id":"committed","vector":[1,0]}]}),
                &Limits::default(),
            )
            .unwrap_err();
        assert_eq!(error.outcome, "unknown");
        assert_eq!(
            call(
                &mut store,
                "/v1/records/count",
                json!({"collection":reference})
            )["count"],
            "1"
        );
        let identity = store.live.db.identity();
        drop(store);
        let mut reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.live.db.identity(), identity);
        assert_eq!(
            call(
                &mut reopened,
                "/v1/records/get",
                json!({"collection":reference,"id":"committed"})
            )["record"]["id"],
            "committed"
        );
    }
    #[test]
    fn released_or_expired_snapshots_count_until_inflight_reader_drops_them() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = Store::open(&temp.path().join("data.rvec")).unwrap();
        let snapshot = call(&mut store, "/v1/snapshots/create", json!({}));
        let id = snapshot["snapshot_id"].as_str().unwrap();
        let reader = store.snapshot(Some(id)).unwrap().unwrap();
        call(
            &mut store,
            "/v1/snapshots/release",
            json!({"snapshot_id":id}),
        );
        assert_eq!(
            store
                .dispatch("/v1/snapshots/create", json!({}), &Limits::default())
                .unwrap_err()
                .status,
            429
        );
        drop(reader);
        assert_eq!(
            store
                .dispatch("/v1/snapshots/create", json!({}), &Limits::default())
                .unwrap()
                .0,
            201
        );
        let id = store.snapshots.keys().next().unwrap().clone();
        Arc::get_mut(store.snapshots.get_mut(&id).unwrap())
            .unwrap()
            .expires = Instant::now();
        let reader = store.snapshots[&id].clone();
        store.cleanup();
        assert!(store.snapshot(Some(&id)).is_err());
        assert_eq!(
            store
                .dispatch("/v1/snapshots/create", json!({}), &Limits::default())
                .unwrap_err()
                .status,
            429
        );
        drop(reader);
        assert_eq!(
            store
                .dispatch("/v1/snapshots/create", json!({}), &Limits::default())
                .unwrap()
                .0,
            201
        );
    }
    #[test]
    fn native_database_with_collections_is_not_silently_adopted() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data.rvec");
        let mut native = Database::create(&path).unwrap();
        native
            .create_collection("original", CollectionConfig::new(1, Metric::Dot))
            .unwrap();
        native.save().unwrap();
        let snapshot = std::fs::read(&path).unwrap();
        let wal = std::fs::read(path.with_extension("rvec.wal")).unwrap();
        assert!(Store::open(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), snapshot);
        assert_eq!(std::fs::read(path.with_extension("rvec.wal")).unwrap(), wal);
    }

    #[test]
    fn failed_recovery_makes_the_store_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data.rvec");
        let mut store = Store::open(&path).unwrap();
        let result: Result<()> = store.commit(|view| {
            std::fs::write(view.db.path(), b"invalid durable data").unwrap();
            Err(ApiError::unavailable().unknown())
        });
        assert!(result.is_err());
        assert!(!store.ready);
        assert_eq!(
            store
                .dispatch("/v1/info", json!({}), &Limits::default())
                .unwrap_err()
                .status,
            503
        );
    }
}
