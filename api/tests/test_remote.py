"""Portable Python remote backend against a real development server."""
import json
import os
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest
from recern_vector.portable import (
    Client, Record, Conflict, CorruptData, InvalidArgument, PermissionDenied,
    Unavailable, export_snapshot, import_snapshot,
)
from test_server import BIN, KEY, Server

pytestmark = pytest.mark.skipif(not BIN, reason="set RECERN_TEST_SERVER_BIN")


@pytest.fixture
def remote(tmp_path):
    server = Server(tmp_path / "http")
    try:
        with Client.remote(server.url, api_key=KEY) as client:
            yield server, client
    finally:
        server.stop()


def test_remote_generation_noops_restart_and_reconnect(remote):
    server, client = remote
    old = client.create_collection("merchant/東京", 2)
    old.upsert("id\n\0", [1, 0], {"u64": 2**64-1, "nested": [1.0]})
    expected = old.get("id\n\0")
    server.listen = server.url.removeprefix("http://")
    server.stop()
    server.start()
    assert old.get("id\n\0") == expected
    with Client.remote(server.url, api_key=KEY) as reconnected:
        assert reconnected.identity == client.identity
        assert reconnected.collection(old.name).identity == old.identity
        reconnected.drop_collection(old.name)
        new = reconnected.create_collection(old.name, 2)
        assert new.identity != old.identity
        for op in (lambda: old.upsert_many([]), lambda: old.get("missing"),
                   lambda: old.delete("missing"), old.count, lambda: old.upsert("bad", [1, 0])):
            with pytest.raises(Conflict):
                op()
        assert new.count() == 0


def test_remote_snapshot_lifetime_and_export_cleanup(remote, tmp_path):
    server, client = remote
    col = client.create_collection("docs", 2)
    col.upsert_many([Record(str(i), [1, i]) for i in range(4)])
    with client.snapshot() as snapshot:
        assert snapshot.capabilities.snapshot_pages
        frozen = snapshot.collection(col.name)
        first = frozen.page(limit=2)
        col.upsert("new", [1, 1])
        assert frozen.count() == 4
        assert len(frozen.page(limit=2, cursor=first.next_cursor).records) == 2
        with pytest.raises(PermissionDenied):
            frozen.upsert_many([])
    with pytest.raises(Unavailable):
        frozen.count()
    for i in range(3):
        archive = tmp_path / f"archive-{i}"
        export_snapshot(client, archive)
        with pytest.raises(Exception):
            export_snapshot(client, archive)  # Must free the acquired lease too.
        destination = Client.embedded(tmp_path / f"restored-{i}.rvec")
        assert import_snapshot(archive, destination)["verified"]
        assert destination.collection(col.name).count() == 5


def test_remote_snapshot_expiry(tmp_path):
    server = Server(tmp_path / "expiry", "--snapshot-ttl-ms", "1000")
    try:
        with Client.remote(server.url, api_key=KEY) as client:
            with client.snapshot() as snapshot:
                time.sleep(1.05)
                from recern_vector.portable import NotFound
                with pytest.raises(NotFound):
                    snapshot.list_collections()
    finally:
        server.stop()


def test_pending_import_rejects_foreign_empty_collection(remote, tmp_path, monkeypatch):
    _, destination = remote
    source = Client.embedded(tmp_path / "source.rvec")
    source.create_collection("docs", 2).upsert("id", [1, 0])
    export_snapshot(source, tmp_path / "archive")
    backend = destination._backend
    original = backend.create
    def lost_reply(spec, generation=None):
        original(spec, generation)
        raise Unavailable("lost acknowledgement")
    monkeypatch.setattr(backend, "create", lost_reply)
    with pytest.raises(Unavailable):
        import_snapshot(tmp_path / "archive", destination, state_path=tmp_path / "state")
    monkeypatch.setattr(backend, "create", original)
    destination.drop_collection("docs")
    destination.create_collection("docs", 2)
    with pytest.raises(Conflict):
        import_snapshot(tmp_path / "archive", destination, state_path=tmp_path / "state", resume=True)


def test_import_rechecks_database_identity_before_any_write(remote, tmp_path):
    server, destination = remote
    source = Client.embedded(tmp_path / "identity-source.rvec")
    source.create_collection("docs", 2).upsert("id", [1, 0])
    export_snapshot(source, tmp_path / "identity-archive")
    server.listen = server.url.removeprefix("http://")
    server.stop()
    server.database.unlink()
    wal = server.database.with_suffix(".rvec.wal")
    if wal.exists():
        wal.unlink()
    server.start()
    with pytest.raises(Conflict):
        import_snapshot(tmp_path / "identity-archive", destination, state_path=tmp_path / "identity-state")
    with Client.remote(server.url, api_key=KEY) as fresh:
        assert fresh.list_collections() == []
    assert not (tmp_path / "identity-state").exists()


@pytest.mark.parametrize("kind", ["redirect", "duplicate", "truncated", "request_id", "drip"])
def test_remote_transport_is_bounded_and_does_not_retry(remote, kind):
    _, real = remote
    # Discovery from a real server, then hostile proxy responses to the first
    # mutation. No API keys or proxy body text may escape through exceptions.
    info = real._backend.transport.request("/v1/info")
    seen = []
    request_id = str(uuid.uuid4())
    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            self.respond(json.dumps(info).encode())
        def respond(self, body, status=200, length=None):
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("X-Request-ID", request_id if kind != "request_id" or self.command == "GET" else "invalid")
            self.send_header("Content-Length", str(len(body) if length is None else length))
            if status == 302:
                self.send_header("Location", "/leak-key")
            self.end_headers()
            try:
                if kind == "drip" and self.command == "POST":
                    for b in body:
                        self.wfile.write(bytes([b])); self.wfile.flush(); time.sleep(.02)
                else:
                    self.wfile.write(body)
            except (OSError, BrokenPipeError):
                pass
        def do_POST(self):
            seen.append(self.path)
            self.rfile.read(int(self.headers["Content-Length"]))
            body = b'{"sensitive-proxy-detail":true}'
            if kind == "duplicate":
                body = b'{"collection":null,"collection":null}'
            self.respond(body, 302 if kind == "redirect" else 201,
                         len(body)+20 if kind == "truncated" else None)
        def log_message(self, *args):
            pass
    http = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=http.serve_forever, daemon=True); thread.start()
    try:
        with Client.remote(f"http://127.0.0.1:{http.server_port}", api_key=KEY, timeout=.15) as client:
            started = time.monotonic()
            with pytest.raises((Unavailable, CorruptData)) as caught:
                client.create_collection("private-name", 2)
            assert time.monotonic() - started < .8
            assert caught.value.write_outcome == "unknown"
            assert KEY not in str(caught.value) and "sensitive" not in str(caught.value)
            assert seen == ["/v1/collections/create"]
    finally:
        http.shutdown(); http.server_close(); thread.join()


@pytest.mark.parametrize("url", ["http://example.com", "https://user:pass@example.com", "http://localhost\n", "https://example.com?q=x"])
def test_remote_configuration_rejected_before_network(url):
    with pytest.raises(InvalidArgument):
        Client.remote(url, api_key=KEY)
