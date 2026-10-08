# recern-vector (Python)

Python bindings for [Recern Vector](../../README.md), a single-file, embedded, inspectable vector database.

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

## Development

```sh
uv venv && uv pip install maturin pytest numpy
source .venv/bin/activate
maturin develop --release
pytest
```
