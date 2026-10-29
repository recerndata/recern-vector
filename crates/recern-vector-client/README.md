# Recern portable Rust client

**Version 0.4.0.** Supports embedded files, the Recern HTTP v1 server and
portable Qdrant 1.19.2 collections. Install with `cargo add recern-vector-client@0.4.0`.

```rust,no_run
use recern_vector_client::{Client, CollectionSpec, HttpOptions, Record};
use serde_json::json;

fn example() -> Result<(), Box<dyn std::error::Error>> {
let client = Client::embedded("prototype.rvec", false)?;
// Change configuration, then use the same data operations:
// let client = Client::remote(HttpOptions::new("https://vectors.example.com",
//                          Some(std::env::var("RECERN_API_KEY")?)))?;
// let client = Client::qdrant(HttpOptions::new("https://qdrant.example.com",
//                          Some(std::env::var("RECERN_API_KEY")?)), "app")?;
let docs = client.create_collection(CollectionSpec::new("documents", 2, "cosine"))?;
docs.upsert(Record::new("one", vec![1.0, 0.0], json!({"lang":"en"})))?;
let hits = docs.search(&[1.0, 0.0], 10, Some(r#"{"lang":"en"}"#), true)?;
client.close()?;
Ok(()) }
```

`Record.metadata_json` and `Hit.metadata_json` preserve signed/unsigned 64-bit
integers and nested `1` versus `1.0`. `Record::new` accepts `serde_json::Value`;
`record.metadata()` parses checked JSON. All metadata/filter JSON rejects
duplicate keys, overflow, non-finite numbers and excess size/depth.

Collections expose `upsert_many`, `get`, `delete`, `count`, `page`, `search`
and generation-checked `drop`. Counts are `u64`. Records/pages are bounded by
256; queries by 10,000 hits. Distances are smaller-is-closer for every backend.
`client.capabilities()` reports backend differences. Qdrant batches are not
declared atomic and cannot supply a consistent portable snapshot. Its native
`completed` acknowledgements are not a Recern fsync guarantee. Its generation
preflight is not an atomic vendor-side collection compare-and-swap: do not
drop/reconfigure Qdrant collections concurrently with application operations.

HTTP uses verified TLS, an optional `HttpOptions.ca_file`, total per-request
deadlines, bounded request/response bytes and no redirects or retries. HTTP is
allowed for authenticated clients only on literal loopback/localhost. Errors
have `code`, optional `request_id` and `write_outcome`; a timed-out or malformed
write response can mean the write committed. Verify before replaying. This is a
blocking API: use a blocking worker when called from an async application.

Native collections are bound to a client-session generation; reacquire them
after reopening. Native snapshots own a read-only committed view. Remote
snapshots have an absolute TTL and become invalid after restart. Call
`snapshot.close()` to release the server lease and observe errors; dropping
its last handle attempts best-effort release. Cursors are opaque and bound to
the database/view/generation. Changing backend configuration does not migrate
data. The resumable archive migration orchestrator remains the Python API;
Rust and Node provide compatible data operations, not separate archive tools.

Run the same application example with `cargo run -p recern-vector-client
--example portable -- --path demo.rvec`, `--remote URL` or `--qdrant URL`.
Set `RECERN_API_KEY` for servers and `--ca path/to/root.pem` for a private CA.
See [portable clients](https://recern.net/vector/docs/portable-clients) for the tested matrix.
