use crate::error::{ApiError, Result};
use recern_vector::{CollectionConfig, Filter, Metric, Quantization};
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, Visitor},
};
use serde_json::{Value, value::RawValue};
use std::{collections::BTreeSet, fmt, io::Write};

pub const JSON_LIMIT: usize = 1_048_576;

// Raw numeric tokens retain integer/float kinds and reject integer overflow
// instead of serde_json's default conversion of large integer literals to f64.
pub fn strict_json(text: &str, max_depth: usize) -> Result<Value> {
    fn parse(text: &str, depth: usize, max: usize) -> Result<Value> {
        if depth > max {
            return Err(ApiError::invalid());
        }
        let raw: &RawValue = serde_json::from_str(text).map_err(|_| ApiError::invalid())?;
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
                let pairs = d.deserialize_map(Pairs).map_err(|_| ApiError::invalid())?;
                let mut map = serde_json::Map::new();
                for (key, value) in pairs {
                    map.insert(key, parse(value.get(), depth + 1, max)?);
                }
                Ok(Value::Object(map))
            }
            Some(b'[') => {
                let items: Vec<&RawValue> =
                    serde_json::from_str(raw).map_err(|_| ApiError::invalid())?;
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
                        .map_err(|_| ApiError::invalid())
                }
            }
            _ => serde_json::from_str(raw).map_err(|_| ApiError::invalid()),
        }
    }
    parse(text, 0, max_depth)
}

pub fn typed<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|_| ApiError::invalid())
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub name: String,
    pub dim: usize,
    #[serde(default = "default_metric")]
    pub metric: String,
    #[serde(default = "default_precision")]
    pub quantization: String,
}
fn default_metric() -> String {
    "cosine".into()
}
fn default_precision() -> String {
    "f32".into()
}
impl Spec {
    pub fn config(&self) -> Result<CollectionConfig> {
        if self.name.is_empty() || self.name.len() > 255 || !(1..=65536).contains(&self.dim) {
            return Err(ApiError::invalid());
        }
        let metric = match self.metric.as_str() {
            "cosine" => Metric::Cosine,
            "l2" => Metric::L2,
            "dot" => Metric::Dot,
            _ => return Err(ApiError::invalid()),
        };
        let precision = match self.quantization.as_str() {
            "f32" => Quantization::F32,
            "int8" => Quantization::Int8,
            _ => return Err(ApiError::invalid()),
        };
        Ok(CollectionConfig::new(self.dim, metric).with_quantization(precision))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ref {
    pub name: String,
    pub identity: String,
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    #[serde(default)]
    pub snapshot_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Describe {
    pub name: String,
    #[serde(default)]
    pub snapshot_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    pub spec: Spec,
    #[serde(default)]
    pub creation_token: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DropCollection {
    pub collection: Ref,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordInput {
    pub id: String,
    pub vector: Vec<Value>,
    #[serde(default = "default_metadata")]
    pub metadata_json: String,
}
fn default_metadata() -> String {
    "null".into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upsert {
    pub collection: Ref,
    pub records: Vec<RecordInput>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lookup {
    pub collection: Ref,
    pub id: String,
    #[serde(default)]
    pub snapshot_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delete {
    pub collection: Ref,
    pub id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Count {
    pub collection: Ref,
    #[serde(default)]
    pub snapshot_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub collection: Ref,
    #[serde(default)]
    pub snapshot_id: Option<String>,
    #[serde(default = "default_page")]
    pub limit: usize,
    #[serde(default)]
    pub cursor: Option<String>,
}
fn default_page() -> usize {
    100
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Search {
    pub collection: Ref,
    pub query: Vec<Value>,
    #[serde(default = "default_k")]
    pub k: usize,
    #[serde(default)]
    pub snapshot_id: Option<String>,
    #[serde(default)]
    pub filter_json: Option<String>,
    #[serde(default)]
    pub exact: bool,
}
fn default_k() -> usize {
    10
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    pub snapshot_id: String,
}

pub fn id(id: &str) -> Result<()> {
    if id.len() > 65536 {
        Err(ApiError::invalid())
    } else {
        Ok(())
    }
}
pub fn token(value: &str, min: usize) -> Result<()> {
    if !(min..=128).contains(&value.len())
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err(ApiError::invalid());
    }
    Ok(())
}
pub fn uuid(value: &str) -> Result<()> {
    let parsed = uuid::Uuid::parse_str(value).map_err(|_| ApiError::invalid())?;
    if parsed.to_string() != value {
        return Err(ApiError::invalid());
    }
    Ok(())
}
pub fn vector(input: &[Value], spec: &Spec) -> Result<Vec<f32>> {
    if input.len() != spec.dim {
        return Err(ApiError::invalid());
    }
    let vector = input
        .iter()
        .map(|v| {
            v.as_f64()
                .map(|f| f as f32)
                .filter(|v| v.is_finite())
                .ok_or_else(ApiError::invalid)
        })
        .collect::<Result<Vec<_>>>()?;
    let norm = vector.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>();
    if norm > f64::from(f32::MAX) / 8.0
        || (spec.metric == "cosine" && norm < f64::from(f32::MIN_POSITIVE))
    {
        return Err(ApiError::invalid());
    }
    Ok(vector)
}
pub fn metadata(text: &str) -> Result<Value> {
    if text.len() > JSON_LIMIT {
        return Err(ApiError::invalid());
    }
    let value = strict_json(text, 64)?;
    if encode(&value, JSON_LIMIT).is_err() {
        return Err(ApiError::invalid());
    }
    Ok(value)
}
pub fn filter(text: Option<&str>) -> Result<Option<Filter>> {
    text.map(|s| {
        let value = metadata(s)?;
        if !value.is_object() {
            return Err(ApiError::invalid());
        }
        Filter::from_json(&value).map_err(ApiError::from)
    })
    .transpose()
}

struct Limited {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("size limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub fn encode(value: &impl serde::Serialize, limit: usize) -> Result<Vec<u8>> {
    let mut writer = Limited {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| ApiError::too_large())?;
    Ok(writer.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_values_preserve_number_kinds_and_reject_ambiguous_input() {
        let value = strict_json(
            r#"{"i":18446744073709551615,"n":-9223372036854775808,"f":1.0}"#,
            16,
        )
        .unwrap();
        assert_eq!(value["i"].as_u64(), Some(u64::MAX));
        assert!(value["f"].is_f64());
        for text in [
            r#"{"a":1,"a":2}"#,
            r#"{"a":{"x":1,"x":2}}"#,
            "18446744073709551616",
            "-9223372036854775809",
            "1e400",
            r#""\ud800""#,
        ] {
            assert!(strict_json(text, 16).is_err(), "{text}");
        }
        assert!(metadata(&("[".repeat(65) + "0" + &"]".repeat(65))).is_err());
        assert!(encode(&"hello", 4).is_err());
    }
}
