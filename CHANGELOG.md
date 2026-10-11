# Changelog

## 0.3.0 — 2026-10-11

- Add Python portable profile `recern-vector/1`, with the same collection, record, pagination and filtered-search operations on embedded Recern and Qdrant 1.19.2.
- Preserve logical string IDs and arbitrary JSON metadata through a checked Qdrant envelope. Define filter semantics, smaller-is-closer distances, explicit errors and backend capabilities.
- Add consistent embedded snapshot exports and verified, resumable imports into new embedded or Qdrant collections. Format v1 records dimensions, metric, stored precision, normalization, counts and SHA-256 checksums; interrupted and lost-acknowledgement writes replay by logical ID.
- Add bounded native record pages and persistent database identity access for Rust/Python. Keep `.rvec` format 2, format-1 readers and existing native APIs.
- Add shared Rust/Python filter fixtures, a frozen export-v1 archive, authenticated real-Qdrant CI, transport/migration failure tests, examples and contract documentation.

Remote batches are not declared atomic. Consistent Qdrant export, a Recern server and portable Rust/Node clients are not part of this release scope. Int8 migration transfers stored/dequantized vectors into f32 collections; original embeddings are not recovered.

## 0.2.0

Stable format 2 with format-1 compatibility, checksummed changed-page WAL, checkpoints, int8 quantization, logical filters/adaptive planning and native Node.js bindings. See the [published release](https://github.com/recerndata/recern-vector/releases/tag/v0.2.0).
