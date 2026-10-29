# HTTP API v1 contract for 0.4.0

**Draft 1, 29 October 2026.** This is the first implementation milestone for
Recern Vector 0.4.0. The released engine remains **0.3.0**. The separate
[development server](server.md), version `0.4.0-dev.1`, now implements this
contract from source. Portable remote clients and the full 0.4.0 release remain
the next stages.

The machine-readable source is [`api/openapi.json`](../api/openapi.json), using
[OpenAPI 3.1.1](https://spec.openapis.org/oas/v3.1.1.html) and
[JSON Schema 2020-12](https://json-schema.org/draft/2020-12/json-schema-core).
The HTTP paths use `/v1`; application semantics use portable profile
`recern-vector/1`. These version numbers are independent of the package version,
`.rvec` format 2, and migration archive format 1.

This page supplies normative rules which schemas cannot express, such as
UTF-8 byte limits, vector norms and durable acknowledgement. Schema validation
alone does not establish an implementation's conformance.

## Connection, authentication and discovery

The first server owns one local database. The development address is
`http://127.0.0.1:7337`. Bind to loopback by default. Use HTTPS with verified
certificates outside literal loopback addresses/`localhost`; provide explicit
private-CA configuration when needed. A TLS reverse proxy may forward to a
loopback server. Configure listener/body/connection limits at both layers.

All routes except `GET /health/live` require
`Authorization: Bearer <configured-api-key>`. An absent or invalid key returns
401 with `WWW-Authenticate: Bearer`; an authenticated but forbidden operation
returns 403. No cookies, query-string credentials or implicit browser login.
Disable cross-origin browser access by default. The first key grants access
to this database; it does not provide tenant isolation or granular RBAC.

Clients first read `GET /v1/info` and check `api_version`, `profile`, capabilities
and limits. `database_id` is persistent across a normal restart/checkpoint;
it is independent of hostnames, local paths and credentials. Switching URLs
does not transfer data. Reject incompatible profiles explicitly with `Unsupported`.
Use a stable database identity and collection generations to bind migration
journals, and verify them again when resuming.

`GET /v1/openapi.json` returns the contract implemented by that server.
`GET /health/live` reveals only `{"status":"alive"}`. Authenticated
`GET /health/ready` returns `{"status":"ready"}` only after ownership and WAL
recovery; return 503 while recovering, failed or draining. Liveness does not
prove storage health.

Never forward credentials across redirects: redirects are errors. API keys,
vectors, metadata, names, record IDs, filesystem paths and request bodies must
not appear in error text or access logs. Generate a UUID `X-Request-ID` for each
request, echo it in typed errors, and send `Cache-Control: no-store` on API
responses. Public liveness has the same cache policy. Disable request/response
body logging at the proxy as well.

UUID fields use the canonical lowercase, hyphenated representation, including
request IDs and optional creation tokens.

## Wire values and compatibility

All messages are UTF-8 JSON objects without a BOM. Requests use
`Content-Type: application/json` (an optional UTF-8 charset is accepted) and no
Content-Encoding. Reject malformed UTF-8, duplicate JSON keys at every level,
unpaired surrogates, non-finite numbers, unknown request fields and invalid
types. Reject excess top-level message nesting beyond 16; the separate JSON-text
fields below allow the existing portable nesting limit of 64. Integer request
fields such as `dim` and `k` must be integer tokens, not `1.0`, booleans or strings.

Collection names and record IDs are always in JSON bodies. This preserves the
portable contract's names containing `/` and Unicode, empty record IDs and IDs
containing newline or NUL. Names are 1–255 UTF-8 bytes; IDs are 0–65,536 bytes.
Do not normalize Unicode or interpret numeric-looking IDs as numbers.

Wire records contain `id`, `vector` and **`metadata_json`**, a string containing
the JSON value. Missing input metadata defaults to the string `"null"`; responses
always include it. Searches use **`filter_json`**, either omitted/null or a
string containing a filter object. The string `"null"` is not a filter. This
encoding keeps uint64 values and structural `1` versus `1.0` intact even when
the surrounding HTTP message is parsed by JavaScript.

For example, the following is one record in a wire upsert request:

```json
{
  "id": "merchant/東京",
  "vector": [0.6, 0.8],
  "metadata_json": "{\"n\":18446744073709551615,\"shape\":{\"v\":1.0}}"
}
```

The public portable API still accepts and returns logical JSON metadata. Its
client codec must serialize/parse this inner JSON without losing 64-bit integers
or structural number kinds. JavaScript clients need a lossless representation
or an explicit JSON-text input/output API; ordinary `JSON.parse`/`JSON.stringify`
alone cannot promise that preservation. No implicit conversion of big integers
to strings. JSON text is not an authenticity or signature mechanism.

Metadata and filters each fit 1 MiB **both as supplied UTF-8 JSON text and in
portable canonical encoding**. Validate decoded content using the existing
portable rules: finite numbers, integer range `[-2^63, 2^64-1]`, valid Unicode,
depth at most 64. Whitespace counts toward the text limit. OpenAPI's
`contentMediaType`/`contentSchema` describe these strings; the conformance tests
explicitly decode and validate them because those annotations alone do not
assert their contents.

Vectors round once to float32; length equals collection `dim` (1–65,536).
Reject booleans, strings, non-finite/overflow coordinates, squared norms above
`float32.max/8`, and cosine squared norms below `float32.min_normal`. Int8 reads
return the stored dequantized representation, not the original embeddings.

Counts and commit sequences are canonical decimal **uint64 strings** on the
wire. Parse without IEEE-754 rounding and reject values above `2^64-1`. Unknown
response fields may be ignored; required fields, unsupported enums and malformed
success bodies must never silently turn into empty results. The checked Python
codecs in `portable/_wire.py` are private building blocks, not a remote client.

## Operations and collection generations

Data operations are POSTs to fixed action paths. Even reads use bodies so long
IDs and snapshot references do not enter URLs, logs or URI-length limits.

- `/v1/collections/list`: `{}` for the live view, or `{snapshot_id: ...}` for
  a lease. Returns complete descriptors sorted by Unicode scalar values. The
  first server caps collections at 10,000 (or a lower advertised deployment cap).
- `/v1/collections/create`: a `spec` with `name`, `dim`, optional `metric`
  (default `cosine`) and `quantization` (default `f32`). A new name returns 201.
  An existing name returns 409 `AlreadyExists` unless the creation-token rule
  below applies.
- `/v1/collections/describe`: logical `name` and optional snapshot. Returns
  the spec, durable opaque `identity` and optional `creation_token`.
- `/v1/collections/drop`: a `collection` reference with **both** `name` and
  expected `identity`. Missing names return 404; a replaced generation returns
  409 `Conflict`.
- `/v1/records/upsert`: the collection reference and up to 256 records. Validate
  the entire batch before mutation; duplicate IDs are invalid. An empty batch
  returns `written: 0` after validating the collection generation.
- `/v1/records/get` and `/v1/records/delete`: the reference and a logical ID.
  Missing records return 200 with `record: null` or `deleted: false` respectively.
  A missing collection still returns 404.
- `/v1/records/count`: an exact record count at the returned sequence.
- `/v1/records/page`: the reference, `limit` (1–256, default 100), optional cursor
  and snapshot. Returns detached records plus `next_cursor`.
- `/v1/search`: the reference, query vector, `k` (1–10,000, default 10), optional
  `filter_json`, `exact` (default false) and snapshot.
- `/v1/snapshots/create` and `/v1/snapshots/release`: acquire/release a consistent
  read-only view as described below.

Every operation after describe/create supplies the expected collection
generation. A normal restart/checkpoint preserves it; drop/recreate changes it,
even with the same spec. Validate generations inside the same serialized
operation as the actual read/write, so a stale handle cannot mutate its
replacement. Persist generation changes atomically with collection mutations;
do not infer a generation from the collection name or use an uncoordinated
sidecar file. Existing 0.3.0 embedded identities are not sufficient evidence
that this server rule is implemented.

An optional UUID `creation_token` is persisted with collection creation.
An explicit replay with the same name, token and effective spec returns 200
with the original generation and current durable commit sequence; a different
spec returns 409 `Conflict`. A different/missing token on an existing name
returns 409 `AlreadyExists`. Tokens are scoped to a database and name and
survive restart while that collection exists. This supports lost-acknowledgement
recovery during import; it is not an exactly-once request ledger. After a later
drop, the same token can create a new generation. Never replay automatically
or adopt a replacement based solely on a matching token.

Search/filter behavior follows [portable profile v1](portable-api.md): AND/OR/NOT,
missing versus null, object-only dotted paths and whole-value array/object
equality. Sort hits by `(distance, logical ID using Unicode scalar values)`;
distances are smaller-is-closer: `max(0, 1-cosine)`, squared L2, negative dot.
JavaScript's default UTF-16 string sort needs an explicit scalar comparator.
ANN candidates and top-k boundary ties can differ between backends. No partial
success flag or implicit filter fallback is allowed.

## Persistence, visibility and retries

The single-node server is the only writer of its database files. Refuse a
second server owner. External native writers must not share those files; stale
writer detection is a final error boundary, not coordination between servers.
All requests linearize through the server's operation boundary; describe,
generation checks, batches, reads and snapshot capture cannot observe an
uncommitted in-memory mutation.

A successful mutation response contains `commit: {durable: true, sequence: "..."}`
only after the engine's durable save completes. A read admitted after that
acknowledgement sees that commit or a later one. Snapshot reads deliberately
observe their captured sequence. No-op operations may return an unchanged
sequence. Commit order does not provide transactions spanning requests,
cross-client compare-and-swap, replication or protection against disk loss.

Validate the complete batch, persist as one commit and expose it as one change.
If save fails, recover the authoritative durable state before serving another
read. A lost response, server crash, storage error or deadline after admission
can leave the write committed even though the caller received an error.
Never equate a network timeout with rollback.

Typed errors include `write_outcome`:

- `not_applicable` for reads, discovery and snapshot lease operations;
- `not_committed` only when the server knows the mutation was rejected before
  touching durable state;
- `unknown` after an ambiguous submitted write. On an absent/malformed reply,
  clients also treat the outcome as unknown.

There are **no automatic mutation retries**, including on 429/503/504.
An upsert replay with the same IDs/content converges only if no other writer
changed those IDs. Replaying delete can remove a subsequent writer's replacement.
Describe/read back after lost create/drop acknowledgements and compare the
generation and token before deciding. Import journals must retain their existing
ownership, replay and read-back verification rules. No generic exactly-once or
transactional remote cutover promise.

The first server retains the engine's RAM and O(database-size) save-encoding
costs. A network API does not remove those limits. Incremental encoding and
automatic checkpoints remain 0.5.0 work.

## Deadlines, sizes and admission

`/v1/info` advertises effective limits. The starting defaults are: 64 MiB for
an encoded request or response, 10,000 ms timeout (maximum 30,000 ms), 8 active
operations, 32 queued operations and one snapshot with a 300,000 ms lease.
Deployment configuration can reduce the byte/collection/queue limits or change
the snapshot TTL within the documented schema bounds; logical portable caps
remain 256 records/page, 10,000 hits, 65,536 dimensions and 1 MiB metadata/filter.
`default_timeout_ms` must not exceed `max_timeout_ms`.

Optional `X-Recern-Timeout-Ms` requests a positive integer budget no greater than
the advertised maximum. Start the budget when headers arrive and include body
reading, waiting, execution and response preparation. Apply header/body/idle
socket deadlines and byte limits while reading, including chunked bodies. A
client must additionally impose its own total HTTP deadline, not merely a timeout
per socket read. No pagination/export-wide timeout is implied by one request.

Reject an expired queued mutation before execution. After a durable mutation
starts, let it complete or recover despite the deadline/disconnect; its outcome
can be unknown to the caller. CPU work that cannot be interrupted keeps its
operation slot until finished, even if a 504 has already been returned. Do not
spawn unbounded background work after timed-out requests. Shutdown stops new
admissions, drains active durable operations, and reports unready.

Reject oversized input with 413 `InvalidArgument`. Reject a search/list/other
complete response that cannot fit with 413 `Unsupported` and reason
`response_too_large`; do not silently truncate hits or lists. A page may contain
fewer records than its limit to stay within the cap, with a cursor after the
last returned record. If even one record cannot fit, return 413. A nonterminal
page must make progress and cannot be empty. Encode/measure bounded output
without first allocating an unbounded response.

Queue/snapshot exhaustion returns 429 `Unavailable` with an integer-seconds
`Retry-After`. This is a capacity hint, not permission to replay a write. Global
connection limits must also bound clients still sending headers/bodies; an
operation semaphore alone does not bound slow connections or memory use.

## Cursors, snapshot export and cleanup

Live page cursors are opaque, authenticated tokens bound to database identity,
server boot, collection generation, view, revision and position. They survive
neither restart nor mutations to the database. Malformed/forged tokens return
400 `InvalidArgument`; authentic stale/rebound tokens return 409 `Conflict`.
They are not durable migration checkpoints. Restart iteration after a conflict,
or use a consistent snapshot for a stable complete traversal.

Snapshot creation captures one committed sequence for all collections. Bound
the extra memory and snapshot count; snapshot capacity is separate from queue
capacity. Every read names its `snapshot_id`; collection generations come from
describe/list in that snapshot, including collections later dropped in the live
view. Snapshot pages remain stable under live mutations. Reusing a snapshot
cursor on the live view or another snapshot returns 409 `Conflict`.

The absolute lease deadline is returned as `expires_at`. There is no renewal
route in v1; long exports must fit the configured TTL or fail explicitly and
restart. Missing/expired/restart-lost snapshots return 404 `NotFound`. An operation
admitted before expiry may finish with its retained snapshot reference. Release
is idempotent (`released: false` when already absent); in-flight reads retain
their reference. An expired/released snapshot's memory still in use by a read
counts toward capacity until that read finishes.

Client `snapshot()` will wrap this lease in a read-only backend with
`snapshot_pages: true`; mutations fail locally as `PermissionDenied`. Public
portable metadata and export archive v1 stay unchanged. Use the existing
checksummed [export/import](migration.md) with snapshot pages and close the lease
in `finally`, including cancellation and failures. There is no arbitrary-path
backup endpoint or server-side filesystem access through this data API.

## Errors and implementation gates

The OpenAPI document defines exact statuses and response fields. Codes preserve
the eight existing portable exception categories. 400/405/408/413/415 describe
input or transport rejection; 401/403 permissions; 404 missing resources;
409 existing/conflicting state; 422 unsupported capability; 429 capacity;
500 corrupt/internal state; 503 availability; 504 deadline. A missing record
has a successful null result, not an error. Clients validate code/status pairing
and do not trust or expose free-form proxy/server response bodies.

Run the checked wire/schema cases alongside the shared portable dataset:

```sh
uv pip install --python crates/recern-vector-py/.venv/bin/python -r api/requirements.txt
crates/recern-vector-py/.venv/bin/python -m pytest api/tests -q
node api/tests/wire-roundtrip.mjs
```

Build the Python bindings first as described in their README. Tests validate
OpenAPI structure/references, encoded requests/responses, malformed input,
precision-preserving records/filters, UTF-8/number/size limits and typed errors.
Frozen file-format and export fixtures remain untouched. These are contract
and codec tests. `api/tests/test_server.py` additionally exercises the actual
server process when `RECERN_TEST_SERVER_BIN` points at the built executable.
The [server guide](server.md) describes tested restart/lost-ack boundaries and
the operational limits; no hosted deployment or remote-SDK migration is claimed.

The separate server package now includes persistent collection generations,
single-writer ownership, bounded HTTP/auth/deadlines, durable serialized operations
and snapshot leases. Next add the Python remote backend, followed by Rust/Node portable clients, and
run the same application and migration suite on embedded Recern, remote Recern
and Qdrant before publishing 0.4.0.
