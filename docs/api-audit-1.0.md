# Public API and recovery audit for Vector 1.0

Scope: Recern Vector 1.0.0. The fixes below retain snapshot formats 1/2/3,
WAL 1, HTTP v1, portable
profile v1 and export v1. Released fixtures and artifacts are not rewritten.

## Scope and evidence

The audit covers the boundaries in [the compatibility policy](compatibility.md):

| Surface | Reviewed behavior | Executable evidence |
|---|---|---|
| Rust engine | Public exports/fields, generic batch inputs, error variants, input bounds, read-only access, search/recall, persistence | `tests/compatibility/native-client.rs`, native integration tests, `public_api_validation.rs` |
| Rust portable client | Embedded/HTTP/Qdrant descriptors, close, snapshots, errors and ambiguous writes, JSON precision, deadlines | Client tests, frozen external HTTP consumer, shared SDK interoperability suite |
| Native Python | Exported classes, keyword arguments, result properties, buffer ownership, locks, collection lifetime, save behavior | Installed-wheel `test_recern_vector.py`, read-only result declarations in `_native.pyi` |
| Portable Python | Embedded/HTTP/Qdrant generations, close/no-op behavior, filters, pagination, transport, export/import resume | Shared `test_portable.py`, transport tests, HTTP and migration tests |
| Node.js | Native sync/async operations, copied query buffers, numeric conversion, portable promises, package exports and declarations | Node tests; external strict TypeScript consumer compiled against `npm pack` output |
| HTTP/OpenAPI | Routes, authentication, error categories, limits, write acknowledgements, generations, snapshot leases, shutdown/restart | OpenAPI/schema fixtures, real subprocess HTTP tests, 0.4/0.5/candidate matrix |
| CLI | Documented commands/flags, validation, durable writes, query output, nonzero failures | CLI quickstart/interoperability tests and shared engine validation |
| Files and migrations | Frozen readers, checksums, bounds, pending changes, WAL tails, upgrades/backups, resumed imports | Native recovery tests, released-reader checks, export-v1 fixtures and migration tests |

`recern-vector-server` exposes implementation modules for its binary and tests;
they are not an embeddable server SDK covered by the 1.x client API.
`PortableSession` is likewise an internal bridge. Neither changes the public
HTTP contract. Native APIs retain explicit `save()`; portable mutations save
before success. Qdrant acknowledgements retain Qdrant's own durability semantics.

The external native Rust program is compiled/run unchanged against published
0.5.0 and the candidate. It uses struct literals, exhaustive error/strategy
matches, public result fields and `Send + Sync` bounds. The existing external
portable Rust consumer is retained in the HTTP version matrix. TypeScript checks
exercise native and portable package entry points, bigint counts, JSON text,
`PortableError`, and `await using` on a packed client. Disposal types require
TypeScript 5.2+; CI uses 6.0.3 with Node 22.

## Confirmed issues and fixes

- **Native Python/Node stale collection objects.** A handle retained only a
  name; dropping and recreating that name let it read or overwrite the replacement.
  Each binding now retains an opaque in-memory generation token. Validation and
  the operation share one database lock, including Node's asynchronous search.
  Old handles raise the existing native missing-collection error. Compaction
  preserves a handle; a drop/recreate invalidates it.
- **Portable Python lifetime differences.** Embedded and Qdrant handles now bind
  to the original collection. A missing collection produces `NotFound`; a
  replacement produces `Conflict`. An embedded reload after a failed durable
  write invalidates old handles: reacquire them from the client. `close()` is
  idempotent and rejects later operations, including empty batches, on all
  backends. Already-started Python operations finish before close. Immutable
  embedded snapshots have independent client lifetimes.
- **Node numeric conversion and native persistence bounds.** Fractional values
  and values such as `2**32 + 1` previously became `1` through N-API conversion.
  Dimension, HNSW and search inputs are validated before casting. Stored HNSW
  `ef` values must fit `1..=u32::MAX`; zero search/recall `ef` is rejected.
- **Non-finite native search results.** Finite f32 components could still produce
  infinite distances. New vectors and queries must have squared norm at most
  `f32::MAX / 8`, reserving arithmetic/quantization headroom. Older records remain
  readable; a search producing a non-finite hit now fails explicitly. The raw
  `Metric::distance` primitive documents normalized cosine inputs and asserts
  equal dimensions in both debug and optimized builds.
- **Python Qdrant deadlines.** A server trickling bytes could evade the socket's
  per-read timeout. Each request now has a total caller deadline and at most
  eight unfinished I/O workers per transport. Timed-out writes are not retried.
  Qdrant generation checks are preflight checks; administrative replacement
  racing a request remains outside the adapter's guarantees.
- **Type declarations.** TypeScript now declares the actual `PortableError`
  constructor and asynchronous disposal. Native Python result attributes are
  declared read-only, matching runtime behavior.
- **Native Python JSON conversion.** Cyclic metadata/filter objects could
  recurse without a bound in Rust. Input conversion now rejects nesting deeper
  than 64 and expansions beyond one million values before mutation. Tests
  include cycles, a valid nested roundtrip and an exponentially shared subtree.
  Portable Python conversion also enforces the expansion budget before its
  existing metadata byte limit.
- **Persistence acknowledgements.** Unix parent-directory sync errors now
  propagate. A failed checkpoint publication keeps WAL available instead of
  resetting it. A reopened writable handle establishes a new file/WAL/directory
  durability barrier on its first save, even without changes. A complete frame
  recovered after a failed writer sync must not get a false no-op acknowledgement.
  Exports also sync the parent of the newly created archive directory on Unix.

## Crash and failure coverage

Tests terminate a subprocess after a complete WAL write, WAL sync, temporary
snapshot write, snapshot sync, rename before directory sync, published checkpoint,
and WAL reset. Explicit format-2 upgrades exercise the snapshot boundaries while
retaining the standalone old-format backup. Recovery verifies records, search,
identity/sequence and the ability to continue writing; a stale writer cannot
replace the recovered commit.

Additional tests cover a returned write error after a partial temporary snapshot,
a returned failure after rename, a failed recovery barrier followed by a no-op
retry, a torn second commit in formats 2 and 3 retaining the preceding successful
commit, matched base/WAL restoration at a new path, checkpoint-only restoration,
and explicit rejection of a foreign WAL. Existing tests retain exhaustive torn
frame cuts, corrupt checksums/directories, read-only behavior, concurrent writers,
released-file upgrades and resumed migration verification.

These are process-crash, malformed-file and injected-I/O-failure tests. They do
not simulate a device losing power or prove persistence on every filesystem.
Unix directory sync follows the separate directory barrier required by
[fsync](https://man7.org/linux/man-pages/man2/fsync.2.html). Windows flushes file
data before replacement; this implementation has no unprivileged directory sync
barrier comparable to Unix. Windows process recovery passing does not establish
power-loss durability. See Microsoft's
[FlushFileBuffers documentation](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers).

## Recovery procedure

1. Stop all writers. Preserve the original `.rvec` and matching `.rvec.wal` as
   one offline pair before attempting recovery. Copying only a live snapshot can
   discard commits still present solely in its WAL.
2. Work on a copy in an isolated directory. Open it read-only first and inspect
   collections, record counts, known records and searches. Truncated final frames
   are ignored; corrupt complete frames or a foreign WAL fail explicitly.
3. Open the verified copy writable and call `checkpoint()`. Successful completion
   creates a standalone snapshot. Keep the original pair until the restored
   application has been checked. Never delete a WAL to hide a corruption error.
4. After an ambiguous save/upgrade error, reopen and inspect the sequence and
   logical records. For portable imports, reuse the checked export and its
   matching progress journal with `resume=True`; do not merge unrelated data.
5. A storage downgrade uses the retained format-2 backup or an export/import to
   an older supported target. Installing an older executable does not downgrade
   format 3. Workspace 1.0.0's existing embedded reader still accepts only 1/2.

## Release validation boundary

The ordinary CI workflow exercises Rust and HTTP on Linux/macOS/Windows, Python
native/remote operations on those hosts, Node and packed TypeScript on those
hosts, and authenticated Qdrant 1.19.2 on Linux. The separate compatibility
workflow runs all 27 released/candidate HTTP client/server cells. Its wheel is
built outside the checkout so the source provenance remains clean.

The [release workflow](https://github.com/recerndata/recern-vector/actions/workflows/release.yml)
checks the actual platform distributions, minimum Rust version, installed
Python wheels/source distribution, npm packages and migration from 0.3/0.4/0.5.
The [release guide](release.md) lists the supported builds. Existing 0.5
measurements do not establish new performance or hardware power-loss guarantees.
This audit does not claim that arbitrary applications or all possible crash
schedules have been exhaustively verified.
