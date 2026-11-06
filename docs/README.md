# Recern Vector documentation

Recern Vector 0.5.0 is an embedded vector database with a local snapshot and a WAL. You open it from Python, Rust, Node.js or the command line, add vectors with JSON metadata, and search them with an HNSW index. Every search can report how it ran, and every collection can report the state of its index and measure its own recall. Version 0.5.0 provides a separate [single-node HTTP server](server.md) and [portable Python, Rust and Node clients](portable-clients.md), including SDK migration and interoperability tests. See the [release guide](release.md) for packages, native archives and compatibility.

> **Version 0.5.0.** Incremental encoding, paged format 3, automatic WAL checkpoints
> and published Node packages. Readers retain formats 1/2. HTTP/profile/export v1
> remain unchanged. See the [persistence guide](persistence-0.5.md).
>
> **Workspace 1.0.0 cannot open format 3.** Keep existing files at format 2 when
> using its current reader; see [release compatibility](release.md).

## When it fits

- Semantic search, RAG retrieval or recommendations **inside an application**: a desktop app, a script, a notebook, a backend process that owns its data.
- Up to a few million vectors that fit in memory.
- You want to **see** what the index is doing: why a query was slow, whether a filter was applied efficiently, what recall you actually get.

## When it does not

- Many processes writing directly to the same database file. Use a single database owner. The [HTTP server](server.md) serializes HTTP access to its own database; it is not a distributed or highly available service.
- Data much larger than memory. The whole database is loaded on open.
- You need a larger-than-memory engine or bounded latency regardless of maintenance. Format-3 saves encode changed records, but open/checkpoint still process a full image and cached pages add RAM and padding. Format-2 saves retain full-image encoding (see [Limitations and next work](limitations.md)).

## Contents

| Page | What it covers |
|---|---|
| [0.5.0 release](release.md) | Packages, native artifacts, checksums and upgrade compatibility |
| [Persistence in 0.5.0](persistence-0.5.md) | Incremental pages, checkpoint policy, upgrade, recovery and measured limits |
| [Getting started](getting-started.md) | Install, then a first database from Python, Rust, Node.js and the CLI |
| [First vector search](semantic-search.md) | A complete Python example, filtered results, query inspection and Workspace compatibility |
| [Concepts](concepts.md) | Databases, collections, records, metrics, the HNSW index, saving, threads |
| [Filters](filters.md) | Metadata filter syntax and how filtered searches are planned |
| [Inspecting and tuning](inspection.md) | `explain`, `stats`, `estimate_recall`, choosing `ef`, `compact` |
| [Python API](python.md) | Every class, method and exception in the `recern_vector` package |
| [Portable clients](portable-clients.md) | Python remote, Rust and Node across embedded Recern, Recern HTTP and Qdrant |
| [Portable Python API](portable-api.md) | Profile v1, embedded/Qdrant backends, shared semantics and capabilities |
| [HTTP API contract (0.4.0)](http-api.md) | Released OpenAPI, wire values, authentication, durable writes, generations and snapshot leases |
| [HTTP server](server.md) | Build and run, release status, limits, single-writer ownership, recovery and TLS proxy guidance |
| [Export and migration](migration.md) | Consistent snapshots, JSONL manifests, dry-run, resume and verification |
| [Rust API](rust.md) | The `recern-vector` crate |
| [Node.js](nodejs.md) | Native builds, TypeScript definitions and asynchronous search |
| [Command line](cli.md) | The `recern-vector` command |
| [File format](file-format.md) | What is inside a `.rvec` file |
| [Limitations and next work](limitations.md) | Current bounds and what comes next |
| [Preparing 1.0](compatibility.md) | Public API stability, deprecation, HTTP version matrix and remaining release gates |

Runnable examples are in [`examples/`](../examples/README.md). The [benchmark report](https://recern.net/vector/benchmarks) includes 0.5 development-engine persistence and full SIFT1M/GloVe-100 f32/int8 measurements, with the unchanged engine retained for 0.5.0. Historical 0.4.0/faiss results remain separately labeled. The earlier comparison with LanceDB and sqlite-vec is kept in the [historical report](https://recern.net/vector/benchmarks/historical).

See [Node.js](nodejs.md) and the [durability contract](file-format.md).
