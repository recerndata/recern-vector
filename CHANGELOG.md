# Changelog

## Unreleased — 0.4.0 server and portable clients source milestone

- Define draft HTTP API v1 in `api/openapi.json` and `docs/http-api.md`, mapping
  portable profile v1 to versioned operations, authentication, typed errors,
  request limits, durable acknowledgements, collection generations and snapshot
  leases. Keep `.rvec` and migration formats unchanged.
- Add wire request/response fixtures and private Python codecs preserving
  arbitrary IDs, uint64 values and structural integer/float metadata. Reject
  duplicate JSON keys, invalid Unicode, non-finite values and malformed errors.
- Add a separate Rust `recern-vector-server` package (`0.4.0-dev.1`) with bearer
  authentication, loopback listeners, bounded HTTP/1.1 requests and workers,
  durable serialized writes, generation checks and snapshot leases. Catalog and
  data changes share one native WAL commit; stale handles cannot access a
  replacement generation.
- Add durable sequence access and borrowed search-result visitation to the
  engine, allowing bounded server responses without cloning all hit metadata.
  Keep the existing native search behavior and frozen format/export fixtures.
- Test real HTTP operations, shared filters, int8, concurrent clients, request
  caps, torn WAL recovery and restart/lost acknowledgements; inject commit and
  deadline/shutdown boundaries. Configure server HTTP CI on Linux/macOS/Windows.
- Gate CI and installed-wheel checks on OpenAPI/schema validation and shared
  portable fixtures. Release preparation and distribution remain; this is not
  a published 0.4.0 release.
- Add the Python `Client.remote` backend, generation-bound handles, verified TLS,
  total HTTP deadlines, typed errors and explicit snapshot lease cleanup. Extend
  resumable migration to/from the server, including lost creation acknowledgements.
- Add a separate portable Rust client and asynchronous Node `./portable` entry
  point for embedded Recern, Recern HTTP and Qdrant. Share validators/adapters
  between Rust/Node; preserve uint64 and structural numeric kinds using JSON text.
- Run the frozen profile across three languages/backends; verify cross-language
  data, migration, private-CA TLS and packed Node contents. Keep native APIs and
  format/export compatibility fixtures unchanged.

## 0.3.0 — 2026-10-11

- Add Python portable profile `recern-vector/1`, with the same collection, record, pagination and filtered-search operations on embedded Recern and Qdrant 1.19.2.
- Preserve logical string IDs and arbitrary JSON metadata through a checked Qdrant envelope. Define filter semantics, smaller-is-closer distances, explicit errors and backend capabilities.
- Add consistent embedded snapshot exports and verified, resumable imports into new embedded or Qdrant collections. Format v1 records dimensions, metric, stored precision, normalization, counts and SHA-256 checksums; interrupted and lost-acknowledgement writes replay by logical ID.
- Add bounded native record pages and persistent database identity access for Rust/Python. Keep `.rvec` format 2, format-1 readers and existing native APIs.
- Add shared Rust/Python filter fixtures, a frozen export-v1 archive, authenticated real-Qdrant CI, transport/migration failure tests, examples and contract documentation.

Remote batches are not declared atomic. Consistent Qdrant export, a Recern server and portable Rust/Node clients are not part of this release scope. Int8 migration transfers stored/dequantized vectors into f32 collections; original embeddings are not recovered.

## 0.2.0

Stable format 2 with format-1 compatibility, checksummed changed-page WAL, checkpoints, int8 quantization, logical filters/adaptive planning and native Node.js bindings. See the [published release](https://github.com/recerndata/recern-vector/releases/tag/v0.2.0).
