"""Version-pinned ANN release measurement, separate from the historical report.

See bench/releases/0.4.0/README.md for the environment and commands.
No existing benchmark result or index is overwritten.
"""

import argparse
import gc
import hashlib
import importlib.metadata
import json
import platform
import subprocess
import time
from datetime import datetime, timezone
from pathlib import Path

import faiss
import h5py
import numpy as np
import recern_vector as rv

VERSION = "0.4.0"
K = 10
EF = [10, 20, 40, 80, 160, 320, 640, 1280]
DATASETS = {
    "sift": ("sift-128-euclidean.hdf5", "SIFT1M", "l2"),
    "glove": ("glove-100-angular.hdf5", "GloVe-100", "cosine"),
}


def sha256(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def sysctl(name):
    result = subprocess.run(["sysctl", "-n", name], capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(f"Cannot read hardware metadata: {name}")
    return result.stdout.strip()


def measure(search, queries, truth, repeats):
    results = []
    for repeat in range(repeats):
        for query in queries[:100]:
            search(query)
        order = np.random.default_rng(20261102 + repeat).permutation(len(queries))
        elapsed, recalls = [], []
        for i in order:
            start = time.perf_counter_ns()
            found = search(queries[i])
            elapsed.append((time.perf_counter_ns() - start) / 1e6)
            recalls.append(len(set(found) & set(truth[i].tolist())) / K)
        results.append({
            "recall": float(np.mean(recalls)),
            "qps": float(len(elapsed) * 1000 / sum(elapsed)),
            "p50_ms": float(np.percentile(elapsed, 50)),
            "p95_ms": float(np.percentile(elapsed, 95)),
        })
    return {
        "recall": float(np.mean([r["recall"] for r in results])),
        "qps": float(np.median([r["qps"] for r in results])),
        "p50_ms": float(np.median([r["p50_ms"] for r in results])),
        "p95_ms": float(np.median([r["p95_ms"] for r in results])),
        "qps_min": min(r["qps"] for r in results),
        "qps_max": max(r["qps"] for r in results),
        "repetitions": results,
    }


def recern(train, queries, truth, metric, work, threads, repeats, precision):
    path = work / f"recern-{precision}.rvec"
    if path.exists():
        raise FileExistsError(f"Choose a fresh work directory: {path}")
    db = rv.Database.create(path)
    collection = db.create_collection("bench", dim=train.shape[1], metric=metric,
                                      m=16, ef_construction=200, quantization=precision)
    ids = [str(i) for i in range(len(train))]
    start = time.perf_counter()
    collection.upsert_many(ids, train, threads=threads)
    build = time.perf_counter() - start
    start = time.perf_counter()
    db.save()
    save = time.perf_counter() - start
    start = time.perf_counter()
    db.checkpoint()
    checkpoint = time.perf_counter() - start
    del ids, collection, db
    gc.collect()
    start = time.perf_counter()
    db = rv.Database.open_read_only(path)
    collection = db["bench"]
    opened = time.perf_counter() - start
    runs = []
    for ef in EF:
        result = measure(lambda q: [int(h.id) for h in collection.search(q, k=K, ef=ef)],
                         queries, truth, repeats)
        runs.append({"ef": ef, **result})
        print(f"recern-{precision} ef={ef}: recall={result['recall']:.4f}, qps={result['qps']:.0f}", flush=True)
    size = path.stat().st_size + sum(p.stat().st_size for p in [Path(str(path) + ".wal")] if p.exists())
    return {"key": f"recern-{precision}", "name": f"Recern Vector {VERSION} · {precision}",
            "version": VERSION, "precision": precision, "build_seconds": build,
            "save_seconds": save, "checkpoint_seconds": checkpoint, "open_seconds": opened,
            "disk_bytes": size, "runs": runs}


def reference(train, queries, truth, metric, work, threads, repeats):
    data = train.copy() if metric == "cosine" else train
    if metric == "cosine": faiss.normalize_L2(data)
    index = faiss.IndexHNSWFlat(train.shape[1], 16,
                               faiss.METRIC_INNER_PRODUCT if metric == "cosine" else faiss.METRIC_L2)
    index.hnsw.efConstruction = 200
    faiss.omp_set_num_threads(threads)
    start = time.perf_counter()
    index.add(data)
    build = time.perf_counter() - start
    faiss.omp_set_num_threads(1)
    path = work / "faiss-hnsw.index"
    start = time.perf_counter()
    faiss.write_index(index, str(path))
    save = time.perf_counter() - start
    del index, data
    gc.collect()
    start = time.perf_counter()
    index = faiss.read_index(str(path))
    opened = time.perf_counter() - start
    def search(query):
        q = query.reshape(1, -1).copy() if metric == "cosine" else query.reshape(1, -1)
        if metric == "cosine": faiss.normalize_L2(q)
        return index.search(q, K)[1][0].tolist()
    runs = []
    for ef in EF:
        index.hnsw.efSearch = ef
        result = measure(search, queries, truth, repeats)
        runs.append({"ef": ef, **result})
        print(f"faiss-hnsw ef={ef}: recall={result['recall']:.4f}, qps={result['qps']:.0f}", flush=True)
    return {"key": "faiss-hnsw", "name": "faiss HNSW · f32", "precision": "f32",
            "version": importlib.metadata.version("faiss-cpu"), "build_seconds": build,
            "save_seconds": save, "open_seconds": opened, "disk_bytes": path.stat().st_size,
            "runs": runs}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dataset", choices=DATASETS)
    parser.add_argument("--data-dir", type=Path, default=Path(__file__).resolve().parents[1] / "bench-data")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--queries", type=int, default=1000)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--build-threads", type=int, default=4)
    parser.add_argument("--subset", type=int)
    args = parser.parse_args()
    if importlib.metadata.version("recern-vector") != VERSION:
        raise RuntimeError(f"Use released recern-vector=={VERSION}")
    if min(args.queries, args.repeats, args.build_threads) < 1:
        parser.error("queries, repeats and build-threads must be positive")
    if args.output.exists(): raise FileExistsError(args.output)
    args.work.mkdir(parents=True, exist_ok=False)
    filename, label, metric = DATASETS[args.dataset]
    source = args.data_dir / filename
    with h5py.File(source) as f:
        train = np.asarray(f["train"][:args.subset] if args.subset else f["train"], dtype=np.float32)
        queries = np.asarray(f["test"][:args.queries], dtype=np.float32)
        truth = np.asarray(f["neighbors"][:args.queries, :K])
    if len(queries) != args.queries: raise ValueError("Not enough queries in dataset")
    if args.subset:
        if args.subset < K: raise ValueError("subset must contain at least k records")
        exact_data = train.copy()
        exact_queries = queries.copy()
        if metric == "cosine":
            faiss.normalize_L2(exact_data); faiss.normalize_L2(exact_queries)
        flat = faiss.IndexFlatIP(train.shape[1]) if metric == "cosine" else faiss.IndexFlatL2(train.shape[1])
        flat.add(exact_data)
        truth = flat.search(exact_queries, K)[1]
        del flat, exact_data, exact_queries
    package_dir = Path(rv.__file__).resolve().parent
    report = {
        "schema": 1, "release": VERSION, "date": datetime.now(timezone.utc).date().isoformat(),
        "scope": "smoke subset" if args.subset else "full published dataset",
        "dataset": {"key": args.dataset, "label": label, "file": filename,
                    "sha256": sha256(source), "records": len(train), "dim": train.shape[1], "metric": metric},
        "method": {"k": K, "queries": len(queries), "repeats": args.repeats,
                   "warmup_queries_per_repeat": min(100, len(queries)), "build_threads": args.build_threads,
                   "search_threads": 1, "m": 16, "ef_construction": 200, "ef_sweep": EF,
                   "truth": "exact on subset" if args.subset else "published ANN-Benchmarks neighbors",
                   "latency": "Python call through returned IDs; recall calculation excluded",
                   "qps_aggregation": "median of repeated single-query loops; no concurrent clients",
                   "build_repetitions": 1},
        "machine": {"cpu": sysctl("machdep.cpu.brand_string"), "memory_gb": round(int(sysctl("hw.memsize")) / 2**30),
                    "physical_cores": int(sysctl("hw.physicalcpu")), "logical_cores": int(sysctl("hw.logicalcpu")),
                    "os": f"macOS {platform.mac_ver()[0]}", "architecture": platform.machine(), "python": platform.python_version()},
        "packages": {name: importlib.metadata.version(name) for name in ["recern-vector", "faiss-cpu", "numpy", "h5py"]},
        "recern_binaries": {p.name: sha256(p) for p in package_dir.glob("*.so")},
        "engines": [],
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    for precision in ["f32", "int8"]:
        report["engines"].append(recern(train, queries, truth, metric, args.work, args.build_threads, args.repeats, precision))
        args.output.write_text(json.dumps(report, indent=2) + "\n")
        gc.collect()
    report["engines"].append(reference(train, queries, truth, metric, args.work, args.build_threads, args.repeats))
    report["complete"] = True
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Complete: {args.output}", flush=True)


if __name__ == "__main__": main()
