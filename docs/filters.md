# Filters

A filter restricts a search to records whose metadata matches. Filters use a MongoDB-style JSON syntax in Python and on the command line, and can also be built in code in Rust.

```python
collection.search(query, k=10, filter={
    "lang": "en",                          # equality
    "year": {"$gte": 2020, "$lt": 2025},   # range
    "source.kind": {"$in": ["docs", "blog"]},  # one of several values, dotted path
})
```

## Syntax

- A filter is a JSON object. **Top-level keys are combined with AND.** There is no OR or NOT yet; use `$in` for "one of".
- A key is a field name, or a **dotted path** into nested objects: `"source.kind"` matches `{"source": {"kind": "docs"}}`.
- A plain value means equality: `{"lang": "en"}` is the same as `{"lang": {"$eq": "en"}}`.
- An object whose keys all start with `$` is a set of operators on that field, combined with AND.

| Operator | Matches when the field… | Argument |
|---|---|---|
| `$eq` | equals the value | any JSON value |
| `$in` | equals one of the values | array |
| `$gt`, `$gte` | is greater than (or equal to) the bound | number |
| `$lt`, `$lte` | is less than (or equal to) the bound | number |

Any other operator is an error.

## Matching rules

- **A record without the field never matches**, including records without metadata.
- **Numbers compare by value:** `1` equals `1.0`.
- **Ranges apply to numbers only.** A string field never matches `$gt` and friends; store dates as numbers (for example, a Unix timestamp or `20240131`).
- **Equality compares whole values.** For `{"tags": ["a", "b"]}`, the filter `{"tags": "a"}` does **not** match (unlike MongoDB); `{"tags": ["a", "b"]}` does. To filter by membership, store one record field per tag (`{"tag_a": true}`) or keep a single tag per record.

## How filtered searches run

Before searching, Recern Vector estimates the filter's **selectivity**: the share of records that match, measured on a random sample of up to 512 live records.

- The planner estimates matching distance work plus metadata scan work, and compares it with an HNSW traversal budget. Small/selective subsets use `filtered_exact`; the decision depends on count, dimension, k, ef and selectivity.
- Other filters run inside HNSW. The candidate budget grows inversely with estimated selectivity, bounded by collection size. `explain().ef` reports the actual budget. These estimates do not guarantee latency or recall.

`explain()` shows which strategy ran, the estimated selectivity and how many nodes were visited:

```python
report = collection.explain(query, k=10, filter={"tenant": "t7"})
print(report.strategy, report.filter_selectivity, report.visited)
# filtered_exact 0.006 100
```

A filtered search returns `k` results whenever at least `k` records match. With the graph strategy, recall can be lower than without a filter at the same `ef`, because fewer of the candidates the search looks at are eligible. In our tests with filters matching 4–10% of records, recall@10 was about 0.8 at `ef=10` and 0.99 at `ef=64`. If filtered recall matters, compare against exact search on your data:

```python
truth = {h.id for h in collection.search(query, k=10, exact=True, filter=f)}
found = {h.id for h in collection.search(query, k=10, ef=64, filter=f)}
print(len(truth & found) / 10)
```

`estimate_recall()` measures unfiltered searches only.

## Rust

```rust
use recern_vector::Filter;
use serde_json::json;

// Parse the JSON syntax...
let f = Filter::from_json(&json!({"lang": "en", "year": {"$gte": 2020}}))?;

// ...or build it in code.
let f = Filter::And(vec![
    Filter::eq("lang", "en"),
    Filter::between("year", Some(2020.0), None),   // inclusive bounds
    Filter::is_in("source.kind", [json!("docs"), json!("blog")]),
]);
```

`Filter::Range { field, gt, gte, lt, lte }` gives exclusive bounds as well.

## Logical operators in 0.2.0

`{"$or": [{"lang": "en"}, {"score": {"$gte": 10}}], "$not": {"hidden": true}}` combines OR and NOT with the implicit AND between keys. `$and`/`$or` require nonempty arrays of filter objects; `$not` requires one filter object. Unknown logical operators and more than 64 nested levels are rejected. NOT negates the whole predicate, so `$not: {"field": value}` matches a missing field as well. Positive predicates still do not match missing fields. These are Recern semantics, not full MongoDB compatibility.
