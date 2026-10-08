# Limitations and roadmap

Recern Vector is a prototype (0.0.x). This page lists what it does not do yet, so you can decide whether it fits.

## Current limitations

- **Everything is in memory.** Opening a file loads the whole database; the data must fit in RAM.
- **Saving rewrites the whole file.** There is no write-ahead log yet, so frequent small saves of a large database are slow. Batch your changes and save once.
- **One writer.** Two processes saving the same file overwrite each other's changes.
- **Single upserts link sequentially.** Use `upsert_many` for bulk loads; it builds on all cores.
- **`float32` only**, no quantization: memory is about `4 × dim` bytes per vector.
- **Filters** support AND, equality, `$in` and numeric ranges. No OR, NOT, string ranges or array membership yet. See [Filters](filters.md).
- **No Node.js bindings** yet.
- **The file format will change** before 0.1.

## Roadmap

Next, for **0.1**:

1. A stable, versioned file format with compatibility tests.
2. Incremental writes (a write-ahead log), so saving costs as much as the change, not the whole database.
3. Documentation and examples (this site is the start).

After that, depending on what users ask for:

- `int8` scalar quantization to cut memory about four times.
- Smarter planning for filtered searches.
- Opening Recern Vector files in the [Recern workspace](https://recern.net) to browse collections, inspect the graph and measure recall visually.
- Node.js bindings.

**Not planned:** a server mode, sharding or replication, GPU acceleration, or a hosted service. Recern Vector stays a small embedded library on purpose.

Ideas and use cases are welcome in [GitHub issues](https://github.com/recerndata/recern-vector/issues).
