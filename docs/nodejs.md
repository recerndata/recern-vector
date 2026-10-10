# Node.js

Recern Vector 0.2.0 includes native Node-API 8 bindings, TypeScript definitions and asynchronous search. Use Node.js 22 or later. The package is not published to npm yet.

## Install a release build

Choose the archive for your operating system and CPU from the [0.2.0 release](https://github.com/recerndata/recern-vector/releases/tag/v0.2.0):

| Platform | Archive |
|---|---|
| Linux x64 | [recern-vector-0.2.0-node-Linux-X64.tar.gz](https://github.com/recerndata/recern-vector/releases/download/v0.2.0/recern-vector-0.2.0-node-Linux-X64.tar.gz) |
| macOS Apple silicon | [recern-vector-0.2.0-node-macOS-ARM64.tar.gz](https://github.com/recerndata/recern-vector/releases/download/v0.2.0/recern-vector-0.2.0-node-macOS-ARM64.tar.gz) |
| Windows x64 | [recern-vector-0.2.0-node-Windows-X64.tar.gz](https://github.com/recerndata/recern-vector/releases/download/v0.2.0/recern-vector-0.2.0-node-Windows-X64.tar.gz) |

Each archive contains a `package` directory with the native binary, loader, types and package metadata. Verify its checksum against the release's [SHA256SUMS](https://github.com/recerndata/recern-vector/releases/download/v0.2.0/SHA256SUMS), extract it, and load the directory with `require('./package')`. These are platform-specific builds, not a universal binary. On other platforms, build from source.

## First database

Save this as `example.cjs` beside the extracted `package` directory:

```js
const { Database } = require('./package');

async function main() {
  const db = Database.openOrCreate('docs.rvec');
  // Use a fresh path for this example; createCollection rejects an existing name.
  const docs = db.createCollection('docs', {
    dim: 3,
    metric: 'cosine',
    quantization: 'int8',
  });
  docs.upsert('a', new Float32Array([1, 0, 0]), { lang: 'en' });
  db.save(); // commits to the WAL

  const hits = await docs.searchAsync(new Float32Array([1, 0, 0]), 5, {
    filter: { $or: [{ lang: 'en' }, { lang: 'ru' }], $not: { hidden: true } },
  });
  console.log(hits);
  db.checkpoint(); // self-contained snapshot; pause writes before copying it
}

main().catch(error => { console.error(error); process.exitCode = 1; });
```

Run `node example.cjs`. On an existing database, open the collection with `db.collection('docs')` instead of creating it again.

## API and concurrency

`new Database(path)` opens an existing database. `Database.create`, `openOrCreate` and `openReadOnly` are factories. Collections support `upsert`, atomic `upsertMany`, `get`, `delete`, `search`, `searchAsync`, `explain`, `stats` and `compact`. Full signatures are in [`index.d.ts`](../crates/recern-vector-node/index.d.ts).

`searchAsync` copies the input `Float32Array` and searches on a worker while holding a shared database lock. Synchronous mutations wait for active searches; other methods run on the calling thread. Use Node worker threads for large imports or saves. Read-only handles reject mutations. A stale writer must reopen before saving again.

Metadata must be JSON-compatible. Use strings for integer values outside JavaScript's safe integer range.

## Build from source

With Node.js 22+ and Rust 1.89+:

```sh
git clone --branch v0.2.0 https://github.com/recerndata/recern-vector.git
cd recern-vector/crates/recern-vector-node
npm run build
npm test
```

This creates a native binary for the current host. See [file format and durability](file-format.md) for WAL recovery and checkpoint behavior, and [filters](filters.md) for logical filter syntax.
