"""Look inside a collection: how a search ran, the state of the index, and
which ef gives the recall you need.

    pip install recern-vector numpy
    python inspect_and_tune.py
"""

import tempfile
from pathlib import Path

import numpy as np
import recern_vector as rv

rng = np.random.default_rng(11)
db = rv.Database.create(Path(tempfile.mkdtemp()) / "tune.rvec")
docs = db.create_collection("docs", dim=96, metric="cosine")

# Loosely clustered random data: hard enough that recall depends on ef.
centers = rng.standard_normal((50, 96)).astype(np.float32)
labels = rng.integers(0, 50, size=20_000)
vectors = centers[labels] + 2.0 * rng.standard_normal((20_000, 96)).astype(np.float32)
metadatas = [{"tenant": f"t{i % 200}", "kind": "faq" if i % 10 == 0 else "doc"} for i in range(20_000)]
docs.upsert_many([f"d{i}" for i in range(20_000)], vectors, metadatas)
query = vectors[123]

# 1. explain(): the same results as search(), plus how they were found.
report = docs.explain(query, k=10)
print(f"plain search    strategy={report.strategy:<15} ef={report.ef} "
      f"visited={report.visited:>5} time={report.elapsed_ms:.3f} ms")

# A broad filter (10% of records) is applied while walking the graph...
report = docs.explain(query, k=10, filter={"kind": "faq"})
print(f"kind=faq        strategy={report.strategy:<15} selectivity≈{report.filter_selectivity:.1%} "
      f"visited={report.visited:>5}")

# ...while a very selective one (0.5%) switches to an exact scan of the
# matching records, which is both faster and exact.
report = docs.explain(query, k=10, filter={"tenant": "t7"})
print(f"tenant=t7       strategy={report.strategy:<15} selectivity≈{report.filter_selectivity:.1%} "
      f"visited={report.visited:>5}")

# 2. stats(): the shape and health of the index.
stats = docs.stats()
print(f"\nlive={stats['live']} deleted={stats['deleted']} layers={stats['nodes_per_layer']}")
print(f"avg degree on layer 0={stats['avg_degree_layer0']:.1f}, unreachable={stats['unreachable']}")
mb = (stats["vector_bytes"] + stats["graph_bytes"] + stats["metadata_bytes"]) / 2**20
print(f"memory ≈ {mb:.1f} MB")

# 3. estimate_recall(): compare the index with exact search on a sample of
# stored vectors, for several ef values, and pick the smallest ef that
# reaches your target.
recall = docs.estimate_recall(sample=200, k=10, ef_values=[16, 32, 64, 128, 256])
print(f"\nrecall@{recall.k} over {recall.sample} queries (exact search p50 {recall.exact_p50_ms:.3f} ms)")
for point in recall.points:
    print(f"  ef={point.ef:<4} recall={point.recall:.3f}  p50={point.p50_ms:.3f} ms")
target = 0.99
best = next((p for p in recall.points if p.recall >= target), recall.points[-1])
print(f"smallest ef with recall ≥ {target}: {best.ef}  ->  docs.search(query, ef={best.ef})")

# 4. compact(): deleted and replaced records keep their space (and stay in
# the graph as waypoints) until the collection is rebuilt.
for i in range(0, 20_000, 4):
    docs.delete(f"d{i}")
print(f"\nafter deleting 25%: live={docs.stats()['live']} deleted={docs.stats()['deleted']}")
docs.compact()
print(f"after compact():    live={docs.stats()['live']} deleted={docs.stats()['deleted']}")
