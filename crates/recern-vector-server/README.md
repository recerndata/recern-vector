# Recern Vector server — 0.4.0

Single-node HTTP/1.1 server for portable profile `recern-vector/1`.

```sh
cargo install recern-vector-server --version 0.4.0 --locked
# Or build: cargo build --locked --release -p recern-vector-server
```

Native CLI/server archives are also available from the
[0.4.0 release](https://github.com/recerndata/recern-vector/releases/tag/v0.4.0).
Run with a new/empty database, a private 32–256 byte API-key file and a loopback
listener. Use a verified TLS proxy for remote access.

[Run, limits, ownership and recovery](https://recern.net/vector/docs/server) ·
[HTTP contract](https://recern.net/vector/docs/http-api) ·
[Portable clients](https://recern.net/vector/docs/portable-clients)

Single writer, durable WAL acknowledgements, generation checks, bounded
operations and snapshot leases. The database and leased snapshots fit in RAM;
saves still encode the whole image. Python, Rust and Node portable clients
support embedded Recern, Recern HTTP and Qdrant. Existing application files are
migrated through the checked Python export/import API, not adopted as server files.
