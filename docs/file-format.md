# File format

A Recern Vector database is a single binary file. This page describes format **version 1**. The format is not stable yet and may change before Recern Vector 0.1; files from a newer version are rejected with `CorruptDatabaseError` (Python) or `Error::UnsupportedVersion` (Rust).

## Layout

All integers are little-endian. `str` is a `u32` byte length followed by UTF-8 bytes.

```text
"RVEC"  format_version:u32  collection_count:u32
per collection:
  name:str  dim:u32  metric:u8  m:u32  ef_construction:u32  ef_search:u32
  rng_state:u64  node_count:u32  entry:u32 (u32::MAX = none)  max_level:u32
  per node:   id:str  deleted:u8  has_metadata:u8 [metadata:str]
              layer_count:u8  per layer: link_count:u32 links:u32*
  vectors:    node_count * dim f32
crc32:u32   (IEEE, over every preceding byte)
```

- `metric` is 0 for cosine, 1 for L2, 2 for dot product.
- Metadata is stored as JSON text.
- The HNSW graph is stored as it is in memory, so opening a file does not rebuild the index.
- Deleted nodes are kept (with `deleted = 1`) until `compact()`.
- `rng_state` keeps the level generator's state, so inserts after reopening continue the same random sequence.

## Integrity

- **Checksum.** The last four bytes are a CRC32 of everything before them. A file that fails the check, has the wrong magic bytes or ends early is reported as corrupt rather than loaded partially.
- **Atomic saves.** `save()` writes `FILE.tmp`, flushes it to disk, renames it over `FILE` and flushes the directory. After a crash the file is either the previous version or the new one.

## Size

Roughly `4 × dim` bytes per vector, plus about `4 × links` bytes per node for the graph (an average of about 32 links on the bottom layer with the default `m = 16`), plus ids and metadata. For example, SIFT1M (one million 128-dimensional vectors) takes 631 MB.
