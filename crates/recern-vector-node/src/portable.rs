//! String-only boundary: V8 must never parse metadata/filter JSON internally.
use napi::{Env, Task, bindgen_prelude::*};
use napi_derive::napi;
use recern_vector_client::{
    self as portable, Client, Code, CollectionSpec, Descriptor, HttpOptions, Record,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

#[napi]
pub struct PortableSession {
    inner: Arc<Inner>,
}
struct Inner {
    config: Value,
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    root: Option<Client>,
    views: BTreeMap<String, Client>,
    next_view: u64,
}
fn invalid() -> portable::Error {
    portable::Error {
        code: Code::InvalidArgument,
        request_id: None,
        write_outcome: "not_applicable",
    }
}
fn string<'a>(v: &'a Value, key: &str) -> portable::Result<&'a str> {
    v[key].as_str().ok_or_else(invalid)
}
fn typed<T: for<'de> serde::Deserialize<'de>>(v: Value) -> portable::Result<T> {
    serde_json::from_value(v).map_err(|_| invalid())
}
#[napi]
impl PortableSession {
    #[napi(constructor)]
    pub fn new(options_json: String) -> Result<Self> {
        if options_json.len() > 65536 {
            return Err(Error::from_reason("InvalidArgument"));
        }
        let config = portable::strict_json(&options_json, 16)
            .map_err(|_| Error::from_reason("InvalidArgument"))?;
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                state: Mutex::new(State::default()),
            }),
        })
    }
    #[napi]
    pub fn execute(&self, request_json: String) -> AsyncTask<PortableTask> {
        AsyncTask::new(PortableTask {
            inner: self.inner.clone(),
            request_json,
        })
    }
}
pub struct PortableTask {
    inner: Arc<Inner>,
    request_json: String,
}
impl Inner {
    fn connect(&self) -> portable::Result<Client> {
        let v = &self.config;
        match string(v, "backend")? {
            "embedded" => Client::embedded(
                string(v, "path")?,
                v.get("readOnly")
                    .map_or(Some(false), Value::as_bool)
                    .ok_or_else(invalid)?,
            ),
            kind @ ("remote" | "qdrant") => {
                let api_key = match v.get("apiKey") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(v)) => Some(v.clone()),
                    _ => return Err(invalid()),
                };
                let mut options = HttpOptions::new(string(v, "url")?, api_key);
                if let Some(timeout) = v.get("timeoutMs") {
                    let ms = timeout
                        .as_u64()
                        .filter(|v| (1..=300000).contains(v))
                        .ok_or_else(invalid)?;
                    options.timeout = Duration::from_millis(ms);
                }
                if let Some(path) = v.get("caFile") {
                    options.ca_file = Some(path.as_str().ok_or_else(invalid)?.into());
                }
                if kind == "remote" {
                    Client::remote(options)
                } else {
                    Client::qdrant(
                        options,
                        v.get("namespace")
                            .map_or(Some("recern"), Value::as_str)
                            .ok_or_else(invalid)?,
                    )
                }
            }
            _ => Err(invalid()),
        }
    }
    fn operation(&self, request: Value) -> portable::Result<Value> {
        let mut state = self.state.lock().map_err(|_| invalid())?;
        if state.root.is_none() {
            state.root = Some(self.connect()?);
        }
        let view = request
            .get("view")
            .map_or(Some("root"), Value::as_str)
            .ok_or_else(invalid)?;
        let client = if view == "root" {
            state.root.as_ref().unwrap()
        } else {
            state.views.get(view).ok_or_else(invalid)?
        }
        .clone();
        drop(state);
        let p = &request["params"];
        Ok(match string(&request, "operation")? {
            "info" => json!({"identity":client.identity(),"capabilities":client.capabilities()}),
            "list" => json!(client.list_collections()?),
            "create" => {
                let spec: CollectionSpec = typed(p["spec"].clone())?;
                json!(
                    client
                        .create_with_token(spec, p["creationToken"].as_str())?
                        .descriptor()
                )
            }
            "collection" => json!(client.collection(string(p, "name")?)?.descriptor()),
            "drop" => {
                client.drop_collection(string(p, "name")?)?;
                Value::Null
            }
            "snapshot" => {
                let snapshot = client.snapshot()?;
                let mut state = self.state.lock().map_err(|_| invalid())?;
                state.next_view += 1;
                let view = state.next_view.to_string();
                let result = json!({"view":view,"identity":snapshot.identity(),"capabilities":snapshot.capabilities()});
                state.views.insert(view, snapshot);
                result
            }
            "close" => {
                // Root close releases all outstanding leases too. Collection
                // handles retain the shared closed flag and fail explicitly.
                if view == "root" {
                    let views = {
                        let mut state = self.state.lock().map_err(|_| invalid())?;
                        std::mem::take(&mut state.views)
                    };
                    let mut failure = None;
                    for value in views.into_values() {
                        if let Err(e) = value.close() {
                            failure = Some(e);
                        }
                    }
                    client.close()?;
                    if let Some(e) = failure {
                        return Err(e);
                    }
                } else {
                    client.close()?;
                    self.state.lock().map_err(|_| invalid())?.views.remove(view);
                }
                Value::Null
            }
            op => {
                let d: Descriptor = typed(p["collection"].clone())?;
                let col = client.bind(d)?;
                match op {
                    "upsert" => {
                        let records: Vec<Record> = typed(p["records"].clone())?;
                        json!(col.upsert_many(&records)?)
                    }
                    "get" => json!(col.get(string(p, "id")?)?),
                    "delete" => json!(col.delete(string(p, "id")?)?),
                    "count" => json!(col.count()?.to_string()),
                    "page" => json!(col.page(
                        p["limit"].as_u64().ok_or_else(invalid)? as usize,
                        p["cursor"].as_str()
                    )?),
                    "search" => {
                        let query: Vec<f32> = typed(p["query"].clone())?;
                        let k = p["k"]
                            .as_u64()
                            .filter(|v| *v <= 10000)
                            .ok_or_else(invalid)? as usize;
                        json!(col.search(
                            &query,
                            k,
                            p["filterJson"].as_str(),
                            p["exact"].as_bool().ok_or_else(invalid)?
                        )?)
                    }
                    _ => return Err(invalid()),
                }
            }
        })
    }
}
impl Task for PortableTask {
    type Output = String;
    type JsValue = String;
    fn compute(&mut self) -> Result<String> {
        let result = if self.request_json.len() > 64 * 1024 * 1024 {
            Err(invalid())
        } else {
            portable::strict_json(&self.request_json, 16).and_then(|v| self.inner.operation(v))
        };
        Ok(match result {
            Ok(value) => json!({"ok":value}),
            Err(error) => json!({"error":error}),
        }
        .to_string())
    }
    fn resolve(&mut self, _: Env, output: String) -> Result<String> {
        Ok(output)
    }
}
