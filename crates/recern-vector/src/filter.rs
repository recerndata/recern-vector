use serde_json::{Map, Value};

use crate::error::{Error, Result};

/// Metadata predicate applied during search.
///
/// Fields are addressed by dotted paths (`"source.lang"`). A record without
/// the field never matches.
#[derive(Clone, Debug, PartialEq)]
pub enum Filter {
    Eq {
        field: String,
        value: Value,
    },
    In {
        field: String,
        values: Vec<Value>,
    },
    Range {
        field: String,
        gt: Option<f64>,
        gte: Option<f64>,
        lt: Option<f64>,
        lte: Option<f64>,
    },
    And(Vec<Filter>),
}

impl Filter {
    pub fn eq(field: impl Into<String>, value: impl Into<Value>) -> Self {
        Filter::Eq {
            field: field.into(),
            value: value.into(),
        }
    }

    pub fn is_in(field: impl Into<String>, values: impl IntoIterator<Item = Value>) -> Self {
        Filter::In {
            field: field.into(),
            values: values.into_iter().collect(),
        }
    }

    /// Inclusive range. Either bound may be omitted.
    pub fn between(field: impl Into<String>, gte: Option<f64>, lte: Option<f64>) -> Self {
        Filter::Range {
            field: field.into(),
            gt: None,
            gte,
            lt: None,
            lte,
        }
    }

    pub fn matches(&self, metadata: Option<&Value>) -> bool {
        match self {
            Filter::And(filters) => filters.iter().all(|f| f.matches(metadata)),
            Filter::Eq { field, value } => lookup(metadata, field).is_some_and(|v| same(v, value)),
            Filter::In { field, values } => {
                lookup(metadata, field).is_some_and(|v| values.iter().any(|x| same(v, x)))
            }
            Filter::Range {
                field,
                gt,
                gte,
                lt,
                lte,
            } => match lookup(metadata, field).and_then(Value::as_f64) {
                Some(x) => {
                    gt.is_none_or(|b| x > b)
                        && gte.is_none_or(|b| x >= b)
                        && lt.is_none_or(|b| x < b)
                        && lte.is_none_or(|b| x <= b)
                }
                None => false,
            },
        }
    }

    /// Parses a MongoDB-style filter:
    ///
    /// ```text
    /// {"lang": "en", "year": {"$gte": 2020, "$lt": 2025}, "tag": {"$in": ["a", "b"]}}
    /// ```
    ///
    /// Top-level keys are combined with AND. Supported operators: `$eq`,
    /// `$in`, `$gt`, `$gte`, `$lt`, `$lte`.
    pub fn from_json(value: &Value) -> Result<Self> {
        let object = value
            .as_object()
            .ok_or_else(|| Error::InvalidArgument("filter must be a JSON object".into()))?;
        let mut parts = Vec::with_capacity(object.len());
        for (field, condition) in object {
            match condition {
                Value::Object(ops) if !ops.is_empty() && ops.keys().all(|k| k.starts_with('$')) => {
                    parts.push(parse_operators(field, ops)?);
                }
                other => parts.push(Filter::eq(field.clone(), other.clone())),
            }
        }
        Ok(combine(parts))
    }
}

fn parse_operators(field: &str, ops: &Map<String, Value>) -> Result<Filter> {
    let mut parts = Vec::new();
    let (mut gt, mut gte, mut lt, mut lte) = (None, None, None, None);
    for (op, arg) in ops {
        match op.as_str() {
            "$eq" => parts.push(Filter::eq(field, arg.clone())),
            "$in" => {
                let values = arg.as_array().ok_or_else(|| {
                    Error::InvalidArgument(format!("$in on '{field}' expects an array"))
                })?;
                parts.push(Filter::is_in(field, values.iter().cloned()));
            }
            "$gt" | "$gte" | "$lt" | "$lte" => {
                let bound = arg.as_f64().ok_or_else(|| {
                    Error::InvalidArgument(format!("{op} on '{field}' expects a number"))
                })?;
                match op.as_str() {
                    "$gt" => gt = Some(bound),
                    "$gte" => gte = Some(bound),
                    "$lt" => lt = Some(bound),
                    _ => lte = Some(bound),
                }
            }
            other => {
                return Err(Error::InvalidArgument(format!(
                    "unsupported filter operator {other}"
                )));
            }
        }
    }
    if gt.is_some() || gte.is_some() || lt.is_some() || lte.is_some() {
        parts.push(Filter::Range {
            field: field.to_owned(),
            gt,
            gte,
            lt,
            lte,
        });
    }
    Ok(combine(parts))
}

fn combine(mut parts: Vec<Filter>) -> Filter {
    if parts.len() == 1 {
        parts.pop().unwrap()
    } else {
        Filter::And(parts)
    }
}

fn lookup<'a>(metadata: Option<&'a Value>, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(metadata?, |value, key| value.get(key))
}

/// JSON equality that treats `1` and `1.0` as equal.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_and_matches_mongo_style_filters() {
        let filter = Filter::from_json(&json!({
            "lang": "en",
            "year": {"$gte": 2020, "$lt": 2025},
            "source.kind": {"$in": ["docs", "blog"]}
        }))
        .unwrap();
        let doc = json!({"lang": "en", "year": 2022, "source": {"kind": "docs"}});
        assert!(filter.matches(Some(&doc)));
        assert!(!filter.matches(Some(
            &json!({"lang": "en", "year": 2025, "source": {"kind": "docs"}})
        )));
        assert!(!filter.matches(Some(
            &json!({"lang": "de", "year": 2022, "source": {"kind": "docs"}})
        )));
        assert!(!filter.matches(None));
    }

    #[test]
    fn numbers_compare_by_value() {
        assert!(Filter::eq("n", json!(1)).matches(Some(&json!({"n": 1.0}))));
    }

    #[test]
    fn empty_filter_matches_everything() {
        let filter = Filter::from_json(&json!({})).unwrap();
        assert!(filter.matches(None));
    }

    #[test]
    fn rejects_unknown_operators() {
        assert!(Filter::from_json(&json!({"a": {"$regex": "x"}})).is_err());
        assert!(Filter::from_json(&json!([1, 2])).is_err());
    }
}
