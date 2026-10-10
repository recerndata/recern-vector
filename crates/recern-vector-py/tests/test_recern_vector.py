import math

import numpy as np
import pytest

import recern_vector as rv


@pytest.fixture
def path(tmp_path):
    return tmp_path / "test.rvec"


def random_vectors(n, dim, seed=0):
    return np.random.default_rng(seed).standard_normal((n, dim)).astype(np.float32)


def test_round_trip_with_context_manager(path):
    vectors = random_vectors(300, 16)
    ids = [f"doc-{i}" for i in range(300)]
    metadatas = [{"i": i, "lang": "en" if i % 2 else "de", "tags": ["a", None, 1.5]} for i in range(300)]

    with rv.Database.create(path) as db:
        docs = db.create_collection("docs", dim=16, metric="l2")
        assert docs.upsert_many(ids, vectors, metadatas) == 300
        expected = [h.id for h in docs.search(vectors[7], k=5)]

    db = rv.Database(path)
    assert db.collection_names() == ["docs"]
    assert "docs" in db and "missing" not in db
    docs = db["docs"]
    assert (docs.name, docs.dim, docs.metric, len(docs)) == ("docs", 16, "l2", 300)
    assert [h.id for h in docs.search(vectors[7], k=5)] == expected
    assert expected[0] == "doc-7"

    record = docs.get("doc-3")
    assert record.metadata == {"i": 3, "lang": "en", "tags": ["a", None, 1.5]}
    assert np.allclose(record.vector, vectors[3])
    assert docs.get("nope") is None


def test_context_manager_does_not_save_on_error(path):
    rv.Database.create(path)
    with pytest.raises(RuntimeError):
        with rv.Database(path) as db:
            db.create_collection("c", dim=2)
            raise RuntimeError("boom")
    assert rv.Database(path).collection_names() == []


def test_accepts_lists_float64_and_float32(path):
    db = rv.Database.create(path)
    c = db.create_collection("c", dim=3, metric="cosine")
    c.upsert("list", [1.0, 0.0, 0.0])
    c.upsert("f64", np.array([0.0, 1.0, 0.0]))
    c.upsert("f32", np.array([0.0, 0.0, 2.0], dtype=np.float32), {"kind": "f32"})
    c.upsert_many(["a", "b"], [[1.0, 1.0, 0.0], (0.0, 1.0, 1.0)])

    hit = c.search(np.array([0, 0, 5], dtype=np.float32), k=1)[0]
    assert hit.id == "f32"
    assert hit.distance == pytest.approx(0.0, abs=1e-6)
    assert hit.metadata == {"kind": "f32"}
    assert math.isclose(sum(x * x for x in c.get("a").vector), 1.0, rel_tol=1e-6)
    assert "a" in c and len(c) == 5


def test_filters_and_explain(path):
    db = rv.Database.create(path)
    c = db.create_collection("c", dim=8)
    vectors = random_vectors(2000, 8, seed=1)
    c.upsert_many(
        [str(i) for i in range(2000)],
        vectors,
        [{"lang": ["en", "de", "fr"][i % 3], "year": 2015 + i % 10, "rare": i == 77} for i in range(2000)],
    )

    hits = c.search(vectors[0], k=10, filter={"lang": "de", "year": {"$gte": 2020}})
    assert len(hits) == 10
    assert all(h.metadata["lang"] == "de" and h.metadata["year"] >= 2020 for h in hits)

    report = c.explain(vectors[0], k=3, filter={"rare": True})
    assert report.strategy == "filtered_exact"
    assert [h.id for h in report.hits] == ["77"]

    report = c.explain(vectors[0], k=3, ef=100)
    assert report.strategy == "hnsw" and report.ef == 100
    assert report.visited > 0 and report.distance_computations > 0 and report.elapsed_ms >= 0

    assert c.explain(vectors[0], k=3, exact=True).strategy == "exact"


def test_stats_recall_delete_compact(path):
    db = rv.Database.create(path)
    c = db.create_collection("c", dim=16, m=8, ef_construction=100, ef_search=32)
    c.upsert_many([str(i) for i in range(1000)], random_vectors(1000, 16, seed=2))

    report = c.estimate_recall(sample=50, k=10, ef_values=[16, 128])
    assert (report.k, report.sample) == (10, 50)
    assert [p.ef for p in report.points] == [16, 128]
    assert report.points[1].recall >= 0.95

    assert c.delete("5") and not c.delete("5")
    stats = c.stats()
    assert stats["live"] == 999 and stats["deleted"] == 1
    assert stats["m"] == 8 and stats["ef_search"] == 32
    assert stats["unreachable"] == 0 and stats["nodes_per_layer"][0] == 1000
    assert c.compact() == 1
    assert c.stats()["deleted"] == 0


def test_errors(path):
    db = rv.Database.create(path)
    c = db.create_collection("c", dim=3)

    with pytest.raises(FileExistsError):
        rv.Database.create(path)
    with pytest.raises(ValueError, match="already exists"):
        db.create_collection("c", dim=3)
    with pytest.raises(ValueError, match="unknown metric"):
        db.create_collection("d", dim=3, metric="manhattan")
    with pytest.raises(KeyError):
        db["missing"]
    with pytest.raises(ValueError, match="3 dimensions"):
        c.upsert("x", [1.0, 2.0])
    with pytest.raises(ValueError, match="zero vector"):
        c.upsert("x", [0.0, 0.0, 0.0])
    with pytest.raises(ValueError, match="2 ids but 1 vectors"):
        c.upsert_many(["a", "b"], [[1.0, 2.0, 3.0]])
    with pytest.raises(ValueError, match="1-D"):
        c.search(np.ones((2, 3), dtype=np.float32))
    with pytest.raises(ValueError, match="unsupported filter operator"):
        c.search([1.0, 2.0, 3.0], filter={"a": {"$regex": "x"}})
    with pytest.raises(ValueError, match="metadata"):
        c.upsert("x", [1.0, 2.0, 3.0], {"bad": object()})

    db.drop_collection("c")
    with pytest.raises(KeyError):
        len(c)


def test_corrupt_file_raises_dedicated_error(path):
    rv.Database.create(path)
    data = bytearray(path.read_bytes())
    data[-1] ^= 0xFF
    path.write_bytes(bytes(data))
    with pytest.raises(rv.CorruptDatabaseError, match="checksum"):
        rv.Database(path)
    assert issubclass(rv.CorruptDatabaseError, rv.RecernVectorError)


def test_reprs(path):
    db = rv.Database.create(path)
    c = db.create_collection("c", dim=2)
    c.upsert("a", [1.0, 2.0])
    assert repr(c) == 'Collection(name="c", dim=2, metric="cosine", len=1)'
    assert repr(c.search([1.0, 2.0], k=1)[0]).startswith('Hit(id="a"')
    assert "collections=[\"c\"]" in repr(db)


def test_large_batch_matches_single_inserts(tmp_path):
    vectors = random_vectors(500, 32, seed=3)
    ids = [str(i) for i in range(500)]
    batch = rv.Database.create(tmp_path / "batch.rvec").create_collection("c", dim=32)
    single = rv.Database.create(tmp_path / "single.rvec").create_collection("c", dim=32)
    batch.upsert_many(ids, vectors)
    for i, v in zip(ids, vectors):
        single.upsert(i, v)
    query = random_vectors(1, 32, seed=4)[0]
    assert [h.id for h in batch.search(query)] == [h.id for h in single.search(query)]


def test_parallel_upsert_many(path):
    db = rv.Database.create(path)
    c = db.create_collection("c", dim=32, metric="l2")
    vectors = random_vectors(5000, 32, seed=5)
    assert c.upsert_many([str(i) for i in range(5000)], vectors, threads=4) == 5000
    stats = c.stats()
    assert stats["live"] == 5000 and stats["unreachable"] == 0
    assert c.estimate_recall(sample=100, ef_values=[128]).points[0].recall >= 0.95


def test_failed_batch_is_atomic(path):
    db = rv.Database.create(path)
    c = db.create_collection("c", dim=2)
    c.upsert("keep", [1.0, 0.0], {"v": 1})
    with pytest.raises(ValueError, match="zero vector"):
        c.upsert_many(["keep", "new", "bad"], [[0.0, 1.0], [1.0, 1.0], [0.0, 0.0]])
    assert len(c) == 1 and c.get("keep").metadata == {"v": 1}


def test_writes_wait_for_searches_in_other_threads(path):
    import threading
    from concurrent.futures import ThreadPoolExecutor

    rng = np.random.default_rng(3)
    vectors = rng.standard_normal((5_000, 32)).astype(np.float32)
    db = rv.Database.create(path)
    c = db.create_collection("c", dim=32)
    c.upsert_many([str(i) for i in range(5_000)], vectors)

    errors = []

    def writer():
        try:
            for i in range(300):
                c.upsert(f"w{i}", vectors[i])
                if i % 50 == 0:
                    c.delete(str(i))
        except BaseException as e:  # a panic would surface as BaseException
            errors.append(e)

    thread = threading.Thread(target=writer)
    with ThreadPoolExecutor(4) as pool:
        results = pool.map(lambda q: c.search(q, k=5, ef=128), vectors[:2_000])
        thread.start()
        assert all(len(hits) == 5 for hits in results)
        thread.join()

    assert errors == []
    assert len(c) == 5_000 + 300 - 6


def test_v2_int8_wal_logical_filters_and_read_only(tmp_path):
    import recern_vector as rv
    path = tmp_path / "v2.rvec"
    db = rv.Database.create(path)
    c = db.create_collection("q", 3, quantization="int8")
    c.upsert("a", [1, 0, 0], {"lang": "en"})
    c.upsert("b", [0, 1, 0], {"lang": "de"})
    db.save()
    assert c.stats()["vector_bytes"] == 14
    assert c.stats()["quantization"] == "int8"
    assert c.search([1, 0, 0], filter={"$not": {"lang": "en"}})[0].id == "b"
    with rv.Database.open_read_only(path) as ro:
        assert ro.read_only
        assert len(ro["q"]) == 2
        with pytest.raises(ValueError, match="read-only"):
            ro.save()
    stale = rv.Database(path)
    c.delete("b")
    db.save()
    with pytest.raises(ValueError, match="another handle"):
        stale.save()
    db.checkpoint()
    assert len(rv.Database(path)["q"]) == 1
    assert rv.FORMAT_VERSION == 2
