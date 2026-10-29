# Limitations and next work

Recern Vector 0.3.0 includes a stable versioned format, WAL, int8, logical filters, adaptive filtered planning and Rust/Python/Node.js APIs, plus a [portable Python API](portable-api.md) and [verified export/import](migration.md) for embedded Recern and Qdrant.

- The database and index must fit in RAM. Save/open also allocate encoded snapshot buffers.
- WAL writes changed pages, but encoding/comparison are still O(database size). Layout shifts may dirty many pages. Use checkpoints to bound journal size; automatic checkpoint scheduling is not included.
- Concurrent saves serialize and stale writers are rejected. There is no transaction merge or multi-writer MVCC; reopen after another writer commits.
- Int8 saves vector storage (dim + 4 bytes per vector versus 4 × dim), at the cost of quantization error. IDs, graph, metadata and persistence buffers are additional memory. Validate recall against original f32 data for your workload.
- Filter planning is a heuristic using a sampled selectivity and an operation-count estimate. There is no metadata index, string range comparison or array membership operator. HNSW remains approximate.
- Node.js provides native bindings and an asynchronous search method. Other operations are synchronous; use worker threads for large builds or saves. Source build and CI artifacts are prepared; npm publication is a separate release step.
- Filesystem durability requires local locks and atomic rename. Network filesystems and external file edits are unsupported.
- Portable Qdrant batches are not declared atomic. Its pages are not a consistent snapshot during writes, so portable export is supported only by embedded Recern and the 0.4.0 source HTTP server; importing into Qdrant is supported. The adapter owns its collection metadata and point envelopes and does not adopt arbitrary existing Qdrant collections.
- Migration requires an immutable archive and exclusive ownership of new destination collections. It has resume and read-back verification, but no live change capture, transactional cutover or automatic remote rollback. Int8 exports contain stored/dequantized vectors, and imports use f32 with possible cosine renormalization.

Engine improvements remain incremental encoding and a page directory, automatic checkpoint policy, metadata indexes, broader quantization/recall benchmarks, and packaged Node binaries for additional targets. The [single-node development server](server.md) is a separate package around the embedded engine; sharding, replication and hosted operation are not current capabilities.

## Planned API portability and migration

Working targets as of 29 October 2026. Version numbers are release scopes, not delivery dates. Version 0.3.0 is published. The 0.4.0 server, portable clients and SDK migration source milestone is implemented; final release/artifact and upgrade gates remain before publication. Later versions remain planned.

Development now prioritizes Recern Vector: **0.4.0 → 0.5.0 → 1.0.0**, followed by Recern Workspace **1.0.0** with Microsoft SQL Server and stabilization. Workspace 0.5.0 is the released desktop baseline. This sequence does not require filling every intermediate minor version or expanding the agreed adapter scope.

The goal is to take a prototype built with Recern Vector into production either on an external vector database or on a future Recern server. Applications using the portable API should retain their collection, record and search logic when the backend configuration changes. Data transfer, credentials, capacity planning and backend-specific features still require explicit setup.

| Target | Scope | Release gate |
|---|---|---|
| **0.3.0 (available)** | Python portable profile v1; embedded and Qdrant 1.19.2 adapters; consistent embedded export and resumable import into either backend. Rust/Python share filter fixtures; export v1 has a frozen fixture. | Same example and contract suite on embedded and authenticated Qdrant. Logical IDs, JSON metadata, distances and filters are checked; interruption/lost-ack recovery and read-back verification are tested. Existing native APIs and format-1/2 fixtures must continue passing. Cross-platform/package CI remains a release gate. |
| **0.4.0** | Separate, single-node Recern server and a remote backend for the same portable client. Publish a versioned HTTP/OpenAPI contract, authentication, TLS deployment guidance, timeouts and explicit write/read guarantees. Extend portable Rust/Node clients against the shared suite. | The same portable example and contract tests run on embedded Recern, remote Recern and Qdrant. Reconnect, restart recovery and unsupported-capability cases are covered. Writes respect the engine's single-writer boundary. |
| **0.5.0** | Improve write performance with incremental encoding and a page directory; add an automatic checkpoint policy, current f32/int8 memory and recall benchmarks, and published Node packages for supported platforms. | Measure commit cost and peak memory as databases grow. Exercise crash recovery at WAL/checkpoint boundaries, verify the actual published language packages, and document recall/precision tradeoffs on reproducible datasets. Metadata indexes remain a separate optimization until workloads justify them. |
| **1.0.0** | Stabilize the supported public API, migration format and compatibility/deprecation policy after both migration paths are proven. | Document the tested client/server/provider version matrix, pass upgrade and migration suites, and publish the operational limits. This milestone does not imply clustering, high availability or every vendor's API. |

Milvus is the next candidate external adapter after Qdrant proves the contract. Its implementation should follow an actual integration need, without making support for many providers a prerequisite for 1.0. A first Recern server is an intermediate step towards enterprise deployment; replication, sharding, tenant isolation, RBAC/SSO and service guarantees require their own scoped work.

### Next milestone: 0.4.0 implementation order

The scopes below define 0.4.0; progress on the first stage is recorded here.
Keep the released package version at 0.3.0 until the next release is prepared.

**29 October 2026:** the [HTTP contract draft](http-api.md), wire fixtures and
precision-preserving Python codecs are joined by the separate Rust
[development server](server.md). Real HTTP tests cover authentication, shared
filters, generations, bounded requests/responses, snapshot leases, concurrency,
WAL recovery and restart after acknowledgements. Unit tests also cover commit
failures and deadline/shutdown boundaries. [Python remote and portable Rust/Node
clients](portable-clients.md) now share the three-backend profile; SDK tests cover
verified migration, restart/reconnect, lost acknowledgements, lease cleanup,
private-CA TLS and precision-preserving interchange. Final artifact/upgrade
validation, version changes and publication remain before a 0.4.0 release.

1. **Specify the remote contract.** Map portable profile `recern-vector/1` to versioned HTTP routes and OpenAPI schemas. Define collection identities, cursors, capabilities, filter semantics, distances and error responses. Specify request, batch, response and concurrency limits, timeouts, authentication and TLS deployment requirements. Document whether a timed-out write may have committed and which operations can be replayed safely; do not add blind automatic write retries.
2. **Implement a separate single-node server.** Wrap the embedded engine with one writer owning the database, bounded request handling and explicit read visibility. Acknowledge writes only after durable persistence. Cover exclusive ownership, startup recovery, graceful shutdown, health/readiness checks and consistent export. Verify authentication failures and avoid exposing keys, vectors or metadata in logs.
3. **Connect the portable clients.** Start with a remote backend for the existing Python client, then implement the portable Rust and Node.js clients against the same contract fixtures. Exercise the shared operations on embedded Recern, remote Recern and Qdrant; report backend differences through capabilities and explicit errors. Existing native APIs remain available.
4. **Prove migration and package behavior.** Run the same application example against all supported backends. Test embedded-to-server migration, interrupted imports, lost acknowledgements, reconnects, server restart after acknowledged writes and concurrent clients. Keep file-format 1/2 and export-v1 fixtures frozen. Verify shipped client/server artifacts and document the tested platform and backend matrix before publishing 0.4.0.

The first server remains subject to the embedded engine's RAM and O(database size) save-encoding limits. The 0.4.0 documentation and benchmarks must disclose them; the incremental persistence work belongs to 0.5.0. A remote endpoint alone is not evidence of enterprise availability or scalability.

### Completion gates before returning to Workspace 1.0

- **0.5.0:** incremental encoding/page-directory work and automatic checkpoints pass crash-injection and recovery checks. Publish reproducible commit-latency, peak-memory and f32/int8 recall measurements, and verify the distributed Node packages on their advertised targets. A storage-layout change requires an explicit format version and upgrade path.
- **Vector 1.0.0:** freeze the supported API and migration contract with a documented deprecation policy. Test upgrades from released fixtures, backup/restore, both migration destinations and the supported client/server version matrix. Record operational limits and recovery procedures; resolve known data-loss and compatibility defects before release.
- **Workspace handoff:** verify how the released Workspace 0.5.0 file reader handles files from the new Vector versions. It currently uses the 0.2.0 engine API, so shared format numbers alone do not prove compatibility. Test read-only opening, snapshot/WAL refresh and unsupported-format errors. Plan any required reader upgrade and the remote-server connection as explicit Workspace 1.0 work alongside its existing roadmap.

### Contract defined for 0.3.0

The implemented definitions, backend differences and error categories are in the [portable profile v1](portable-api.md) and [migration format v1](migration.md). The requirements below remain the compatibility checklist for future adapters.

- **Portable core:** create/get/list collections; upsert/batch upsert, get/delete records, paginated record iteration and filtered vector search. Keep existing low-level APIs available. File operations such as `save()` and `checkpoint()`, graph inspection, `explain()` and engine tuning remain explicit backend extensions.
- **Semantics, not just names:** logical string IDs; dimensions and metric validation; metadata value types; missing versus null fields; numeric comparisons, dotted paths, AND/OR/NOT; hit fields, error categories and write acknowledgement/read visibility. Do not silently weaken filters, drop metadata, or claim atomic remote batches without a backend guarantee.
- **Capabilities:** report the supported portable profile and optional operations. Reject unsupported behavior with a useful error before running or migrating it. A common client is not a promise of wire compatibility with every vendor's native API.
- **Distances:** retain Recern's smaller-is-closer convention (`1 - cosine`, squared L2, negative dot product) and test each adapter's conversion. ANN results need quality/tolerance checks rather than a promise of identical top-k ordering across engines. Remote timeouts and retries must not masquerade as empty successful results.
- **Logical ID and metadata mapping:** keep original application IDs and JSON metadata intact through a reversible adapter envelope. Recern accepts arbitrary string IDs, whereas [Qdrant point IDs are unsigned integers or UUIDs](https://qdrant.tech/documentation/manage-data/points/). Test deterministic mapping, collisions, repeated upserts, null/scalar metadata and reserved-field conflicts before calling the adapter compatible.
- **Migration format:** a versioned manifest plus streamed records, with collection dimensions, metric, normalization/precision information, counts and checksums. Export a consistent source snapshot and rebuild the target index. Include a dry run, resumable batches and post-import checks. This format is separate from `.rvec` and its WAL.
- **Precision disclosure:** cosine vectors are already normalized in Recern, and int8 storage cannot recover the original f32 embeddings. Migration can transfer stored/dequantized vectors without invoking an embedding model; lossless transfer of pre-quantization originals requires retaining those originals separately. Record this in the manifest and validate recall on representative queries.

### Compatibility evidence

Use shared golden datasets for logical IDs, metadata, all supported filter operators, dimensions, zero/invalid vectors, metric conversion, repeated mutations and error behavior. Run adapter tests against the actual supported backend versions in CI, not just mocked responses. Test the exact-search reference with numeric tolerances where supported, then measure ANN recall separately. Verify record counts, round trips, interrupted migrations and reads after acknowledged writes.

Backend differences are observable: [Milvus documents different score directions for L2 versus cosine/IP](https://milvus.io/docs/metric.md). The portable contract must make these conversions explicit before users can switch backends safely.
