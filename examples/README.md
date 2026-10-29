# Examples

Each example runs on its own and prints what it does.

| Example | What it shows | Needs |
|---|---|---|
| [`python/quickstart.py`](python/quickstart.py) | Create a database, batch insert with metadata, search, filter, update, delete, save and reopen | `numpy` |
| [`python/portable.py`](python/portable.py) | Same data logic on embedded Recern, Qdrant and source-only Recern HTTP (`--remote`) | Published 0.3.0 for file/Qdrant; 0.4.0 source for Recern HTTP |
| [`portable.rs`](../crates/recern-vector-client/examples/portable.rs) | Portable Rust data logic on all three backends | 0.4.0 source client crate |
| [`python/migrate.py`](python/migrate.py) | Snapshot export, checked import, dry-run and interruption recovery | recern-vector >= 0.3.0 |
| [`python/inspect_and_tune.py`](python/inspect_and_tune.py) | `explain()` for plain and filtered searches, `stats()`, choosing `ef` with `estimate_recall()`, `compact()` | `numpy` |
| [`python/semantic_search.py`](python/semantic_search.py) | Semantic search over short notes with real text embeddings and a metadata filter | `sentence-transformers` |
| [`python/rag_retrieval.py`](python/rag_retrieval.py) | The retrieval half of RAG: chunk documents, index them once, fetch context and build a prompt | `sentence-transformers` |
| [`cli/quickstart.sh`](cli/quickstart.sh) | The `recern-vector` command: init, insert JSON Lines, query with `--explain`, inspect, recall, delete, compact | `cargo install recern-vector-cli` |
| [`quickstart.rs`](../crates/recern-vector/examples/quickstart.rs) | The Rust API: batch insert, filters in JSON and in code, `explain`, `stats`, recall, save and reopen | — |

```sh
pip install recern-vector numpy
python examples/python/quickstart.py

pip install sentence-transformers          # downloads a ~90 MB model on first run
python examples/python/semantic_search.py "how do I make my queries faster?"

cargo run --release --example quickstart
```

The documentation lives in [`docs/`](../docs/README.md) and at [recern.net/vector/docs](https://recern.net/vector/docs).

For the portable examples, first install `recern-vector >= 0.3.0` (or build the bindings from source). Then:

```sh
python examples/python/portable.py --path /tmp/portable-demo.rvec
docker compose -f dev/compose.yml up -d
export RECERN_QDRANT_API_KEY=recern-vector-tests-local-only
python examples/python/portable.py --qdrant http://127.0.0.1:6335

python examples/python/migrate.py /tmp/portable-export --source /tmp/portable-demo.rvec
python examples/python/migrate.py /tmp/portable-export --qdrant http://127.0.0.1:6335 --namespace migrated --dry-run
python examples/python/migrate.py /tmp/portable-export --qdrant http://127.0.0.1:6335 --namespace migrated
# After interruption, repeat the last command with --resume.
docker compose -f dev/compose.yml down
```

Use fresh paths/namespaces for migration examples; existing destination collections are rejected. Read the [contract](../docs/portable-api.md) and [migration guarantees](../docs/migration.md) before a production cutover.
