# Recern Vector

[![PyPI](https://img.shields.io/pypi/v/recern-vector?label=PyPI)](https://pypi.org/project/recern-vector/) [![crates.io](https://img.shields.io/crates/v/recern-vector?label=crates.io)](https://crates.io/crates/recern-vector) [![crates.io CLI](https://img.shields.io/crates/v/recern-vector-cli?label=crates.io%20CLI)](https://crates.io/crates/recern-vector-cli) [![CI](https://github.com/recerndata/recern-vector/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/recerndata/recern-vector/actions/workflows/ci.yml) [![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

**A single-file vector database. Embedded, inspectable, boring in the best way.**

> **Version 0.4.0:** authenticated single-node HTTP server and portable Python, Rust and Node clients for embedded Recern, Recern HTTP and Qdrant. Checked, resumable migration retains file format 2, format-1 readers and export format 1.

```sh
pip install recern-vector           # Python
cargo add recern-vector             # Rust library
cargo install recern-vector-cli     # the recern-vector command
cargo add recern-vector-client      # portable Rust API
cargo install recern-vector-server  # HTTP server
```

Release: [v0.4.0](https://github.com/recerndata/recern-vector/releases/tag/v0.4.0) ([PyPI](https://pypi.org/project/recern-vector/), [crates.io](https://crates.io/crates/recern-vector)) · Project page: [recern.net/vector](https://recern.net/vector) · Benchmark report: [recern.net/vector/benchmarks](https://recern.net/vector/benchmarks)

**Documentation:** [recern.net/vector/docs](https://recern.net/vector/docs) (sources in [`docs/`](docs/README.md)) · **Examples:** [`examples/`](examples/README.md) — quickstart, semantic search, RAG retrieval, tuning recall, CLI and Rust

Recern Vector stores vectors, JSON metadata and an HNSW index in a local snapshot with a WAL for incremental commits. A checkpoint produces a standalone database file. Its internals are part of the API: every query can explain how it was executed, every collection reports its graph structure and memory, and recall can be measured against exact search at any time.

## Quick start (CLI)

```sh
cargo build --release
alias rv=target/release/recern-vector

rv init docs.rvec
rv create-collection docs.rvec chunks --dim 384 --metric cosine
rv insert docs.rvec chunks chunks.jsonl     # {"id": "...", "vector": [...], "metadata": {...}} per line; --threads N

rv query docs.rvec chunks --like chunk-42 -k 5 --explain
rv query docs.rvec chunks --vector '[0.1, ...]' --filter '{"lang": "en", "year": {"$gte": 2020}}'

rv inspect docs.rvec                        # layers, degree, reachability, memory, hints
rv recall docs.rvec chunks --ef 16,32,64,128
rv delete docs.rvec chunks chunk-1 chunk-2
rv compact docs.rvec chunks
```

## Python

```python
import numpy as np
import recern_vector as rv

with rv.Database.open_or_create("docs.rvec") as db:          # saves on clean exit
    chunks = db.create_collection("chunks", dim=384, metric="cosine")
    chunks.upsert_many(ids, np.asarray(embeddings, dtype=np.float32), metadatas)
    hits = chunks.search(query, k=5, filter={"lang": "en"})
    print(chunks.explain(query, k=5), chunks.stats())
```

Build and test from `crates/recern-vector-py` (see its [README](crates/recern-vector-py/README.md)). One `abi3` wheel covers CPython 3.11+. Calls release the GIL during search, batch inserts, recall estimation and saving.

## Portable Python API (0.3.0)

Keep application data operations the same while changing backend configuration:

```python
from recern_vector.portable import Client, Record

client = Client.embedded("prototype.rvec")
# client = Client.qdrant("https://vectors.example.com", api_key=key, namespace="app")
docs = client.create_collection("docs", dim=3)
docs.upsert_many([Record("doc:1", [0.8, 0.1, 0.2], {"lang": "en"})])
hits = docs.search([0.8, 0.1, 0.2], filter={"lang": "en"})
```

The [portable contract](docs/portable-api.md) defines IDs, metadata, filters, distances, errors and backend capabilities. [Export/import](docs/migration.md) transfers a consistent embedded snapshot to new embedded or Qdrant collections, with checksums, dry-run, resumable batches and read-back verification. Qdrant 1.19.2 is the tested external backend; it does not promise atomic remote batches or consistent portable exports.

Install or upgrade with `pip install --upgrade recern-vector` to use the portable API. Runnable examples: [`portable.py`](examples/python/portable.py), [`migrate.py`](examples/python/migrate.py).

## Rust API

```rust
use recern_vector::{CollectionConfig, Database, Filter, Metric, RecallOptions, SearchOptions};

let mut db = Database::open_or_create("docs.rvec")?;
let chunks = db.create_collection("chunks", CollectionConfig::new(384, Metric::Cosine))?;
chunks.upsert("chunk-1", &embedding, Some(serde_json::json!({"lang": "en"})))?;
chunks.upsert_many(records)?;                          // atomic batch, index built on all cores

let options = SearchOptions::default().filter(Filter::eq("lang", "en"));
let report = chunks.explain(&query, 10, &options)?;   // hits + strategy, visited nodes, timing
let recall = chunks.estimate_recall(&RecallOptions::default())?;
db.save()?;
```

## What is inside

| Part | Implementation |
|---|---|
| Metrics | cosine (vectors normalized on insert), L2, dot product |
| Index | HNSW with the neighbor-selection heuristic; exact scan on request |
| Updates | upsert and delete by string id; deleted nodes stay as graph waypoints until `compact` |
| Batch build | `upsert_many` links records in parallel with one lock per node (as in hnswlib); nodes still being inserted are never used as descent points, and a final pass re-links any record search could not reach. Batches are atomic; with one thread the graph is identical to sequential upserts |
| Filters | equality, `$in`, `$gt`/`$gte`/`$lt`/`$lte`, dotted paths, `$and`, `$or`, `$not` |
| Filter planning | sampled selectivity and operation-cost estimates choose exact scan or HNSW with an adaptive candidate budget |
| Introspection | `stats()` (layers, degree, unreachable records, memory), `explain()`, `estimate_recall()` |
| Storage | stable format 2, format-1 compatibility, checksummed page WAL, atomic checkpoints, stale-writer protection |
| Precision | f32 or per-vector int8 quantization; distance calculations operate directly on stored codes |
| Python | PyO3 bindings; float32 NumPy arrays are read through the buffer protocol |
| Node.js | native Node-API bindings, Float32Array input and asynchronous search |

The file layout is documented in [`storage.rs`](crates/recern-vector/src/storage.rs).

## Benchmark

Standard ANN-Benchmarks datasets, top-10, one query at a time from Python (after a warm-up pass), Apple M3 Max (14 cores). Full method, sweeps and reproduction: [bench/RESULTS.md](bench/RESULTS.md).

**SIFT1M** (1M × 128, L2) — QPS at a recall@10 target:

| Engine | ≥ 0.90 | ≥ 0.95 | ≥ 0.99 | Build (threads) |
|---|---|---|---|---|
| **Recern Vector** HNSW | **15,973** | **9,041** | **4,409** | 35 s (14) |
| faiss HNSWFlat | 13,012 | 6,961 | 3,571 | 32 s (14) |
| LanceDB IVF_HNSW_SQ | 765 | 731 | — | 28 s (all) |
| LanceDB IVF_PQ (`num_sub_vectors=32`) | 245 | 245 | 245 | 11 s (all) |
| sqlite-vec, exact scan | 12 | 12 | 12 | 5 s (1) |

**GloVe-100** (1.18M × 100, cosine):

| Engine | ≥ 0.90 | ≥ 0.95 | Build (threads) |
|---|---|---|---|
| **Recern Vector** | **2,373** | **635** | 48 s (14) |
| faiss HNSWFlat | 1,687 | 356 | 43 s (14) |
| LanceDB IVF_HNSW_SQ | — (max 0.879) | — | 44 s (all) |
| LanceDB IVF_PQ (`num_sub_vectors=25`) | 271 | 167 | 8 s (all) |
| sqlite-vec, exact scan | 7 | 7 | 4 s (1) |

Searches run on one thread for Recern Vector and faiss.

What this says about the prototype:

- **Faster than the reference HNSW at equal recall: 1.2–1.3× on SIFT and 1.4–1.8× on GloVe.** Recall per `ef` is slightly higher than faiss (denser layer-0 links), and search prefetches every neighbor's vector before computing distances, so memory loads overlap.
- **Build is within 1.1–1.2× of faiss** on the same cores and scales ~10× on 14 cores.
- **Single-query latency is 4–21× lower than LanceDB** from Python. Against sqlite-vec's exact scan it is ~760× faster on SIFT and ~90× on GloVe at recall ≥ 0.95.
- The Python layer adds about 1 µs per query (`bench/profile_search.py`).

`cargo run --release --example bench -- [records] [dim] [noise]` runs a quick synthetic sanity check without downloading datasets.

## Version 0.2.0

- Versioned format 2, frozen compatibility fixtures and format-1 migration.
- Checksummed WAL with changed-page writes, crash recovery, checkpoint and stale-writer protection.
- Optional `Quantization::Int8` / Python `quantization="int8"` per collection.
- `$and`, `$or`, `$not` filters with cost-based exact/HNSW selection and adaptive candidate budgets.
- [Native Node.js bindings](crates/recern-vector-node/README.md), including asynchronous search.

A database now uses a snapshot plus WAL. Call `checkpoint()` before copying a standalone `.rvec`. `save()` makes pending changes durable; it does not checkpoint. See the [format and durability contract](docs/file-format.md) and [current limitations](docs/limitations.md). Historical benchmark results above describe the pre-0.2 f32 engine and have not been rerun for int8.

**0.4.0** adds the [HTTP/OpenAPI v1 contract](docs/http-api.md), a separate
[single-node server](docs/server.md), and [portable Python, Rust and Node clients](docs/portable-clients.md)
for embedded Recern, Recern HTTP and Qdrant. Migration tests cover interrupted
imports, lost acknowledgements, restart/reconnect, snapshot leases and cross-language
precision. Native format-1/2 readers and export format 1 remain compatible.

The [release guide](docs/release.md) lists the actual packages, native artifacts,
checksums, tested platforms and upgrade guarantees. Node.js is distributed as
platform-specific npm tarballs in the GitHub release; public npm distribution
remains planned for 0.5.0. The HTTP server is single-node and owns its database;
its RAM and O(database size) save-encoding limits remain.

Next: Vector **0.5.0 → 1.0.0**, then Workspace **1.0.0**. Incremental encoding,
automatic checkpoints and current f32/int8 benchmarks belong to 0.5.0; API and
migration/deprecation policy stabilization belong to 1.0.0. See the
[roadmap](docs/limitations.md#planned-api-portability-and-migration).

## Contributing

Issues and pull requests are welcome. Before sending a change, run:

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
