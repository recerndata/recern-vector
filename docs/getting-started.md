# Getting started

## Install

```sh
pip install recern-vector           # Python 3.11+
cargo add recern-vector             # Rust library
cargo install recern-vector-cli     # the recern-vector command
```

Python wheels are published for Linux (x86_64, aarch64), macOS (Intel, Apple silicon) and Windows (x86_64). On other platforms pip builds from source, which needs a Rust toolchain.

## Python

```python
import numpy as np
import recern_vector as rv

with rv.Database.open_or_create("docs.rvec") as db:      # saved on a clean exit
    docs = db.create_collection("docs", dim=384, metric="cosine")

    # ids, a float32 array of shape (n, 384), and one metadata dict per record
    docs.upsert_many(ids, embeddings, metadatas)

    hits = docs.search(query, k=5, filter={"lang": "en"})
    for hit in hits:
        print(hit.id, hit.distance, hit.metadata)

    print(docs.explain(query, k=5))      # strategy, nodes visited, time
```

`create_collection` fails if the collection already exists. When reopening a file, use `db["docs"]` (or `db.collection("docs")`), or check first with `"docs" in db`.

Embeddings can come from any model. With [sentence-transformers](https://www.sbert.net):

```python
from sentence_transformers import SentenceTransformer

model = SentenceTransformer("sentence-transformers/all-MiniLM-L6-v2")
embeddings = model.encode(texts, normalize_embeddings=True)   # float32, (n, 384)
query = model.encode("how do I make my queries faster?", normalize_embeddings=True)
```

See [`examples/python/semantic_search.py`](../examples/python/semantic_search.py) and [`examples/python/rag_retrieval.py`](../examples/python/rag_retrieval.py) for complete programs.

## Rust

```rust
use recern_vector::{CollectionConfig, Database, Filter, Metric, SearchOptions};
use serde_json::json;

let mut db = Database::open_or_create("docs.rvec")?;
let docs = db.create_collection("docs", CollectionConfig::new(384, Metric::Cosine))?;
docs.upsert("doc-1", &embedding, Some(json!({"lang": "en"})))?;
docs.upsert_many(records)?;   // iterator of (id, vector, Option<metadata>)

let options = SearchOptions::default().filter(Filter::eq("lang", "en"));
let hits = docs.search(&query, 5, &options)?;
db.save()?;
```

A complete program: [`quickstart.rs`](../crates/recern-vector/examples/quickstart.rs) (`cargo run --release --example quickstart`).

## Node.js

Native builds for Linux x64, macOS arm64 and Windows x64 are attached to the 0.2.0 release. The package is not yet on npm. See [Node.js](nodejs.md) for download links, setup and an asynchronous search example.

## Command line

```sh
recern-vector init docs.rvec
recern-vector create-collection docs.rvec docs --dim 384 --metric cosine
recern-vector insert docs.rvec docs records.jsonl
recern-vector query docs.rvec docs --like doc-42 -k 5 --explain
recern-vector inspect docs.rvec
```

`records.jsonl` has one record per line: `{"id": "doc-1", "vector": [0.1, ...], "metadata": {"lang": "en"}}`. See [Command line](cli.md) and [`examples/cli/quickstart.sh`](../examples/cli/quickstart.sh).

## Next

- [Concepts](concepts.md): how a database, its collections and the index fit together.
- [Inspecting and tuning](inspection.md): measure recall on your data and pick `ef`.
