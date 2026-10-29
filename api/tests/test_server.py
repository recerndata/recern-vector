"""Real loopback HTTP and subprocess restart tests for the development server.

Set RECERN_TEST_SERVER_BIN to the built Rust executable. No deployed service or
portable remote SDK is implied by these lower-level HTTP tests.
"""
import concurrent.futures
import json
import math
import os
from pathlib import Path
import re
import socket
import subprocess
import struct
import time
import urllib.error
import urllib.request
import uuid
import zlib

import pytest
from test_http_contract import API, PORTABLE, resolve, validator

KEY = "recern-local-server-tests-only-000000000000"
BIN = os.environ.get("RECERN_TEST_SERVER_BIN")
pytestmark = pytest.mark.skipif(not BIN, reason="set RECERN_TEST_SERVER_BIN for real server integration tests")


class Server:
    def __init__(self, path, *options):
        self.path = path
        path.mkdir(exist_ok=True)
        self.database = path / "server.rvec"
        self.key_file = path / "key"
        self.key_file.write_bytes(KEY.encode() + b"\n")
        self.options = options
        self.process = None
        self.start()

    def start(self):
        self.log_path = self.path / "server.log"
        self.log = self.log_path.open("w", encoding="utf-8")
        self.process = subprocess.Popen([str(Path(BIN).resolve()), "--database", str(self.database),
            "--api-key-file", str(self.key_file), "--listen", getattr(self, "listen", "127.0.0.1:0"), *self.options],
            stdout=subprocess.DEVNULL, stderr=self.log)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            # Read through another file description. Seeking the same stderr
            # fd moves the child's write offset and can reorder split writes.
            match = re.search(r"listening on (127\.0\.0\.1:\d+)\r?\n", self.log_path.read_text(encoding="utf-8"))
            if match:
                self.url = "http://" + match[1]
                assert self.call("/health/ready")[0] == 200
                return
            if self.process.poll() is not None:
                raise AssertionError("server startup failed: " + self.log_path.read_text(encoding="utf-8"))
            time.sleep(.02)
        self.stop(kill=True)
        raise AssertionError("server did not start")

    def stop(self, kill=False):
        if self.process and self.process.poll() is None:
            self.process.kill() if kill else self.process.terminate()
            self.process.wait(timeout=10)
        if hasattr(self, "log"):
            text = self.log_path.read_text(encoding="utf-8")
            assert KEY not in text
            assert "merchant/東京" not in text
            assert "sensitive-metadata" not in text
            self.log.close()

    def call(self, path, data=None, *, key=KEY, headers=None, raw=None, method=None, schema=True):
        method = method or ("POST" if data is not None or raw is not None else "GET")
        payload = raw if raw is not None else None if data is None else json.dumps(data, ensure_ascii=False, allow_nan=False).encode()
        h = {"Content-Type": "application/json"}
        if key is not None:
            h["Authorization"] = "Bearer " + key
        h.update(headers or {})
        request = urllib.request.Request(self.url + path, data=payload, headers=h, method=method)
        try:
            response = urllib.request.urlopen(request, timeout=10)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            body = response.read()
            status, response_headers = response.status, dict(response.headers)
        value = json.loads(body)
        assert response_headers["cache-control"] == "no-store"
        request_id = response_headers["x-request-id"]
        assert str(uuid.UUID(request_id)) == request_id
        if schema and path in API["paths"] and method.lower() in API["paths"][path]:
            operation = API["paths"][path][method.lower()]
            expected = resolve(operation["responses"][str(status)])
            validator(expected["content"]["application/json"]["schema"]).validate(value)
        if status >= 400:
            assert value["error"]["request_id"] == request_id
            assert KEY not in body.decode()
            assert "sensitive" not in body.decode()
        return status, value, response_headers

    def create(self, name="merchant/東京", metric="cosine", quantization="f32", token=None):
        status, result, _ = self.call("/v1/collections/create", {"spec": {"name": name, "dim": 2,
            "metric": metric, "quantization": quantization}, "creation_token": token})
        assert status in (200, 201), result
        return {"name": name, "identity": result["collection"]["identity"]}, result

    def upsert(self, collection, records):
        return self.call("/v1/records/upsert", {"collection": collection, "records": records})


@pytest.fixture
def server(tmp_path):
    server = Server(tmp_path)
    try:
        yield server
    finally:
        server.stop()


def wire_records():
    return [{"id": r["id"], "vector": r["vector"], "metadata_json": json.dumps(r["metadata"], ensure_ascii=False)} for r in PORTABLE["records"]]


def test_auth_discovery_and_contract_routes(server):
    assert server.call("/health/live", key=None)[0] == 200
    for path in ("/health/ready", "/v1/info", "/v1/openapi.json"):
        for key in (None, "incorrect-test-key-00000000000000000000"):
            status, result, headers = server.call(path, key=key)
            assert status == 401 and result["error"]["code"] == "PermissionDenied"
            assert headers["www-authenticate"] == "Bearer"
    assert server.call("/v1/openapi.json")[1] == API
    info = server.call("/v1/info")[1]
    assert info["profile"] == "recern-vector/1"
    assert info["capabilities"]["atomic_batches"]
    assert info["server_version"].startswith("0.4.0-dev")
    assert server.call("/v1/info?key=ignored", schema=False)[0] == 400
    assert server.call("/missing", schema=False)[0] == 404
    status, _, headers = server.call("/v1/search", method="GET", schema=False)
    assert status == 405 and headers["allow"] == "POST"


@pytest.mark.parametrize("metric", ["cosine", "l2", "dot"])
def test_actual_server_matches_shared_portable_filter_and_record_contract(server, metric):
    collection, created = server.create(metric=metric)
    assert created["commit"]["durable"]
    assert server.upsert(collection, wire_records())[1]["written"] == len(PORTABLE["records"])
    info = server.call("/v1/info")[1]
    names = server.call("/v1/collections/list", {})[1]["collections"]
    assert [d["name"] for d in names] == [collection["name"]]
    for case in PORTABLE["filters"]:
        status, result, _ = server.call("/v1/search", {"collection": collection, "query": [.6,.8], "k": 20,
            "filter_json": json.dumps(case["filter"]), "exact": True})
        assert status == 200, result
        assert {h["id"] for h in result["hits"]} == set(case["ids"])
        assert result["sequence"] == info["sequence"]
        for hit in result["hits"]:
            expected = next(r for r in PORTABLE["records"] if r["id"] == hit["id"])
            assert json.loads(hit["metadata_json"]) == expected["metadata"]
            vector = expected["vector"]
            if metric == "cosine":
                norm = math.sqrt(sum(v*v for v in vector))
                distance = max(0, 1 - sum(x/norm*y for x,y in zip(vector,[.6,.8])))
            elif metric == "l2":
                distance = sum((x-y)**2 for x,y in zip(vector,[.6,.8]))
            else:
                distance = -sum(x*y for x,y in zip(vector,[.6,.8]))
            assert hit["distance"] == pytest.approx(distance, abs=2e-6)
    seen, cursor = [], None
    while True:
        status, result, _ = server.call("/v1/records/page", {"collection": collection, "limit": 2, "cursor": cursor})
        assert status == 200
        seen.extend(r["id"] for r in result["records"])
        cursor = result["next_cursor"]
        if cursor is None:
            break
    assert len(seen) == len(set(seen)) == len(PORTABLE["records"])
    assert set(seen) == {r["id"] for r in PORTABLE["records"]}
    for record in PORTABLE["records"]:
        value = server.call("/v1/records/get", {"collection": collection, "id": record["id"]})[1]["record"]
        assert json.loads(value["metadata_json"]) == record["metadata"]
    for bad in PORTABLE["invalid_filters"]:
        assert server.call("/v1/search", {"collection": collection, "query": [1,0], "filter_json": json.dumps(bad)})[0] == 400


def test_validation_is_atomic_and_errors_do_not_echo_data(server):
    collection, _ = server.create()
    for bad in [{"id": "bad", "vector": [1]}, {"id": "bad", "vector": [0,0]},
                {"id": "bad", "vector": [True,1]}, {"id": "bad", "vector": [1e20,0]},
                {"id": "bad", "vector": [1,0], "metadata_json": '{"x":18446744073709551616}'},
                {"id": "bad", "vector": [1,0], "metadata_json": '{"x":1,"x":2}'}]:
        assert server.upsert(collection, [{"id":"sensitive-metadata","vector":[1,0]}, bad])[0] == 400
        assert server.call("/v1/records/count", {"collection":collection})[1]["count"] == "0"
    assert server.upsert(collection,[{"id":"same","vector":[1,0]}]*2)[0] == 400
    assert server.upsert(collection,[])[1]["written"] == 0
    assert server.call("/v1/records/get", {"collection":collection,"id":"missing"})[1]["record"] is None
    assert server.call("/v1/records/delete", {"collection":collection,"id":"missing"})[1]["deleted"] is False
    for raw in (b'{"x":1,"x":2}', b'{"x":NaN}', b'{"x":"\xff"}', b'\xef\xbb\xbf{}'):
        assert server.call("/v1/search", raw=raw)[0] == 400
    assert server.call("/v1/search", {}, headers={"Content-Type":"text/plain"})[0] == 415
    assert server.call("/v1/search", {}, headers={"Content-Encoding":"gzip"})[0] == 415
    assert server.call("/v1/search", {}, headers={"X-Recern-Timeout-Ms":"1.0"})[0] == 400
    assert server.call("/v1/search", {}, headers={"X-Recern-Timeout-Ms":"30001"})[0] == 400
    assert server.call("/v1/collections/create", raw=b'{"spec":{"name":"float","dim":2.0}}')[0] == 400
    assert server.call("/v1/records/count", {"collection":{"name":"界"*86,"identity":"x"}})[0] == 400
    assert server.call("/v1/records/count", {"collection":{"name":"docs","identity":"bad token"}})[0] == 400
    assert server.call("/v1/collections/list", {"snapshot_id":"bad token"})[0] == 400
    assert server.call("/v1/collections/list", raw=iter([b'{', b'}']))[0] == 200


def test_int8_exact_search_is_over_the_stored_representation(server):
    collection,_ = server.create(quantization="int8")
    assert server.upsert(collection,[{"id":"q","vector":[.2,.8],"metadata_json":"[1,1.0]"}])[0] == 200
    stored = server.call('/v1/records/get',{'collection':collection,'id':'q'})[1]['record']
    assert type(json.loads(stored['metadata_json'])[0]) is int
    assert type(json.loads(stored['metadata_json'])[1]) is float
    result = server.call('/v1/search',{'collection':collection,'query':[.6,.8],'exact':True})[1]['hits'][0]
    expected = max(0,1-sum(x*y for x,y in zip(stored['vector'],[.6,.8])))
    assert result['distance'] == pytest.approx(expected,abs=2e-6)


def test_generation_and_acknowledgements_survive_crash_and_wal_recovery(server):
    token = str(uuid.uuid4())
    collection, created = server.create(token=token)
    assert server.create(token=token)[1]["collection"]["identity"] == collection["identity"]
    status, acknowledgement, _ = server.upsert(collection,[{"id":"1","vector":[1,0],"metadata_json":'{"n":18446744073709551615,"obj":{"v":1.0}}'}])
    assert status == 200 and acknowledgement["commit"]["durable"]
    before = server.call("/v1/info")[1]
    server.stop(kill=True)
    # A crash may leave a partial trailing WAL frame; it must not erase the
    # acknowledged commit before it. No frozen fixture is modified.
    with Path(str(server.database)+".wal").open("ab") as wal:
        length = struct.pack('<Q', 1000)
        wal.write(length + struct.pack('<I', zlib.crc32(length)) + b'partial-payload')
    server.start()
    after = server.call("/v1/info")[1]
    assert before["database_id"] == after["database_id"]
    assert before["sequence"] == after["sequence"]
    value = server.call("/v1/records/get", {"collection":collection,"id":"1"})[1]["record"]
    metadata = json.loads(value["metadata_json"])
    assert metadata["n"] == 2**64-1 and type(metadata["obj"]["v"]) is float
    assert server.create(token=token)[1]["collection"]["identity"] == collection["identity"]
    assert server.call("/v1/collections/drop", {"collection":collection})[0] == 200
    replacement,_ = server.create()
    assert replacement["identity"] != collection["identity"]
    assert server.upsert(collection,[{"id":"old","vector":[1,0]}])[0] == 409
    assert server.call("/v1/records/count", {"collection":replacement})[1]["count"] == "0"


def test_live_and_snapshot_cursors_and_lease_capacity(server):
    collection,_ = server.create()
    server.upsert(collection, wire_records())
    live = server.call("/v1/records/page", {"collection":collection,"limit":1})[1]["next_cursor"]
    snapshot = server.call("/v1/snapshots/create", {})[1]
    assert snapshot["capabilities"]["snapshot_pages"]
    lease = snapshot["snapshot_id"]
    assert server.call("/v1/snapshots/create", {})[0] == 429
    page = server.call("/v1/records/page", {"collection":collection,"snapshot_id":lease,"limit":1})[1]
    stable = page["next_cursor"]
    server.upsert(collection,[{"id":"new","vector":[1,0]}])
    assert server.call("/v1/records/page", {"collection":collection,"cursor":live})[0] == 409
    assert server.call("/v1/records/page", {"collection":collection,"cursor":stable})[0] == 409
    assert server.call("/v1/records/page", {"collection":collection,"snapshot_id":lease,"cursor":stable})[0] == 200
    assert server.call("/v1/records/count", {"collection":collection,"snapshot_id":lease})[1]["count"] == str(len(PORTABLE["records"]))
    assert server.call("/v1/records/page", {"collection":collection,"cursor":"not-authentic"})[0] == 400
    server.call("/v1/collections/drop", {"collection":collection})
    assert server.call("/v1/records/count", {"collection":collection,"snapshot_id":lease})[0] == 200
    assert server.call("/v1/snapshots/release", {"snapshot_id":lease})[1]["released"]
    assert not server.call("/v1/snapshots/release", {"snapshot_id":lease})[1]["released"]
    assert server.call("/v1/records/count", {"collection":collection,"snapshot_id":lease})[0] == 404


def test_cursors_from_a_previous_boot_are_conflicts(server):
    collection,_ = server.create()
    server.upsert(collection,wire_records())
    cursor = server.call("/v1/records/page", {"collection":collection,"limit":1})[1]["next_cursor"]
    server.stop(kill=True)
    server.start()
    assert server.call("/v1/records/page", {"collection":collection,"cursor":cursor})[0] == 409


def test_parallel_clients_are_serialized_without_lost_writes(server):
    collection,_ = server.create()
    def write(i):
        status,result,_ = server.upsert(collection,[{"id":str(i),"vector":[1,i/100]}])
        assert status == 200
        return int(result["commit"]["sequence"])
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        sequences = list(pool.map(write,range(24)))
    assert len(set(sequences)) == 24
    assert server.call("/v1/records/count", {"collection":collection})[1]["count"] == "24"


def test_owner_lock_and_loopback_policy(server):
    result = subprocess.run([BIN,"--database",str(server.database),"--api-key-file",str(server.key_file)],capture_output=True,timeout=10)
    assert result.returncode != 0 and b"database_owned" in result.stderr
    result = subprocess.run([BIN,"--database",str(server.path/"other.rvec"),"--api-key-file",str(server.key_file),"--listen","0.0.0.0:0"],capture_output=True,timeout=10)
    assert result.returncode != 0 and b"loopback" in result.stderr
    assert not (server.path/"other.rvec").exists()


def test_response_caps_paging_and_expired_leases(tmp_path):
    running = Server(tmp_path,"--max-request-bytes","4096","--max-response-bytes","4096","--snapshot-ttl-ms","1000")
    try:
        collection,_ = running.create()
        for i in range(8):
            assert running.upsert(collection,[{"id":str(i),"vector":[1,0],"metadata_json":json.dumps("x"*1000)}])[0] == 200
        status, page, _ = running.call("/v1/records/page", {"collection":collection,"limit":256})
        assert status == 200 and 0<len(page["records"])<8 and page["next_cursor"]
        assert running.call("/v1/search", {"collection":collection,"query":[1,0],"k":8,"exact":True})[0] == 413
        assert running.call("/v1/search", raw=b' '*4097)[0] == 413
        assert running.call("/v1/collections/list", raw=iter([b' '*2048]*3))[0] == 413
        assert running.call("/v1/openapi.json")[0] == 413
        lease = running.call("/v1/snapshots/create", {})[1]["snapshot_id"]
        time.sleep(1.05)
        assert running.call("/v1/records/count", {"collection":collection,"snapshot_id":lease})[0] == 404
        assert running.call("/v1/snapshots/create", {})[0] == 201
    finally:
        running.stop()


def test_lost_ack_does_not_produce_partial_batches(server):
    collection,_ = server.create()
    payload=json.dumps({"collection":collection,"records":[{"id":str(i),"vector":[1,0]} for i in range(128)]}).encode()
    host,port=server.url.removeprefix('http://').split(':')
    with socket.create_connection((host,int(port))) as conn:
        conn.sendall((f'POST /v1/records/upsert HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {KEY}\r\nContent-Type: application/json\r\nContent-Length: {len(payload)}\r\n\r\n').encode()+payload)
        # Close without reading an acknowledgement. The request may be rejected
        # or commit in full. A partial batch is never a valid recovered state.
    time.sleep(.1)
    server.stop(kill=True)
    server.start()
    count=int(server.call('/v1/records/count',{'collection':collection})[1]['count'])
    assert count in (0,128)


def test_incomplete_request_body_obeys_deadline(server):
    host,port=server.url.removeprefix('http://').split(':')
    with socket.create_connection((host,int(port)),timeout=3) as conn:
        conn.sendall((f'POST /v1/records/upsert HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {KEY}\r\nContent-Type: application/json\r\nX-Recern-Timeout-Ms: 50\r\nContent-Length: 1000\r\n\r\n{{').encode())
        reply=b''
        while data:=conn.recv(4096):
            reply+=data
    assert reply.startswith(b'HTTP/1.1 408')
    value=json.loads(reply.split(b'\r\n\r\n',1)[1])
    assert value['error']['write_outcome']=='not_committed'
