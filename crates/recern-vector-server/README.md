# Recern Vector server (development)

Separate Rust HTTP/1.1 server for portable profile `recern-vector/1`.
Source milestone `0.4.0-dev.1`; not a published 0.4.0 release.

From the repository root:

```sh
cargo build --locked --release -p recern-vector-server
```

Run with a new/empty database, a private 32–256 byte API-key file and a loopback
listener. Use a verified TLS proxy for remote access.

[Run, limits, ownership and recovery](../../docs/server.md) ·
[HTTP contract](../../docs/http-api.md) · [OpenAPI](../../api/openapi.json)

Single writer, durable WAL acknowledgements, generation checks, bounded
operations and snapshot leases. The database and leased snapshots fit in RAM;
saves still encode the whole image. Portable remote SDKs and their complete
migration validation are the next stage.
