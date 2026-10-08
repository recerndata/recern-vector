"""Semantic search over a handful of notes with sentence-transformers.

    pip install recern-vector sentence-transformers
    python semantic_search.py "how do I make my queries faster?"

The first run downloads the all-MiniLM-L6-v2 model (about 90 MB).
"""

import sys
import tempfile
from pathlib import Path

import recern_vector as rv
from sentence_transformers import SentenceTransformer

NOTES = [
    ("Add an index on the columns you filter by most often.", "postgres"),
    ("VACUUM reclaims space left by updated and deleted rows.", "postgres"),
    ("EXPLAIN ANALYZE shows the plan a query actually used and its timings.", "postgres"),
    ("Connection pooling keeps the number of open database connections low.", "postgres"),
    ("Set a TTL on cache keys so stale entries expire on their own.", "redis"),
    ("Use pipelining to send many Redis commands in one round trip.", "redis"),
    ("Sorted sets keep members ordered by score, which suits leaderboards.", "redis"),
    ("Watch memory usage: Redis keeps the whole dataset in RAM.", "redis"),
    ("Replicas let you spread read traffic across several servers.", "general"),
    ("Back up regularly and test that the backups actually restore.", "general"),
    ("Slow queries often come from scanning far more rows than they return.", "general"),
    ("Embeddings turn text into vectors so similar meanings end up close together.", "vectors"),
    ("HNSW trades a little recall for much faster nearest-neighbour search.", "vectors"),
    ("Cosine distance compares the direction of two vectors, not their length.", "vectors"),
]

model = SentenceTransformer("sentence-transformers/all-MiniLM-L6-v2")
texts = [text for text, _ in NOTES]
# normalize_embeddings is optional for cosine collections (Recern Vector
# normalizes on insert) but keeps the vectors ready for any metric.
embeddings = model.encode(texts, normalize_embeddings=True)  # float32, shape (n, 384)

db = rv.Database.create(Path(tempfile.mkdtemp()) / "notes.rvec")
notes = db.create_collection("notes", dim=embeddings.shape[1], metric="cosine")
notes.upsert_many(
    [f"note-{i}" for i in range(len(NOTES))],
    embeddings,
    [{"text": text, "topic": topic} for text, topic in NOTES],
)

question = sys.argv[1] if len(sys.argv) > 1 else "how do I make my queries faster?"
query = model.encode(question, normalize_embeddings=True)

print(f"Q: {question}\n")
for hit in notes.search(query, k=3):
    print(f"  {1 - hit.distance:.2f}  [{hit.metadata['topic']}] {hit.metadata['text']}")

print("\nOnly Redis notes:")
for hit in notes.search(query, k=2, filter={"topic": "redis"}):
    print(f"  {1 - hit.distance:.2f}  {hit.metadata['text']}")

db.save()
