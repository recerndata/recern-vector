# Examples

Each example runs on its own and prints what it does.

| Example | What it shows | Needs |
|---|---|---|
| [`python/quickstart.py`](python/quickstart.py) | Create a database, batch insert with metadata, search, filter, update, delete, save and reopen | `numpy` |
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
