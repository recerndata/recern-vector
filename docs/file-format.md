# File format and durability

Recern Vector 0.2.0 writes **format 2** and reads **formats 1 and 2**. These layouts are stable: incompatible changes require a new format number and a reader/migration path. Newer unsupported versions fail explicitly. Golden binary fixtures created with 0.0.2 and 0.2.0 are checked into `crates/recern-vector/tests/fixtures`; tests never regenerate them.

A database consists of `name.rvec` (snapshot), `name.rvec.wal` (committed changes since checkpoint) and `name.rvec.lock` (OS advisory coordination, no data). Call `checkpoint()` before copying just the `.rvec` file; stop concurrent writers while taking that copy. Otherwise back up snapshot and WAL together while no writes run. Copying an active snapshot alone can lose committed changes that only exist in WAL.

## Snapshot 2

All integers and IEEE-754 floats are little-endian. Strings are `u32` byte length followed by UTF-8. Collections are ordered lexicographically by name. Offsets are not Rust struct layouts.

```text
"RVEC" version:u32=2 identity:u128 sequence:u64 collection_count:u32
per collection:
  name:str dim:u32 metric:u8 encoding:u8
  m:u32 ef_construction:u32 ef_search:u32
  rng_state:u64 node_count:u32 entry:u32 max_level:u32
  if encoding=0: node_count*dim f32 values
  if encoding=1: node_count f32 scales, then node_count*dim signed i8 codes
  per node:
    id:str deleted:u8 has_metadata:u8 [metadata_json:str]
    layer_count:u8
    per layer: link_count:u32 links:u32*
crc32:u32 (IEEE polynomial 0xEDB88320, all preceding bytes)
```

Metric codes: cosine=0, squared L2=1, negative dot product=2. Encoding codes: f32=0, int8=1. Entry `u32::MAX` means no entry. Unknown encodings are rejected. Graph links, levels, counts, UTF-8, metadata, finite floats and checksums are validated when opening.

Int8 is symmetric per-vector quantization: `round(x / scale)`, limited to [-127,127], initially `scale=max(abs(x))/127` (zero L2/dot vectors use scale 1). Cosine codes use the reciprocal code-vector norm as their stored scale, retaining unit length after reconstruction. Returned vectors are approximations; originals are not retained. Exact search and recall refer to these stored approximations.

## Format 1 compatibility

Format 1 starts `RVEC version:u32=1 collection_count:u32`, omits identity, sequence and encoding, and places all f32 values **after** the node graph rather than before it. Remaining collection fields and checksum are unchanged. Opening does not migrate a file. The first save/checkpoint on a writable handle migrates it atomically to format 2; older releases cannot read the migrated file. Read-only handles never migrate.

## WAL 1

```text
"RVWAL\0\x01\0" identity:u128
per committed frame:
  payload_length:u64 length_crc32:u32 (checksum of the length bytes)
  payload:
    sequence:u64 new_snapshot_length:u64 previous_snapshot_crc32:u32 (the snapshot footer checksum)
    changed pages until end of payload:
      offset:u64 byte_count:u32 bytes[byte_count]
  payload_crc32:u32
```

Pages start at multiples of 4096, are ordered and nonoverlapping, and hold at most 4096 bytes. Each frame is an atomic save of the entire database, including collection creation/deletion and HNSW changes. Applying all pages and the target length reconstructs the snapshot including its own checksum. Unchanged pages are not written. An incomplete final frame or partially created header is ignored; complete frames with a wrong checksum, identity or sequence fail instead of silently losing data.

`save()` appends a frame and fsyncs before success. Mutations before save are in memory only. `checkpoint()` syncs a temporary snapshot, atomically renames it, syncs the parent directory where supported, then atomically resets WAL. Recovery ignores old frames already included by the snapshot sequence, making a crash between these steps safe. A no-op save does not append.

## Concurrency and bounds

In-process saves serialize. File locks coordinate snapshots and commits across processes; a handle whose durable state differs from disk fails with a stale-writer error and must reopen. This is optimistic single-writer coordination, not merging or MVCC. All access must use Recern's API; external rewriting, hard-link aliases, removing a live lock file, or filesystems without reliable local locks/atomic rename are unsupported.

`open_read_only()` acquires a shared lock when present, replays committed WAL and permits no mutations or persistence. It does not create sidecars. The result is a point-in-time in-memory snapshot; reopen to see later commits.

Incremental **disk writes** are implemented. Encoding, comparison and recovery still process a full in-memory image at save/open, and shifting collection boundaries can dirty many pages. Peak memory includes encoded snapshots in addition to the index. WAL has no automatic size limit in 0.2.0; checkpoint periodically. On platforms/filesystems where directory fsync is unavailable, that final directory durability guarantee depends on the OS.
