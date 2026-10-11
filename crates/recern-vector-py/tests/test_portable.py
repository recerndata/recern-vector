import json
import hashlib
import math
import os
import random
from pathlib import Path
import uuid

import pytest
from recern_vector import Database
from recern_vector.portable import (
    Client, Record, InvalidArgument, NotFound, AlreadyExists, Conflict, Unsupported,
    PermissionDenied, CorruptData, Unavailable, export_snapshot, import_snapshot, inspect_export,
)

FIXTURE = json.loads((Path(__file__).resolve().parents[3] / "crates/recern-vector/tests/fixtures/portable-v1.json").read_text(encoding="utf-8"))


@pytest.fixture(params=["embedded", "qdrant"])
def client(request, tmp_path):
    if request.param == "embedded":
        yield Client.embedded(tmp_path / "test.rvec")
    else:
        url = os.getenv("RECERN_TEST_QDRANT_URL")
        if not url:
            pytest.skip("set RECERN_TEST_QDRANT_URL for real Qdrant contract tests")
        client = Client.qdrant(url, namespace="test_" + uuid.uuid4().hex[:16], api_key=os.getenv("RECERN_TEST_QDRANT_API_KEY"))
        try:
            yield client
        finally:
            for name in client.list_collections():
                client.drop_collection(name)


@pytest.mark.parametrize("metric", ["cosine", "l2", "dot"])
def test_shared_filter_and_record_contract(client, metric):
    collection = client.create_collection("collection/世界", 2, metric)
    records = [Record(**r) for r in FIXTURE["records"]]
    assert collection.upsert_many(records) == len(records)
    assert client.list_collections() == [collection.name]
    assert collection.count() == len(records)
    for case in FIXTURE["filters"]:
        hits = collection.search([0.6, 0.8], k=20, filter=case["filter"], exact=True)
        assert {h.id for h in hits} == set(case["ids"]), case
        for hit in hits:
            expected = next(r for r in records if r.id == hit.id)
            assert hit.metadata == expected.metadata
            vector = expected.vector
            if metric == "cosine":
                norm = math.sqrt(sum(x*x for x in vector))
                distance = max(0, 1 - sum(x/norm*y for x,y in zip(vector,[.6,.8])))
            elif metric == "l2":
                distance = sum((x-y)**2 for x,y in zip(vector,[.6,.8]))
            else:
                distance = -sum(x*y for x,y in zip(vector,[.6,.8]))
            assert hit.distance == pytest.approx(distance, abs=2e-6)
    for invalid in FIXTURE["invalid_filters"]:
        with pytest.raises(InvalidArgument):
            collection.search([1,0], filter=invalid)
    assert {r.id for r in collection.iter_records(batch_size=2)} == {r.id for r in records}
    assert list(client.collection(collection.name).iter_records(batch_size=1))
    for record in records:
        assert collection.get(record.id).metadata == record.metadata
    collection.upsert(records[0].id, [1,1], {"updated": True})
    assert collection.count() == len(records)
    assert collection.get(records[0].id).metadata == {"updated": True}
    assert collection.delete(records[0].id)
    assert not collection.delete(records[0].id)
    assert collection.get(records[0].id) is None
    assert collection.count() == len(records) - 1
    assert len(list(collection.iter_records(batch_size=1))) == len(records) - 1


def test_validation_is_before_writes(client):
    collection = client.create_collection("docs", 2)
    for record in [Record("bad", [1]), Record("bad", [0,0]), Record("bad", [float('nan'),1]),
                   Record("bad", [True,1]), Record("bad", [1,0], {"bad": float('inf')}),
                   Record("bad", [1,0], {"bad": 2**65}), Record(1, [1,0])]:
        with pytest.raises(InvalidArgument):
            collection.upsert_many([Record("valid", [1,0]), record])
        assert collection.count() == 0
    with pytest.raises(InvalidArgument):
        collection.upsert_many([Record("same", [1,0]), Record("same", [0,1])])
    with pytest.raises(AlreadyExists):
        client.create_collection("docs", 2)
    with pytest.raises(NotFound):
        client.collection("absent")
    with pytest.raises(InvalidArgument):
        collection.page(limit=0)
    with pytest.raises(InvalidArgument):
        collection.page(cursor="invalid!")
    with pytest.raises(InvalidArgument):
        collection.search([1,0], k=0)
    assert collection.count() == 0


def test_json_scalar_reserved_and_numeric_metadata(client):
    c = client.create_collection("types", 1, "dot")
    values = [None, False, True, 1, 1.0, -0.0, "hello", [1, None, True],
              {"_recern": "mine", "id": "mine", "big": 2**64-1, "negative": -(2**63)}]
    for i,value in enumerate(values):
        c.upsert(str(i), [1], value)
        assert json.dumps(c.get(str(i)).metadata, sort_keys=True) == json.dumps(value, sort_keys=True)


def test_page_cannot_be_used_on_another_collection(client):
    first = client.create_collection("first", 1, "l2")
    first.upsert_many([Record(str(i), [i]) for i in range(5)])
    second = client.create_collection("second", 1, "l2")
    cursor = first.page(limit=2).next_cursor
    assert cursor is not None
    with pytest.raises(InvalidArgument):
        second.page(cursor=cursor)


def test_embedded_durability_snapshot_and_stale_cursor(tmp_path):
    path = tmp_path / "db.rvec"
    client = Client.embedded(path)
    c = client.create_collection("docs", 2)
    c.upsert_many([Record(str(i), [1,i]) for i in range(4)])
    snapshot = client.snapshot()
    cursor = c.page(limit=2).next_cursor
    c.upsert("new", [1,1])
    with pytest.raises(InvalidArgument):
        c.page(cursor=cursor)
    assert snapshot.collection("docs").count() == 4
    assert Client.embedded(path, read_only=True).collection("docs").count() == 5
    with pytest.raises(PermissionDenied):
        snapshot.collection("docs").upsert("no", [1,1])
    stale = Client.embedded(path)
    c.upsert("latest", [1,1])
    with pytest.raises(Conflict):
        stale.collection("docs").upsert("stale", [1,1])
    assert stale.collection("docs").get("stale") is None


def _archive(tmp_path, *, quantization="f32"):
    source = Client.embedded(tmp_path / "source.rvec")
    c = source.create_collection("migrate", 2, quantization=quantization)
    c.upsert_many([Record(**r) for r in FIXTURE["records"]])
    source.create_collection("empty", 2, "l2")
    archive = tmp_path / "export"
    manifest = export_snapshot(source, archive, batch_size=2)
    return archive, manifest


def test_migration_dry_run_resume_and_verify(client, tmp_path):
    archive, manifest = _archive(tmp_path)
    state = tmp_path / "progress.json"
    assert inspect_export(archive)["records"] == 5
    assert import_snapshot(archive, client, dry_run=True, state_path=state)["records"] == 5
    assert not state.exists()
    assert client.list_collections() == []
    def interrupt(name, done, total):
        if done == 2:
            raise RuntimeError("simulate interruption")
    with pytest.raises(RuntimeError, match="interruption"):
        import_snapshot(archive, client, state_path=state, batch_size=2, progress=interrupt)
    assert client.collection("migrate").count() == 2
    result = import_snapshot(archive, client, state_path=state, batch_size=2, resume=True)
    assert result["verified"] and result["records"] == 5
    assert client.collection("migrate").count() == 5
    assert client.collection("empty").count() == 0
    assert import_snapshot(archive, client, state_path=state, resume=True)["verified"]
    with pytest.raises(Conflict):
        import_snapshot(archive, client, state_path=state)
    with pytest.raises(AlreadyExists):
        import_snapshot(archive, client, state_path=tmp_path / "different.json")


def test_migration_replays_after_ack_before_journal(client, tmp_path, monkeypatch):
    archive, _ = _archive(tmp_path)
    state = tmp_path / "progress.json"
    original = client._backend.upsert
    def lost_reply(name, records):
        original(name, records)
        raise Unavailable("connection lost after commit")
    monkeypatch.setattr(client._backend, "upsert", lost_reply)
    with pytest.raises(Unavailable):
        import_snapshot(archive, client, state_path=state, batch_size=2)
    monkeypatch.setattr(client._backend, "upsert", original)
    assert import_snapshot(archive, client, state_path=state, batch_size=2, resume=True)["verified"]
    assert client.collection("migrate").count() == 5


def test_migration_int8_discloses_precision(client, tmp_path):
    archive, manifest = _archive(tmp_path, quantization="int8")
    entry = next(e for e in manifest["collections"] if e["name"] == "migrate")
    assert entry["source_precision"] == "int8" and entry["vectors"] == "stored-dequantized"
    assert import_snapshot(archive, client)["verified"]
    assert client.collection("migrate").spec.quantization == "f32"


def test_bad_archive_writes_nothing(client, tmp_path):
    archive, manifest = _archive(tmp_path)
    file = archive / manifest["collections"][0]["file"]
    file.write_bytes(file.read_bytes() + b'{}\n')
    with pytest.raises(CorruptData):
        import_snapshot(archive, client)
    assert client.list_collections() == []


def test_resume_detects_changed_destination(client, tmp_path):
    archive, _ = _archive(tmp_path)
    state = tmp_path / "progress.json"
    import_snapshot(archive, client, state_path=state)
    client.collection("migrate").upsert("doc/東京", [1,0], {"changed": True})
    with pytest.raises(Conflict):
        import_snapshot(archive, client, state_path=state, resume=True)


def test_resume_rejects_corrupt_creation_intent_before_writes(tmp_path, monkeypatch):
    archive, _ = _archive(tmp_path)
    client = Client.embedded(tmp_path / "target.rvec")
    state = tmp_path / "journal.json"
    original = client._backend.create
    def offline(*args, **kwargs):
        raise Unavailable("unavailable before create")
    monkeypatch.setattr(client._backend, "create", offline)
    with pytest.raises(Unavailable):
        import_snapshot(archive, client, state_path=state)
    damaged = json.loads(state.read_text(encoding="utf-8"))
    damaged["collections"]["empty"]["intent"] = "not-a-UUID"
    state.write_text(json.dumps(damaged), encoding="utf-8")
    monkeypatch.setattr(client._backend, "create", original)
    with pytest.raises(Conflict):
        import_snapshot(archive, client, state_path=state, resume=True)
    assert client.list_collections() == []


def test_qdrant_capabilities_and_collision(client, tmp_path, monkeypatch):
    if client.capabilities.backend != "qdrant":
        pytest.skip("Qdrant-specific guarantee")
    assert not client.capabilities.atomic_batches
    with pytest.raises(Unsupported):
        export_snapshot(client, tmp_path / "not-created")
    assert not (tmp_path / "not-created").exists()
    with pytest.raises(Unsupported):
        client.create_collection("int8", 2, quantization="int8")
    from recern_vector.portable import qdrant
    c = client.create_collection("collision", 2)
    fixed = qdrant.point_id("first")
    monkeypatch.setattr(qdrant, "point_id", lambda _: fixed)
    c.upsert("first", [1,0], "preserved")
    with pytest.raises(Conflict):
        c.upsert("second", [0,1])
    assert c.get("first").metadata == "preserved"


def test_migration_reopens_client_after_lost_create_reply(client, tmp_path, monkeypatch):
    archive, _ = _archive(tmp_path)
    state = tmp_path / "progress.json"
    backend = client._backend
    original = backend.create
    def lost_reply(spec, generation=None):
        original(spec, generation=generation)
        raise Unavailable("create committed but reply was lost")
    monkeypatch.setattr(backend, "create", lost_reply)
    with pytest.raises(Unavailable):
        import_snapshot(archive, client, state_path=state)
    if client.capabilities.backend == "embedded":
        reopened = Client.embedded(backend.db.path)
    else:
        reopened = Client.qdrant(backend.url, namespace=backend.namespace,
                                 api_key=os.getenv("RECERN_TEST_QDRANT_API_KEY"))
    assert reopened.identity == client.identity
    assert import_snapshot(archive, reopened, state_path=state, resume=True)["verified"]


@pytest.mark.parametrize("damage", ["duplicate", "unsafe_filename", "future_format", "checksum"])
def test_archive_preflight_rejects_damage_before_mutating(tmp_path, damage):
    archive, manifest = _archive(tmp_path)
    entry = next(e for e in manifest["collections"] if e["name"] == "migrate")
    path = archive / entry["file"]
    expected_error = CorruptData
    if damage == "duplicate":
        raw = path.read_bytes()
        raw += raw.splitlines(keepends=True)[0]
        path.write_bytes(raw)
        entry["records"] += 1
        entry["sha256"] = hashlib.sha256(raw).hexdigest()
    elif damage == "unsafe_filename":
        entry["file"] = "../source.rvec"
    elif damage == "future_format":
        manifest["version"] = 999
        expected_error = Unsupported
    else:
        entry["sha256"] = "0" * 64
    (archive / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
    target = Client.embedded(tmp_path / "target.rvec")
    state = tmp_path / "journal.json"
    with pytest.raises(expected_error):
        import_snapshot(archive, target, state_path=state)
    assert target.list_collections() == []
    assert not state.exists()


def test_export_of_snapshot_stays_frozen(tmp_path):
    source = Client.embedded(tmp_path / "db.rvec")
    c = source.create_collection("items", 1, "l2")
    c.upsert("before", [1])
    frozen = source.snapshot()
    c.upsert("after", [2])
    manifest = export_snapshot(frozen, tmp_path / "frozen")
    assert manifest["collections"][0]["records"] == 1
    assert inspect_export(tmp_path / "frozen")["records"] == 1


def test_resume_rejects_replaced_database(tmp_path):
    archive, _ = _archive(tmp_path)
    path = tmp_path / "destination.rvec"
    state = tmp_path / "progress.json"
    target = Client.embedded(path)
    import_snapshot(archive, target, state_path=state)
    # A new database at the same path is not the original import destination.
    path.unlink()
    for suffix in [".wal", ".lock"]:
        Path(str(path) + suffix).unlink(missing_ok=True)
    replacement = Client.embedded(path)
    assert replacement.identity != target.identity
    with pytest.raises(Conflict):
        import_snapshot(archive, replacement, state_path=state, resume=True)
    assert replacement.list_collections() == []


def test_qdrant_authentication():
    url, key = os.getenv("RECERN_TEST_QDRANT_URL"), os.getenv("RECERN_TEST_QDRANT_API_KEY")
    if not url or not key:
        pytest.skip("requires an authenticated real Qdrant")
    with pytest.raises(PermissionDenied):
        Client.qdrant(url, api_key="wrong-key").list_collections()


def test_foreign_qdrant_data_is_not_adopted(client):
    if client.capabilities.backend != "qdrant":
        pytest.skip("Qdrant-specific envelope")
    from recern_vector.portable.qdrant import point_id
    c = client.create_collection("foreign", 2)
    b = client._backend
    b._request("PUT", b._route(c.name) + "/points?wait=true", {
        "points": [{"id": point_id("logical"), "vector": [1,0], "payload": {"raw": True}}]})
    with pytest.raises(CorruptData):
        c.upsert("logical", [0,1])
    with pytest.raises(CorruptData):
        c.get("logical")
    assert c.count() == 1


def test_qdrant_namespace_prefixes_do_not_overlap(client):
    if client.capabilities.backend != "qdrant":
        pytest.skip("Qdrant-specific namespaces")
    backend = client._backend
    child = Client.qdrant(backend.url, namespace=backend.namespace + "_child",
                          api_key=os.getenv("RECERN_TEST_QDRANT_API_KEY"))
    try:
        client.create_collection("parent", 2)
        child.create_collection("child", 2)
        assert client.list_collections() == ["parent"]
        assert child.list_collections() == ["child"]
        with pytest.raises(NotFound):
            client.collection("child")
    finally:
        for name in child.list_collections():
            child.drop_collection(name)


def test_frozen_export_v1_round_trip(client, tmp_path):
    archive = Path(__file__).parent / "fixtures" / "export-v1"
    assert inspect_export(archive)["records"] == 2
    result = import_snapshot(archive, client, state_path=tmp_path / "v1.json")
    assert result["verified"]
    assert client.collection("frozen").get("01").metadata == {"tag": "original", "nullable": None}
    assert client.collection("frozen").get("東京").vector == [0,1]


@pytest.mark.parametrize("filename", ["v1-0.0.2.rvec", "v2-0.2.0.rvec"])
def test_migrate_frozen_native_formats_without_changing_source(client, tmp_path, filename):
    source_path = Path(__file__).resolve().parents[2] / "recern-vector/tests/fixtures" / filename
    before = source_path.read_bytes()
    source = Client.embedded(source_path, read_only=True)
    archive = tmp_path / "legacy-export"
    manifest = export_snapshot(source, archive)
    assert source_path.read_bytes() == before
    expected_count = sum(e["records"] for e in manifest["collections"])
    assert import_snapshot(archive, client)["records"] == expected_count
    assert client.list_collections() == source.list_collections()


def test_approximate_search_quality_and_filter_contract(client):
    rng = random.Random(307)
    records = [Record(f"r{i}", [rng.gauss(0, 1) for _ in range(16)], {"group": i % 3}) for i in range(512)]
    c = client.create_collection("ann", 16)
    for start in range(0, len(records), 256):
        c.upsert_many(records[start:start + 256])
    matched, total = 0, 0
    for r in records[::47]:
        predicate = {"$or": [{"group": 0}, {"group": 1}], "$not": {"group": 2}}
        reference = c.search(r.vector, k=5, filter=predicate, exact=True)
        approximate = c.search(r.vector, k=5, filter=predicate)
        assert len(approximate) == 5
        assert all(h.metadata["group"] in (0, 1) for h in approximate)
        assert all(math.isfinite(h.distance) for h in approximate)
        matched += len({h.id for h in reference} & {h.id for h in approximate})
        total += len(reference)
    # A regression check on this seeded dataset, not a workload benchmark.
    assert matched / total >= 0.9
