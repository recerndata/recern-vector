# Concepts

## Database

A database is one file, by convention with the `.rvec` extension. It holds any number of named collections.

- **Opening loads everything into memory.** Searches never touch the disk.
- **Changes stay in memory until you save.** `save()` writes the whole database to a temporary file, flushes it to disk and renames it over the original, so the file on disk is always either the old version or the new one, never a mix. In Python, `with rv.Database(...) as db:` saves when the block ends without an exception.
- **One writer.** Two processes that open the same file and both save will overwrite each other's changes. Share a database between processes only for reading.

## Collections

A collection is a set of records with a fixed **dimension** and **metric**, plus its own HNSW index. Choose them when you create the collection; they cannot change later.

| Setting | Default | Meaning |
|---|---|---|
| `dim` | required | Vector length, 1 to 65,536 |
| `metric` | `cosine` | `cosine`, `l2` or `dot` (see below) |
| `m` | 16 | Links per node on upper layers of the graph; layer 0 allows `2 * m`. 2 to 256 |
| `ef_construction` | 200 | Candidate list size while building. Higher builds a better graph, more slowly |
| `ef_search` | 64 | Default candidate list size while searching. Higher finds more true neighbours, more slowly |

Collection names are 1 to 255 bytes of UTF-8.

## Records

A record has:

- an **id**: any string, unique within the collection. Inserting a record with an existing id replaces it (upsert).
- a **vector** of `dim` finite `float32` values. NaN and infinity are rejected; for `cosine`, the zero vector is rejected too.
- optional **metadata**: any JSON value, usually an object (in Python, a `dict` of strings, numbers, booleans, `None`, lists and nested dicts). Metadata is returned with search results; object fields can be used in [filters](filters.md).

## Metrics

Every metric is a **distance**: smaller is closer, and results are sorted by increasing distance.

| Metric | Distance | Notes |
|---|---|---|
| `cosine` | `1 − cos(a, b)`, from 0 to 2 | Vectors are normalized when stored, so `get()` returns the normalized vector. Use for most text embeddings |
| `l2` | squared Euclidean distance | Not the square root: compare distances, don't read them as lengths |
| `dot` | negative inner product | For models trained for maximum inner product search. Distances can be negative |

For cosine, similarity is `1 - distance`.

## The HNSW index

Each collection keeps a [hierarchical navigable small world](https://arxiv.org/abs/1603.09320) graph. A search starts at the top layer, descends greedily, and on the bottom layer explores the `ef` closest candidates it has found so far. The `k` best of those are returned.

- **`ef` trades speed for recall.** A larger `ef` visits more nodes and finds more of the true nearest neighbours. It must be at least `k`; a smaller value is raised to `k`. Pass `ef=` per search, or set the collection's `ef_search`. [Inspecting and tuning](inspection.md) shows how to choose it.
- **Exact search** (`exact=True`) scans every record. It always returns the true nearest neighbours and is the baseline for measuring recall.
- **Batch inserts** (`upsert_many`) build index links on all CPU cores. A batch is atomic: if any vector is invalid, nothing is written. With `threads=1` the result is identical to inserting records one at a time.
- **Deletes** remove a record from results immediately, but its node stays in the graph as a waypoint until `compact()` rebuilds the collection. Replacing a record works the same way.

## Threads

In Python, searches and other reads release the GIL and run in parallel from several threads. Writes (`upsert`, `upsert_many`, `delete`, `compact`, `create_collection`, `drop_collection`) wait until running reads finish and then run alone. `upsert_many` and `compact` use all cores by themselves.

In Rust, `Collection` is `Sync`: share `&Collection` between threads for searching, and use a lock of your choice around writes.
