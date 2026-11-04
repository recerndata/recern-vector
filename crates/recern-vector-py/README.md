# recern-vector (Python)

Python bindings for [Recern Vector](https://github.com/recerndata/recern-vector), a single-file, embedded, inspectable vector database.

> **Version 0.5.0.** Incremental encoding, paged format 3, automatic WAL checkpoints and explicit storage upgrades. Readers retain formats 1/2; HTTP/profile/export contracts remain v1. Workspace 1.0.0 cannot open format 3.

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

## Persistence in 0.5.0

Automatic checkpoints, `set_checkpoint_policy`, `checkpoint_policy` and
`persistence_stats`, paged format 3, `format_version` and explicit
`upgrade_storage` are described in the [persistence guide](https://recern.net/vector/docs/persistence-0.5).
Existing format-2 files remain at format 2 until an explicit upgrade with a backup.

## Remote backend (0.4.0)

`Client.remote(url, api_key=key, timeout=10.0, ca_file=None)` is available in
0.4.0 wheels and the source distribution.
It uses verified TLS, bounded deadlines and generation-bound handles, and
supports the same Python snapshot export/resumable import workflow against
the Recern HTTP server. Use context managers to release snapshot leases. See
[portable clients](../../docs/portable-clients.md) for all three language APIs.

## Portable client (0.3.0)

```python
from recern_vector.portable import Client, Record

client = Client.embedded("prototype.rvec")
# client = Client.qdrant("https://vectors.example.com", api_key=key)
docs = client.create_collection("docs", dim=3)
docs.upsert_many([Record("document:1", [1, 0, 0], {"lang": "en"})])
print(docs.search([1, 0, 0], filter={"lang": "en"}))
```

Portable embedded writes save before returning; the native `Database` API retains explicit saves. `export_snapshot`, `inspect_export` and `import_snapshot` support consistent embedded exports, dry-run, interrupted import recovery and record verification on embedded/Qdrant destinations. Qdrant 1.19.2 is tested; remote batches are not declared atomic, and remote snapshot export is unsupported. See the [portable contract](https://github.com/recerndata/recern-vector/blob/main/docs/portable-api.md) and [migration guide](https://github.com/recerndata/recern-vector/blob/main/docs/migration.md).

## Development

```sh
uv venv && uv pip install maturin pytest numpy
source .venv/bin/activate
maturin develop --release
pytest
```

0.2.0 adds int8 (`create_collection(..., quantization="int8")`), logical `$or`/`$not` filters, durable WAL commits, `checkpoint()` and `Database.open_read_only()`. Checkpoint before copying a standalone `.rvec` file. Original f32 vectors are not retained for int8 collections.
