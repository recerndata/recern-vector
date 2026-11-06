"""Failure paths at the HTTP boundary; successful behavior uses real Qdrant."""
import json
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

from recern_vector.portable import Client, CorruptData, InvalidArgument, PermissionDenied, Unavailable, Unsupported


@pytest.fixture
def server():
    running = []
    def start(dispatch):
        seen = []
        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                seen.append(self.path)
                status, headers, body, delay = dispatch(self.path)
                time.sleep(delay)
                self.send_response(status)
                for name, value in headers.items():
                    self.send_header(name, value)
                self.end_headers()
                try:
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError):
                    pass
            def log_message(self, *args):
                pass
        http = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=http.serve_forever, daemon=True)
        thread.start()
        running.append((http, thread))
        return f"http://127.0.0.1:{http.server_port}", seen
    yield start
    for http, thread in running:
        http.shutdown()
        http.server_close()
        thread.join()


@pytest.mark.parametrize("status,error", [(401, PermissionDenied), (403, PermissionDenied), (429, Unavailable), (503, Unavailable)])
def test_http_errors_are_explicit_and_do_not_leak_bodies(server, status, error):
    url, seen = server(lambda _: (status, {}, b"sensitive proxy detail", 0))
    with pytest.raises(error) as caught:
        Client.qdrant(url, api_key="private-key")
    assert "sensitive" not in str(caught.value) and "private-key" not in str(caught.value)
    assert len(seen) == 1  # No implicit retry, including after ambiguous writes.


def test_credentials_are_not_forwarded_on_redirect(server):
    url, seen = server(lambda _: (302, {"Location": "/redirected"}, b"", 0))
    with pytest.raises(Unavailable):
        Client.qdrant(url, api_key="private-key")
    assert seen == ["/"]


def test_timeout_is_not_an_empty_success(server):
    url, seen = server(lambda _: (200, {}, b'{"version":"1.19.2"}', 0.2))
    with pytest.raises(Unavailable):
        Client.qdrant(url, timeout=0.03)
    assert seen == ["/"]


@pytest.mark.parametrize("body,error", [(b"broken", CorruptData), (b"[]", CorruptData),
    (b'{"version":null}', Unsupported), (b'{"version":"1.18.0"}', Unsupported)])
def test_invalid_or_unsupported_server_response(server, body, error):
    url, _ = server(lambda _: (200, {}, body, 0))
    with pytest.raises(error):
        Client.qdrant(url)


def test_malformed_success_is_not_an_empty_collection_list(server):
    def dispatch(path):
        value = {"version": "1.19.2"} if path == "/" else {"status": "ok", "result": None}
        return 200, {}, json.dumps(value).encode(), 0
    url, _ = server(dispatch)
    with pytest.raises(CorruptData):
        Client.qdrant(url).list_collections()


@pytest.mark.parametrize("url,key", [("http://remote.example", "key"), ("http://user:password@localhost", None),
    ("http://localhost:bad", None), ("http://localhost/?secret=key", None), ("http://localhost\n", None),
    ("http://localhost", "bad\nkey"), ("http://localhost", "bad☃key")])
def test_invalid_transport_configuration_rejected_before_network(url, key):
    with pytest.raises(InvalidArgument):
        Client.qdrant(url, api_key=key)


def test_qdrant_deadline_covers_a_slowly_streamed_body():
    requests = []
    class Drip(BaseHTTPRequestHandler):
        def do_GET(self):
            requests.append(self.path)
            self.send_response(200)
            self.end_headers()
            try:
                for byte in b'{"version":"1.19.2"}':
                    self.wfile.write(bytes([byte]))
                    self.wfile.flush()
                    time.sleep(0.03)  # Each byte is faster than the socket timeout.
            except (BrokenPipeError, ConnectionResetError):
                pass
        def log_message(self, *args):
            pass
    with ThreadingHTTPServer(('127.0.0.1', 0), Drip) as http:
        thread = threading.Thread(target=http.serve_forever, daemon=True)
        thread.start()
        try:
            started = time.monotonic()
            # A busy runner can hit the socket timeout before the caller's
            # total deadline. Both must retain the public error category and
            # bounded duration; diagnostic wording is not part of the API.
            with pytest.raises(Unavailable, match='deadline|timed out'):
                Client.qdrant(f'http://127.0.0.1:{http.server_port}', timeout=0.08)
            assert time.monotonic() - started < 0.5
            assert requests == ['/']
        finally:
            http.shutdown()
            thread.join()


def test_qdrant_total_deadline_bounds_a_blocked_transport(monkeypatch):
    """The total budget must work even when the transport never times out."""
    entered, release, finished = (threading.Event() for _ in range(3))
    requests = []

    class Response:
        sent = False

        def __enter__(self):
            return self

        def __exit__(self, *args):
            finished.set()

        def read1(self, size):
            if self.sent:
                return b''
            self.sent = True
            entered.set()
            # Fail safely if the caller deadline regresses: no hanging test.
            release.wait(2)
            return b'{"version":"1.19.2"}'

    class Opener:
        def open(self, request, timeout):
            requests.append(request.full_url)
            return Response()

    monkeypatch.setattr(urllib.request, 'build_opener', lambda *args: Opener())
    try:
        started = time.monotonic()
        with pytest.raises(Unavailable, match='total deadline'):
            Client.qdrant('http://127.0.0.1:6333', timeout=0.08)
        assert time.monotonic() - started < 1
        assert entered.wait(1)
        assert requests == ['http://127.0.0.1:6333/']
    finally:
        release.set()
    assert finished.wait(2)
