# Inspecting and tuning

Recern Vector exposes what the index is doing as part of the API. Three calls cover most questions:

| Question | Call |
|---|---|
| How did this search run? Why was it slow? | `explain(query, ...)` |
| What state is the index in? How much memory does it use? | `stats()` |
| How many of the true nearest neighbours do I get, and at what `ef`? | `estimate_recall(...)` |

A complete program: [`examples/python/inspect_and_tune.py`](../examples/python/inspect_and_tune.py).

## explain

`explain()` takes the same arguments as `search()` and returns the hits together with how they were found.

```python
report = collection.explain(query, k=10, filter={"kind": "faq"})
report.hits                  # same as search()
report.strategy              # "hnsw", "exact" or "filtered_exact"
report.ef                    # candidate list size used (None for exact strategies)
report.visited               # nodes the search looked at
report.distance_computations
report.filter_selectivity    # estimated share of records matching the filter, or None
report.elapsed_ms
```

- `visited` is the main cost driver. Compare it to the collection size: an unfiltered HNSW search on 20,000 records typically visits a few hundred to a few thousand nodes.
- A broad filter can make the graph search visit many more nodes, because most candidates are rejected. If `visited` approaches the collection size, a narrower filter or a separate collection per tenant may be faster.
- See [Filters](filters.md#how-filtered-searches-run) for how the strategy is chosen.

On the command line, add `--explain` to `query`.

## stats

```python
stats = collection.stats()
```

| Key | Meaning |
|---|---|
| `name`, `dim`, `metric`, `quantization`, `m`, `ef_construction`, `ef_search` | Collection settings |
| `live` | Records that searches can return |
| `deleted` | Deleted or replaced records still in the graph until `compact()` |
| `nodes_per_layer` | Nodes on each graph layer, bottom first. The bottom layer holds every node |
| `avg_degree_layer0` | Average links per node on the bottom layer (the maximum is `2 * m`) |
| `unreachable` | Live records that no search can reach from the entry point. Should be 0 |
| `vector_bytes`, `graph_bytes`, `metadata_bytes` | Approximate memory use |

`recern-vector inspect file.rvec` prints the same information for every collection and suggests `compact` when a tenth or more of the nodes are deleted.

## estimate_recall and choosing ef

`estimate_recall()` takes a sample of stored vectors as queries, runs exact search to get the true top-`k`, then runs the index at each `ef` and reports the share of true neighbours found (recall@k) and the latency.

```python
report = collection.estimate_recall(sample=200, k=10, ef_values=[16, 32, 64, 128, 256])
for p in report.points:
    print(p.ef, round(p.recall, 3), p.p50_ms)
```

```text
16   0.970  0.016
32   0.987  0.022
64   0.992  0.037
128  0.994  0.070
256  0.998  0.134
```

Pick the smallest `ef` that reaches your target, and pass it to `search(..., ef=...)` or create the collection with that `ef_search`. Recall depends on the data, so measure on your own vectors; random test data is usually harder than real embeddings.

The queries are stored vectors, which slightly favours the index compared with unseen queries. For a stricter check, run `search(q, exact=True)` against `search(q, ef=...)` for queries from your application.

On the command line: `recern-vector recall file.rvec collection --ef 16,32,64,128`.

## compact

Deleting or replacing a record hides it from results at once, but its node stays in the graph (searches pass through it) and keeps its memory. `compact()` rebuilds the collection from the live records, using all cores, and returns how many records it removed.

```python
removed = collection.compact()
```

Compact after large deletions or many replacements, then `save()` to commit. Use `checkpoint()` to also reclaim space in the snapshot and reset WAL.
