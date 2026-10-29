use crate::error::{Error, Result};
use recern_vector::{CollectionConfig, Filter, Metric, Quantization};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, MapAccess, Visitor},
};
use serde_json::{Value, value::RawValue};
use std::{collections::BTreeSet, fmt, io::Write};

pub const PROFILE: &str = "recern-vector/1";
pub const MAX_HTTP_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn encode(value: &Value, limit: usize) -> Result<Vec<u8>> {
    struct Limited {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other("HTTP byte limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut out = Limited {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut out, value).map_err(|_| Error::invalid())?;
    Ok(out.bytes)
}
// Raw numeric tokens retain integer/float kinds and reject integer overflow
// instead of serde_json's default conversion of large integer literals to f64.
pub fn strict_json(text: &str, max_depth: usize) -> Result<Value> {
    fn parse(text: &str, depth: usize, max: usize) -> Result<Value> {
        if depth > max {
            return Err(Error::invalid());
        }
        let raw: &RawValue = serde_json::from_str(text).map_err(|_| Error::invalid())?;
        let raw = raw.get();
        match raw.as_bytes().first() {
            Some(b'{') => {
                struct Pairs;
                impl<'de> Visitor<'de> for Pairs {
                    type Value = Vec<(String, &'de RawValue)>;
                    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                        f.write_str("an object")
                    }
                    fn visit_map<A: MapAccess<'de>>(
                        self,
                        mut map: A,
                    ) -> std::result::Result<Self::Value, A::Error> {
                        let mut pairs = Vec::new();
                        let mut keys = BTreeSet::new();
                        while let Some(key) = map.next_key::<String>()? {
                            if !keys.insert(key.clone()) {
                                return Err(de::Error::custom("duplicate key"));
                            }
                            pairs.push((key, map.next_value()?));
                        }
                        Ok(pairs)
                    }
                }
                let mut d = serde_json::Deserializer::from_str(raw);
                let pairs = d.deserialize_map(Pairs).map_err(|_| Error::invalid())?;
                let mut map = serde_json::Map::new();
                for (key, value) in pairs {
                    map.insert(key, parse(value.get(), depth + 1, max)?);
                }
                Ok(Value::Object(map))
            }
            Some(b'[') => {
                let items: Vec<&RawValue> =
                    serde_json::from_str(raw).map_err(|_| Error::invalid())?;
                items
                    .into_iter()
                    .map(|v| parse(v.get(), depth + 1, max))
                    .collect::<Result<Vec<_>>>()
                    .map(Value::Array)
            }
            Some(b'-' | b'0'..=b'9') if !raw.contains(['.', 'e', 'E']) => {
                if let Ok(v) = raw.parse::<i64>() {
                    Ok(Value::from(v))
                } else {
                    raw.parse::<u64>()
                        .map(Value::from)
                        .map_err(|_| Error::invalid())
                }
            }
            _ => serde_json::from_str(raw).map_err(|_| Error::invalid()),
        }
    }
    parse(text, 0, max_depth)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionSpec {
    pub name: String,
    pub dim: usize,
    pub metric: String,
    pub quantization: String,
}
impl CollectionSpec {
    pub fn new(name: impl Into<String>, dim: usize, metric: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            dim,
            metric: metric.into(),
            quantization: "f32".into(),
        }
    }
    pub(crate) fn config(&self) -> Result<CollectionConfig> {
        if self.name.is_empty() || self.name.len() > 255 || !(1..=65536).contains(&self.dim) {
            return Err(Error::invalid());
        }
        let metric = match self.metric.as_str() {
            "cosine" => Metric::Cosine,
            "l2" => Metric::L2,
            "dot" => Metric::Dot,
            _ => return Err(Error::invalid()),
        };
        let q = match self.quantization.as_str() {
            "f32" => Quantization::F32,
            "int8" => Quantization::Int8,
            _ => return Err(Error::invalid()),
        };
        Ok(CollectionConfig::new(self.dim, metric).with_quantization(q))
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub vector: Vec<f32>,
    #[serde(default = "null_json")]
    pub metadata_json: String,
}
fn null_json() -> String {
    "null".into()
}
impl Record {
    pub fn new(id: impl Into<String>, vector: Vec<f32>, metadata: Value) -> Self {
        Self {
            id: id.into(),
            vector,
            metadata_json: metadata.to_string(),
        }
    }
    pub fn metadata(&self) -> Result<Value> {
        metadata(&self.metadata_json)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hit {
    pub id: String,
    pub distance: f32,
    pub metadata_json: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub records: Vec<Record>,
    pub next_cursor: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    pub backend: String,
    pub profile: String,
    pub atomic_batches: bool,
    pub consistent_export: bool,
    pub quantization: Vec<String>,
    pub max_batch: usize,
    pub exact_search: bool,
    pub snapshot_pages: bool,
}
impl Capabilities {
    pub(crate) fn native(read_only: bool) -> Self {
        Self {
            backend: "embedded".into(),
            profile: PROFILE.into(),
            atomic_batches: true,
            consistent_export: true,
            quantization: vec!["f32".into(), "int8".into()],
            max_batch: 256,
            exact_search: true,
            snapshot_pages: read_only,
        }
    }
    pub(crate) fn validate(&self) -> Result<()> {
        if self.profile != PROFILE
            || !(1..=256).contains(&self.max_batch)
            || self.quantization.is_empty()
            || self.quantization.iter().any(|q| q != "f32" && q != "int8")
        {
            return Err(Error::corrupt());
        }
        Ok(())
    }
}
pub(crate) fn id(value: &str) -> Result<()> {
    if value.len() <= 65536 {
        Ok(())
    } else {
        Err(Error::invalid())
    }
}
pub(crate) fn token(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(Error::corrupt());
    }
    Ok(())
}
pub(crate) fn uint64(value: &Value) -> Result<u64> {
    let s = value.as_str().ok_or_else(Error::corrupt)?;
    if s.is_empty() || (s.len() > 1 && s.starts_with('0')) || !s.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(Error::corrupt());
    }
    s.parse().map_err(|_| Error::corrupt())
}
pub fn metadata(text: &str) -> Result<Value> {
    if text.len() > 1_048_576 {
        return Err(Error::invalid());
    }
    let value = strict_json(text, 64)?;
    if serde_json::to_vec(&value)
        .map_err(|_| Error::invalid())?
        .len()
        > 1_048_576
    {
        return Err(Error::invalid());
    }
    Ok(value)
}
pub(crate) fn filter(text: Option<&str>) -> Result<Option<Filter>> {
    text.map(|s| {
        let value = metadata(s)?;
        if !value.is_object() {
            return Err(Error::invalid());
        }
        Filter::from_json(&value).map_err(Error::from)
    })
    .transpose()
}
pub(crate) fn vector(value: &[f32], spec: &CollectionSpec) -> Result<()> {
    if value.len() != spec.dim || value.iter().any(|v| !v.is_finite()) {
        return Err(Error::invalid());
    }
    let norm: f64 = value.iter().map(|&v| f64::from(v).powi(2)).sum();
    if norm > f64::from(f32::MAX) / 8.0
        || (spec.metric == "cosine" && norm < f64::from(f32::MIN_POSITIVE))
    {
        return Err(Error::invalid());
    }
    Ok(())
}
pub(crate) fn records(records: &[Record], spec: &CollectionSpec) -> Result<()> {
    let mut seen = BTreeSet::new();
    if records.len() > 256 {
        return Err(Error::invalid());
    }
    for r in records {
        id(&r.id)?;
        vector(&r.vector, spec)?;
        metadata(&r.metadata_json)?;
        if !seen.insert(&r.id) {
            return Err(Error::invalid());
        }
    }
    Ok(())
}
pub(crate) fn decode<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|_| Error::corrupt())
}
