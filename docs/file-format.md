# File format and durability

Published Recern Vector **0.2.0–0.4.0** writes format **2** and reads **1/2**. Development **0.5.0-dev.1** reads **1/2/3** and creates new files in paged format **3**. Existing format-2 files keep their layout on save/checkpoint; conversion requires an explicit `upgrade_storage()` / `upgradeStorage()` / CLI `upgrade-storage` call. Format-1 files first checkpoint to format 2. These are separate storage versions from the package version.

Old 0.2/0.3/0.4 readers, including Workspace 1.0.0's embedded 0.2.0 reader, reject format 3 as unsupported. To return to an older reader, retain the generated format-2 backup or export/import with portable migration v1. Export/import rebuilds the index; it is not a binary downgrade. Upgrades include pending in-memory changes, while the backup contains the last durable state. Stop other writers, inspect the returned backup path, and reopen after any upgrade error: an error after rename can mean the conversion already committed.

Frozen binary fixtures retain old layouts and cover format 3 separately; tests never regenerate them. The [portable migration format](migration.md), HTTP API v1 and WAL 1 are unchanged.

A database consists of `name.rvec` (snapshot), `name.rvec.wal` (committed changes since checkpoint) and `name.rvec.lock` (OS advisory coordination, no data). Call `checkpoint()` before copying just the `.rvec` file; stop concurrent writers while taking that copy. Otherwise back up snapshot and WAL together while no writes run. Copying an active snapshot alone can lose committed changes that only exist in WAL.

## Snapshot 3 (0.5 development)

Integers are little-endian. The file contains N fixed **4096-byte pages**, followed by a four-byte checksum-tree root. Each page reserves its last four bytes for IEEE CRC32 of the preceding 4092 bytes. The first page is:

```text
0..4    "RVEC"
4..8    version:u32 = 3
8..24   identity:u128 (nonzero)
24..32  sequence:u64
32..40  page_count:u64 (includes the header)
40..48  next_collection_id:u64 (nonzero, never reused)
48..4092 zero padding
4092..4096 page_crc32:u32
```

Every other page is either a free page (4092 zero bytes and its CRC), or one chunk of an object. Directory information lives in the pages themselves:

```text
0       kind:u8 (1=collection header, 2=node)
1..4    zero padding
4..12   collection_id:u64
12..16  physical_node_id:u32 (zero for collection headers)
16..20  chunk_index:u32
20..24  payload_bytes:u32 (1..4060)
24..28  chunk_count:u32
28..32  zero padding
32..4092 payload then zero padding
4092..4096 page_crc32:u32
```

Chunks are numbered from zero without duplicates/gaps; every non-final chunk carries 4060 bytes. Opening scans and validates the directory, rejecting orphan nodes, duplicate collection names, missing/extra nodes, unknown kinds and invalid counts, graph structure or payloads. Physical node IDs and page addresses remain stable until compaction or collection replacement. Freed pages are reused; the snapshot does not automatically shrink.

A collection-header object uses the format-2 collection fields from `name` through `max_level`. A node object contains its **own** vector (dim f32 values, or one f32 scale followed by dim i8 codes), followed by the format-2 node fields from `id` through its graph links. Original int8 codes/scales are serialized directly, without dequantization or requantization.

The footer is a CRC32 Merkle-tree root, **not** a CRC of the entire file. Tree leaves are page CRCs in file order, padded with zero leaves to the next power of two. A parent is `CRC32("RV3T" || left:u32 || right:u32)`. With one page the root is that page's CRC. The tree is reconstructed at open and updated in memory along changed paths at save; it is not another persisted page region. Checksums detect corruption, not malicious tampering.

The writer tracks changed records and HNSW neighbors. Each save serializes those objects, compares their pages and appends only changed pages plus the root to WAL. A clean save encodes no records. Steady-state writer validation reads the snapshot header/footer and checks for a newer WAL tail, using the already validated prefix. Growth of the checksum tree occasionally rebuilds its compact CRC array. Compaction rewrites its collection; open, explicit upgrade and checkpoint still process the complete image.

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
    sequence:u64 new_snapshot_length:u64 previous_snapshot_checksum:u32 (format-2 CRC or format-3 tree root)
    changed pages until end of payload:
      offset:u64 byte_count:u32 bytes[byte_count]
  payload_crc32:u32
```

Pages start at multiples of 4096, are ordered and nonoverlapping, and hold at most 4096 bytes. Each frame is an atomic save of the entire database, including collection creation/deletion and HNSW changes. Applying all pages and the target length reconstructs the snapshot including its own checksum. Unchanged pages are not written. An incomplete final frame or partially created header is ignored; complete frames with a wrong checksum, identity or sequence fail instead of silently losing data.

`save()` appends a frame and fsyncs before success. Mutations before save are in memory only. `checkpoint()` syncs a temporary snapshot, atomically renames it, syncs the parent directory where supported, then atomically resets WAL. Recovery ignores old frames already included by the snapshot sequence, making a crash between these steps safe. A no-op save does not append.

## Concurrency and bounds

In-process saves serialize. File locks coordinate snapshots and commits across processes; a handle whose durable state differs from disk fails with a stale-writer error and must reopen. This is optimistic single-writer coordination, not merging or MVCC. All access must use Recern's API; external rewriting, hard-link aliases, removing a live lock file, or filesystems without reliable local locks/atomic rename are unsupported.

`open_read_only()` acquires a shared lock when present, replays committed WAL and permits no mutations or persistence. It does not create sidecars. The result is a point-in-time in-memory snapshot; reopen to see later commits.

Format 2 retains full-image encoding/comparison at save and can dirty many pages when collection boundaries move. Format 3 caches encoded pages in addition to the in-memory index; it is not an mmap/out-of-core engine. Small records consume at least one page each, so int8 memory savings do not imply proportional file-size savings. Open/checkpoint remain O(database size). Published 0.4.0 has no automatic checkpoint scheduling. Development 0.5.0-dev.1 adds byte/commit triggers; they are not hard quotas, and failed maintenance does not undo a synced WAL commit. See [policy, statistics and recovery checks](persistence-0.5.md). On platforms/filesystems where directory fsync is unavailable, that final directory durability guarantee depends on the OS.
