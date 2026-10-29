use crate::{
    Backend, Capabilities, CollectionSpec, Descriptor, HttpOptions, Record,
    error::{Code, Error, Result},
    http::Transport,
    model,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use uuid::Uuid;
const ID_NAMESPACE: Uuid = Uuid::from_u128(0xc6c9778086c34d47a0f4d956ccacb498);
fn point_id(id: &str) -> String {
    Uuid::new_v5(&ID_NAMESPACE, id.as_bytes()).to_string()
}
fn metric(s: &str) -> &'static str {
    match s {
        "cosine" => "Cosine",
        "l2" => "Euclid",
        _ => "Dot",
    }
}

pub(crate) struct Qdrant {
    http: Transport,
    namespace: String,
    prefix: String,
    identity: String,
}
impl Qdrant {
    pub fn connect(options: HttpOptions, namespace: &str) -> Result<Self> {
        if namespace.is_empty()
            || namespace.len() > 32
            || !namespace
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(Error::invalid());
        }
        let http = Transport::new(options, false)?;
        let info = http.request("GET", "/", None, false, false)?;
        let version = info["version"]
            .as_str()
            .ok_or_else(|| Error::new(Code::Unsupported))?;
        let parts = version
            .split('.')
            .map(str::parse::<u32>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| Error::new(Code::Unsupported))?;
        if parts.len() != 3 || parts[0] != 1 || parts.as_slice() < [1, 19, 2].as_slice() {
            return Err(Error::new(Code::Unsupported));
        }
        let identity = format!(
            "qdrant:{:x}",
            Sha256::digest(format!("{}\0{namespace}", http.url))
        );
        Ok(Self {
            http,
            namespace: namespace.into(),
            prefix: format!("rv1_{namespace}_"),
            identity,
        })
    }
    fn physical(&self, name: &str) -> String {
        format!("{}{:x}", self.prefix, Sha256::digest(name.as_bytes()))
    }
    fn route(&self, name: &str) -> String {
        format!("/collections/{}", self.physical(name))
    }
    fn request(
        &self,
        method: &str,
        route: &str,
        body: Option<&Value>,
        write: bool,
    ) -> Result<Value> {
        let value = self.http.request(method, route, body, false, write)?;
        if value["status"] != "ok" || value.get("result").is_none() {
            return Err(if write {
                Error::corrupt().write()
            } else {
                Error::corrupt()
            });
        }
        Ok(value["result"].clone())
    }
    fn details(&self, name: &str) -> Result<Descriptor> {
        let value = self.request("GET", &self.route(name), None, false)?;
        let config = &value["config"];
        let marker = &config["metadata"]["recern"];
        let v = &config["params"]["vectors"];
        let generation = marker["generation"]
            .as_str()
            .ok_or_else(|| Error::new(Code::Unsupported))?;
        if marker["profile"] != model::PROFILE
            || marker["name"] != name
            || marker["namespace"] != self.namespace
            || Uuid::parse_str(generation)
                .map(|v| v.to_string())
                .ok()
                .as_deref()
                != Some(generation)
            || v.get("datatype").is_some_and(|v| v != "float32")
            || !config["quantization_config"].is_null()
        {
            return Err(Error::new(Code::Unsupported));
        }
        let m = match v["distance"].as_str() {
            Some("Cosine") => "cosine",
            Some("Euclid") => "l2",
            Some("Dot") => "dot",
            _ => return Err(Error::new(Code::Unsupported)),
        };
        let spec = CollectionSpec::new(
            name,
            v["size"].as_u64().ok_or_else(Error::corrupt)? as usize,
            m,
        );
        spec.config().map_err(|_| Error::corrupt())?;
        Ok(Descriptor {
            spec,
            identity: generation.into(),
            creation_token: Some(generation.into()),
        })
    }
    fn payload(&self, point: &Value) -> Result<(String, String)> {
        let p = &point["payload"]["_recern"];
        let id = p["id"].as_str().ok_or_else(Error::corrupt)?;
        let text = p["metadata"].as_str().ok_or_else(Error::corrupt)?;
        model::id(id)
            .and_then(|_| model::metadata(text).map(|_| ()))
            .map_err(|_| Error::corrupt())?;
        if p["profile"] != model::PROFILE || point["id"].as_str() != Some(point_id(id).as_str()) {
            return Err(Error::corrupt());
        }
        Ok((id.into(), text.into()))
    }
    fn record(&self, point: &Value, spec: &CollectionSpec) -> Result<Record> {
        let (id, metadata_json) = self.payload(point)?;
        let vector: Vec<f32> = model::decode(point["vector"].clone())?;
        let record = Record {
            id,
            vector,
            metadata_json,
        };
        model::records(std::slice::from_ref(&record), spec).map_err(|_| Error::corrupt())?;
        Ok(record)
    }
    fn retrieve(&self, name: &str, ids: Vec<String>, vectors: bool) -> Result<Vec<Value>> {
        model::decode(self.request(
            "POST",
            &format!("{}/points?consistency=all", self.route(name)),
            Some(&json!({"ids":ids,"with_payload":true,"with_vector":vectors})),
            false,
        )?)
    }
    fn completed(&self, value: &Value) -> Result<()> {
        if value["status"] != "completed" {
            return Err(Error::unavailable().write());
        }
        Ok(())
    }
}
impl Backend for Qdrant {
    fn identity(&self) -> String {
        self.identity.clone()
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            backend: "qdrant".into(),
            profile: model::PROFILE.into(),
            atomic_batches: false,
            consistent_export: false,
            quantization: vec!["f32".into()],
            max_batch: 256,
            exact_search: true,
            snapshot_pages: false,
        }
    }
    fn snapshot(&self) -> Result<Arc<dyn Backend>> {
        Err(Error::new(Code::Unsupported))
    }
    fn call(&self, route: &str, body: Value) -> Result<Value> {
        if route == "/v1/collections/list" {
            let result = self.request("GET", "/collections", None, false)?;
            let mut list = Vec::new();
            for entry in result["collections"]
                .as_array()
                .ok_or_else(Error::corrupt)?
            {
                let name = entry["name"].as_str().ok_or_else(Error::corrupt)?;
                let suffix = name.strip_prefix(&self.prefix);
                if !suffix.is_some_and(|v| {
                    v.len() == 64
                        && v.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                }) {
                    continue;
                }
                let info = self.request("GET", &format!("/collections/{name}"), None, false)?;
                let logical = info["config"]["metadata"]["recern"]["name"]
                    .as_str()
                    .ok_or_else(Error::corrupt)?;
                if self.physical(logical) != name {
                    return Err(Error::corrupt());
                }
                list.push(self.details(logical)?);
                if list.len() > 10000 {
                    return Err(Error::new(Code::Unsupported));
                }
            }
            return Ok(json!({"collections":list}));
        }
        if route == "/v1/collections/create" {
            let spec: CollectionSpec = model::decode(body["spec"].clone())?;
            spec.config()?;
            if spec.quantization != "f32" {
                return Err(Error::new(Code::Unsupported));
            }
            match self.details(&spec.name) {
                Ok(d) if body["creation_token"] == d.identity && d.spec == spec => {
                    return Ok(json!({"collection":d,"completed":true}));
                }
                Ok(_) => return Err(Error::new(Code::AlreadyExists)),
                Err(e) if e.code == Code::NotFound => (),
                Err(e) => return Err(e),
            }
            let generation = body["creation_token"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            self.request("PUT", &self.route(&spec.name), Some(&json!({"vectors":{"size":spec.dim,"distance":metric(&spec.metric),"datatype":"float32"},
                "metadata":{"recern":{"profile":model::PROFILE,"name":spec.name,"namespace":self.namespace,"generation":generation}}})), true)?;
            let d = self.details(&spec.name)?;
            if d.identity != generation || d.spec != spec {
                return Err(Error::new(Code::Conflict).write());
            }
            return Ok(json!({"collection":d,"completed":true}));
        }
        let name = if route == "/v1/collections/describe" {
            body["name"].as_str()
        } else {
            body["collection"]["name"].as_str()
        }
        .ok_or_else(Error::invalid)?;
        let d = self.details(name)?;
        if body.get("collection").is_some() && body["collection"]["identity"] != d.identity {
            return Err(Error::new(Code::Conflict));
        }
        let physical = self.route(name);
        Ok(match route {
            "/v1/collections/describe" => json!({"collection":d}),
            "/v1/collections/drop" => {
                self.request("DELETE", &physical, None, true)?;
                json!({"completed":true})
            }
            "/v1/records/upsert" => {
                let records: Vec<Record> = model::decode(body["records"].clone())?;
                model::records(&records, &d.spec)?;
                if records.is_empty() {
                    return Ok(json!({"written":0,"completed":true}));
                }
                let expected: std::collections::BTreeMap<_, _> =
                    records.iter().map(|r| (point_id(&r.id), &r.id)).collect();
                if expected.len() != records.len() {
                    return Err(Error::new(Code::Conflict));
                }
                for point in self.retrieve(name, expected.keys().cloned().collect(), false)? {
                    let (id, _) = self.payload(&point)?;
                    if expected
                        .get(point["id"].as_str().ok_or_else(Error::corrupt)?)
                        .map(|v| v.as_str())
                        != Some(id.as_str())
                    {
                        return Err(Error::new(Code::Conflict));
                    }
                }
                let points = records
                    .iter()
                    .map(|r| {
                        let mut p = projection(&r.metadata()?);
                        p["id"] = json!(r.id);
                        p["profile"] = json!(model::PROFILE);
                        p["metadata"] = json!(r.metadata_json);
                        Ok(json!({"id":point_id(&r.id),"vector":r.vector,"payload":{"_recern":p}}))
                    })
                    .collect::<Result<Vec<_>>>()?;
                self.completed(&self.request(
                    "PUT",
                    &format!("{physical}/points?wait=true&ordering=strong"),
                    Some(&json!({"points":points})),
                    true,
                )?)?;
                json!({"written":records.len(),"completed":true})
            }
            "/v1/records/get" | "/v1/records/delete" => {
                let id = body["id"].as_str().ok_or_else(Error::invalid)?;
                let points = self.retrieve(name, vec![point_id(id)], true)?;
                if points.len() > 1 {
                    return Err(Error::corrupt());
                }
                let record = points
                    .first()
                    .map(|p| self.record(p, &d.spec))
                    .transpose()?;
                if record.as_ref().is_some_and(|r| r.id != id) {
                    return Err(Error::new(Code::Conflict));
                }
                if route == "/v1/records/get" {
                    json!({"record":record})
                } else {
                    if record.is_some() {
                        self.completed(&self.request(
                            "POST",
                            &format!("{physical}/points/delete?wait=true&ordering=strong"),
                            Some(&json!({"points":[point_id(id)]})),
                            true,
                        )?)?;
                    }
                    json!({"deleted":record.is_some(),"completed":true})
                }
            }
            "/v1/records/count" => {
                let result = self.request(
                    "POST",
                    &format!("{physical}/points/count?consistency=all"),
                    Some(&json!({"exact":true})),
                    false,
                )?;
                json!({"count":result["count"].as_u64().ok_or_else(Error::corrupt)?.to_string()})
            }
            "/v1/records/page" => {
                let context = json!([self.identity, name, d.identity]);
                let mut request =
                    json!({"limit":body["limit"],"with_payload":true,"with_vector":true});
                if let Some(cursor) = body["cursor"].as_str() {
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
                    let offset = value["offset"].as_str().ok_or_else(Error::invalid)?;
                    Uuid::parse_str(offset).map_err(|_| Error::invalid())?;
                    request["offset"] = json!(offset);
                }
                let result = self.request(
                    "POST",
                    &format!("{physical}/points/scroll?consistency=all"),
                    Some(&request),
                    false,
                )?;
                let records = result["points"]
                    .as_array()
                    .ok_or_else(Error::corrupt)?
                    .iter()
                    .map(|p| self.record(p, &d.spec))
                    .collect::<Result<Vec<_>>>()?;
                let next_cursor = if result["next_page_offset"].is_null() {
                    None
                } else {
                    let offset = result["next_page_offset"]
                        .as_str()
                        .ok_or_else(Error::corrupt)?;
                    Uuid::parse_str(offset).map_err(|_| Error::corrupt())?;
                    Some(
                        URL_SAFE_NO_PAD
                            .encode(json!({"context":context,"offset":offset}).to_string()),
                    )
                };
                json!({"records":records,"next_cursor":next_cursor})
            }
            "/v1/search" => {
                let filter = body["filter_json"]
                    .as_str()
                    .map(model::metadata)
                    .transpose()?;
                let result = self.request("POST", &format!("{physical}/points/query?consistency=all"), Some(&json!({"query":body["query"],"limit":body["k"],"with_payload":true,"params":{"exact":body["exact"]},"filter":filter.as_ref().map(qdrant_filter)})), false)?;
                let mut hits = Vec::new();
                for point in result["points"].as_array().ok_or_else(Error::corrupt)? {
                    let (id, text) = self.payload(point)?;
                    let score = point["score"]
                        .as_f64()
                        .filter(|v| v.is_finite())
                        .ok_or_else(Error::corrupt)?;
                    let distance = match d.spec.metric.as_str() {
                        "cosine" => (1.0 - score).max(0.0),
                        "l2" => score * score,
                        _ => -score,
                    };
                    hits.push(json!({"id":id,"distance":distance,"metadata_json":text}));
                }
                json!({"hits":hits})
            }
            _ => return Err(Error::new(Code::Unsupported)),
        })
    }
}

// Frozen Python v1 envelope: full values (including arrays/objects) have typed
// equality keys; only object paths are traversed. bool never equals numeric 1.
fn hex_float(n: f64) -> String {
    if n == 0.0 {
        return "0x0.0p+0".into();
    }
    let bits = n.to_bits();
    let exp = ((bits >> 52) & 2047) as i32;
    let fraction = bits & ((1u64 << 52) - 1);
    format!(
        "{}0x{}.{fraction:013x}p{:+}",
        if n.is_sign_negative() { "-" } else { "" },
        if exp == 0 { 0 } else { 1 },
        if exp == 0 { -1022 } else { exp - 1023 }
    )
}
fn typed(v: &Value) -> Value {
    match v {
        Value::Null => json!(["null"]),
        Value::Bool(v) => json!(["bool", v]),
        Value::String(v) => json!(["string", v]),
        Value::Number(n) if n.is_f64() => json!(["float", hex_float(n.as_f64().unwrap())]),
        Value::Number(n) => json!(["int", n.to_string()]),
        Value::Array(v) => json!(["array", v.iter().map(typed).collect::<Vec<_>>()]),
        Value::Object(v) => json!([
            "object",
            v.iter()
                .map(|(k, v)| json!([k, typed(v)]))
                .collect::<Vec<_>>()
        ]),
    }
}
fn equality(v: &Value) -> String {
    if v.is_number() {
        json!(["number", hex_float(v.as_f64().unwrap())]).to_string()
    } else {
        typed(v).to_string()
    }
}
fn path_key(s: &str) -> String {
    let mut out = String::from("p");
    for b in s.as_bytes() {
        use std::fmt::Write;
        write!(&mut out, "{b:02x}").unwrap();
    }
    out
}
fn projection(v: &Value) -> Value {
    fn visit(
        v: &Value,
        prefix: Option<&str>,
        eq: &mut Map<String, Value>,
        num: &mut Map<String, Value>,
    ) {
        if let Some(obj) = v.as_object() {
            for (k, v) in obj {
                if k.contains('.') {
                    continue;
                }
                let path = prefix.map_or_else(|| k.clone(), |p| format!("{p}.{k}"));
                let key = path_key(&path);
                eq.insert(key.clone(), json!(equality(v)));
                if let Some(n) = v.as_f64() {
                    num.insert(key, json!(n));
                }
                visit(v, Some(&path), eq, num);
            }
        }
    }
    let (mut eq, mut num) = (Map::new(), Map::new());
    visit(v, None, &mut eq, &mut num);
    json!({"eq":eq,"num":num})
}
fn qdrant_filter(v: &Value) -> Value {
    let mut parts = Vec::new();
    for (field, condition) in v.as_object().unwrap() {
        if field == "$and" || field == "$or" {
            parts.push(json!({if field == "$and" {"must"} else {"should"}: condition.as_array().unwrap().iter().map(qdrant_filter).collect::<Vec<_>>()}));
        } else if field == "$not" {
            parts.push(json!({"must_not":[qdrant_filter(condition)]}));
        } else {
            let singleton = Map::from_iter([("$eq".into(), condition.clone())]);
            let ops = condition
                .as_object()
                .filter(|m| m.keys().any(|k| k.starts_with('$')))
                .unwrap_or(&singleton);
            let mut ranges = Map::new();
            for (op, arg) in ops {
                let key = format!("_recern.eq.{}", path_key(field));
                if op == "$eq" {
                    parts.push(json!({"key":key,"match":{"value":equality(arg)}}));
                } else if op == "$in" {
                    let matches = arg
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| json!({"key":key,"match":{"value":equality(v)}}))
                        .collect::<Vec<_>>();
                    parts.push(if matches.is_empty() {
                        json!({"must_not":[{}]})
                    } else {
                        json!({"should":matches})
                    });
                } else {
                    ranges.insert(op[1..].into(), json!(arg.as_f64().unwrap()));
                }
            }
            if !ranges.is_empty() {
                parts.push(json!({"key":format!("_recern.num.{}",path_key(field)),"range":ranges}));
            }
        }
    }
    if parts.is_empty() {
        json!({})
    } else {
        json!({"must":parts})
    }
}
