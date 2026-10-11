# Portable export and migration

**Recern Vector 0.3.0** adds a versioned export and resumable import for `recern_vector.portable`. Supported paths are embedded Recern → embedded Recern and embedded Recern → Qdrant. A consistent export from live Qdrant is not implemented: `export_snapshot` raises `Unsupported` before creating files. Use Qdrant's native snapshot tools for its backups.

This is an application data transfer, not a copy of an HNSW index. The destination rebuilds its index from records. Source files are never modified or deleted, and existing destination collections are never merged or replaced.

## Export, inspect, dry-run, import

```python
from recern_vector.portable import Client, export_snapshot, inspect_export, import_snapshot

source = Client.embedded("prototype.rvec", read_only=True)
export_snapshot(source, "prototype-export", batch_size=100)
report = inspect_export("prototype-export")
print(report["records"], report["sha256"])

target = Client.qdrant("https://vectors.example.com", api_key=key, namespace="production")
print(import_snapshot("prototype-export", target, dry_run=True))
result = import_snapshot("prototype-export", target,
                         state_path="prototype-import.json", batch_size=100)
assert result["verified"]
```

Use `collections=["documents", "products"]` on export to choose a subset. The output directory must not exist. Export acquires an immutable view of committed snapshot + WAL, streams records into numbered JSONL files, fsyncs them, and writes `manifest.json` last. A failed export removes only the new directory it created. An existing read-only snapshot stays at its original view, even if a writer later saves new data.

`inspect_export` checks the manifest, every record, dimensions, precision declarations, counts, SHA-256 digests and duplicate logical IDs. Duplicate detection uses a temporary on-disk SQLite index; JSONL buffering is bounded. A valid archive can still exceed a server's capacity or request limit. Dry-run performs the same data checks and checks collection-name conflicts, without creating destination collections, a journal or a lock file. It does not reserve names, test future capacity or guarantee credentials have write permission. Constructing an embedded target client may itself create an empty `.rvec` file.

## Resume and verification

```python
result = import_snapshot("prototype-export", target,
                         state_path="prototype-import.json", resume=True)
```

Keep the export and journal unchanged until import completes. A journal binds the exact manifest bytes (SHA-256), destination identity, collection configurations, creation intents and acknowledged record counts. It contains no API key. Its default location is next to the export directory, with a destination-specific suffix. A journal inside the archive is rejected. A process lock prevents two importers from writing the same journal simultaneously; use one journal per import and exclusive ownership of the destination collections.

Each batch, at most 256 records, follows this sequence:

1. Upsert by the original logical IDs and wait for acknowledgement.
2. Read every record back, comparing the ID, JSON metadata and stored vectors.
3. Atomically write and fsync the new journal progress.

If the process dies after the server commits but before progress is saved, resume replays those IDs. It also verifies all previously acknowledged records before moving ahead. Creation intent is saved before creating a collection; Qdrant's generation marker permits recovery after a lost create acknowledgement. An embedded pending collection can be adopted only when empty and compatible, under the exclusive-ownership requirement.

The final collection counts must match. A completed journal can be resumed again to recheck every record. `verified=True` means counts, IDs, metadata and stored vectors were checked; it does not mean ANN recall is identical. Vector comparison uses relative tolerance `2e-5` and absolute tolerance `2e-6`, accounting for float32 and cosine normalization. Metadata preserves scalar/array/object form and numeric types; object key order and textual JSON whitespace are not significant.

Do not write to or replace destination collections during migration. The importer detects configuration changes, different database identities, Qdrant generation changes, altered acknowledged records and unexpected counts; it is not a distributed transaction or a general reconciliation engine. Concurrent raw writes can invalidate guarantees. Timeouts may leave a committed batch. There is no automatic remote rollback: inspect the error and keep the archive/journal for safe replay. Removing the journal is not a way to merge an existing destination.

Archives must remain immutable during preflight and import. They are validated before any writes and checksummed again while reading; modifying one during import can leave a partially written destination followed by an error. This format is not a signature or an authenticity mechanism. Filesystem errors while creating export/journal files can surface as `OSError`; data/contract errors use the [portable error categories](portable-api.md#errors).

## Format v1

The export is a directory, separate from `.rvec` format 2 and its WAL:

```text
prototype-export/
  manifest.json
  0000.jsonl
  0001.jsonl
```

The manifest contains `format: "recern-vector-export"`, integer `version: 1`, `profile: "recern-vector/1"`, `consistent_snapshot: true`, `source`, UTC `created_at`, and an ordered `collections` list. Each entry has:

| Field | Meaning |
|---|---|
| `name`, `dim`, `metric` | Logical collection definition |
| `source_precision` | `f32` or `int8` |
| `vectors` | `stored-f32` or `stored-dequantized`, respectively |
| `normalization` | `unit-at-insertion` for cosine, otherwise `none` |
| `records` | Number of live records, excluding deleted/replaced nodes |
| `file` | Zero-padded sequential filename, such as `0000.jsonl` |
| `sha256` | Lowercase SHA-256 over the exact UTF-8 file bytes, including newlines |

Each line is exactly one JSON object with `id`, `vector` and `metadata`, followed by a newline. Unknown format/profile versions, duplicate JSON keys, unsafe filenames, symlinked record files, duplicate IDs, non-finite numbers, invalid vectors, missing newlines, mismatched hashes/counts and inconsistent precision declarations are rejected. Limits are 10,000 collections, 4 MiB per manifest or line, plus the portable metadata/vector limits. Empty collections have an empty JSONL file and its SHA-256 digest. Additional descriptive manifest fields can be added without changing record semantics.

The checked-in `crates/recern-vector-py/tests/fixtures/export-v1` archive is frozen compatibility evidence. Incompatible semantic changes require a new export/profile version and a documented reader or migration path.

## Precision and cutover

Imports create **f32** collections. An f32 source exports stored values, already normalized for cosine. An int8 source exports its dequantized stored representation, not its pre-quantization embeddings. The manifest makes this explicit, and cosine insertion may normalize it again. No embedding model is called and lost precision cannot be recovered. Retain original embeddings if you need a lossless transfer of the original input vectors.

Pause source writers for the final export, import and verify, then run representative queries and measure recall/latency before changing the application's backend configuration. There is no change-data-capture or live dual-write mechanism in 0.3.0. Keep the source as a rollback point until the application has been checked.

The runnable [`examples/python/migrate.py`](../examples/python/migrate.py) supports `--source`, `--target-file`, `--qdrant`, `--dry-run` and `--resume`. Create the export once; omit `--source` on subsequent dry-run/import/resume invocations. Set `RECERN_QDRANT_API_KEY` for the Qdrant example rather than placing credentials in command-line arguments.
