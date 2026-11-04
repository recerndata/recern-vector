# Recern Vector 0.5.0: packages and compatibility

Version **0.5.0** adds incremental record/HNSW encoding, paged format 3,
automatic WAL checkpoints, explicit storage upgrades with a format-2 backup,
and published Node.js packages with automatic platform selection. The portable
profile, HTTP API, migration/export format and WAL contract remain version 1.

## Install

```sh
pip install --upgrade recern-vector==0.5.0
cargo add recern-vector@0.5.0
cargo add recern-vector-client@0.5.0
cargo install recern-vector-cli --version 0.5.0 --locked
cargo install recern-vector-server --version 0.5.0 --locked
npm install @recern/vector@0.5.0
```

Python requires CPython 3.11+. ABI3 wheels are tested on 3.11 and 3.14; the
interoperability suite also runs on 3.12. The sdist is built and installed
separately. Rust requires 1.89+; the minimum and stable compilers are checked.
Node.js requires 22+. npm installs the exact-version native package for the
supported operating system, CPU and libc. Keep optional dependencies enabled;
installation does not run a binary-download script.

## Platforms and artifacts

[GitHub release v0.5.0](https://github.com/recerndata/recern-vector/releases/tag/v0.5.0)
contains native CLI/server archives, Python wheels/sdist, Rust source packages,
Node registry packages, self-contained Node archives and SHA256SUMS.

| Platform | CLI + HTTP server | npm native dependency |
|---|---|---|
| Linux x64, glibc 2.35+ | `recern-vector-0.5.0-linux-x64.tar.gz` | `@recern/vector-linux-x64-gnu` |
| Linux ARM64, glibc 2.35+ | `recern-vector-0.5.0-linux-arm64.tar.gz` | `@recern/vector-linux-arm64-gnu` |
| macOS Intel, macOS 13+ | `recern-vector-0.5.0-darwin-x64.tar.gz` | `@recern/vector-darwin-x64` |
| macOS Apple silicon, macOS 13+ | `recern-vector-0.5.0-darwin-arm64.tar.gz` | `@recern/vector-darwin-arm64` |
| Windows x64 | `recern-vector-0.5.0-win32-x64.zip` | `@recern/vector-win32-x64-msvc` |

Use the umbrella `@recern/vector` package for ordinary npm installs. The
self-contained GitHub Node archives have `recern-vector-0.5.0-node-<platform>.tgz`
names. Linux musl, Windows ARM64 and other targets have no advertised prebuilt
addon. Build from source on other targets; only the listed hosts are verified.

Checksums use UTF-8 text with portable relative names:

```sh
sha256sum --check SHA256SUMS
# macOS: shasum -a 256 -c SHA256SUMS
```

## Upgrade and backup

1. Stop writers and back up a matching snapshot/WAL pair, or checkpoint and copy
   the standalone snapshot while writes remain stopped.
2. Install 0.5.0. Readers support formats **1, 2 and 3**. Ordinary saves keep
   format-2 files in format 2; format 1 first checkpoints to format 2.
3. New databases use format **3**. To opt an existing format-2 file into the new
   layout, run `recern-vector upgrade-storage data.rvec`, Python
   `db.upgrade_storage()` or Node `db.upgradeStorage()`. Keep the returned
   standalone format-2 backup. Pending data is included in the converted file;
   its backup contains the last durable state.
4. An error after snapshot rename can mean the upgrade committed. Reopen to
   inspect rather than blindly replaying mutations. Conversion is not a binary
   downgrade. Older 0.2/0.3/0.4 readers reject format 3 explicitly.

**Workspace 1.0.0 cannot open format 3.** It uses the Vector 0.2.0 reader. Keep
files at format 2 when inspecting them with that Workspace release. Its reader
upgrade and UI/refresh integration are separate future work. Published-reader
checks cover old-format reads, upgraded backups, explicit rejection and portable
export-v1 transfer back to an older engine; they do not establish Workspace UI
support for format 3.

The single-node HTTP server owns its database. Stop it before replacing its
snapshot/WAL or changing engine versions. HTTP/profile/export v1 remain stable;
credentials, deployment and backend-specific features need explicit configuration.

## Measurements and operating limits

The [reproducible measurements](../bench/releases/0.5.0-dev.1/README.md) were run
against the 0.5.0-dev.1 engine source. Its engine implementation is retained for
0.5.0; raw samples, hashes and plots keep their actual development-build
provenance. These are not fresh measurements of every distributed release
binary. Historical 0.4.0 data remains separate.

Format-3 saves encode changed records and HNSW neighbours. Open/checkpoint still
process the full image; database, graph and cached pages must fit in RAM. Page
padding can dominate file size. Full GloVe int8 reached **15.21 GiB** process peak
RSS in this run. Int8 reduces vector payload, without guaranteeing lower total
process peaks or faster searches. The native benchmark disables automatic
checkpoint scheduling and uses warm caches; it is not an HTTP capacity test.

See [persistence, failure behavior and recovery checks](persistence-0.5.md),
[byte layout](file-format.md) and [remaining limits](limitations.md). This release
is single-node; clustering, replication and high availability are not included.
