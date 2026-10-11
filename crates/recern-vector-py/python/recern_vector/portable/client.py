"""A synchronous v1 client. Backend configuration is separate from data logic."""
from __future__ import annotations

from collections.abc import Iterable, Iterator
from os import PathLike
from typing import Any, Literal

from .filters import validate_filter
from .model import (
    Capabilities, CollectionSpec, Hit, InvalidArgument, MAX_BATCH, Metric, Page, Record, Unsupported,
    integer, validate_text, validated_records, vector_value,
)


class Client:
    def __init__(self, backend):
        self._backend = backend

    @classmethod
    def embedded(cls, path: str | PathLike[str], *, read_only: bool = False) -> Client:
        from .embedded import EmbeddedBackend
        return cls(EmbeddedBackend(path, read_only=read_only))

    @classmethod
    def qdrant(cls, url: str, *, api_key: str | None = None, timeout: float = 10.0,
               namespace: str = "recern", ca_file: str | None = None) -> Client:
        from .qdrant import QdrantBackend
        return cls(QdrantBackend(url, api_key=api_key, timeout=timeout, namespace=namespace, ca_file=ca_file))

    @property
    def capabilities(self) -> Capabilities:
        return self._backend.capabilities

    @property
    def identity(self) -> str:
        """A destination identity; contains no credentials."""
        return self._backend.identity

    def list_collections(self) -> list[str]:
        return self._backend.list_collections()

    def create_collection(self, name: str, dim: int, metric: Metric = "cosine", *,
                          quantization: Literal["f32", "int8"] = "f32") -> Collection:
        spec = CollectionSpec(name, dim, metric, quantization)
        if quantization not in self.capabilities.quantization:
            raise Unsupported(f"{self.capabilities.backend} cannot store {quantization} through profile v1")
        self._backend.create(spec)
        return self.collection(name)

    def collection(self, name: str) -> Collection:
        validate_text(name, "collection name", 255, nonempty=True)
        spec, generation = self._backend.describe(name)
        return Collection(self, spec, generation)

    def drop_collection(self, name: str) -> None:
        validate_text(name, "collection name", 255, nonempty=True)
        self._backend.drop(name)

    def snapshot(self) -> Client:
        if not self.capabilities.consistent_export:
            raise Unsupported("this backend cannot supply a consistent portable export; use its native snapshot tools")
        return Client(self._backend.snapshot())


class Collection:
    def __init__(self, client: Client, spec: CollectionSpec, identity: str):
        self.client, self.spec, self.identity = client, spec, identity

    @property
    def name(self) -> str:
        return self.spec.name

    def upsert(self, id: str, vector: Iterable[float], metadata: Any = None) -> None:
        self.upsert_many([Record(id, vector, metadata)])

    def upsert_many(self, records: Iterable[Record]) -> int:
        records = validated_records(records, self.spec)
        if not records:
            return 0
        return self.client._backend.upsert(self.name, records)

    def get(self, id: str) -> Record | None:
        return self.client._backend.get(self.name, validate_text(id))

    def delete(self, id: str) -> bool:
        return self.client._backend.delete(self.name, validate_text(id))

    def count(self) -> int:
        return self.client._backend.count(self.name)

    def page(self, *, limit: int = 100, cursor: str | None = None) -> Page:
        integer(limit, "page limit", 1, MAX_BATCH)
        return self.client._backend.page(self.name, limit, cursor)

    def iter_records(self, *, batch_size: int = 100) -> Iterator[Record]:
        cursor = None
        while True:
            page = self.page(limit=batch_size, cursor=cursor)
            yield from page.records
            if page.next_cursor is None:
                break
            cursor = page.next_cursor

    def search(self, query: Iterable[float], k: int = 10, *,
               filter: dict[str, Any] | None = None, exact: bool = False) -> list[Hit]:
        integer(k, "k", 1, 10_000)
        if type(exact) is not bool:
            raise InvalidArgument("exact must be a boolean")
        query = vector_value(query, self.spec)
        filter = validate_filter(filter)
        hits = self.client._backend.search(self.name, query, k, filter, exact)
        return sorted(hits, key=lambda h: (h.distance, h.id))
