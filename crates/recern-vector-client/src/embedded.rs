use crate::{
    Backend, Capabilities, CollectionSpec, Descriptor, Record,
    error::{Code, Error, Result},
    model,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use recern_vector::{Database, SearchOptions};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

pub(crate) struct Embedded {
    state: Mutex<State>,
    read_only: bool,
    identity: String,
    session: String,
}
struct State {
    db: Database,
    generations: BTreeMap<String, String>,
    revision: u64,
}
impl Embedded {
    pub fn open(path: &Path, read_only: bool) -> Result<Self> {
        let db = if read_only {
            Database::open_read_only(path)?
        } else {
            Database::open_or_create(path)?
        };
        if !read_only && db.identity() == 0 {
            db.save()?;
        }
        let generations = db
            .collections()
            .map(|c| (c.name().to_string(), Uuid::new_v4().simple().to_string()))
            .collect();
        let identity = format!("embedded:{:032x}:{}", db.identity(), db.path().display());
        Ok(Self {
            state: Mutex::new(State {
                db,
                generations,
                revision: 0,
            }),
            read_only,
            identity,
            session: Uuid::new_v4().to_string(),
        })
    }
    fn describe(state: &State, name: &str) -> Result<Descriptor> {
        let col = state.db.collection(name)?;
        let c = col.config();
        Ok(Descriptor {
            spec: CollectionSpec {
                name: name.into(),
                dim: c.dim,
                metric: c.metric.as_str().into(),
                quantization: c.quantization.as_str().into(),
            },
            identity: state
                .generations
                .get(name)
                .ok_or_else(Error::corrupt)?
                .clone(),
            creation_token: None,
        })
    }
}
impl Backend for Embedded {
    fn capabilities(&self) -> Capabilities {
        Capabilities::native(self.read_only)
    }
    fn identity(&self) -> String {
        self.identity.clone()
    }
    fn call(&self, route: &str, body: Value) -> Result<Value> {
        let mut state = self.state.lock().map_err(|_| Error::unavailable())?;
        let write = matches!(
            route,
            "/v1/collections/create"
                | "/v1/collections/drop"
                | "/v1/records/upsert"
                | "/v1/records/delete"
        );
        if write && self.read_only {
            return Err(Error::new(Code::PermissionDenied));
        }
        let name = body["collection"]["name"].as_str().unwrap_or("");
        if body.get("collection").is_some() {
            let d = Self::describe(&state, name)?;
            if body["collection"]["identity"] != d.identity {
                return Err(Error::new(Code::Conflict));
            }
        }
        let operation = || -> Result<Value> {
            Ok(match route {
                "/v1/collections/list" => {
                    json!({"collections":state.db.collections().map(|c| Self::describe(&state, c.name())).collect::<Result<Vec<_>>>()?})
                }
                "/v1/collections/describe" => {
                    json!({"collection": Self::describe(&state, body["name"].as_str().ok_or_else(Error::invalid)?)?})
                }
                "/v1/collections/create" => {
                    let spec: CollectionSpec = model::decode(body["spec"].clone())?;
                    state.db.create_collection(&spec.name, spec.config()?)?;
                    state
                        .generations
                        .insert(spec.name.clone(), Uuid::new_v4().simple().to_string());
                    json!({"collection":Self::describe(&state, &spec.name)?})
                }
                "/v1/collections/drop" => {
                    state.db.drop_collection(name)?;
                    state.generations.remove(name);
                    json!({})
                }
                "/v1/records/upsert" => {
                    let records: Vec<Record> = model::decode(body["records"].clone())?;
                    let items = records
                        .iter()
                        .map(|r| Ok((r.id.clone(), r.vector.clone(), Some(r.metadata()?))))
                        .collect::<Result<Vec<_>>>()?;
                    let written = state.db.collection_mut(name)?.upsert_many(items)?;
                    json!({"written":written})
                }
                "/v1/records/get" => {
                    json!({"record":state.db.collection(name)?.get(body["id"].as_str().ok_or_else(Error::invalid)?).map(|r| Record::new(r.id, r.vector, r.metadata.unwrap_or(Value::Null)))})
                }
                "/v1/records/delete" => {
                    json!({"deleted":state.db.collection_mut(name)?.delete(body["id"].as_str().ok_or_else(Error::invalid)?)})
                }
                "/v1/records/count" => {
                    json!({"count":state.db.collection(name)?.len().to_string()})
                }
                "/v1/records/page" => {
                    let context =
                        json!([self.session, name, state.revision, state.generations[name]]);
                    let offset = if let Some(cursor) = body["cursor"].as_str() {
                        let raw = URL_SAFE_NO_PAD
                            .decode(cursor)
                            .map_err(|_| Error::invalid())?;
                        let value = model::strict_json(
                            std::str::from_utf8(&raw).map_err(|_| Error::invalid())?,
                            16,
                        )?;
                        if value["context"] != context {
                            return Err(Error::new(Code::Conflict));
                        }
                        value["offset"]
                            .as_u64()
                            .and_then(|n| usize::try_from(n).ok())
                            .ok_or_else(Error::invalid)?
                    } else {
                        0
                    };
                    let page = state.db.collection(name)?.records_page(
                        offset,
                        body["limit"].as_u64().ok_or_else(Error::invalid)? as usize,
                    )?;
                    let cursor = page.next_offset.map(|offset| {
                        URL_SAFE_NO_PAD
                            .encode(json!({"context":context,"offset":offset}).to_string())
                    });
                    json!({"records":page.records.into_iter().map(|r| Record::new(r.id, r.vector, r.metadata.unwrap_or(Value::Null))).collect::<Vec<_>>(),"next_cursor":cursor})
                }
                "/v1/search" => {
                    let query: Vec<f32> = model::decode(body["query"].clone())?;
                    let options = SearchOptions {
                        exact: body["exact"].as_bool().ok_or_else(Error::invalid)?,
                        filter: model::filter(body["filter_json"].as_str())?,
                        ..Default::default()
                    };
                    let hits = state.db.collection(name)?.search(
                        &query,
                        body["k"].as_u64().ok_or_else(Error::invalid)? as usize,
                        &options,
                    )?;
                    json!({"hits":hits.into_iter().map(|h| json!({"id":h.id,"distance":h.distance,"metadata_json":h.metadata.unwrap_or(Value::Null).to_string()})).collect::<Vec<_>>()})
                }
                _ => return Err(Error::new(Code::Unsupported)),
            })
        };
        let mut operation = operation;
        let mut value = operation()?;
        if write && let Err(error) = state.db.save() {
            // Discard any unacknowledged in-memory mutation. Reopening failures
            // propagate; the caller must reopen rather than assume a commit.
            let db = Database::open(state.db.path()).map_err(|e| Error::from(e).write())?;
            state.generations = db
                .collections()
                .map(|c| (c.name().into(), Uuid::new_v4().simple().to_string()))
                .collect();
            state.db = db;
            state.revision += 1;
            return Err(Error::from(error).write());
        }
        if write {
            state.revision += 1;
            value["commit"] = json!({"durable":true,"sequence":state.db.sequence().to_string()});
        }
        Ok(value)
    }
    fn snapshot(&self) -> Result<Arc<dyn Backend>> {
        let state = self.state.lock().map_err(|_| Error::unavailable())?;
        let db = Database::open_read_only(state.db.path())?;
        Ok(Arc::new(Self {
            state: Mutex::new(State {
                db,
                generations: state.generations.clone(),
                revision: state.revision,
            }),
            read_only: true,
            identity: self.identity.clone(),
            session: Uuid::new_v4().to_string(),
        }))
    }
}
