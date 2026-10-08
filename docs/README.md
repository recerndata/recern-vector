# Recern Vector documentation

Recern Vector is an embedded vector database. A database is a single file; there is no server. You open the file from Python, Rust or the command line, add vectors with JSON metadata, and search them with an HNSW index. Every search can report how it ran, and every collection can report the state of its index and measure its own recall.

> **Status: prototype (0.0.x).** The API and the file format will change between releases. Version 0.1 is reserved for a stable, versioned file format.

## When it fits

- Semantic search, RAG retrieval or recommendations **inside an application**: a desktop app, a script, a notebook, a backend process that owns its data.
- Up to a few million vectors that fit in memory.
- You want to **see** what the index is doing: why a query was slow, whether a filter was applied efficiently, what recall you actually get.

## When it does not

- Many processes writing to the same database at once, or a shared database behind a network API. Use a database server.
- Data much larger than memory. The whole database is loaded on open.
- You need durable writes after every single change. Today `save()` rewrites the whole file (see [Limitations and roadmap](limitations.md)).

## Contents

| Page | What it covers |
|---|---|
| [Getting started](getting-started.md) | Install, then a first database from Python, Rust and the CLI |
| [Concepts](concepts.md) | Databases, collections, records, metrics, the HNSW index, saving, threads |
| [Filters](filters.md) | Metadata filter syntax and how filtered searches are planned |
| [Inspecting and tuning](inspection.md) | `explain`, `stats`, `estimate_recall`, choosing `ef`, `compact` |
| [Python API](python.md) | Every class, method and exception in the `recern_vector` package |
| [Rust API](rust.md) | The `recern-vector` crate |
| [Command line](cli.md) | The `recern-vector` command |
| [File format](file-format.md) | What is inside a `.rvec` file |
| [Limitations and roadmap](limitations.md) | What the prototype does not do yet, and what comes next |

Runnable examples are in [`examples/`](../examples/README.md). Benchmarks against faiss, LanceDB and sqlite-vec: [recern.net/vector/benchmarks](https://recern.net/vector/benchmarks).
