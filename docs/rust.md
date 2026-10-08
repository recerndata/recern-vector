# Rust API

```sh
cargo add recern-vector
```

The full API reference is on [docs.rs/recern-vector](https://docs.rs/recern-vector). This page is a map of the main types.

| Type | Role |
|---|---|
| `Database` | A database file. `create`, `open`, `open_or_create`, `create_collection`, `collection`, `collection_mut`, `drop_collection`, `collections`, `save` |
| `CollectionConfig` | `CollectionConfig::new(dim, Metric::Cosine)`, optionally `.with_hnsw(HnswParams { m, ef_construction, ef_search })` |
| `Collection` | `upsert`, `upsert_many`, `upsert_many_with_threads`, `delete`, `get`, `search`, `explain`, `stats`, `estimate_recall`, `compact`, `len`, `contains` |
| `SearchOptions` | `SearchOptions::default().ef(64).filter(f)`, or `.exact()` |
| `Filter` | `Filter::from_json(&value)`, `Filter::eq`, `Filter::is_in`, `Filter::between`, `Filter::And`, `Filter::Range`. See [Filters](filters.md#rust) |
| `SearchHit`, `SearchReport`, `Strategy` | Search results and how they were found |
| `CollectionStats`, `RecallOptions`, `RecallReport`, `RecallPoint` | Introspection, see [Inspecting and tuning](inspection.md) |
| `Error`, `Result` | Every fallible call returns `recern_vector::Result<T>` |

```rust
use recern_vector::{CollectionConfig, Database, Metric, SearchOptions};

let mut db = Database::open_or_create("docs.rvec")?;
let docs = db.create_collection("docs", CollectionConfig::new(3, Metric::Cosine))?;
docs.upsert("a", &[0.1, 0.9, 0.0], None)?;
docs.upsert("b", &[0.8, 0.1, 0.1], None)?;

let hits = docs.search(&[0.2, 0.8, 0.0], 1, &SearchOptions::default())?;
assert_eq!(hits[0].id, "a");
db.save()?;
```

`upsert_many` takes any iterator of `(id, vector, metadata)` where the id is `Into<String>`, the vector is `AsRef<[f32]>` and the metadata is `Option<serde_json::Value>`.

`Database` and `Collection` are `Send + Sync`. Searches take `&self` and can run from many threads at once; writes take `&mut self`.

A complete program: [`quickstart.rs`](../crates/recern-vector/examples/quickstart.rs).
