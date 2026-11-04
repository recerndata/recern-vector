# Recern Vector for Node.js

> **0.5.0** adds `setCheckpointPolicy`, `checkpointPolicy` and
> `persistenceStats`, paged format 3, `formatVersion` and explicit `upgradeStorage`. See the [policy guide](../../docs/persistence-0.5.md).

The native API and asynchronous `./portable` entry point support embedded Recern,
Recern HTTP and Qdrant. See the [portable clients guide](https://recern.net/vector/docs/portable-clients).

```sh
npm install @recern/vector@0.5.0
```

npm chooses the exact-version native addon for Linux x64/ARM64 (glibc 2.35+),
macOS Intel/Apple silicon or Windows x64. Keep optional dependencies enabled.
Self-contained archives are also attached to the [0.5.0 GitHub release](https://github.com/recerndata/recern-vector/releases/tag/v0.5.0).
See [release packages and compatibility](https://recern.net/vector/docs/release).
New files use format 3, which Workspace 1.0.0 cannot read; existing format-2 files
retain their format until an explicit storage upgrade.

Native Node-API 8 bindings using [napi-rs](https://napi.rs/docs/introduction/getting-started). Build from this repository with Node.js 22+ and Rust 1.89+:

```sh
cd crates/recern-vector-node
npm run build
npm test
```

```js
const {Database} = require('@recern/vector');
const db = Database.openOrCreate('docs.rvec');
const docs = db.createCollection('docs', {dim: 3, metric: 'cosine', quantization: 'int8'});
docs.upsert('a', new Float32Array([1, 0, 0]), {lang: 'en'});
db.save();
const hits = await docs.searchAsync(new Float32Array([1, 0, 0]), 5, {
  filter: {$or: [{lang: 'en'}, {lang: 'ru'}], $not: {hidden: true}}
});
db.checkpoint(); // makes the .rvec snapshot portable without WAL
```

`new Database(path)` opens an existing file; `create`, `openOrCreate` and `openReadOnly` are factories. Collections support upsert, atomic upsertMany, get, delete, search, searchAsync, explain, stats and compact. Definitions are in `index.d.ts`. Metadata must be JSON-compatible; use strings for integers outside JavaScript's safe numeric range.

`searchAsync` copies the Float32Array before dispatch and holds a shared database lock on a worker. Sync mutations wait for running searches; a dropped collection fails subsequent operations. Read-only handles reject changes. Saves fsync the WAL, and a stale writer must reopen. Other methods block the calling thread: use Node worker threads for large imports or persistence.

`npm run build` creates a native binary for the current host. Release builds test Linux x64/ARM64, macOS Intel/Apple silicon and Windows x64 and upload target-specific artifacts. The npm umbrella package selects a platform addon; GitHub archives are self-contained builds for one platform.
