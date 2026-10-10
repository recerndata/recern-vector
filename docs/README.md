# Recern Vector documentation

Recern Vector is an embedded vector database with a local snapshot and a WAL; there is no server. You open it from Python, Rust, Node.js or the command line, add vectors with JSON metadata, and search them with an HNSW index. Every search can report how it ran, and every collection can report the state of its index and measure its own recall.

> **Version 0.2.0.** Stable format 2 with format-1 compatibility; WAL, int8, logical filters and native Node.js bindings.

## When it fits

- Semantic search, RAG retrieval or recommendations **inside an application**: a desktop app, a script, a notebook, a backend process that owns its data.
- Up to a few million vectors that fit in memory.
- You want to **see** what the index is doing: why a query was slow, whether a filter was applied efficiently, what recall you actually get.

## When it does not

- Many processes writing to the same database at once, or a shared database behind a network API. Use a database server.
- Data much larger than memory. The whole database is loaded on open.
- You need constant-cost commits regardless of database size. WAL writes changed pages, but `save()` still encodes and compares a full in-memory image (see [Limitations and next work](limitations.md)).

## Contents

| Page | What it covers |
|---|---|
| [Getting started](getting-started.md) | Install, then a first database from Python, Rust, Node.js and the CLI |
| [Concepts](concepts.md) | Databases, collections, records, metrics, the HNSW index, saving, threads |
| [Filters](filters.md) | Metadata filter syntax and how filtered searches are planned |
| [Inspecting and tuning](inspection.md) | `explain`, `stats`, `estimate_recall`, choosing `ef`, `compact` |
| [Python API](python.md) | Every class, method and exception in the `recern_vector` package |
| [Rust API](rust.md) | The `recern-vector` crate |
| [Node.js](nodejs.md) | Native builds, TypeScript definitions and asynchronous search |
| [Command line](cli.md) | The `recern-vector` command |
| [File format](file-format.md) | What is inside a `.rvec` file |
| [Limitations and next work](limitations.md) | Current bounds and what comes next |

Runnable examples are in [`examples/`](../examples/README.md). Benchmarks against faiss, LanceDB and sqlite-vec: [recern.net/vector/benchmarks](https://recern.net/vector/benchmarks).

See [Node.js](nodejs.md) and the [durability contract](file-format.md).
