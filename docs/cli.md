# Command line

```sh
cargo install recern-vector-cli
recern-vector --help
```

Every command takes the database file first. Commands that change the database save it before they exit.

## init

```sh
recern-vector init FILE
```

Creates an empty database. Fails if the file exists.

## create-collection

```sh
recern-vector create-collection FILE NAME --dim N [--metric cosine|l2|dot] [--m 16] [--ef-construction 200] [--ef-search 64]
```

See [Concepts](concepts.md#collections) for the settings.

## insert

```sh
recern-vector insert FILE COLLECTION INPUT.jsonl [--threads N]
```

Inserts or replaces records from a JSON Lines file, one record per line:

```json
{"id": "doc-1", "vector": [0.12, -0.4, 0.33], "metadata": {"lang": "en", "year": 2024}}
```

`metadata` is optional. The index is built on all cores unless `--threads` is given. An invalid line stops the import with its line number, and nothing from the file is written.

## query

```sh
recern-vector query FILE COLLECTION (--vector '[...]' | --like ID) [-k 10] [--ef N] [--exact] [--filter JSON] [--explain]
```

| Option | Meaning |
|---|---|
| `--vector '[0.1, 0.2, ...]'` | Query vector as a JSON array |
| `--like ID` | Use the vector of a stored record as the query |
| `-k`, `--k` | Number of results (default 10) |
| `--ef` | Candidate list size for this query |
| `--exact` | Scan every record instead of using the index |
| `--filter` | Metadata filter, see [Filters](filters.md) |
| `--explain` | Print the strategy, `ef`, filter selectivity, nodes visited, distance computations and time |

```text
     #  id                          distance  metadata
     1  lemon                        0.13155  {"color":"yellow","price":0.6}
     2  banana                       0.25315  {"color":"yellow","price":0.8}

  strategy      hnsw
  ef            64
  selectivity   ~50.0% of records match the filter
  visited       8 nodes
  distances     8 computed
  time          5 µs
```

## delete

```sh
recern-vector delete FILE COLLECTION ID [ID ...]
```

## inspect

```sh
recern-vector inspect FILE
```

For each collection: record counts, settings, nodes per layer, average degree, reachability and memory. Suggests `compact` when a tenth or more of the nodes are deleted.

## recall

```sh
recern-vector recall FILE COLLECTION [--sample 100] [-k 10] [--ef 16,32,64,128,256]
```

Measures recall@k of the index against exact search, with p50 and p95 latency for each `ef`. See [Inspecting and tuning](inspection.md#estimate_recall-and-choosing-ef).

## compact

```sh
recern-vector compact FILE COLLECTION
```

Rebuilds the collection without deleted records.

A runnable walkthrough: [`examples/cli/quickstart.sh`](../examples/cli/quickstart.sh).
