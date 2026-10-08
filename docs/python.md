# Python API

```sh
pip install recern-vector
```

```python
import recern_vector as rv
```

The package ships type hints (`py.typed`). Requires CPython 3.11 or later.

## Database

A database file and its collections. Everything is held in memory; changes reach the disk on `save()` or at the end of a `with` block.

| Member | Description |
|---|---|
| `rv.Database(path)` | Open an existing file. Raises `OSError` if it cannot be read, `CorruptDatabaseError` if it is not a valid database |
| `rv.Database.create(path)` | Create a new, empty file. Raises `FileExistsError` if it exists |
| `rv.Database.open_or_create(path)` | Open the file, or create it if missing |
| `db.path` | The file path, as a `pathlib.Path` |
| `db.create_collection(name, dim, metric="cosine", m=16, ef_construction=200, ef_search=64)` | Create a collection and return it. Raises `ValueError` if it exists or a setting is invalid. See [Concepts](concepts.md#collections) |
| `db.collection(name)`, `db[name]` | An existing collection. Raises `KeyError` if missing |
| `name in db` | Whether a collection exists |
| `db.collection_names()` | Names of all collections |
| `db.drop_collection(name)` | Delete a collection |
| `db.save()` | Write the database to disk atomically |
| `with db:` | Calls `save()` when the block exits without an exception. On an exception nothing is saved |

## Collection

Returned by `create_collection` and `db[name]`. A collection object is a handle: it stays valid while the database is open, and sees every change.

| Member | Description |
|---|---|
| `c.name`, `c.dim`, `c.metric` | Settings |
| `len(c)` | Number of live records |
| `id in c` | Whether a record exists |
| `c.upsert(id, vector, metadata=None)` | Insert or replace one record |
| `c.upsert_many(ids, vectors, metadatas=None, *, threads=None)` | Insert or replace many records, building the index on `threads` cores (default: all). Atomic. Returns the number of records written |
| `c.delete(id)` | Remove a record. Returns whether it existed |
| `c.get(id)` | The `Record`, or `None` |
| `c.search(query, k=10, *, ef=None, exact=False, filter=None)` | The `k` nearest records, as a list of `Hit` sorted by distance |
| `c.explain(query, k=10, *, ef=None, exact=False, filter=None)` | A `SearchReport`: the hits and how the search ran |
| `c.stats()` | Index structure, reachability and memory, as a `dict`. See [Inspecting and tuning](inspection.md#stats) |
| `c.estimate_recall(sample=100, k=10, ef_values=(16, 32, 64, 128, 256), seed=42)` | A `RecallReport` comparing the index with exact search |
| `c.compact()` | Rebuild without deleted records. Returns how many were removed |

Search arguments:

- `query`: a vector of length `dim`.
- `k`: how many results, at least 1.
- `ef`: candidate list size for this search; defaults to the collection's `ef_search` and is never smaller than `k`.
- `exact`: scan every record instead of using the index.
- `filter`: a metadata filter, see [Filters](filters.md).

### Vectors

A vector can be a NumPy array, a list or tuple of numbers, or anything that supports the buffer protocol. `float32` arrays are read directly; other types are converted.

For `upsert_many`, `vectors` is a 2-D array of shape `(len(ids), dim)` (fastest as contiguous `float32`) or a sequence of vectors. `metadatas`, if given, has one entry per id; an entry may be `None`.

### Metadata

Metadata is converted to JSON: `dict` (string keys), `list`, `tuple`, `str`, `int` (up to 64 bits), `float` (finite), `bool` and `None`. Other types raise `ValueError`.

## Result types

All result objects are read-only.

**`Hit`**: `id: str`, `distance: float`, `metadata: dict | None`. For cosine collections, similarity is `1 - distance`.

**`Record`**: `id: str`, `vector: list[float]` (normalized for cosine collections), `metadata`.

**`SearchReport`**: `hits: list[Hit]`, `strategy: "hnsw" | "exact" | "filtered_exact"`, `ef: int | None`, `visited: int`, `distance_computations: int`, `filter_selectivity: float | None`, `elapsed_ms: float`.

**`RecallReport`**: `k`, `sample`, `exact_p50_ms`, `points: list[RecallPoint]`.

**`RecallPoint`**: `ef`, `recall` (0 to 1), `p50_ms`, `p95_ms`.

## Errors

| Exception | When |
|---|---|
| `ValueError` | Wrong dimension, NaN or infinite values, a zero vector in a cosine collection, an invalid setting or filter, unsupported metadata, a collection that already exists |
| `KeyError` | A collection that does not exist |
| `FileExistsError` | `Database.create` on an existing file |
| `OSError` | The file cannot be read or written |
| `rv.CorruptDatabaseError` | The file is not a Recern Vector database, is damaged, or was written by a newer format version. Subclass of `rv.RecernVectorError` |

`rv.FORMAT_VERSION` is the file format version this build writes.

## Threads

Searches, `get`, `stats`, `estimate_recall` and `save` release the GIL and run in parallel from several threads. Writes wait for running reads and then run alone; `upsert_many` and `compact` use all cores themselves.

```python
from concurrent.futures import ThreadPoolExecutor

with ThreadPoolExecutor(8) as pool:
    results = list(pool.map(lambda q: collection.search(q, k=10), queries))
```
