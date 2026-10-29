# Single-node server (0.4.0)

**Recern Vector 0.4.0.** Install `recern-vector-server` from crates.io or
use a verified native archive from the [0.4.0 release](release.md). Python,
Rust and Node portable clients use the [HTTP v1 contract](http-api.md).

```sh
cargo install recern-vector-server --version 0.4.0 --locked
```

The server provides authenticated collection/record operations, filtered vector
search, durable write acknowledgements, generation checks and consistent snapshot
leases. It wraps the same in-memory engine; data and indexes must fit in RAM,
and each save still encodes/compares the full database image. Snapshot creation
loads an additional committed database image, including the WAL.

## Build and run

Build from the repository root with Rust 1.89 or later:

```sh
cargo build --locked --release -p recern-vector-server
mkdir -p server-data
umask 077
openssl rand -hex 32 > server-data/api-key
target/release/recern-vector-server \
  --database server-data/server.rvec \
  --api-key-file server-data/api-key \
  --listen 127.0.0.1:7337
```

On Windows, use `target/release/recern-vector-server.exe` and create the key file
with an OS cryptographic random generator. The file contains 32–256 printable
ASCII bytes, optionally followed by LF or CRLF. Keep the key file private. Keys
do not appear in command-line arguments or server request logs.

The parent data directory must exist. A missing database is created; an empty
native database can be initialized. A nonempty arbitrary native `.rvec` is
refused instead of silently adopted. Migration from an embedded database is a
separate data transfer: use the Python remote backend and checked
[export/import](migration.md), available from this release.

The server accepts **loopback listeners only**, including `127.0.0.1:0` for an
OS-assigned development port. It does not terminate TLS. Put a verified HTTPS
reverse proxy in front for remote access. There is no anonymous mode for the
data API, no cookie authentication and no default browser CORS access.

## HTTP example

Only liveness is public:

```sh
curl http://127.0.0.1:7337/health/live
# {"status":"alive"}
```

This Python example uses the standard library and the key file. It is an HTTP
example. The [portable clients guide](portable-clients.md) shows the implemented
`Client.remote()` API:

```python
import json
from pathlib import Path
from urllib.request import Request, HTTPRedirectHandler, ProxyHandler, build_opener

key = Path("server-data/api-key").read_text().strip()
base = "http://127.0.0.1:7337"

class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None

opener = build_opener(NoRedirect(), ProxyHandler({}))

def call(path, body=None):
    payload = None if body is None else json.dumps(body).encode("utf-8")
    request = Request(base + path, data=payload, headers={
        "Authorization": "Bearer " + key,
        "Content-Type": "application/json",
        "X-Recern-Timeout-Ms": "10000",
    })
    with opener.open(request, timeout=15) as response:
        return json.load(response)

info = call("/v1/info")
assert info["profile"] == "recern-vector/1"
created = call("/v1/collections/create", {
    "spec": {"name": "documents", "dim": 2, "metric": "cosine"}
})
collection = {"name": "documents", "identity": created["collection"]["identity"]}
ack = call("/v1/records/upsert", {
    "collection": collection,
    "records": [{"id": "doc/one", "vector": [0.6, 0.8],
                 "metadata_json": json.dumps({"lang": "en"})}]
})
assert ack["commit"]["durable"] is True
result = call("/v1/search", {
    "collection": collection, "query": [0.6, 0.8], "k": 5,
    "filter_json": json.dumps({"lang": "en"}), "exact": True
})
print(result["hits"])
```

The returned generation must accompany subsequent data operations. Restart
preserves it; drop/recreate changes it. A stale reference returns 409 rather
than accessing the replacement collection. Metadata uses JSON text on the wire
to preserve large integers and structural integer/float distinctions. See the
contract for all field, filter, error and cursor rules.

Do not automatically replay failed writes. A timeout or lost response can leave
the write committed. The server keeps an admitted durable operation running
even after the HTTP caller receives 504 or disconnects. On save failure it
reloads durable state before another operation; failed recovery makes it unready.
Startup and recovery after a failed commit also perform a checkpoint/fsync barrier
before readiness, so a complete but previously unacknowledged WAL frame is not
mistaken for a new durable no-op acknowledgement. This is a recovery barrier;
periodic automatic checkpoint scheduling remains 0.5.0 work.

## Ownership, storage and shutdown

One server owns a database through its `.server-owner` advisory file lock.
An additional process using the same canonical path fails at startup. Symlink
aliases resolve to that path. Hard links, file replacement, network filesystems
and native writers bypassing the server are unsupported. The existing native
stale-writer check is retained as a final error boundary.

The server keeps its catalog and physical collections in **one format-2 database**.
Logical names map to opaque physical collection generations. Catalog updates and
user mutations share one native WAL commit, so a crash cannot leave a generation
in a separate, inconsistent metadata sidecar. The catalog also retains cursor
authentication material. Server-managed files contain these internal records;
do not inspect/edit them as if they were ordinary native application collections.
Workspace integration remains follow-up. Logical export through the remote SDK
is now available. Existing format-1/2 and migration-v1 fixtures are unchanged.

SIGINT and, on Unix, SIGTERM stop new admissions and drain started blocking
operations before releasing ownership. Native CPU/IO work cannot be forcibly
cancelled safely, so graceful shutdown has no fixed completion-time guarantee.
A forced kill relies on WAL recovery at the next open. Back up only a matching
snapshot/WAL pair with the server stopped; preserve the key configuration
separately. A live copy of only the `.rvec` file is not a consistent backup.

## Limits and TLS proxy

Defaults are 64 MiB per request/response, 10-second request budgets (at most
30 seconds), 8 active workers, 32 waiting operations, and one snapshot with a
5-minute absolute lease. Workers serialize access to the database. Timed-out
blocking workers retain their capacity until done; an expired queued write is
rejected before mutation. There are at most 64 accepted HTTP connections,
5-second header deadlines and a 45-second connection/response-drain bound.
HTTP/1.1 connections close after each response in this first implementation.

Flags include `--max-request-bytes`, `--max-response-bytes`, `--active-operations`,
`--queued-operations`, `--max-snapshots`, `--snapshot-ttl-ms` and
`--request-timeout-ms`. Request/response caps can be lowered to 4 KiB. A small
response cap can reject the complete OpenAPI document as well as large search
results. Pages make bounded progress and return a continuation cursor; complete
search/list responses are rejected instead of truncated. `/v1/info` reports the
effective limits.

Snapshot pages retain one committed sequence while live data changes. A snapshot
lease expires after its advertised deadline and can be released explicitly.
Restart loses leases and invalidates live cursors; an authentic cursor from an
earlier boot returns 409. There is no lease-renewal API. Expired/released views
still referenced by an in-flight read continue counting toward capacity.

A TLS proxy should preserve the Authorization header, disable request-body/access
logging for this API, and **never retry POSTs upstream**. For example, inside an
existing HTTPS virtual host (certificate and host configuration are operator-owned):

```nginx
location / {
    proxy_pass http://127.0.0.1:7337;
    proxy_http_version 1.1;
    proxy_set_header Authorization $http_authorization;
    proxy_request_buffering off;
    proxy_next_upstream off;
    client_max_body_size 64m;
    client_body_timeout 10s;
    proxy_send_timeout 35s;
    proxy_read_timeout 35s;
    access_log off;
}
```

This is deployment guidance, not a tested managed-service configuration. Use
verified certificates/private CAs in clients. No clustering, replication,
multi-tenant authorization, granular RBAC or hosted service is provided here.

## Validation and next work

Local tests cover the actual Rust HTTP process, not mocked success replies:
authentication, all shared portable filter cases across cosine/L2/dot, int8,
Unicode IDs, numeric precision, atomic validation, snapshots/cursors, sizes,
chunked bodies, concurrent clients, lost acknowledgements, a torn WAL tail and
restart after acknowledged writes. Rust tests additionally inject failed commit
and lost-ack boundaries and check queued/started write deadlines and shutdown
draining. Existing engine persistence/compatibility tests remain required.

```sh
cargo test --locked
cargo clippy --workspace --all-targets -- -D warnings
cargo build --locked -p recern-vector-server
RECERN_TEST_SERVER_BIN="$PWD/target/debug/recern-vector-server" \
  crates/recern-vector-py/.venv/bin/python -m pytest api/tests -q
```

Build the Python bindings and install `api/requirements.txt` for the schema and
test tools. CI runs the server HTTP suite on Linux, macOS and Windows. A workflow
configuration alone is not proof all platform jobs have passed; consult the
current commit's checks for that evidence.

The [portable Python, Rust and Node clients](portable-clients.md) run the
shared profile against embedded Recern, Recern HTTP and Qdrant. Release checks
exercise installed wheels, native CLI/server archives and Node tarballs, private-CA
TLS, upgrade from 0.3.0, lost acknowledgements and migration recovery.

See the [release and compatibility guide](release.md) for artifacts and limits.
