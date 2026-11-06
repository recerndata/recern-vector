# API stability and the Vector 1.0 release gate

**Status: preparation for 1.0.0; the current stable release is 0.5.0.**
This document defines the intended 1.x compatibility policy and the evidence
required before publishing 1.0. It does not announce a 1.0 release or turn an
untested server/platform combination into a supported one.

## Supported public surface

The stability commitment covers the documented operations and their observable
behavior, including returned types, errors, defaults, persistence and ordering:

| Surface | Contract boundary |
|---|---|
| Rust engine | Public items re-exported by `recern_vector`, their public fields and methods |
| Rust portable client | Public `recern_vector_client` types, methods and error codes |
| Python | The `recern_vector` exports, documented native methods, and `recern_vector.portable` exports |
| Node.js | The `@recern/vector` and `@recern/vector/portable` entry points and their TypeScript declarations |
| HTTP | `/v1`, the OpenAPI document and the normative [HTTP rules](http-api.md) |
| Portable data | Profile `recern-vector/1`: IDs, metadata, filters, distances, capabilities and acknowledgements |
| Persistent data | Versioned snapshot/WAL formats and migration/export format 1 |
| CLI | Documented subcommands, flags, exit behavior and documented machine-readable output |

Private modules, underscore-prefixed implementation helpers, `PortableSession`,
internal graph/page layouts in memory, build scripts and human-readable diagnostic
wording are not application extension points. They do not weaken the documented
file-format or error-category guarantees. Log text is not a machine-readable API.

## Package versions and deprecation

The 1.x release policy follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Patches fix behavior within the documented contract. Minor versions add compatible
features. Removing or changing a supported API incompatibly requires a new major
version. A deprecation must name a replacement, include migration instructions and
appear in a minor release before removal in a later major release. Deprecated
public APIs remain supported for the rest of 1.x.

Rust compatibility includes struct literals, exhaustive enum matches, trait
bounds and public field types; an apparently additive change can break callers.
Review changes against [Cargo's compatibility guidance](https://doc.rust-lang.org/cargo/reference/semver.html)
and compile external consumers before declaring them compatible. Introducing
`non_exhaustive` on existing public types is also a compatibility decision, not
a silent patch-level fix.

Changes to Python keyword parameters/defaults, JavaScript promises, TypeScript
unions, precision handling, ownership or error categories require the same
review. Correct rejection of malformed or unsupported input is not a promise
to continue accepting it. Security or corruption fixes must describe their
observable effects in release notes.

Minimum runtimes and supported platforms are part of each tested release matrix.
Do not raise their floors silently in a patch. A proposed change requires an
explicit migration plan and updated installation tests. The 0.5 baseline is
Rust 1.89+, CPython 3.11+ and Node.js 22+; the [release page](release.md) lists the
actual platform builds and operating-system requirements.

## Protocol and storage versions are independent

A package version does not implicitly increment HTTP v1, portable profile v1,
export v1, WAL 1, or snapshot format 3. Clients negotiate the existing profile,
capabilities and limits. New optional operations require capability discovery;
a new client must not send unadvertised request fields to an older server.
Unknown response fields may be ignored, but missing required fields, unknown
required capabilities and malformed success responses must fail explicitly.

Keep logical IDs, JSON numeric kinds, missing/null behavior, AND/OR/NOT,
distance direction and ambiguous-write errors stable. A timeout can follow a
committed write; changing a failure into an empty result or adding automatic
write retries would violate the contract. ANN candidates and measured recall
can change with index/search improvements; they are not byte-identical results.

Keep all released binary and export fixtures unchanged. A storage change must
have its own explicit version and tested migration path. Existing format-2 files
stay format 2 on ordinary saves. New format-3 files cannot be read by published
0.2/0.3/0.4 engines or Workspace 1.0.0. Format upgrades require a recoverable
backup; they are not reversible by installing an older executable.

## Executable HTTP compatibility matrix

`scripts/http_compatibility_matrix.py` tests client sets from **0.4.0**, **0.5.0**
and, when supplied, the **source candidate** against each of the three server
versions. A cell contains 17 public-contract checks: Python remote operations
and a Python/Rust/Node data and migration roundtrip. It covers restart/reconnect,
stale collection handles, snapshot leases, export/import, ambiguous write
outcomes, identity checks, bounded transport failures, metadata precision,
filters, pagination and all three distance metrics. It is not a benchmark or a claim that every
method of every SDK has been exhaustively exercised across versions.

- Python comes from a separately installed distribution; source-tree imports
  are rejected.
- Rust uses an external frozen consumer compiled against exact published crate
  versions, or the explicit source dependency. Each generated Cargo.lock is
  checked and retained.
- Node uses the exact self-contained published native archives, or the source
  build. npm installation is a separate release gate.
- Native server/Node archive hashes are pinned in
  `tests/compatibility/releases.json`. Do not replace a published baseline with
  newly built binaries under the same version label.
- The workflow runs on Linux x64, macOS ARM64 and Windows x64. A source-inclusive
  run has nine client/server cells (153 checks) per OS. Skipped tests fail the gate. Results,
  logs and consumer lockfiles are retained as CI artifacts.

Run the published baseline matrix in a new folder:

```sh
python scripts/http_compatibility_matrix.py --platform darwin-arm64 \
  --work /tmp/recern-http-matrix
```

Use `linux-x64` or `win32-x64` on the corresponding host. Pass `--source-wheel`,
`--source-server` and `--source-node` together to include a built candidate;
the workflow shows the complete build commands. A successful run writes
`report.json` with actual versions, source commit, scope and test counts.

## Remaining gates before 1.0

1. Audit the full native and portable API against this boundary and retain
   external source/TypeScript consumers. Resolve any proposed breaking changes
   before declaring the final 1.0 API frozen.
2. Pass the HTTP version matrix and the existing shared embedded/HTTP/Qdrant
   contract suite on the exact supported Qdrant version. Later Qdrant versions
   passing a version check are not automatically tested or supported.
3. Exercise released-file upgrades, interrupted WAL/checkpoint recovery,
   standalone checkpoint backups, matched snapshot/WAL restoration, export-v1
   transfers and both migration destinations. Verify restored data and explicit
   rejection by incompatible readers; never rewrite frozen fixtures.
4. Publish measured memory/latency bounds and tested recovery procedures.
   Incremental saves do not remove full-image open/checkpoint costs or the
   single-writer boundary.
5. Validate the actual Rust/Python/Node distributions and CLI/server artifacts
   on every advertised target, then publish the final evidence matrix and
   release notes. Passing a development workflow alone is not a stable release.

Workspace's format-3 reader and remote Recern connection remain a separate
integration milestone. The Vector 1.0 matrix does not establish those UI flows.
