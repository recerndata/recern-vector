# Recern Vector 0.4.0: packages and compatibility

Recern Vector 0.4.0 adds the single-node HTTP server and compatible portable
Python, Rust and Node.js clients for embedded Recern, Recern HTTP and Qdrant.
The native engine keeps file format 2, format-1 readers and migration export
format 1. Existing native APIs remain available.

## Install

```sh
pip install --upgrade recern-vector==0.4.0
cargo add recern-vector@0.4.0
cargo add recern-vector-client@0.4.0
cargo install recern-vector-cli --version 0.4.0 --locked
cargo install recern-vector-server --version 0.4.0 --locked
```

Python requires CPython 3.11+. Rust packages require Rust 1.89+; the release
workflow checks the minimum compiler as well as the current stable compiler.
Python wheels are ABI3 and are tested on CPython 3.11 and 3.14; the Linux
interoperability suite also runs on 3.12. A source distribution is built and
installed independently of the wheels.

[GitHub release v0.4.0](https://github.com/recerndata/recern-vector/releases/tag/v0.4.0)
contains the exact release binaries, Node packages, Python distributions,
Rust source packages and `SHA256SUMS`.

## Native and Node artifacts

| Platform | CLI + HTTP server | Node npm tarball |
|---|---|---|
| Linux x64, glibc 2.35+ | `recern-vector-0.4.0-linux-x64.tar.gz` | `recern-vector-0.4.0-node-linux-x64.tgz` |
| Linux ARM64, glibc 2.35+ | `recern-vector-0.4.0-linux-arm64.tar.gz` | `recern-vector-0.4.0-node-linux-arm64.tgz` |
| macOS Intel, macOS 13+ | `recern-vector-0.4.0-darwin-x64.tar.gz` | `recern-vector-0.4.0-node-darwin-x64.tgz` |
| macOS Apple silicon, macOS 13+ | `recern-vector-0.4.0-darwin-arm64.tar.gz` | `recern-vector-0.4.0-node-darwin-arm64.tgz` |
| Windows x64 | `recern-vector-0.4.0-win32-x64.zip` | `recern-vector-0.4.0-node-win32-x64.tgz` |

Linux native archives are built on Ubuntu 22.04 and use glibc. They are not musl
packages. Python wheel compatibility is stated by the wheel's own manylinux or
macOS tag and can differ from these native archive minimums. Windows and macOS
archives are developer binaries, without installer signing/notarization.

Native archives contain `bin/recern-vector`, `bin/recern-vector-server`
(`.exe` on Windows), licenses, the release OpenAPI document and usage notes.
Extract and verify the corresponding SHA-256 before running them.

Node requires Node.js 22+. Select the tarball matching both OS and CPU, then:

```sh
# Linux x64 example; change the artifact name for another supported platform.
npm install https://github.com/recerndata/recern-vector/releases/download/v0.4.0/recern-vector-0.4.0-node-linux-x64.tgz
```

Use `require('@recern/vector')` for the original native API and
`require('@recern/vector/portable')` for portable async data operations. Tarballs
contain the native binary, JavaScript, TypeScript definitions and licenses;
installation does not download or compile code. OS/CPU metadata rejects a
mismatched package. Public npm registry distribution and platform selection
remain planned for 0.5.0; these GitHub tarballs are installable npm packages.

## Upgrade and backup

1. Back up a matching `.rvec` snapshot and `.wal` while its owner is stopped, or
   call `checkpoint()` and copy the resulting standalone database with no writer.
2. Install 0.4.0. An ordinary 0.3.0 embedded file/WAL opens directly; the format
   version has not changed. Reacquire native handles after reopening.
3. To move an embedded database to HTTP, export a checked portable archive and
   import it into a new server database with the Python API. A server owns an
   internal catalog and does not adopt an ordinary application file in place.
4. Reconnect application clients and verify counts and representative searches
   before switching traffic. Keep the original backup and migration archive.

Release validation creates a database/WAL with the actual published 0.3.0 wheel,
opens and extends it with the installed 0.4.0 wheel, and verifies that a 0.3.0
read-only client can still read those records. A 0.3.0 export imports into 0.4.0,
and a 0.4.0 format-1 archive imports with 0.3.0. The format-1/2 golden files and
export-v1 fixture are retained unchanged. This read-compatibility check is not a
promise that arbitrary future downgrades or concurrent old/new writers are safe.

Server identity and collection generations persist across normal restart.
Snapshots and live cursor sessions do not survive restart. A stale handle cannot
write into a recreated collection; migration resume rechecks database identity,
creation tokens, generations and previously acknowledged records.

## What release checks establish

The release workflow builds and tests five native platforms, their Node packages
and Python wheels. Tests install/extract the artifacts rather than relying on an
editable checkout. The full three-language, three-backend profile and verified
private-CA TLS run against the packaged server, Rust example and installed Node
package; the external provider is authenticated Qdrant 1.19.2. Source-distribution
builds and packaged Rust crates are checked separately before PyPI publication.

The workflow is dispatched on `main`, with publication explicitly enabled after
local validation. Every required package/platform/upgrade job must succeed before
trusted PyPI publication. Registry uploads and the GitHub tag/release use that
same source commit; the website documentation is generated from it afterwards.
Release assets have a shared SHA-256 manifest. No registry credential is embedded
in client binaries, archives or migration journals.

The server remains single-node: data and index fit in RAM and saves still encode
and compare a full image. It does not add replication, sharding, tenant RBAC,
a managed service or the 0.5.0 incremental-encoding/checkpoint work. Qdrant batches
are not declared atomic and its portable pages are not a consistent export
snapshot. See [limits](limitations.md), [server operations](server.md) and
[migration guarantees](migration.md).
