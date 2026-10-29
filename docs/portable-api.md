# Portable Python API v1

Available from **Recern Vector 0.3.0**. Install with `pip install --upgrade recern-vector`, or build from source as described in the [Python package README](../crates/recern-vector-py/README.md).

`recern_vector.portable` separates application data operations from backend configuration. The released profile `recern-vector/1` (`API_VERSION = 1`) supports embedded Recern and Qdrant. The existing native APIs remain available. The 0.4.0 source milestone adds `Client.remote` and compatible Rust/Node clients; see the [portable clients guide](portable-clients.md).

The first 0.4.0 milestone defines a [draft HTTP/OpenAPI contract](http-api.md)
for that server. It includes checked wire examples and private Python codecs.
The [separate development server](server.md) now implements the HTTP contract
from source. The remote backend is now implemented in this checkout; it is not
in the published 0.3.0 wheel.

## One application, two backends

```python
from recern_vector.portable import Client, Record

client = Client.embedded("prototype.rvec")
# To run the same data logic against a server:
# client = Client.qdrant("https://vectors.example.com", api_key=key,
#                        namespace="my_app", timeout=10.0)

docs = client.create_collection("documents", dim=3, metric="cosine")
docs.upsert_many([
    Record("doc:one", [0.9, 0.1, 0.2], {"lang": "en", "year": 2026}),
    Record("doc:two", [0.2, 0.8, 0.1], {"lang": "ja", "year": 2025}),
])
hits = docs.search([0.9, 0.1, 0.2], k=5,
                   filter={"$or": [{"lang": "en"}, {"year": {"$gte": 2026}}]})
print([(h.id, h.distance, h.metadata) for h in hits])
print(docs.get("doc:one"))
```

Changing the client does not copy data. Use the [migration API](migration.md) before pointing an existing application at the destination. `examples/python/portable.py` runs the same collection and search logic against either backend.

## Operations

All calls are synchronous. Collection creation fails with `AlreadyExists`; obtaining a missing collection raises `NotFound`. Reacquire handles after dropping/recreating a collection; do not change collection configuration through a different client while using it.

| Operation | Result |
|---|---|
| `Client.remote(url, api_key=key, timeout=10.0, ca_file=None)` | **0.4.0 source:** verified Recern HTTP v1 backend; context manager/`close()` releases resources |
| `Client.embedded(path, read_only=False)` | Opens or creates a database; read-only mode requires an existing file |
| `Client.qdrant(url, api_key=None, timeout=10.0, namespace="recern", ca_file=None)` | Checks the server version; uses certificate verification and optional custom CA |
| `client.create_collection(name, dim, metric="cosine", quantization="f32")` | A new `Collection` |
| `client.collection(name)` | Existing `Collection`, with `name`, `spec` and an opaque `identity` |
| `client.list_collections()` | Sorted logical collection names belonging to this client/namespace |
| `client.drop_collection(name)` | Deletes the collection and its records |
| `collection.upsert(id, vector, metadata=None)` | Inserts or replaces the complete record; returns `None` |
| `collection.upsert_many(records)` | Writes at most 256 `Record` objects; returns the count; duplicate IDs within a batch are invalid |
| `collection.get(id)` | `Record(id, vector, metadata)` or `None` |
| `collection.delete(id)` | Whether the ID existed before deletion |
| `collection.count()` | Exact live record count at the time of the read |
| `collection.page(limit=100, cursor=None)` | `Page(records, next_cursor)`; limit 1–256; `None` marks the end |
| `collection.iter_records(batch_size=100)` | Iterator over pages |
| `collection.search(query, k=10, filter=None, exact=False)` | `list[Hit(id, distance, metadata)]`; `k` 1–10,000 |
| `client.snapshot()` | Read-only embedded view of committed data; Qdrant raises `Unsupported` |

Empty batches write nothing. Inputs are validated before any record in a batch is written. This does not make a remote batch transactional. Results and records are dataclasses; metadata and vectors are detached values.

## Data and distances

- IDs are case-sensitive Unicode strings, including empty strings, up to 65,536 UTF-8 bytes. They are never parsed as integers. Collection names are nonempty Unicode strings up to 255 UTF-8 bytes.
- Dimensions are integers from 1 to 65,536. Vectors are converted to finite float32; booleans and strings are rejected. Squared norm must not exceed `float32.max / 8`. Cosine additionally requires squared norm at least `float32.min_normal`, avoiding zero/underflow normalization. A backend may impose a stricter resource limit and will return an explicit error.
- Metadata is any finite JSON value, including a scalar, array or null. Object keys are strings. Integers must fit `[-2^63, 2^64-1]`; nesting is limited to 64 and encoded metadata to 1 MiB. Tuples and arbitrary Python objects are not part of this profile. Store identifiers as strings when interoperating with languages whose numbers cannot represent large integers exactly.
- Cosine vectors are normalized on insertion. Int8 reads return dequantized stored values. Retain original embeddings separately when their original precision matters.

Distances always sort **smaller first**:

| Metric | Portable distance | Qdrant conversion |
|---|---|---|
| `cosine` | `max(0, 1 - cosine similarity)` | `max(0, 1 - score)` |
| `l2` | Sum of squared coordinate differences | Square the Euclidean score |
| `dot` | Negative dot product | Negate the score |

Returned hits are sorted by `(distance, logical ID)`. ANN engines can return different candidates, and ties at the top-k boundary need not select identical IDs. Use `exact=True` and numeric tolerances to compare adapters. Exact int8 search is exact over the stored quantized representation, not over the original embeddings. No new Qdrant/portable performance or recall benchmark is claimed by the existing engine benchmark report.

## Filter semantics

The profile preserves [Recern filters](filters.md). Object entries combine with AND. `$and` and `$or` take nonempty lists of filters; `$not` takes one filter. `{}` and an omitted filter match everything.

- `{"field": value}` and `{"field": {"$eq": value}}` compare the whole value. `$in` compares the whole value to each member; an empty `$in` matches nothing. An array field does not implicitly match one of its elements.
- Dotted paths traverse objects only: `"customer.country"`. They do not index arrays or address literal dots in a key. Such keys are retained in metadata. Keys starting with `$` can be stored but cannot be used as direct field predicates.
- A missing field fails equality, membership and range predicates. A present null matches `{"field": null}`. Negation also matches records where the negated field is missing.
- `$gt`, `$gte`, `$lt`, `$lte` accept finite numeric bounds and compare numeric fields as float64. Booleans are distinct from numbers. Numeric scalar equality also uses float64 (`1` equals `1.0`); large adjacent integers can therefore compare equal after conversion.
- Structural array/object equality preserves nested number types (`{"n": 1}` differs from `{"n": 1.0}`), array order and nulls; object key order is irrelevant. `$eq` is necessary when the literal object being matched contains `$`-prefixed keys.

Qdrant's native payload semantics are different. The adapter stores original metadata as a JSON string and indexes typed whole-value equality tokens plus numeric projections under encoded paths. It never silently translates array equality into membership or missing fields into null. The shared golden fixture is consumed by Rust and both Python backends.

## Acknowledgements and capabilities

| Capability | Embedded | Qdrant |
|---|---|---|
| `profile` | `recern-vector/1` | `recern-vector/1` |
| `atomic_batches` | `True` | `False` |
| `consistent_export` | `True` | `False` |
| `quantization` | `("f32", "int8")` | `("f32",)` |
| `snapshot_pages` | `True` only for a read-only snapshot | `False` |
| `exact_search`, `max_batch` | `True`, 256 | `True`, 256 |

Embedded writes call the native durable `save()` before returning. The native `Database` API still requires explicit saves. One portable client serializes its operations; stale writers from other clients fail with `Conflict`. There is no transaction merging. Opening a snapshot loads committed snapshot + WAL into a separate immutable in-memory database; the source and snapshot both consume memory. Export record buffering is bounded, but the embedded engine itself is not out-of-core.

Qdrant writes use `wait=true` and `ordering=strong`, and require a completed acknowledgement. Reads use `consistency=all`. These are Qdrant request settings, not a new transaction or replication guarantee; server configuration and availability still matter. There is no automatic retry. On timeout, the write may already have committed: read it back or replay the same logical IDs. Concurrent writes to the same ID have no compare-and-swap guarantee. The timeout bounds socket operations, not the total duration of a multi-request operation.

Page cursors are opaque, backend-specific and not a durable resume format. Embedded cursors are bound to a client session and mutation revision; mutation invalidates them. Qdrant pages observe the server separately at each request and can miss concurrent changes. Stop writers for application-level iteration that must cover every record. The migration journal has separate resume semantics.

## Qdrant ownership and version support

The tested server is **Qdrant 1.19.2**. Compatible later 1.x versions pass the version gate but are not automatically a tested matrix entry. Earlier versions and a future major version raise `Unsupported`. The adapter uses REST with Python's standard library; no `qdrant-client` dependency is required.

Logical names map to `rv1_<namespace>_<SHA-256(name)>`. Collection metadata records the profile and a generation UUID. Logical IDs map to UUIDv5 using the frozen namespace `c6c97780-86c3-4d47-a0f4-d956ccacb498`; the original ID remains in each `_recern` payload envelope. Existing points are checked before replacement. Mapping collisions, malformed envelopes and incompatible collections raise errors. Envelope fields and projections are adapter-owned; manage these collections exclusively through this adapter. Direct payload/vector edits or concurrent collection replacement bypass its checks and are unsupported. Existing arbitrary Qdrant collections are not adopted.

An API key requires HTTPS except for literal loopback addresses and `localhost`. Redirects are refused, certificates are verified, and keys/request bodies are excluded from error messages. The transport caps a request or response at 64 MiB; reduce batch/page size for large records. Namespaces separate logical ownership; they are not a security or tenant-isolation boundary.

See Qdrant's [collection API](https://api.qdrant.tech/api-reference/collections/create-collection), [query API](https://api.qdrant.tech/api-reference/search/query-points/), [point model](https://qdrant.tech/documentation/manage-data/points/) and [filter semantics](https://qdrant.tech/documentation/search/filtering/).

## Errors

All profile errors inherit `PortableError`; unsuccessful operations never become empty successful results.

| Error | Meaning |
|---|---|
| `InvalidArgument` | Invalid input, filter, cursor or HTTP 400/422 |
| `NotFound` | Missing collection or required migration journal |
| `AlreadyExists` | Collection, export or destination already exists |
| `Conflict` | Stale writer, ID collision, changed destination or mismatched migration journal |
| `Unsupported` | Unsupported profile, server version, precision or snapshot capability |
| `PermissionDenied` | Read-only mutation or HTTP 401/403 |
| `Unavailable` | Transport/I/O failure, throttling or ambiguous write acknowledgement |
| `CorruptData` | Invalid database, archive, envelope or server response |

## Local contract tests

After building the Python bindings:

```sh
docker compose -f dev/compose.yml up -d
export RECERN_TEST_QDRANT_URL=http://127.0.0.1:6335
export RECERN_TEST_QDRANT_API_KEY=recern-vector-tests-local-only
crates/recern-vector-py/.venv/bin/python -m pytest crates/recern-vector-py/tests
docker compose -f dev/compose.yml down
```

The compose service binds to loopback and uses a public, local-test-only key. CI runs the same contract against that pinned, authenticated Qdrant version; the wheel release workflow gates publication on those tests. Without the environment variables, local Qdrant cases are skipped explicitly. Frozen file formats 1/2, the portable filter fixture and migration export v1 are compatibility fixtures; do not regenerate them when changing implementations.
