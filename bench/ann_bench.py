"""Recall / latency benchmark on ANN-Benchmarks datasets.

Compares Recern Vector with LanceDB and sqlite-vec on the same data and
queries. Every engine answers top-10 queries one at a time from Python, and
recall@10 is measured against the ground-truth neighbors shipped with the
dataset.

    python bench/ann_bench.py sift  [--queries 1000] [--engines recern,lancedb,sqlite-vec]
    python bench/ann_bench.py glove

Datasets (HDF5 from http://ann-benchmarks.com) are expected in bench-data/.
Results are written to bench/results/<dataset>.json after each engine.
"""

import argparse
import json
import os
import platform
import shutil
import sqlite3
import subprocess
import time
from pathlib import Path

import h5py
import numpy as np

ROOT = Path(__file__).resolve().parent
DATA = ROOT.parent / "bench-data"
WORK = ROOT / "work"
RESULTS = ROOT / "results"
K = 10
EF_SWEEP = [10, 20, 40, 80, 160, 320, 640, 1280]

DATASETS = {
    "sift": ("sift-128-euclidean.hdf5", "l2"),
    "glove": ("glove-100-angular.hdf5", "cosine"),
}


def load(name):
    file, metric = DATASETS[name]
    with h5py.File(DATA / file) as f:
        train = np.asarray(f["train"], dtype=np.float32)
        test = np.asarray(f["test"], dtype=np.float32)
        truth = np.asarray(f["neighbors"][:, :K])
    return train, test, truth, metric


def exact_neighbors(train, queries, metric):
    if metric == "cosine":
        train = train / np.linalg.norm(train, axis=1, keepdims=True)
        queries = queries / np.linalg.norm(queries, axis=1, keepdims=True)
        dist = -queries @ train.T
    else:
        dist = (queries**2).sum(1)[:, None] - 2 * queries @ train.T + (train**2).sum(1)[None, :]
    return np.argsort(dist, axis=1)[:, :K]


def recall(found, truth):
    return len(set(found) & set(truth.tolist())) / K


def measure(search, queries, truth):
    """Runs `search(q) -> list[int]` for every query; returns recall and latency.
    An untimed warm-up pass over the first 100 queries comes first, so no
    configuration pays for cold caches."""
    for q in queries[:100]:
        search(q)
    times, recalls = [], []
    for q, t in zip(queries, truth):
        start = time.perf_counter()
        found = search(q)
        times.append(time.perf_counter() - start)
        recalls.append(recall(found, t))
    times_ms = np.array(times) * 1e3
    return {
        "recall": float(np.mean(recalls)),
        "p50_ms": float(np.percentile(times_ms, 50)),
        "p95_ms": float(np.percentile(times_ms, 95)),
        "qps": float(len(times) / np.sum(times)),
    }


def dir_size(path):
    path = Path(path)
    if path.is_file():
        return path.stat().st_size
    return sum(p.stat().st_size for p in path.rglob("*") if p.is_file())


def bench_recern(train, queries, truth, metric, work, previous=None):
    """With `previous` (an earlier result for the same file), the saved index
    is reused and its build/save timings are carried over."""
    import recern_vector as rv

    path = work / "recern.rvec"
    if previous and path.exists():
        build, save = previous["build_s"], previous["save_s"]
    else:
        path.unlink(missing_ok=True)
        db = rv.Database.create(path)
        c = db.create_collection("bench", dim=train.shape[1], metric=metric, m=16, ef_construction=200)
        ids = [str(i) for i in range(len(train))]
        start = time.perf_counter()
        c.upsert_many(ids, train, threads=os.cpu_count())
        build = time.perf_counter() - start
        start = time.perf_counter()
        db.save()
        save = time.perf_counter() - start
        del db, c
    start = time.perf_counter()
    c = rv.Database(path)["bench"]
    open_s = time.perf_counter() - start

    runs = []
    for ef in EF_SWEEP:
        result = measure(lambda q: [int(h.id) for h in c.search(q, k=K, ef=ef)], queries, truth)
        runs.append({"config": f"ef={ef}", **result})
        print(f"  recern ef={ef}: {result}")
    return {
        "engine": "Recern Vector",
        "index": "HNSW m=16 ef_construction=200",
        "build_s": build,
        "save_s": save,
        "open_s": open_s,
        "disk_bytes": dir_size(path),
        "threads": f"{os.cpu_count()} build, 1 search",
        "runs": runs,
    }


def bench_build_scaling(train, metric):
    """Index build time only: Recern on one thread and on all cores, faiss
    HNSW on all cores. No search sweep."""
    import faiss
    import recern_vector as rv

    ids = [str(i) for i in range(len(train))]
    times = {}
    for threads in (1, os.cpu_count()):
        (WORK / f"build-{threads}.rvec").unlink(missing_ok=True)
        db = rv.Database.create(WORK / f"build-{threads}.rvec")
        c = db.create_collection("bench", dim=train.shape[1], metric=metric, m=16, ef_construction=200)
        start = time.perf_counter()
        c.upsert_many(ids, train, threads=threads)
        times[f"recern_{threads}"] = time.perf_counter() - start
        print(f"  recern build, {threads} threads: {times[f'recern_{threads}']:.1f} s")
        del c, db
        (WORK / f"build-{threads}.rvec").unlink(missing_ok=True)

    faiss.omp_set_num_threads(os.cpu_count())
    data = train / np.linalg.norm(train, axis=1, keepdims=True) if metric == "cosine" else train
    kind = faiss.METRIC_INNER_PRODUCT if metric == "cosine" else faiss.METRIC_L2
    index = faiss.IndexHNSWFlat(train.shape[1], 16, kind)
    index.hnsw.efConstruction = 200
    start = time.perf_counter()
    index.add(np.ascontiguousarray(data))
    times[f"faiss_{os.cpu_count()}"] = time.perf_counter() - start
    print(f"  faiss build, {os.cpu_count()} threads: {times[f'faiss_{os.cpu_count()}']:.1f} s")
    return {"cores": os.cpu_count(), "seconds": times}


def lance_table(train, metric, work, name):
    import lancedb
    import pyarrow as pa

    path = work / f"lance-{name}"
    shutil.rmtree(path, ignore_errors=True)
    db = lancedb.connect(path)
    dim = train.shape[1]
    table = pa.table(
        {
            "id": pa.array(np.arange(len(train), dtype=np.int64)),
            "vector": pa.FixedSizeListArray.from_arrays(pa.array(train.reshape(-1)), dim),
        }
    )
    return db.create_table("bench", table), path


def bench_lancedb(train, queries, truth, metric, work, index_type, num_sub_vectors=None):
    from lancedb.index import HnswSq, IvfPq

    suffix = f"-sub{num_sub_vectors}" if num_sub_vectors else ""
    tbl, path = lance_table(train, metric, work, index_type.lower() + suffix)
    start = time.perf_counter()
    if index_type == "IVF_PQ":
        tbl.create_index(
            "vector", config=IvfPq(distance_type=metric, num_sub_vectors=num_sub_vectors)
        )
        sweep = [(20, 1), (50, 1), (20, 10), (50, 10), (100, 20), (100, 50), (200, 100)]
        configure = lambda b, p: b.nprobes(p[0]).refine_factor(p[1])
        label = lambda p: f"nprobes={p[0]} refine={p[1]}"
    else:
        tbl.create_index(
            "vector",
            config=HnswSq(distance_type=metric, m=16, ef_construction=200),
        )
        sweep = [(20, ef) for ef in [10, 20, 40, 80, 160, 320]] + [(50, 160), (100, 160), (100, 320)]
        configure = lambda b, p: b.nprobes(p[0]).ef(p[1])
        label = lambda p: f"nprobes={p[0]} ef={p[1]}"
    build = time.perf_counter() - start

    runs = []
    for params in sweep:
        def search(q):
            builder = tbl.search(q).distance_type(metric).limit(K).select(["id", "_distance"])
            return configure(builder, params).to_arrow()["id"].to_pylist()

        result = measure(search, queries, truth)
        runs.append({"config": label(params), **result})
        print(f"  lancedb {index_type} {label(params)}: {result}")
    return {
        "engine": "LanceDB",
        "index": index_type
        + (
            " m=16 ef_construction=200"
            if index_type != "IVF_PQ"
            else f" num_sub_vectors={num_sub_vectors}" if num_sub_vectors else " (defaults)"
        ),
        "build_s": build,
        "disk_bytes": dir_size(path),
        "threads": "multi-threaded build and search",
        "runs": runs,
    }


def bench_faiss(train, queries, truth, metric, work):
    """Reference HNSW implementation with the same parameters, one thread."""
    import faiss

    faiss.omp_set_num_threads(1)
    dim = train.shape[1]
    if metric == "cosine":
        train = train / np.linalg.norm(train, axis=1, keepdims=True)
        queries = queries / np.linalg.norm(queries, axis=1, keepdims=True)
        index = faiss.IndexHNSWFlat(dim, 16, faiss.METRIC_INNER_PRODUCT)
    else:
        index = faiss.IndexHNSWFlat(dim, 16)
    index.hnsw.efConstruction = 200
    start = time.perf_counter()
    index.add(np.ascontiguousarray(train))
    build = time.perf_counter() - start
    path = work / "faiss-hnsw.index"
    faiss.write_index(index, str(path))

    runs = []
    for ef in EF_SWEEP:
        index.hnsw.efSearch = ef

        def search(q):
            _, ids = index.search(q.reshape(1, -1), K)
            return ids[0].tolist()

        result = measure(search, queries, truth)
        runs.append({"config": f"ef={ef}", **result})
        print(f"  faiss ef={ef}: {result}")
    return {
        "engine": "faiss",
        "index": "HNSWFlat m=16 ef_construction=200",
        "build_s": build,
        "disk_bytes": dir_size(path),
        "threads": "1 (build and search)",
        "runs": runs,
    }


def bench_sqlite_vec(train, queries, truth, metric, work, max_queries=100):
    import sqlite_vec

    path = work / "sqlite-vec.db"
    path.unlink(missing_ok=True)
    conn = sqlite3.connect(path)
    conn.enable_load_extension(True)
    sqlite_vec.load(conn)
    dim = train.shape[1]
    distance = " distance_metric=cosine" if metric == "cosine" else ""
    conn.execute(f"create virtual table v using vec0(embedding float[{dim}]{distance})")
    start = time.perf_counter()
    with conn:
        conn.executemany(
            "insert into v(rowid, embedding) values (?, ?)",
            ((i, train[i].tobytes()) for i in range(len(train))),
        )
    build = time.perf_counter() - start

    def search(q):
        rows = conn.execute(
            "select rowid from v where embedding match ? and k = ?", (q.tobytes(), K)
        ).fetchall()
        return [r[0] for r in rows]

    result = measure(search, queries[:max_queries], truth[:max_queries])
    print(f"  sqlite-vec exact: {result}")
    conn.close()
    return {
        "engine": "sqlite-vec",
        "index": "vec0, exact scan (no ANN index)",
        "build_s": build,
        "disk_bytes": dir_size(path),
        "threads": "1",
        "queries": max_queries,
        "runs": [{"config": "exact", **result}],
    }


def machine():
    chip = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True)
    mem = subprocess.run(["sysctl", "-n", "hw.memsize"], capture_output=True, text=True)
    return {
        "cpu": chip.stdout.strip() or platform.processor(),
        "memory_gb": round(int(mem.stdout.strip() or 0) / 2**30),
        "os": f"macOS {platform.mac_ver()[0]}",
        "python": platform.python_version(),
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("dataset", choices=DATASETS)
    parser.add_argument("--queries", type=int, default=1000)
    parser.add_argument("--engines", default="recern,lancedb-hnsw,lancedb-pq,sqlite-vec")
    parser.add_argument(
        "--reuse", action="store_true", help="reuse a saved Recern index from an earlier run"
    )
    parser.add_argument(
        "--subset", type=int, help="smoke test: use the first N records and exact ground truth"
    )
    args = parser.parse_args()

    train, test, truth, metric = load(args.dataset)
    queries, truth = test[: args.queries], truth[: args.queries]
    if not args.subset:
        # Inputs for the Rust-only `search_bench` example.
        (WORK / args.dataset).mkdir(parents=True, exist_ok=True)
        queries.tofile(WORK / args.dataset / "queries.f32")
        truth.astype(np.uint32).tofile(WORK / args.dataset / "truth.u32")
    if args.subset:
        train = train[: args.subset]
        truth = exact_neighbors(train, queries, metric)
    work = WORK / (args.dataset + (f"-{args.subset}" if args.subset else ""))
    work.mkdir(parents=True, exist_ok=True)
    RESULTS.mkdir(exist_ok=True)
    out = RESULTS / f"{args.dataset}{f'-subset{args.subset}' if args.subset else ''}.json"
    report = json.loads(out.read_text()) if out.exists() else {}
    report.update(
        {
            "dataset": DATASETS[args.dataset][0],
            "records": len(train),
            "dim": train.shape[1],
            "metric": metric,
            "queries": len(queries),
            "k": K,
            "machine": machine(),
        }
    )
    report.setdefault("engines", {})
    print(f"{args.dataset}: {len(train):,} x {train.shape[1]} ({metric}), {len(queries)} queries")

    runners = {
        "recern": lambda: bench_recern(
            train, queries, truth, metric, work, report["engines"].get("recern") if args.reuse else None
        ),
        "faiss-hnsw": lambda: bench_faiss(train, queries, truth, metric, work),
        "build-scaling": lambda: bench_build_scaling(train, metric),
        "lancedb-pq-tuned": lambda: bench_lancedb(
            train, queries, truth, metric, work, "IVF_PQ", num_sub_vectors=train.shape[1] // 4
        ),
        "lancedb-hnsw": lambda: bench_lancedb(train, queries, truth, metric, work, "IVF_HNSW_SQ"),
        "lancedb-pq": lambda: bench_lancedb(train, queries, truth, metric, work, "IVF_PQ"),
        "sqlite-vec": lambda: bench_sqlite_vec(train, queries, truth, metric, work),
    }
    for engine in args.engines.split(","):
        print(f"[{engine}]")
        if engine == "build-scaling":
            report["build_scaling"] = runners[engine]()
        else:
            report["engines"][engine] = runners[engine]()
        out.write_text(json.dumps(report, indent=2))
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
