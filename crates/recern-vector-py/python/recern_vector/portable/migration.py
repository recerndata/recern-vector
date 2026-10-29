"""Versioned, checked streaming exports and resumable imports.

Export is supported only from a backend with a consistent snapshot capability.
Import requires exclusive application ownership of the destination collections.
There is no implicit embedding model, collection replacement or remote rollback.
"""
from __future__ import annotations

import hashlib
import json
import math
import os
from pathlib import Path
import shutil
import sqlite3
import tempfile
from contextlib import closing, contextmanager
from datetime import datetime, timezone
from dataclasses import asdict
import uuid

from .model import (
    PROFILE, MAX_BATCH, AlreadyExists, CollectionSpec, Conflict, CorruptData,
    InvalidArgument, NotFound, Record, Unsupported, dumps, integer, validated_records,
)

FORMAT = "recern-vector-export"
FORMAT_VERSION = 1
MAX_RECORD_BYTES = 4 * 1024 * 1024
MAX_MANIFEST_BYTES = 4 * 1024 * 1024
MAX_JOURNAL_BYTES = 16 * 1024 * 1024


def _json(raw):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate JSON key")
            result[key] = value
        return result
    return json.loads(raw, object_pairs_hook=unique,
                      parse_constant=lambda x: (_ for _ in ()).throw(ValueError(x)))


def _sync_dir(path):
    if os.name != "nt":
        fd = os.open(path, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)


def _atomic_json(path, value):
    fd, temp = tempfile.mkstemp(prefix="." + path.name + ".", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write((dumps(value) + "\n").encode())
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temp, path)
        _sync_dir(path.parent)
    finally:
        if os.path.exists(temp):
            os.unlink(temp)


def export_snapshot(client, destination, *, collections=None, batch_size=100):
    """Write manifest.json last; partial exports are never valid imports."""
    integer(batch_size, "batch_size", 1, MAX_BATCH)
    # A remote snapshot is a bounded server lease, including on failures before
    # destination.mkdir(). Always release it instead of waiting for its TTL.
    with client.snapshot() as snapshot:
        return _export_snapshot(snapshot, destination, collections, batch_size)


def _export_snapshot(snapshot, destination, collections, batch_size):
    names = snapshot.list_collections() if collections is None else list(collections)
    if len(names) != len(set(names)) or len(names) > 10_000:
        raise InvalidArgument("export requires unique collection names (at most 10000)")
    sources = [snapshot.collection(name) for name in names]
    destination = Path(destination)
    try:
        destination.mkdir()
    except FileExistsError as e:
        raise AlreadyExists("export destination already exists") from e
    manifest = {"format": FORMAT, "version": FORMAT_VERSION, "profile": PROFILE,
                "consistent_snapshot": True, "source": snapshot.capabilities.backend,
                "created_at": datetime.now(timezone.utc).isoformat(), "collections": []}
    try:
        for index, collection in enumerate(sources):
            filename = f"{index:04d}.jsonl"
            digest, count = hashlib.sha256(), 0
            with (destination / filename).open("xb") as stream:
                for record in collection.iter_records(batch_size=batch_size):
                    # Existing low-level databases may contain values outside
                    # profile v1; fail explicitly rather than dropping data.
                    record = validated_records([record], collection.spec)[0]
                    line = (dumps(asdict(record)) + "\n").encode("utf-8")
                    if len(line) > MAX_RECORD_BYTES:
                        raise Unsupported("stored record exceeds the portable export limit")
                    stream.write(line)
                    digest.update(line)
                    count += 1
                stream.flush()
                os.fsync(stream.fileno())
            if count != collection.count():
                raise Conflict("source changed during snapshot export")
            manifest["collections"].append({"name": collection.name, "dim": collection.spec.dim,
                "metric": collection.spec.metric, "source_precision": collection.spec.quantization,
                "vectors": "stored-dequantized" if collection.spec.quantization == "int8" else "stored-f32",
                "normalization": "unit-at-insertion" if collection.spec.metric == "cosine" else "none",
                "records": count, "file": filename, "sha256": digest.hexdigest()})
        if len((dumps(manifest) + "\n").encode()) > MAX_MANIFEST_BYTES:
            raise Unsupported("export manifest exceeds 4 MiB; export fewer collections")
        _atomic_json(destination / "manifest.json", manifest)
        _sync_dir(destination)
        return manifest
    except Exception:
        # Only this newly created directory is removed, never an existing export.
        shutil.rmtree(destination)
        raise


def _manifest(source):
    source = Path(source).resolve()
    try:
        with (source / "manifest.json").open("rb") as f:
            raw = f.read(MAX_MANIFEST_BYTES + 1)
        if len(raw) > MAX_MANIFEST_BYTES:
            raise ValueError("manifest too large")
        manifest = _json(raw)
        if manifest["format"] != FORMAT or type(manifest["version"]) is not int or manifest["version"] != FORMAT_VERSION or manifest["profile"] != PROFILE:
            raise Unsupported("unsupported migration format or portable API version")
        if manifest["consistent_snapshot"] is not True:
            raise ValueError("export is not a consistent snapshot")
        entries = manifest["collections"]
        if type(entries) is not list or len(entries) > 10_000:
            raise ValueError("invalid collections")
        names = set()
        for index, entry in enumerate(entries):
            CollectionSpec(entry["name"], entry["dim"], entry["metric"])
            if entry["name"] in names or entry["file"] != f"{index:04d}.jsonl":
                raise ValueError("duplicate collection or unsafe filename")
            names.add(entry["name"])
            integer(entry["records"], "record count", 0, 2**63 - 1)
            if entry["source_precision"] not in ("f32", "int8") or entry["vectors"] != ("stored-dequantized" if entry["source_precision"] == "int8" else "stored-f32"):
                raise ValueError("unknown precision")
            if entry["normalization"] != ("unit-at-insertion" if entry["metric"] == "cosine" else "none"):
                raise ValueError("unknown normalization")
            if not isinstance(entry["sha256"], str) or len(entry["sha256"]) != 64 or any(c not in "0123456789abcdef" for c in entry["sha256"]):
                raise ValueError("invalid checksum")
            path = source / entry["file"]
            if path.is_symlink() or not path.is_file():
                raise ValueError("missing or symlinked records file")
        return source, manifest, hashlib.sha256(raw).hexdigest()
    except (OSError, ValueError, TypeError, KeyError) as e:
        raise CorruptData("invalid or incomplete migration archive") from e


def _records(source, entry):
    spec = CollectionSpec(entry["name"], entry["dim"], entry["metric"])
    count, digest = 0, hashlib.sha256()
    try:
        with (source / entry["file"]).open("rb") as stream:
            while line := stream.readline(MAX_RECORD_BYTES + 1):
                if len(line) > MAX_RECORD_BYTES or not line.endswith(b"\n"):
                    raise ValueError("oversized or incomplete record")
                digest.update(line)
                value = _json(line)
                if type(value) is not dict or set(value) != {"id", "vector", "metadata"}:
                    raise ValueError("invalid record fields")
                record = validated_records([Record(**value)], spec)[0]
                count += 1
                yield record
        if count != entry["records"] or digest.hexdigest() != entry["sha256"]:
            raise ValueError("record count or SHA-256 does not match")
    except (OSError, ValueError, TypeError, KeyError) as e:
        raise CorruptData(f"invalid records for collection {entry['name']!r}") from e


def inspect_export(source):
    """Validate every record/checksum with bounded RAM; duplicate IDs use disk."""
    source, manifest, fingerprint = _manifest(source)
    with tempfile.TemporaryDirectory(prefix="recern-import-check-") as temp:
        with closing(sqlite3.connect(Path(temp) / "ids.sqlite")) as ids:
            ids.execute("CREATE TABLE ids (id TEXT PRIMARY KEY)")
            for entry in manifest["collections"]:
                ids.execute("DELETE FROM ids")
                for record in _records(source, entry):
                    try:
                        ids.execute("INSERT INTO ids VALUES (?)", (record.id,))
                    except sqlite3.IntegrityError as e:
                        raise CorruptData("duplicate logical ID in migration archive") from e
                ids.commit()
    return {"manifest": manifest, "sha256": fingerprint,
            "records": sum(e["records"] for e in manifest["collections"])}


@contextmanager
def _journal_lock(path):
    with Path(str(path) + ".lock").open("a+b") as lock:
        lock.seek(0)
        if not lock.read(1):
            lock.write(b"\0")
            lock.flush()
        lock.seek(0)
        try:
            if os.name == "nt":
                import msvcrt
                msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as e:
            raise Conflict("another import owns this progress journal") from e
        try:
            yield
        finally:
            lock.seek(0)
            if os.name == "nt":
                msvcrt.locking(lock.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                fcntl.flock(lock, fcntl.LOCK_UN)


def _same_record(expected, actual, spec):
    if actual is None or actual.id != expected.id or dumps(actual.metadata) != dumps(expected.metadata) or len(actual.vector) != spec.dim:
        return False
    vector = expected.vector
    if spec.metric == "cosine":
        norm = math.sqrt(math.fsum(v * v for v in vector))
        vector = [v / norm for v in vector]
    return all(math.isclose(a, b, rel_tol=2e-5, abs_tol=2e-6) for a, b in zip(vector, actual.vector))


def _check_record(collection, record):
    if not _same_record(record, collection.get(record.id), collection.spec):
        raise Conflict(f"destination record differs from the archive: {record.id!r}")


def import_snapshot(source, client, *, dry_run=False, resume=False, state_path=None, batch_size=100, progress=None):
    """Preflight all data before writing; replay unacknowledged batches by ID.

    All destination collections are new. Import never merges arbitrary existing
    data or drops/replaces a collection. F32 targets receive stored/dequantized
    vectors; cosine insertion can renormalize them. Keep original embeddings if
    pre-quantization precision matters.
    """
    integer(batch_size, "batch_size", 1, MAX_BATCH)
    if type(dry_run) is not bool or type(resume) is not bool:
        raise InvalidArgument("dry_run and resume must be booleans")
    if dry_run and resume:
        raise InvalidArgument("dry_run checks a new import; use resume separately")
    checked = inspect_export(source)
    source = Path(source).resolve()
    entries = checked["manifest"]["collections"]
    if getattr(client._backend, "read_only", False):
        raise Unsupported("cannot import into a read-only client")
    verify_identity = getattr(client._backend, "verify_identity", None)
    if verify_identity is not None:
        verify_identity()  # A connected endpoint may have restarted with another database.
    key = hashlib.sha256(client.identity.encode()).hexdigest()[:16]
    state_path = Path(state_path) if state_path is not None else source.with_name(source.name + ".import-" + key + ".json")
    state_path = state_path.resolve()
    if state_path == source or source in state_path.parents:
        raise InvalidArgument("the progress journal must be outside the immutable export directory")
    # Dry-run takes no lock and creates no state, collection or ID.
    if dry_run:
        existing = set(client.list_collections())
        conflicts = [e["name"] for e in entries if e["name"] in existing]
        if conflicts:
            raise AlreadyExists("dry run found existing destination collections")
        return {"dry_run": True, "collections": len(entries), "records": checked["records"],
                "source_precisions": sorted({e["source_precision"] for e in entries}), "target_precision": "f32"}
    with _journal_lock(state_path):
        if state_path.exists():
            if not resume:
                raise Conflict("a progress journal exists; pass resume=True to continue")
            try:
                with state_path.open("rb") as stream:
                    raw = stream.read(MAX_JOURNAL_BYTES + 1)
                if len(raw) > MAX_JOURNAL_BYTES:
                    raise ValueError()
                state = _json(raw)
                if type(state["version"]) is not int or state["version"] != 1 or state["archive"] != checked["sha256"] or state["destination"] != client.identity:
                    raise ValueError()
                if set(state["collections"]) != {e["name"] for e in entries}:
                    raise ValueError()
                for entry in entries:
                    item = state["collections"][entry["name"]]
                    integer(item["done"], "progress", 0, entry["records"])
                    if item["phase"] not in ("pending", "importing", "complete"):
                        raise ValueError()
                    if not isinstance(item["intent"], str):
                        raise ValueError()
                    uuid.UUID(item["intent"])
                    if item["phase"] == "pending":
                        if item["done"] != 0 or item["identity"] is not None:
                            raise ValueError()
                    elif not isinstance(item["identity"], str) or not item["identity"]:
                        raise ValueError()
                    if item["phase"] == "complete" and item["done"] != entry["records"]:
                        raise ValueError()
            except (ValueError, KeyError, TypeError) as e:
                raise Conflict("progress journal does not match this archive and destination") from e
        else:
            if resume:
                raise NotFound("no progress journal exists for this import")
            existing = set(client.list_collections())
            if any(e["name"] in existing for e in entries):
                raise AlreadyExists("import requires new destination collections")
            state = {"version": 1, "archive": checked["sha256"], "destination": client.identity,
                     "collections": {e["name"]: {"phase": "pending", "done": 0, "intent": str(uuid.uuid4()), "identity": None} for e in entries}}
            _atomic_json(state_path, state)
        for entry in entries:
            name = entry["name"]
            item = state["collections"][name]
            spec = CollectionSpec(name, entry["dim"], entry["metric"])
            try:
                collection = client.collection(name)
            except NotFound:
                if item["phase"] != "pending":
                    raise Conflict("destination collection was removed during migration") from None
                client._backend.create(spec, generation=item["intent"])
                collection = client.collection(name)
            if collection.spec != spec:
                raise Conflict("destination collection configuration changed")
            if item["phase"] == "pending":
                if collection.count() or (client.capabilities.backend == "qdrant" and collection.identity != item["intent"]):
                    raise Conflict("destination collection is not this import's empty collection")
                if client.capabilities.backend == "recern" and collection.creation_token != item["intent"]:
                    raise Conflict("destination collection was not created by this import")
                item.update(phase="importing", identity=collection.identity)
                _atomic_json(state_path, state)
            elif collection.identity != item["identity"]:
                raise Conflict("destination collection identity changed")
            batch = []
            def commit():
                collection.upsert_many(batch)
                for record in batch:
                    _check_record(collection, record)
                item["done"] += len(batch)
                _atomic_json(state_path, state)
                if progress is not None:
                    progress(name, item["done"], entry["records"])
                batch.clear()
            skip = item["done"]
            for index, record in enumerate(_records(source, entry)):
                if index < skip:
                    _check_record(collection, record)
                    continue
                batch.append(record)
                if len(batch) == batch_size:
                    commit()
            if batch:
                commit()
            if collection.count() != entry["records"]:
                raise Conflict("destination record count differs after import")
            item["phase"] = "complete"
            _atomic_json(state_path, state)
        return {"dry_run": False, "collections": len(entries), "records": checked["records"],
                "verified": True, "state_path": str(state_path), "target_precision": "f32"}
