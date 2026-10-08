"""Breaks down per-query search cost on a saved index (bench/work/<dataset>/recern.rvec)."""

import os
import sys
import time

import numpy as np

sys.path.insert(0, os.path.dirname(__file__))
from ann_bench import WORK, load  # noqa: E402

import recern_vector as rv  # noqa: E402

dataset = sys.argv[1] if len(sys.argv) > 1 else "sift"
with_faiss = "--faiss" in sys.argv
train, test, truth, metric = load(dataset)
queries = test[:1000]
c = rv.Database(WORK / dataset / "recern.rvec")["bench"]


def loop(fn, repeat=3):
    """Mean µs per query: best of `repeat` passes after a warm-up pass."""
    best = float("inf")
    for i in range(repeat + 1):
        start = time.perf_counter()
        for q in queries:
            fn(q)
        if i:
            best = min(best, (time.perf_counter() - start) / len(queries) * 1e6)
    return best


print(f"call overhead: c.dim {loop(lambda q: c.dim):.2f} µs")


for ef in (10, 80):
    total = loop(lambda q: c.search(q, k=10, ef=ef))
    k1 = loop(lambda q: c.search(q, k=1, ef=ef))
    reports = [c.explain(q, k=10, ef=ef) for q in queries]
    inner = np.mean([r.elapsed_ms for r in reports]) * 1e3
    dist = np.mean([r.distance_computations for r in reports])
    print(f"ef={ef:3}: search() {total:6.1f} µs (k=1: {k1:6.1f}) | inside Rust {inner:6.1f} µs | "
          f"Python layer {total - inner:5.1f} µs | distances {dist:7.0f}")

if with_faiss:
    import faiss

    faiss.omp_set_num_threads(os.cpu_count())
    data = train / np.linalg.norm(train, axis=1, keepdims=True) if metric == "cosine" else train
    kind = faiss.METRIC_INNER_PRODUCT if metric == "cosine" else faiss.METRIC_L2
    index = faiss.IndexHNSWFlat(train.shape[1], 16, kind)
    index.hnsw.efConstruction = 200
    index.add(np.ascontiguousarray(data))
    faiss.omp_set_num_threads(1)
    for ef in (10, 80):
        index.hnsw.efSearch = ef
        faiss.cvar.hnsw_stats.reset()
        total = loop(lambda q: index.search(q.reshape(1, -1), 10))
        print(f"faiss ef={ef:3}: search() {total:6.1f} µs | distances {faiss.cvar.hnsw_stats.ndis / len(queries):7.0f}")
