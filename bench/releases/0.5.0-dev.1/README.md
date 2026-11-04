# Vector 0.5.0-dev.1 measurements

Development measurements of the paged/incremental engine, not a published 0.5.0 release. Historical 0.4.0 results and its harness are unchanged. Raw results, source/native-binary checksums and the provenance manifest accompany this report. See [results and plots](RESULTS.md).

## Method

All timed code uses optimized Rust builds through installed Python wheels. The current wheel is built with `maturin build --release`; the same-run baseline is the published PyPI 0.4.0 wheel. The current engine's format-2 path is a second control, keeping compiler/package differences separate from the new storage layout. No debug-build timing is reported.

Each persistence repetition runs in a fresh process and a newly created temporary database. Cases cover 1,000 / 10,000 / 100,000 records, 128 dimensions, cosine, f32 and int8. NumPy generates reproducible `standard_normal` float32 vectors with seed 20261104. HNSW uses m=16, ef construction=200 and four build threads. Three fresh builds per case rotate the order of the three candidates.

After the initial save and checkpoint, each case performs **20 upserts replacing existing IDs**, **20 single-record deletes**, and **20 clean saves**, in that order. Save latency includes writer-position validation, encoding, WAL append and `fsync`; mutation latency is measured separately. There is no checkpoint between those three phases. The current engine's automatic checkpoint scheduling is **disabled** to isolate save cost and compare with 0.4.0's manual scheduling. This deliberately includes the cost of retained WAL history in legacy-format cases; it is not a measurement of production default scheduling or HTTP request latency. Initial/final checkpoint times and warm reopen time are separate fields.

The chosen sizes and 20 replacements measure steady-state page updates and do not cross a checksum-tree capacity boundary. Occasional checksum-tree/allocator growth and synchronous automatic checkpoints can add latency beyond these samples.

The reported aggregate is the median of three per-process p50/p95 measurements, with raw per-save samples retained. Only 20 saves per phase per process are timed; these are observations, not latency SLOs or confidence intervals. Reopening validates the snapshot/WAL and checks the final live record count. Functional recovery and mutation correctness are tested separately.

ANN runs use the **full SIFT1M** (1,000,000 × 128, L2) and **full GloVe-100** (1,183,514 × 100, cosine) ANN-Benchmarks files. Each precision builds one index with four threads. The first 1,000 test queries, top-10 and ef=10/20/40/80/160/320/640/1280 are measured in three single-query loops, each preceded by 100 warmup queries and using a deterministic shuffled order. Recall is compared to the **original f32 dataset's published neighbors**, including for int8. Latency includes the Python call through returned IDs and excludes recall calculation. Repetitions measure query-loop variance, not build-to-build uncertainty.

All filesystem measurements use a warm OS cache on a desktop without CPU/process isolation. Background activity, thermals, allocator behavior and filesystem caching can affect results. These measurements cover native embedded operations, not Qdrant/HTTP throughput, concurrent serving, cold disk, power-loss guarantees or an out-of-core engine. `fsync` has the platform guarantees described in the persistence documentation.

## Memory and file size

`peak_rss_bytes` is the **entire worker process's lifetime high-water RSS**, obtained from `getrusage`: bytes on macOS and normalized from KiB on Linux. It includes Python/NumPy inputs, IDs, the index, encoded pages, WAL/recovery buffers and allocator effects. Stage fields are cumulative high-water marks, **not isolated per-phase peaks or deltas**. This is different from engine-reported `vector_bytes`, `graph_bytes`, metadata bytes and file size. Persistence collection counters are captured after mutation, so vector bytes include replacement tombstones.

Format 3 keeps encoded pages in RAM in addition to the index. A small record uses at least one 4096-byte page, so int8's vector-payload savings do not imply a proportional reduction in file size or total process RSS. Open/checkpoint still process a complete image. WAL retention can also substantially raise legacy-format peak RSS. The raw measurements retain these costs rather than presenting int8 vector bytes as total memory.

## Observed full-image limits

On this Mac, full-image peaks were 12.98 / 11.95 GiB for SIFT f32/int8 and 12.55 / 15.21 GiB for GloVe f32/int8. Both precision modes used the same 3.81 GiB SIFT and 4.51 GiB GloVe snapshots. Int8 reduced vector payloads but did not consistently reduce whole-process peak RSS and did not accelerate queries. Bulk-save buffers, cached padding and repeated full-image validation remain material costs. These are measured limits of this development implementation, not hidden by the incremental-write improvement.

## Reproduce

Use Python 3.13 and separate environments; pinned package versions are also in each raw report. Download the datasets into `bench-data` as described in [the benchmark guide](../../README.md). The harness refuses existing output/work paths and removes only its own temporary case databases.

```sh
uv venv --python 3.13 /tmp/vector-050-bench
uv pip install --python /tmp/vector-050-bench/bin/python \
  maturin==1.16.0 numpy==2.5.3 h5py==3.16.0
/tmp/vector-050-bench/bin/python -m maturin build --release \
  --manifest-path crates/recern-vector-py/Cargo.toml --out /tmp/vector-050-wheels
uv pip install --python /tmp/vector-050-bench/bin/python /tmp/vector-050-wheels/*.whl
uv venv --python 3.13 /tmp/vector-040-bench
uv pip install --python /tmp/vector-040-bench/bin/python recern-vector==0.4.0 numpy==2.5.3

python3 bench/vector_050_bench.py persistence \
  --current-python /tmp/vector-050-bench/bin/python --current-version 0.5.0.dev1 \
  --baseline-python /tmp/vector-040-bench/bin/python \
  --output /tmp/vector-050-results/persistence.json --work /tmp/vector-050-persistence
python3 bench/vector_050_bench.py ann --dataset sift \
  --current-python /tmp/vector-050-bench/bin/python --current-version 0.5.0.dev1 \
  --output /tmp/vector-050-results/sift.json --work /tmp/vector-050-sift
python3 bench/vector_050_bench.py ann --dataset glove \
  --current-python /tmp/vector-050-bench/bin/python --current-version 0.5.0.dev1 \
  --output /tmp/vector-050-results/glove.json --work /tmp/vector-050-glove
```

Run the three benchmark commands sequentially. Validate the raw reports and regenerate tables/plots with:

```sh
MPLCONFIGDIR=/tmp/vector-050-mpl /tmp/vector-050-bench/bin/python \
  bench/report_050.py --reports /tmp/vector-050-results --plots
```

Install the pinned Matplotlib version from `requirements.txt` for plotting. The validator rejects incomplete runs, missing/duplicate cases, incorrect work counters and changed source/binaries between datasets.

 `--subset 1000 --queries 20 --repeats 1` is an ANN harness smoke test with recomputed exact subset ground truth; those results must not be presented as full-dataset measurements.

## Compatibility and CI

A native, authenticated Qdrant **1.19.2** on loopback was used for the local contract suite: **258 Python/API/cross-language tests passed**, with six backend-inapplicable skips; **8 Node tests passed**, including Qdrant. Tests cover authentication, filters, numeric metadata, namespace isolation, envelope protection, migration/resume/lost acknowledgements and Python/Rust/Node data exchange. No Qdrant throughput benchmark is implied.

Ordinary CI now also verifies published **0.2.0 / 0.3.0 / 0.4.0** readers and runs a small release-build persistence harness check on Linux. The 0.2 reader check covers the engine used by Workspace 1.0.0: format-2 writes and the upgrade backup remain readable, while format 3 is rejected without data modification. This is not a Workspace UI/refresh test. Rust, HTTP and native Node tests run on Linux, Windows and macOS. Release artifact installation and npm publication remain separate release gates.
