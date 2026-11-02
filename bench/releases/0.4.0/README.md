# Recern Vector 0.4.0 release measurements

Measured on 2026-11-02 with the released PyPI package, not the historical 0.1.0 benchmark environment. The old `bench/results` and October prototype report are unchanged.

## Environment and method

- Apple M3 Max, 36 GiB RAM, macOS 27.0.1, arm64, CPython 3.13.15.
- `recern-vector==0.4.0`, `faiss-cpu==1.15.1`, `numpy==2.5.3`, `h5py==3.16.0`.
- Full SIFT1M (1,000,000 × 128, L2) and GloVe-100 (1,183,514 × 100, cosine). Input SHA-256 values are in the raw JSON files.
- HNSW m=16, ef construction=200, **4 build threads for both libraries**, one query/search thread. There is one graph build per engine and dataset.
- Recern f32, Recern int8, and faiss HNSW f32. Int8 recall is compared to the original, unquantized dataset ground truth.
- First 1,000 test queries, top-10, published ground-truth neighbours. ef=10,20,40,80,160,320,640,1280. Each setting has 3 timed query repetitions, preceded by 100 warm-up queries. Query order is shuffled deterministically per repetition.
- Reported QPS is the median of the three loops. p50/p95 are the medians of their per-loop percentiles. Raw repetitions and QPS min/max are retained; they are not build-to-build confidence intervals.
- Latency includes each Python API through returned IDs, and excludes recall calculation. Recern returns string IDs; faiss returns positional IDs. ID preparation and faiss cosine normalization are outside the build timer; Recern normalizes in its API.
- Save, checkpoint, reopening and file size are recorded separately. A faiss index file does not implement Recern database/WAL semantics. File size is not peak RAM.

This is a desktop measurement without CPU/process isolation. Other activity, scheduling and thermals can influence the results. No filtering, embedding model, HTTP, Qdrant, concurrent clients, crash recovery or out-of-memory dataset is benchmarked.

## Reproduce on macOS

From the repository root, with Python 3.13 available:

```sh
uv venv --python 3.13 /tmp/recern-bench-040-env
uv pip install --python /tmp/recern-bench-040-env/bin/python \
  recern-vector==0.4.0 faiss-cpu==1.15.1 numpy==2.5.3 h5py==3.16.0

OPENBLAS_NUM_THREADS=1 VECLIB_MAXIMUM_THREADS=1 OMP_NUM_THREADS=4 \
  /tmp/recern-bench-040-env/bin/python bench/release_bench.py sift \
  --output /tmp/recern-040-repro/sift.json --work bench/work/release-040-sift \
  --queries 1000 --repeats 3 --build-threads 4
OPENBLAS_NUM_THREADS=1 VECLIB_MAXIMUM_THREADS=1 OMP_NUM_THREADS=4 \
  /tmp/recern-bench-040-env/bin/python bench/release_bench.py glove \
  --output /tmp/recern-040-repro/glove.json --work bench/work/release-040-glove \
  --queries 1000 --repeats 3 --build-threads 4
```

Download the two input HDF5 files into `bench-data` as described in [the benchmark README](../../README.md), or pass `--data-dir`. Use new output and work paths for another run: the harness refuses to overwrite them. `--subset 1000 --queries 20` is a harness smoke check and must never be published as a full-dataset result.

The website importer checks that both datasets are complete, use the same protocol and environment, and match the accompanying provenance manifest. It publishes the script, pinned requirements and raw JSON files alongside the report. Historical results remain at `/vector/benchmarks/historical`.
