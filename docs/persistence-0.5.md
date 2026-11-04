# Persistence work for Vector 0.5.0

Status: **0.5.0-dev.1**, development only. The published baseline is **0.4.0**.
Workspace **1.0.0** has shipped separately; its embedded reader remains 0.2.0.

## Incremental encoding and the page directory

New databases use **format 3**: 4096-byte pages with stable object/chunk addresses,
per-page CRC32, a checksum-tree root, a validated page directory and reusable free
pages. Records and changed HNSW neighbors are tracked separately, including during
parallel insertion. Save serializes only those records, without reconstructing or
comparing the whole database. Original int8 codes and scales are preserved.

A clean save serializes no records and adds no WAL frame. A one-record delete
changes two pages (file header and tombstone); native tests at 32/256/2048 records
check identical WAL growth, one encoded record and 4124 validation bytes. These
are work counters and functional checks, not release-build latency benchmarks.
Steady saves also traverse collection headers/catalog entries; growing the
checksum tree occasionally rebuilds its compact CRC array. Opening, compaction,
checkpoint and explicit format upgrade still have whole-image/collection costs.
The engine keeps vectors, graph and encoded pages in RAM. Pages impose storage
padding, particularly for small and int8 vectors; this is not an out-of-core engine.

Existing files remain compatible on ordinary saves: format 2 stays format 2,
and format 1 first checkpoints to format 2. To explicitly opt into format 3:

```python
db = rv.Database("data.rvec")
print(db.format_version)       # actual file format; rv.FORMAT_VERSION is reader maximum
backup = db.upgrade_storage()  # pathlib.Path for a format-2 backup, None if already 3
```

Rust uses `upgrade_storage() -> Result<Option<PathBuf>>`; Node uses
`upgradeStorage(): string | null` and `formatVersion`; the CLI command is
`recern-vector upgrade-storage data.rvec`. Stop other writers first. New Unix snapshot/WAL files are created with mode 0600, and atomic replacements
preserve existing permissions. The backup
is a standalone, synced copy of the last durable state (mode 0600 on Unix),
created before replacing the original snapshot. Pending changes are included in
the converted snapshot. An error after rename can mean the upgrade committed;
reopen to inspect. Format-1 input must checkpoint to format 2 before this call.

Published 0.2/0.3/0.4 readers (and Workspace 1.0.0's embedded reader) reject format
3 explicitly. Use the backup or migration/export v1 to move back; no silent
binary downgrade is performed. See the [byte layout and upgrade contract](file-format.md).

## Automatic checkpoints

Writable handles now attempt a synchronous checkpoint after either:

- the valid WAL reaches **64 MiB**, including its header; or
- **1,000 committed frames** have accumulated since the base snapshot.

The triggers use OR, exclude incomplete tails and recover the commit count when
reopening. They are maintenance targets, not hard storage quotas: one large
commit or a failing checkpoint can exceed them. The policy is per handle and
is not stored in the file. Reopening uses the defaults unless configured again.
Reads and read-only handles never run maintenance. There is no background timer.

A save first appends and syncs its WAL frame. If the subsequent automatic
checkpoint fails, the committed mutation remains successful. The failure is
available in persistence statistics; a later `save()` retries maintenance even
when no application data changed. It does not append a duplicate frame or
advance the commit sequence. Manual `checkpoint()` continues to return errors
and can also persist pending changes.

A checkpoint writes and syncs a temporary snapshot, atomically renames it and
then resets the WAL. If reset fails after the rename, the handle keeps the new
snapshot state; the old WAL is already included. Directory sync retains the
existing platform/filesystem-dependent guarantees. Process-exit tests do not
establish power-loss durability on every filesystem.

Always call `checkpoint()` explicitly before copying a standalone `.rvec`, with
writers stopped during the copy. Do not infer that a scheduled checkpoint ran.

### Rust

```rust
use recern_vector::{CheckpointPolicy, Database};

let mut db = Database::open("data.rvec")?;
db.set_checkpoint_policy(CheckpointPolicy::new(Some(32 * 1024 * 1024), Some(500))?)?;
db.save()?;
let stats = db.persistence_stats();
if let Some(error) = stats.last_checkpoint_error {
    // The save committed; investigate maintenance without replaying mutations.
    eprintln!("Checkpoint deferred: {error}");
}
// db.set_checkpoint_policy(CheckpointPolicy::disabled())?;
```

Thresholds are positive integers or `None`. A zero threshold is rejected before
changing the policy. Statistics describe this handle's last open/save state,
not a live observation of another process. `automatic_checkpoints` counts
successful automatic maintenance during this handle's lifetime, not a persisted
historical total. `wal_bytes` is the valid WAL prefix; `wal_commits` counts only
frames not already represented by the snapshot.

### Python

```python
import recern_vector as rv

db = rv.Database("data.rvec")
db.set_checkpoint_policy(max_wal_bytes=32 * 1024 * 1024, max_commits=500)
db.save()
print(db.checkpoint_policy)
print(db.persistence_stats())
# db.set_checkpoint_policy()  # both triggers disabled
```

Omitted thresholds are disabled. The statistics dictionary contains
`wal_bytes`, `wal_commits`, `automatic_checkpoints`, `last_checkpoint_error`,
`last_encoded_records`, `last_written_pages` and `last_validated_bytes`. The last
three report work by this handle's last save, not historical totals. Legacy
format-2 saves still encode all physical records; their page/validation counters
are zero (not measured by that path).

### Node.js

```js
const { Database } = require('@recern/vector');
const db = new Database('data.rvec');
db.setCheckpointPolicy({ maxWalBytes: 32 * 1024 * 1024, maxCommits: 500 });
db.save();
console.log(db.checkpointPolicy, db.persistenceStats());
// db.setCheckpointPolicy({}); // both triggers disabled
```

Node thresholds must be positive safe integers; omitted fields disable that
trigger. Statistics use camelCase keys. The native save/checkpoint calls are
synchronous; use a worker for a large database. Portable data operations retain
profile v1 and inherit the embedded engine's default policy.

### HTTP server

```sh
recern-vector-server --database server.rvec --api-key-file key \
  --checkpoint-wal-bytes 33554432 --checkpoint-commits 500
```

Server flags default to 64 MiB and 1,000 commits. A flag value of `0` disables
that trigger; set both to `0` for manual maintenance. Custom policy is retained
when the server reloads after a failed operation. A fixed warning is logged once
per maintenance-failure episode, without paths, keys or records. HTTP API v1,
acknowledgements, snapshot leases and migration format v1 are unchanged.
Automatic checkpoint work is synchronous and can add latency to the save that
triggers it, including time spent inside an HTTP request deadline.

## Compatibility and checks

Readers retain formats **1/2** and add **3**; WAL **1**, portable profile **v1**,
HTTP **v1** and migration **v1** are retained. Frozen old fixtures are never
regenerated. Compatibility checks use real 0.3/0.4 Python packages: format-2 writes
stay readable, format-3 input is rejected without modification, and an export
from format 3 imports into an older engine. A separate check with the published
0.2.0 native binding verifies the reader used by Workspace 1.0.0; this does not
replace a future UI/refresh integration check in Workspace.

The native suite covers both precision modes, byte and commit triggers, no-op
retry, reopening, read-only handles, stale writers, concurrent saves and torn
WAL tails. Checkpoint I/O failures are injected before/after snapshot sync and
after snapshot rename. Subprocesses exit without cleanup after WAL write/sync,
temporary-file write, snapshot sync, snapshot rename and WAL reset; reopening must recover the
complete WAL commit and allow further writes. These hooks exist only in the
Rust test build.

Python/Node tests exercise the actual bindings. Real loopback HTTP tests verify
both flags, disabled scheduling, unchanged durable sequences and process restart.
Cross-platform CI and release-package checks remain required before publication.

## Remaining 0.5.0 work

1. Publish reproducible release-build commit latency and peak-memory results as
   databases grow, plus current f32/int8 recall and memory measurements. Do not
   extrapolate search-only 0.4.0 benchmarks to persistence or concurrent serving.
2. Publish and verify Node packages for advertised targets. GitHub tarballs from
   0.4.0 remain the public distribution until that release gate is completed.
