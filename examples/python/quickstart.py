"""Recern Vector in five minutes: create a database, add vectors with
metadata, search, filter, update, delete, save and reopen.

    pip install recern-vector numpy
    python quickstart.py
"""

import tempfile
from pathlib import Path

import numpy as np
import recern_vector as rv

rng = np.random.default_rng(7)
path = Path(tempfile.mkdtemp()) / "quickstart.rvec"

# A database is one file. Changes stay in memory until save() or a clean
# exit from the `with` block.
with rv.Database.open_or_create(path) as db:
    articles = db.create_collection("articles", dim=64, metric="cosine")

    # Batch insert: vectors as a float32 array, one metadata dict per record.
    # The index is built on all cores, and the batch is atomic: if one vector
    # is invalid, nothing is written.
    ids = [f"article-{i}" for i in range(5_000)]
    vectors = rng.standard_normal((5_000, 64)).astype(np.float32)
    metadatas = [{"lang": ["en", "de", "fr"][i % 3], "year": 2015 + i % 10} for i in range(5_000)]
    articles.upsert_many(ids, vectors, metadatas)
    print(f"{len(articles)} records in {articles.name!r}")

    # Nearest neighbours of a stored vector: the record itself comes first.
    for hit in articles.search(vectors[42], k=3):
        print(f"  {hit.id:<14} distance={hit.distance:.4f}  {hit.metadata}")

    # MongoDB-style metadata filter. Top-level keys are combined with AND.
    english_recent = articles.search(
        vectors[42], k=3, filter={"lang": "en", "year": {"$gte": 2020}}
    )
    print("filtered:", [(h.id, h.metadata["year"]) for h in english_recent])

    # Upsert replaces a record with the same id; get() returns it.
    articles.upsert("article-42", vectors[42], {"lang": "en", "year": 2025, "pinned": True})
    print("updated:", articles.get("article-42").metadata)

    articles.delete("article-7")
    print("article-7 still there?", "article-7" in articles)

# Reopen the file: everything was saved on exit.
db = rv.Database(path)
print(f"reopened {db.path.name}: collections={db.collection_names()}, records={len(db['articles'])}")
