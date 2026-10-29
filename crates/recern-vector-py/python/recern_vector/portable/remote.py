"""Recern HTTP v1 backend. No redirects, mutation retries or implicit TLS bypass."""
from __future__ import annotations

import copy
import hashlib
import http.client
import ipaddress
import json
import math
import queue
import re
import ssl
import threading
import urllib.parse
import uuid
from functools import wraps

from . import _wire as wire
from .model import (
    PROFILE, Capabilities, CollectionSpec, CorruptData, Hit, InvalidArgument,
    PermissionDenied, PortableError, Unavailable, Unsupported, dumps, validate_text,
)


def _response(operation):
    @wraps(operation)
    def checked(self, *args, **kwargs):
        write = operation.__name__ in ("create", "drop", "upsert", "delete")
        try:
            return operation(self, *args, **kwargs)
        except PortableError as error:
            if not hasattr(error, "write_outcome"):
                error.write_outcome = ("not_committed" if isinstance(error, InvalidArgument)
                                       else "unknown") if write else "not_applicable"
            raise
        except (KeyError, ValueError, TypeError, IndexError, OverflowError) as exc:
            error = CorruptData("invalid Recern HTTP response; verify any submitted write")
            error.write_outcome = "unknown" if write else "not_applicable"
            raise error from exc
    return checked


def _token(value):
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,128}", value):
        raise CorruptData("invalid Recern HTTP identity")
    return value


def _response_text(value, label="id", limit=65_536, *, nonempty=False):
    try:
        return validate_text(value, label, limit, nonempty=nonempty)
    except InvalidArgument as exc:
        raise CorruptData("invalid text in HTTP response") from exc


def _capabilities(value, *, snapshot=False):
    if (value["profile"] != PROFILE or value["backend"] != "recern"
            or any(type(value[k]) is not bool for k in
                   ("atomic_batches", "consistent_export", "exact_search", "snapshot_pages"))
            or value["snapshot_pages"] is not snapshot
            or type(value["max_batch"]) is not int or not 1 <= value["max_batch"] <= 256
            or not isinstance(value["quantization"], list) or not value["quantization"]
            or any(q not in ("f32", "int8") for q in value["quantization"])):
        raise CorruptData("unsupported Recern HTTP capabilities")
    return Capabilities("recern", atomic_batches=value["atomic_batches"],
        consistent_export=value["consistent_export"], exact_search=value["exact_search"],
        snapshot_pages=snapshot, max_batch=value["max_batch"], quantization=tuple(value["quantization"]))


class _Transport:
    def __init__(self, url, api_key, timeout, ca_file):
        try:
            parts = urllib.parse.urlsplit(url)
            parts.port
            if (parts.scheme not in ("http", "https") or not parts.hostname
                    or parts.username is not None or parts.password is not None
                    or parts.query or parts.fragment or any(ord(c) <= 32 or ord(c) == 127 for c in url)):
                raise ValueError()
            try:
                local = ipaddress.ip_address(parts.hostname).is_loopback
            except ValueError:
                local = parts.hostname == "localhost"
            if parts.scheme == "http" and not local:
                raise ValueError()
        except (ValueError, TypeError, AttributeError) as exc:
            raise InvalidArgument("use HTTPS (or loopback HTTP) without URL credentials, query or fragment") from exc
        if (isinstance(timeout, bool) or not isinstance(timeout, (int, float))
                or not math.isfinite(timeout) or not 0 < timeout <= 300):
            raise InvalidArgument("timeout must be between 0 and 300 seconds")
        if not isinstance(api_key, str) or not 32 <= len(api_key) <= 256 or any(not 33 <= ord(c) <= 126 for c in api_key):
            raise InvalidArgument("api_key must contain 32-256 printable ASCII characters")
        try:
            self.context = ssl.create_default_context(cafile=ca_file)
        except (OSError, ssl.SSLError) as exc:
            raise InvalidArgument("unable to load TLS trust certificates") from exc
        self.url, self.parts, self.timeout = url.rstrip("/"), parts, float(timeout)
        self.headers = {"Content-Type": "application/json", "Accept": "application/json",
            "Authorization": "Bearer " + api_key, "User-Agent": "recern-vector-python/1",
            "X-Recern-Timeout-Ms": str(min(30_000, max(1, math.ceil(timeout * 1000))))}
        # The caller's deadline includes DNS, TLS, headers and a slowly trickled
        # body. A timed-out worker is never reused or retried. At most eight
        # daemon I/O workers can outlive callers (e.g. an OS DNS stall).
        self.slots = threading.BoundedSemaphore(8)
        self.lock, self.connections, self.closed = threading.Lock(), set(), False
        self.request_limit = self.response_limit = wire.MAX_HTTP_BYTES

    def close(self):
        with self.lock:
            self.closed = True
            for connection in self.connections:
                connection.close()

    def request(self, route, body=None, *, write=False):
        data = None
        if body is not None:
            buffer = bytearray()
            encoder = json.JSONEncoder(ensure_ascii=False, allow_nan=False, sort_keys=True, separators=(",", ":"))
            for chunk in encoder.iterencode(body):
                block = chunk.encode("utf-8")
                if len(buffer) + len(block) > self.request_limit:
                    raise InvalidArgument("request exceeds the advertised HTTP byte limit")
                buffer.extend(block)
            data = bytes(buffer)
        if not self.slots.acquire(blocking=False):
            error = Unavailable("too many unfinished HTTP requests")
            error.write_outcome = "not_committed" if write else "not_applicable"
            raise error
        result, cancelled = queue.Queue(maxsize=1), threading.Event()
        connection_type = http.client.HTTPSConnection if self.parts.scheme == "https" else http.client.HTTPConnection
        kwargs = {"timeout": self.timeout}
        if self.parts.scheme == "https":
            kwargs["context"] = self.context
        connection = connection_type(self.parts.hostname, self.parts.port, **kwargs)
        with self.lock:
            if self.closed:
                self.slots.release()
                raise Unavailable("HTTP client is closed")
            self.connections.add(connection)

        def perform():
            try:
                connection.request("GET" if body is None else "POST", self.parts.path.rstrip("/") + route,
                                   body=data, headers=self.headers)
                response = connection.getresponse()
                length = response.getheader("Content-Length")
                if length is not None and (not length.isdecimal() or int(length) > self.response_limit):
                    raise CorruptData("invalid or oversized HTTP response")
                if response.getheader("Content-Encoding", "identity") != "identity":
                    raise CorruptData("unsupported HTTP response encoding")
                raw = bytearray()
                while not cancelled.is_set():
                    block = response.read1(min(65536, self.response_limit + 1 - len(raw)))
                    if not block:
                        break
                    raw.extend(block)
                    if len(raw) > self.response_limit:
                        raise CorruptData("HTTP response exceeds byte limit")
                if cancelled.is_set():
                    raise TimeoutError()
                if length is not None and len(raw) != int(length):
                    raise CorruptData("truncated HTTP response")
                content_type = response.getheader("Content-Type", "").split(";", 1)[0].strip().lower()
                if content_type != "application/json":
                    raise CorruptData("HTTP response is not JSON")
                request_id = response.getheader("X-Request-ID")
                if not isinstance(request_id, str) or str(uuid.UUID(request_id)) != request_id:
                    raise CorruptData("invalid HTTP request identity")
                if response.status not in (200, 201):
                    error = wire.error_from_wire(response.status, bytes(raw))
                    if error.request_id != request_id:
                        raise CorruptData("HTTP error identity mismatch")
                    raise error
                result.put((wire.loads_message(bytes(raw)), None))
            except BaseException as exc:
                result.put((None, exc))
            finally:
                connection.close()
                with self.lock:
                    self.connections.discard(connection)
                self.slots.release()

        threading.Thread(target=perform, daemon=True, name="recern-http").start()
        try:
            value, error = result.get(timeout=self.timeout)
        except queue.Empty:
            cancelled.set()
            connection.close()
            error, value = TimeoutError(), None
        if error is not None:
            if not isinstance(error, PortableError):
                error = Unavailable("Recern HTTP request failed or exceeded its total deadline")
            if not hasattr(error, "write_outcome"):
                error.write_outcome = "unknown" if write else "not_applicable"
            raise error from None
        return value


class RemoteBackend:
    @_response
    def __init__(self, url, *, api_key, timeout, ca_file):
        self.transport = _Transport(url, api_key, timeout, ca_file)
        self.bound = None
        self.descriptors = {}
        self.descriptor_lock = threading.Lock()
        self.lease = None
        self.read_only = False
        try:
            info = self.transport.request("/v1/info")
            if info["api_version"] != 1 or type(info["api_version"]) is not int or info["profile"] != PROFILE:
                raise Unsupported("server does not implement Recern HTTP v1")
            self.database_id = _token(info["database_id"])
            self.last_sequence = wire.uint64_from_wire(info["sequence"])
            self.capabilities = _capabilities(info["capabilities"])
            self.identity = "remote:" + hashlib.sha256((self.transport.url + "\0" + self.database_id).encode()).hexdigest()
            self.limits = info["limits"]
            for field in ("max_request_bytes", "max_response_bytes"):
                if type(self.limits[field]) is not int or not 4096 <= self.limits[field] <= wire.MAX_HTTP_BYTES:
                    raise CorruptData("invalid HTTP limits")
            self.transport.request_limit = self.limits["max_request_bytes"]
            self.transport.response_limit = self.limits["max_response_bytes"]
            max_ms = self.limits["max_timeout_ms"]
            if type(max_ms) is not int or not 1 <= max_ms <= 30_000:
                raise CorruptData("invalid HTTP timeout limit")
            self.transport.headers["X-Recern-Timeout-Ms"] = str(min(int(self.transport.headers["X-Recern-Timeout-Ms"]), max_ms))
        except BaseException:
            self.transport.close()
            raise

    def bind(self, name, spec, identity):
        result = copy.copy(self)
        result.bound = (spec, identity)
        with self.descriptor_lock:
            generation, token = self.descriptors.get(name, (None, None))
            result.creation_token = token if generation == identity else None
        return result

    @_response
    def verify_identity(self):
        from .model import Conflict
        info = self.transport.request("/v1/info")
        if info["api_version"] != 1 or info["profile"] != PROFILE:
            raise Unsupported("server does not implement Recern HTTP v1")
        if _token(info["database_id"]) != self.database_id:
            raise Conflict("server database was replaced; reopen the client before migrating")

    def _call(self, route, body, *, write=False):
        if self.lease is not None and self.lease["closed"]:
            raise Unavailable("snapshot has been released")
        if write and self.read_only:
            raise PermissionDenied("this snapshot is read-only")
        if self.lease is not None and not write:
            body = {**body, "snapshot_id": self.lease["id"]}
        value = self.transport.request(route, body, write=write)
        if not write:
            self.last_sequence = wire.uint64_from_wire(value["sequence"])
        return value

    def _ref(self, name):
        spec, identity = self.bound if self.bound is not None else self.describe(name)
        if spec.name != name:
            raise InvalidArgument("collection handle name mismatch")
        return {"name": name, "identity": identity}, spec

    def _ack(self, value):
        if value["commit"]["durable"] is not True:
            raise CorruptData("server did not acknowledge a durable write")
        self.last_sequence = wire.uint64_from_wire(value["commit"]["sequence"])

    @_response
    def list_collections(self):
        values = self._call("/v1/collections/list", {})["collections"]
        if not isinstance(values, list) or len(values) > 10000:
            raise CorruptData("invalid collection list")
        names = [_response_text(v["name"], "collection name", 255, nonempty=True) for v in values]
        if len(names) != len(set(names)):
            raise CorruptData("duplicate collections")
        return sorted(names)

    @_response
    def describe(self, name):
        value = self._call("/v1/collections/describe", {"name": name})["collection"]
        return self._descriptor(value, name)

    def _descriptor(self, value, name):
        try:
            spec = CollectionSpec(value["name"], value["dim"], value["metric"], value["quantization"])
        except InvalidArgument as exc:
            raise CorruptData("invalid collection descriptor") from exc
        if spec.name != name:
            raise CorruptData("collection response name mismatch")
        token = value["creation_token"]
        if token is not None and (not isinstance(token, str) or str(uuid.UUID(token)) != token):
            raise CorruptData("invalid collection creation token")
        identity = _token(value["identity"])
        with self.descriptor_lock:
            self.descriptors[name] = (identity, token)
        return spec, identity

    @_response
    def create(self, spec, generation=None):
        from dataclasses import asdict
        value = self._call("/v1/collections/create", {"spec": asdict(spec), "creation_token": generation}, write=True)
        self._ack(value)
        return self._descriptor(value["collection"], spec.name)

    @_response
    def drop(self, name):
        ref, _ = self._ref(name)
        self._ack(self._call("/v1/collections/drop", {"collection": ref}, write=True))

    @_response
    def upsert(self, name, records):
        ref, spec = self._ref(name)
        if len(records) > self.capabilities.max_batch:
            raise InvalidArgument("batch exceeds the advertised server limit")
        value = self._call("/v1/records/upsert", {"collection": ref,
            "records": [wire.record_to_wire(r, spec) for r in records]}, write=True)
        self._ack(value)
        if type(value["written"]) is not int or value["written"] != len(records):
            raise CorruptData("invalid upsert acknowledgement")
        return value["written"]

    @_response
    def get(self, name, id):
        ref, spec = self._ref(name)
        value = self._call("/v1/records/get", {"collection": ref, "id": id})["record"]
        record = None if value is None else wire.record_from_wire(value, spec)
        if record is not None and record.id != id:
            raise CorruptData("record response ID mismatch")
        return record

    @_response
    def delete(self, name, id):
        ref, _ = self._ref(name)
        value = self._call("/v1/records/delete", {"collection": ref, "id": id}, write=True)
        self._ack(value)
        if type(value["deleted"]) is not bool:
            raise CorruptData("invalid deletion acknowledgement")
        return value["deleted"]

    @_response
    def count(self, name):
        ref, _ = self._ref(name)
        return wire.uint64_from_wire(self._call("/v1/records/count", {"collection": ref})["count"])

    @_response
    def page(self, name, limit, cursor):
        from .model import Page
        ref, spec = self._ref(name)
        if cursor is not None:
            validate_text(cursor, "cursor", 4096, nonempty=True)
        value = self._call("/v1/records/page", {"collection": ref, "limit": limit, "cursor": cursor})
        if not isinstance(value["records"], list) or len(value["records"]) > limit:
            raise CorruptData("invalid record page")
        records = [wire.record_from_wire(v, spec) for v in value["records"]]
        next_cursor = value["next_cursor"]
        if next_cursor is not None:
            _response_text(next_cursor, "cursor", 4096, nonempty=True)
            if not records or next_cursor == cursor:
                raise CorruptData("page cursor did not advance")
        if len({r.id for r in records}) != len(records):
            raise CorruptData("duplicate records in page")
        return Page(records, next_cursor)

    @_response
    def search(self, name, query, k, filter, exact):
        ref, _ = self._ref(name)
        value = self._call("/v1/search", {"collection": ref, "query": query, "k": k,
            "filter_json": wire.filter_to_wire(filter), "exact": exact})
        if not isinstance(value["hits"], list) or len(value["hits"]) > k:
            raise CorruptData("invalid search response")
        hits = []
        for v in value["hits"]:
            id, distance = _response_text(v["id"]), v["distance"]
            if type(distance) not in (int, float) or not math.isfinite(distance):
                raise CorruptData("invalid search distance")
            try:
                metadata = wire.metadata_from_wire(v["metadata_json"])
            except InvalidArgument as exc:
                raise CorruptData("invalid search metadata") from exc
            hits.append(Hit(id, float(distance), metadata))
        if len({h.id for h in hits}) != len(hits):
            raise CorruptData("duplicate search results")
        return hits

    @_response
    def snapshot(self):
        if self.read_only:
            raise Unsupported("nested remote snapshots are not supported")
        value = self._call("/v1/snapshots/create", {})
        result = copy.copy(self)
        result.bound = None
        result.lease = {"id": _token(value["snapshot_id"]), "closed": False}
        result.read_only = True
        result.capabilities = _capabilities(value["capabilities"], snapshot=True)
        if value["database_id"] != self.database_id:
            raise CorruptData("snapshot database identity mismatch")
        return result

    def close(self):
        if self.lease is not None:
            if not self.lease["closed"]:
                try:
                    self.transport.request("/v1/snapshots/release", {"snapshot_id": self.lease["id"]})
                finally:
                    self.lease["closed"] = True
        else:
            self.transport.close()
