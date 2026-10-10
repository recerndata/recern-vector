# recern-vector (Python)

Python bindings for [Recern Vector](https://github.com/recerndata/recern-vector), a single-file, embedded, inspectable vector database.

> Version 0.2.0: stable format 2, with compatibility for format-1 databases. Saves commit to WAL; checkpoint before copying a standalone database file.

```sh
pip install recern-vector
```

Wheels cover CPython 3.11+ on Linux (x86_64, aarch64), macOS (Intel, Apple silicon) and Windows (x86_64).

```python
import numpy as np
import recern_vector as rv

with rv.Database.open_or_create("docs.rvec") as db:          # saves on clean exit
    chunks = db.create_collection("chunks", dim=384, metric="cosine")
    chunks.upsert_many(ids, np.asarray(embeddings, dtype=np.float32), metadatas)

    hits = chunks.search(query, k=5, filter={"lang": "en", "year": {"$gte": 2020}})
    report = chunks.explain(query, k=5)          # strategy, visited nodes, elapsed_ms
    recall = chunks.estimate_recall(ef_values=[16, 64, 256])
    print(chunks.stats())
```

`float32` NumPy arrays are read through the buffer protocol without per-element conversion; plain lists and other dtypes also work. `upsert_many` builds index links on all cores (`threads=` to change it) and is atomic: if any vector is invalid, nothing is written.

Benchmarks against faiss, LanceDB and sqlite-vec: [recern.net/vector/benchmarks](https://recern.net/vector/benchmarks).

## Development

```sh
uv venv && uv pip install maturin pytest numpy
source .venv/bin/activate
maturin develop --release
pytest
```

0.2.0 adds int8 (`create_collection(..., quantization="int8")`), logical `$or`/`$not` filters, durable WAL commits, `checkpoint()` and `Database.open_read_only()`. Checkpoint before copying a standalone `.rvec` file. Original f32 vectors are not retained for int8 collections.
