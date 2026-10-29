# Portable clients (0.4.0)

**Recern Vector 0.4.0, 29 October 2026.** Python, Rust and Node.js expose
profile `recern-vector/1` against embedded Recern, the Recern HTTP v1 server and
Qdrant 1.19.2. See [release packages and compatibility](release.md) for installation
and verified platform archives. Recern does not provide a hosted Vector service.

Changing configuration uses the same application data operations. It does not
copy existing data. Use Python's checked [export/import](migration.md) to move
data before switching an application. The migration archive remains format 1;
native formats 1/2 and their frozen fixtures are unchanged.

## Python remote backend

Install `pip install --upgrade recern-vector` (version 0.4.0 or later), then use:

```python
import os
from recern_vector.portable import Client, Record, export_snapshot, import_snapshot

with Client.remote("https://vectors.example.com",
                   api_key=os.environ["RECERN_API_KEY"], timeout=10.0,
                   ca_file="private-root.pem") as client:
    docs = client.create_collection("documents", dim=2, metric="cosine")
    docs.upsert_many([Record("one", [1, 0], {"lang": "en"})])
    hits = docs.search([1, 0], k=5, filter={"lang": "en"}, exact=True)
    export_snapshot(client, "remote-export")
```

Omit `ca_file` for public CA certificates. `Client.embedded(path)` and
`Client.qdrant(url, api_key=key, namespace="app")` use the same collection API.
Remote writes save on the server before a successful durable acknowledgement.
An old remote collection handle carries its original generation; it fails with
`Conflict` after drop/recreate, including empty batches and missing-record
operations. An explicit `client.collection(name)` obtains a new handle.

`client.snapshot()` returns a read-only view with a server lease. Use it as a
context manager or call `close()`. Snapshot pages remain consistent while live
data changes; leases expire at an absolute TTL and disappear on server restart.
`export_snapshot` releases its lease on success and on validation/I/O failures.
An export longer than the lease fails; it does not silently renew or change view.

The Python transport bounds the caller's total HTTP deadline, including DNS,
TLS, headers and trickled response bodies. An OS DNS call can outlive its caller
in a daemon I/O worker; at most eight unfinished workers per transport are
allowed. Closing a client rejects new requests and closes its live connections.

## Rust

Install the `recern-vector-client` crate, version 0.4.0. It leaves the low-level
`recern_vector::Database` API unchanged.

```sh
cargo add recern-vector-client@0.4.0
```

```rust,no_run
use recern_vector_client::{Client, CollectionSpec, HttpOptions, Record};
use serde_json::json;
fn example() -> Result<(), Box<dyn std::error::Error>> {
let client = Client::embedded("prototype.rvec", false)?;
// let client = Client::remote(HttpOptions::new("https://vectors.example.com",
//     Some(std::env::var("RECERN_API_KEY")?)))?;
// let client = Client::qdrant(HttpOptions::new("https://qdrant.example.com",
//     Some(std::env::var("RECERN_API_KEY")?)), "app")?;
let docs = client.create_collection(CollectionSpec::new("documents", 2, "cosine"))?;
docs.upsert(Record::new("one", vec![1.0, 0.0], json!({"lang":"en"})))?;
let hits = docs.search(&[1.0, 0.0], 5, Some(r#"{"lang":"en"}"#), true)?;
client.close()?;
Ok(()) }
```

Collections provide `upsert_many`, `get`, `delete`, `count`, `page`, `search`
and generation-checked `drop`. Counts are `u64`; `Record`/`Hit` carry
`metadata_json` strings. `Record::new` accepts a `serde_json::Value` and
`record.metadata()` parses checked JSON without losing uint64 or nested numeric
kinds. Set `HttpOptions.ca_file` for a private CA and `timeout` for the total
per-request deadline. This API blocks: use an appropriate blocking worker from
an async runtime. Native collection generations are client-session scoped;
reacquire handles when reopening. Close remote snapshots explicitly to observe
release errors; dropping the last handle attempts best-effort release.

The runnable example supports all three backends:

```sh
cargo run -p recern-vector-client --example portable -- --path demo.rvec
# Set RECERN_API_KEY, then use --remote URL or --qdrant URL --namespace app.
# Add --ca path/to/root.pem for a private CA.
```

## Node.js

Install the platform-specific npm tarball from the [0.4.0 GitHub release](release.md).
For example, on Linux x64 with glibc:

```sh
npm install https://github.com/recerndata/recern-vector/releases/download/v0.4.0/recern-vector-0.4.0-node-linux-x64.tgz
```

The native API is `@recern/vector`; the portable entry point is
`@recern/vector/portable`. Direct public-npm publication is still planned for 0.5.0.

```js
const {Client} = require('@recern/vector/portable');
const client = await Client.embedded('prototype.rvec');
// const client = await Client.remote('https://vectors.example.com', {
//   apiKey: process.env.RECERN_API_KEY, timeoutMs: 10000, caFile: 'private-root.pem'
// });
// const client = await Client.qdrant('https://qdrant.example.com', {
//   apiKey: process.env.RECERN_API_KEY, namespace: 'app'
// });
try {
  const docs = await client.createCollection('documents', {dim: 2});
  await docs.upsert('one', [1, 0], '{"lang":"en","uint64":18446744073709551615,"kind":[1.0]}');
  const hits = await docs.search([1, 0], {k: 5, filterJson: '{"lang":"en"}', exact: true});
  console.log(await docs.count()); // 1n
  for await (const record of docs.iterRecords({batchSize: 100})) {
    // record.metadataJson stays lossless JSON text.
  }
} finally {
  await client.close();
}
```

All portable data operations run on native workers and return promises. They
share the Rust adapters and validators. Record vectors accept `number[]` or
`Float32Array`; returned vectors are arrays. Counts are `bigint`, pages have
`records`/`nextCursor`, and hits have `id`/`distance`/`metadataJson`.
Metadata and filters deliberately use JSON **text**. `JSON.parse` followed by
`JSON.stringify` can lose uint64 and nested `1.0`, so do not use that round trip
for precision-sensitive data. Duplicate keys and invalid values are rejected.
The existing synchronous native entry point keeps its original behavior.

Call `snapshot.close()` in a `finally` block. Root-client `close()` also releases
its outstanding snapshot leases. `PortableError` has `code`, `requestId` and
`writeOutcome`. Large argument/response conversion still costs memory; worker
execution does not remove the engine's RAM or O(database size) save limits.

## Backend guarantees and validation

| Capability | Embedded Recern | Recern HTTP | Qdrant 1.19.2 |
|---|---|---|---|
| Collection/record CRUD, pages, exact/ANN filters | Yes | Yes | Yes, portable envelope |
| Atomic validated batch | Yes | Yes | Not declared |
| Consistent snapshot/export source | Yes | Yes, bounded lease | Unsupported |
| Storage precision | f32 / int8 | f32 / int8 | f32 |
| Acknowledgement | Native durable save | Durable WAL save | Native completed operation |

All clients reject unsupported precision/capabilities explicitly. Qdrant
generations are checked before requests, not atomically by the vendor: do not
drop/reconfigure its collections concurrently with data operations. Raw vendor
collections/points without an intact portable envelope are not adopted. Python,
Rust and Node use the same UUID mapping and typed metadata projection, including
array/object equality, null versus missing fields, Unicode paths and OR/NOT.

Remote clients verify HTTPS by default, support a custom CA, bound HTTP body
sizes and reject redirects. They never retry mutations automatically. A timeout,
disconnect or malformed write acknowledgement leaves its outcome potentially
unknown; read back records or verify a creation token/generation before replay.
Errors exclude credentials, request data and free-form proxy bodies. Cursors
are opaque handles, not transferable migration journals or stable offsets.

The shared frozen filter fixture runs on all three backends in each language.
Additional tests cover uint64/numeric kinds, migration from embedded to server
and back, lost create/batch acknowledgements, restart/reconnect, stale handles,
snapshot expiry/cleanup, bounded failures, verified private-CA TLS and packed
Node files. CI runs native/server clients on Linux/macOS/Windows and the full
Qdrant plus Python/Rust/Node interoperability matrix on Linux. Check the current
commit's CI results before treating that platform matrix as passed.

The published artifacts and supported upgrade path are documented in the
[release guide](release.md). Version 0.4.0 does not include
clustering, a managed service, RBAC or the 0.5.0 incremental-persistence work.
