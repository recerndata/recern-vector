# Recern Vector for Node.js — 0.2.0

Native Node-API 8 bindings using [napi-rs](https://napi.rs/docs/introduction/getting-started). Build from this repository with Node.js 22+ and Rust 1.89+:

```sh
cd crates/recern-vector-node
npm run build
npm test
```

```js
const {Database} = require('./');
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

`npm run build` creates a native binary for the current host. CI builds/tests Linux, macOS and Windows and uploads target-specific artifacts. This package has not been published to npm; do not distribute one platform's binary as a universal package. Publish platform packages and a platform-selecting loader before a public npm release.
