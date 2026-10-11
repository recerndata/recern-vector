"""Qdrant REST adapter for profile v1; no optional client dependency required."""
from __future__ import annotations

import hashlib
from functools import wraps
import ipaddress
import json
import math
import re
import socket
import ssl
import urllib.error
import urllib.parse
import urllib.request
import uuid

from .filters import projection, qdrant_filter
from .model import (
    PROFILE, AlreadyExists, Capabilities, CollectionSpec, Conflict, CorruptData,
    Hit, InvalidArgument, NotFound, Page, PermissionDenied, Record, Unavailable,
    PortableError, Unsupported, decode_cursor, dumps, encode_cursor, metadata_value, validate_text,
)

# Frozen mapping: changing it requires a new envelope/profile version.
ID_NAMESPACE = uuid.UUID("c6c97780-86c3-4d47-a0f4-d956ccacb498")
MAX_HTTP_BYTES = 64 * 1024 * 1024
METRICS = {"cosine": "Cosine", "l2": "Euclid", "dot": "Dot"}


def point_id(id):
    return str(uuid.uuid5(ID_NAMESPACE, id))


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def _protocol(operation):
    @wraps(operation)
    def checked(*args, **kwargs):
        try:
            return operation(*args, **kwargs)
        except PortableError:
            raise
        except (KeyError, ValueError, TypeError, IndexError) as e:
            raise CorruptData("Qdrant returned an unexpected response shape") from e
    return checked


class QdrantBackend:
    def __init__(self, url, *, api_key, timeout, namespace, ca_file):
        if not isinstance(url, str):
            raise InvalidArgument("Qdrant URL must be a string")
        try:
            parts = urllib.parse.urlsplit(url)
            parts.port
        except ValueError as e:
            raise InvalidArgument("invalid Qdrant URL") from e
        if any(ord(c) <= 32 or ord(c) == 127 for c in url):
            raise InvalidArgument("Qdrant URL cannot contain whitespace or control characters")
        if parts.scheme not in ("http", "https") or not parts.hostname or parts.username or parts.password or parts.query or parts.fragment:
            raise InvalidArgument("use an HTTP(S) base URL without credentials, query or fragment")
        if not isinstance(namespace, str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,32}", namespace):
            raise InvalidArgument("namespace must contain 1-32 ASCII letters, numbers, underscores or hyphens")
        if isinstance(timeout, bool) or not isinstance(timeout, (int, float)) or not math.isfinite(timeout) or not 0 < timeout <= 300:
            raise InvalidArgument("timeout must be between 0 and 300 seconds")
        if api_key is not None:
            validate_text(api_key, "api_key", 8192, nonempty=True)
            if any(not 33 <= ord(c) <= 126 for c in api_key):
                raise InvalidArgument("invalid API key")
            try:
                local = ipaddress.ip_address(parts.hostname).is_loopback
            except ValueError:
                local = parts.hostname == "localhost"
            if parts.scheme == "http" and not local:
                raise InvalidArgument("an API key requires HTTPS outside loopback")
        self.url, self.timeout, self.namespace = url.rstrip("/"), float(timeout), namespace
        self.prefix = "rv1_" + namespace + "_"
        self.headers = {"Content-Type": "application/json", "User-Agent": "recern-vector-portable/1"}
        if api_key is not None:
            self.headers["api-key"] = api_key
        try:
            context = ssl.create_default_context(cafile=ca_file)
        except (OSError, ssl.SSLError) as e:
            raise InvalidArgument("unable to load TLS trust certificates") from e
        self.opener = urllib.request.build_opener(_NoRedirect(), urllib.request.HTTPSHandler(context=context))
        self.identity = "qdrant:" + hashlib.sha256((self.url + "\0" + namespace).encode()).hexdigest()
        self.capabilities = Capabilities("qdrant")
        version = self._request("GET", "/", envelope=False).get("version", "")
        match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", version) if isinstance(version, str) else None
        if not match or tuple(map(int, match.groups())) < (1, 19, 2) or int(match[1]) != 1:
            raise Unsupported("portable Qdrant requires server 1.19.2 or a compatible later 1.x version")
        self.server_version = version

    def _request(self, method, route, body=None, *, envelope=True):
        data = None if body is None else dumps(body).encode("utf-8")
        if data is not None and len(data) > MAX_HTTP_BYTES:
            raise InvalidArgument("request exceeds 64 MiB; reduce the batch size")
        request = urllib.request.Request(self.url + route, data=data, headers=self.headers, method=method)
        try:
            with self.opener.open(request, timeout=self.timeout) as response:
                raw = response.read(MAX_HTTP_BYTES + 1)
        except urllib.error.HTTPError as e:
            category = {400: InvalidArgument, 401: PermissionDenied, 403: PermissionDenied,
                        404: NotFound, 409: AlreadyExists, 422: InvalidArgument}.get(e.code, Unavailable)
            # Do not expose API keys, a proxy response body or the request data.
            raise category(f"Qdrant returned HTTP {e.code}") from None
        except (urllib.error.URLError, TimeoutError, OSError, socket.timeout) as e:
            raise Unavailable("Qdrant request failed or timed out; a write may already have committed") from e
        if len(raw) > MAX_HTTP_BYTES:
            raise Unavailable("Qdrant response exceeds 64 MiB; reduce the page size")
        try:
            value = json.loads(raw, parse_constant=lambda x: (_ for _ in ()).throw(ValueError(x)))
            if type(value) is not dict:
                raise ValueError()
            if not envelope:
                return value
            if value.get("status") != "ok" or "result" not in value:
                raise ValueError()
            return value["result"]
        except (ValueError, UnicodeError) as e:
            raise CorruptData("invalid Qdrant response") from e

    def _physical(self, name):
        return self.prefix + hashlib.sha256(name.encode("utf-8")).hexdigest()

    def _route(self, name):
        return "/collections/" + self._physical(name)

    @_protocol
    def _details(self, name):
        result = self._request("GET", self._route(name))
        try:
            config = result["config"]
            marker = config["metadata"]["recern"]
            if marker["profile"] != PROFILE or marker["name"] != name or marker["namespace"] != self.namespace:
                raise ValueError()
            uuid.UUID(marker["generation"])
            vectors = config["params"]["vectors"]
            metric = next(k for k, v in METRICS.items() if v == vectors["distance"])
            if vectors.get("datatype", "float32") != "float32" or config.get("quantization_config"):
                raise ValueError()
            return CollectionSpec(name, vectors["size"], metric), marker["generation"]
        except (KeyError, TypeError, ValueError, StopIteration) as e:
            raise Unsupported("collection is not an unmodified portable v1 collection") from e

    def describe(self, name):
        return self._details(name)

    @_protocol
    def list_collections(self):
        result = self._request("GET", "/collections")
        names = []
        for item in result["collections"]:
            # Namespaces can prefix one another ("app" and "app_import").
            # Match the fixed-length hash suffix, not just startswith(prefix).
            if re.fullmatch(re.escape(self.prefix) + r"[0-9a-f]{64}", item["name"]):
                info = self._request("GET", "/collections/" + urllib.parse.quote(item["name"], safe=""))
                try:
                    name = info["config"]["metadata"]["recern"]["name"]
                    validate_text(name, "collection name", 255, nonempty=True)
                    if item["name"] != self._physical(name):
                        raise ValueError()
                    self._details(name)
                except (KeyError, TypeError, ValueError) as e:
                    raise CorruptData("invalid portable collection metadata") from e
                names.append(name)
        return sorted(names)

    def create(self, spec, generation=None):
        if spec.quantization != "f32":
            raise Unsupported("portable Qdrant collections store float32 vectors")
        generation = generation or str(uuid.uuid4())
        self._request("PUT", self._route(spec.name), {
            "vectors": {"size": spec.dim, "distance": METRICS[spec.metric], "datatype": "float32"},
            "metadata": {"recern": {"profile": PROFILE, "name": spec.name,
                                     "namespace": self.namespace, "generation": generation}},
        })
        return self.describe(spec.name)

    def drop(self, name):
        self._details(name)
        self._request("DELETE", self._route(name))

    def _payload(self, point):
        try:
            payload = point["payload"]["_recern"]
            if payload["profile"] != PROFILE:
                raise ValueError()
            id = validate_text(payload["id"])
            if str(point["id"]) != point_id(id):
                raise ValueError()
            metadata = metadata_value(json.loads(payload["metadata"]))
            return id, metadata
        except (KeyError, TypeError, ValueError) as e:
            raise CorruptData("point has an invalid portable envelope or logical ID") from e

    def _record(self, point):
        id, metadata = self._payload(point)
        vector = point.get("vector")
        if not isinstance(vector, list) or not all(type(v) in (int, float) and math.isfinite(v) for v in vector):
            raise CorruptData("point has no valid dense vector")
        return Record(id, vector, metadata)

    def _retrieve(self, name, ids, *, vectors=True):
        return self._request("POST", self._route(name) + "/points?consistency=all", {
            "ids": ids, "with_payload": True, "with_vector": vectors,
        })

    def _ack(self, result):
        if not isinstance(result, dict) or result.get("status") != "completed":
            raise Unavailable("Qdrant did not acknowledge completion; read back or replay the logical IDs")

    @_protocol
    def upsert(self, name, records):
        self._details(name)
        expected = {}
        for r in records:
            mapped = point_id(r.id)
            if mapped in expected and expected[mapped] != r.id:
                raise Conflict("logical IDs collide in the Qdrant point mapping")
            expected[mapped] = r.id
        for point in self._retrieve(name, list(expected), vectors=False):
            id, _ = self._payload(point)
            if expected[str(point["id"])] != id:
                raise Conflict("logical ID mapping would overwrite a different record")
        points = []
        for r in records:
            envelope = {"profile": PROFILE, "id": r.id, "metadata": dumps(r.metadata), **projection(r.metadata)}
            points.append({"id": point_id(r.id), "vector": r.vector, "payload": {"_recern": envelope}})
        result = self._request("PUT", self._route(name) + "/points?wait=true&ordering=strong", {"points": points})
        self._ack(result)
        return len(records)

    @_protocol
    def get(self, name, id):
        spec, _ = self._details(name)
        points = self._retrieve(name, [point_id(id)])
        if not points:
            return None
        record = self._record(points[0])
        if len(points) != 1 or len(record.vector) != spec.dim:
            raise CorruptData("Qdrant returned invalid record dimensions or count")
        if record.id != id:
            raise Conflict("logical ID mapping collision")
        return record

    def delete(self, name, id):
        if self.get(name, id) is None:
            return False
        self._ack(self._request("POST", self._route(name) + "/points/delete?wait=true&ordering=strong", {"points": [point_id(id)]}))
        return True

    @_protocol
    def count(self, name):
        self._details(name)
        result = self._request("POST", self._route(name) + "/points/count?consistency=all", {"exact": True})
        count = result["count"]
        if type(count) is not int or count < 0:
            raise CorruptData("Qdrant returned an invalid count")
        return count

    @_protocol
    def page(self, name, limit, cursor):
        spec, generation = self._details(name)
        context = [self.identity, name, generation]
        offset = decode_cursor(cursor, context)
        if offset is not None:
            try:
                uuid.UUID(offset)
            except (ValueError, TypeError, AttributeError) as e:
                raise InvalidArgument("invalid Qdrant page cursor") from e
        body = {"limit": limit, "with_payload": True, "with_vector": True}
        if offset is not None:
            body["offset"] = offset
        result = self._request("POST", self._route(name) + "/points/scroll?consistency=all", body)
        next_offset = result.get("next_page_offset")
        records = [self._record(p) for p in result["points"]]
        if len(records) > limit or any(len(r.vector) != spec.dim for r in records):
            raise CorruptData("Qdrant returned invalid page dimensions or count")
        if next_offset is not None:
            if not isinstance(next_offset, str):
                raise CorruptData("Qdrant returned a non-UUID page cursor")
            uuid.UUID(next_offset)
        return Page(records,
                    None if next_offset is None else encode_cursor(context, next_offset))

    @_protocol
    def search(self, name, vector, k, filter, exact):
        spec, _ = self._details(name)
        body = {"query": vector, "limit": k, "with_payload": True,
                "params": {"exact": exact}, "filter": qdrant_filter(filter)}
        result = self._request("POST", self._route(name) + "/points/query?consistency=all", body)
        hits = []
        for point in result["points"]:
            id, metadata = self._payload(point)
            score = point.get("score")
            if type(score) not in (int, float) or not math.isfinite(score):
                raise CorruptData("invalid Qdrant distance")
            distance = max(0.0, 1.0 - score) if spec.metric == "cosine" else score * score if spec.metric == "l2" else -score
            hits.append(Hit(id, distance, metadata))
        return hits
