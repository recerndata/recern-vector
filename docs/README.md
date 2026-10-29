# Recern Vector documentation

Recern Vector 0.4.0 is an embedded vector database with a local snapshot and a WAL. You open it from Python, Rust, Node.js or the command line, add vectors with JSON metadata, and search them with an HNSW index. Every search can report how it ran, and every collection can report the state of its index and measure its own recall. Version 0.4.0 provides a separate [single-node HTTP server](server.md) and [portable Python, Rust and Node clients](portable-clients.md), including SDK migration and interoperability tests. See the [release guide](release.md) for packages, native archives and compatibility.

> **Version 0.4.0.** HTTP server, portable Python/Rust/Node clients and checked migration across embedded Recern, Recern HTTP and Qdrant. Format 2, format-1 readers and export v1 are retained.

## When it fits

- Semantic search, RAG retrieval or recommendations **inside an application**: a desktop app, a script, a notebook, a backend process that owns its data.
- Up to a few million vectors that fit in memory.
- You want to **see** what the index is doing: why a query was slow, whether a filter was applied efficiently, what recall you actually get.

## When it does not

- Many processes writing directly to the same database file. Use a single database owner. The [0.4.0 HTTP server](server.md) serializes HTTP access to its own database; it is not a distributed or highly available service.
- Data much larger than memory. The whole database is loaded on open.
- You need constant-cost commits regardless of database size. WAL writes changed pages, but `save()` still encodes and compares a full in-memory image (see [Limitations and next work](limitations.md)).

## Contents

| Page | What it covers |
|---|---|
| [0.4.0 release](release.md) | Packages, native artifacts, checksums and upgrade compatibility |
| [Getting started](getting-started.md) | Install, then a first database from Python, Rust, Node.js and the CLI |
| [Concepts](concepts.md) | Databases, collections, records, metrics, the HNSW index, saving, threads |
| [Filters](filters.md) | Metadata filter syntax and how filtered searches are planned |
| [Inspecting and tuning](inspection.md) | `explain`, `stats`, `estimate_recall`, choosing `ef`, `compact` |
| [Python API](python.md) | Every class, method and exception in the `recern_vector` package |
| [Portable clients (0.4.0)](portable-clients.md) | Python remote, Rust and Node across embedded Recern, Recern HTTP and Qdrant |
| [Portable Python API](portable-api.md) | Profile v1, embedded/Qdrant backends, shared semantics and capabilities |
| [HTTP API contract (0.4.0)](http-api.md) | Released OpenAPI, wire values, authentication, durable writes, generations and snapshot leases |
| [HTTP server](server.md) | Build and run, release status, limits, single-writer ownership, recovery and TLS proxy guidance |
| [Export and migration](migration.md) | Consistent snapshots, JSONL manifests, dry-run, resume and verification |
| [Rust API](rust.md) | The `recern-vector` crate |
| [Node.js](nodejs.md) | Native builds, TypeScript definitions and asynchronous search |
| [Command line](cli.md) | The `recern-vector` command |
| [File format](file-format.md) | What is inside a `.rvec` file |
| [Limitations and next work](limitations.md) | Current bounds and what comes next |

Runnable examples are in [`examples/`](../examples/README.md). Benchmarks against faiss, LanceDB and sqlite-vec: [recern.net/vector/benchmarks](https://recern.net/vector/benchmarks).

See [Node.js](nodejs.md) and the [durability contract](file-format.md).
