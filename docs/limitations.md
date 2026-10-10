# Limitations and next work

Recern Vector 0.2.0 includes a stable versioned format, WAL, int8, logical filters, adaptive filtered planning and Rust/Python/Node.js APIs.

- The database and index must fit in RAM. Save/open also allocate encoded snapshot buffers.
- WAL writes changed pages, but encoding/comparison are still O(database size). Layout shifts may dirty many pages. Use checkpoints to bound journal size; automatic checkpoint scheduling is not included.
- Concurrent saves serialize and stale writers are rejected. There is no transaction merge or multi-writer MVCC; reopen after another writer commits.
- Int8 saves vector storage (dim + 4 bytes per vector versus 4 × dim), at the cost of quantization error. IDs, graph, metadata and persistence buffers are additional memory. Validate recall against original f32 data for your workload.
- Filter planning is a heuristic using a sampled selectivity and an operation-count estimate. There is no metadata index, string range comparison or array membership operator. HNSW remains approximate.
- Node.js provides native bindings and an asynchronous search method. Other operations are synchronous; use worker threads for large builds or saves. Source build and CI artifacts are prepared; npm publication is a separate release step.
- Filesystem durability requires local locks and atomic rename. Network filesystems and external file edits are unsupported.

Next improvements: incremental encoding and a page directory, automatic checkpoint policy, metadata indexes, broader quantization/recall benchmarks, and packaged Node binaries for additional targets. Server mode, sharding and hosted operation remain outside the embedded library's scope.
