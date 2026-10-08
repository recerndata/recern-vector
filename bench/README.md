# ANN benchmarks

Compares Recern Vector with faiss (reference HNSW), LanceDB and sqlite-vec on the standard [ANN-Benchmarks](http://ann-benchmarks.com) datasets. Results: [RESULTS.md](RESULTS.md). The public report page is built with `python3 bench/report_page.py` (template `report_template.html`, output `report.html`).

## Method

- **Datasets:** SIFT1M (1,000,000 × 128, L2) and GloVe-100 (1,183,514 × 100, angular → cosine), with their published ground-truth neighbors.
- **Queries:** the first 1,000 test queries, top-10, one query at a time from Python. sqlite-vec is measured on the first 100 because exact scans are slow.
- **Metric:** recall@10 against ground truth; p50/p95 latency and QPS of the single-query loop. Latency includes each library's Python overhead.
- **Engines and settings:**
  - Recern Vector: HNSW m=16, ef_construction=200, ef swept 10–1280; index built with `upsert_many` on all cores, searched on one thread.
  - faiss `IndexHNSWFlat`: same graph parameters; searched on one thread (`omp_set_num_threads(1)`). Cosine via normalized vectors and inner product.
  - Build scaling (`--engines build-scaling`): Recern on 1 thread and on all cores, faiss on all cores; faiss on 1 thread comes from the `faiss-hnsw` run.
  - LanceDB `IVF_HNSW_SQ` (m=16, ef_construction=200) and `IVF_PQ` (defaults, and `num_sub_vectors = dim / 4`), sweeping nprobes / ef / refine_factor; multi-threaded.
  - sqlite-vec `vec0`: exact scan (no ANN index in the stable release).

## Reproduce

```sh
# datasets (~1 GB)
mkdir -p bench-data
curl -o bench-data/sift-128-euclidean.hdf5 http://ann-benchmarks.com/sift-128-euclidean.hdf5
curl -o bench-data/glove-100-angular.hdf5 http://ann-benchmarks.com/glove-100-angular.hdf5

# Python with loadable SQLite extensions (python.org builds lack them)
uv venv --python 3.13 --managed-python bench/.venv
crates/recern-vector-py/.venv/bin/maturin build --release -m crates/recern-vector-py/Cargo.toml -o bench/wheels
uv pip install --python bench/.venv bench/wheels/*.whl h5py numpy sqlite-vec lancedb faiss-cpu

bench/.venv/bin/python bench/ann_bench.py sift
bench/.venv/bin/python bench/ann_bench.py glove
bench/.venv/bin/python bench/report.py
```

`--engines` selects engines, `--reuse` re-measures a saved Recern index without rebuilding it, and `--subset N` runs a quick smoke test on the first N records with exact ground truth.

## Profiling search

- `bench/.venv/bin/python bench/profile_search.py sift [--faiss]` splits per-query time into the Python layer and the Rust core, and counts distance computations, on the index saved by the last run.
- `cargo run --release --example search_bench -- bench/work/sift/recern.rvec bench/work/sift/queries.f32 bench/work/sift/truth.u32` measures recall and latency in Rust alone, so search changes can be evaluated without rebuilding the index or the wheel. `ann_bench.py` writes the query and ground-truth files next to the index.
