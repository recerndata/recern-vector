import sys
from collections.abc import Iterable, Sequence
from os import PathLike
from pathlib import Path
from typing import Any, Literal, Self

if sys.version_info >= (3, 12):
    from collections.abc import Buffer
else:
    from typing_extensions import Buffer

Vector = Sequence[float] | Buffer
Metadata = dict[str, Any]

FORMAT_VERSION: int

class RecernVectorError(Exception): ...
class CorruptDatabaseError(RecernVectorError): ...

class Hit:
    id: str
    distance: float
    metadata: Metadata | None

class Record:
    id: str
    vector: list[float]
    metadata: Metadata | None

class SearchReport:
    hits: list[Hit]
    strategy: Literal["hnsw", "exact", "filtered_exact"]
    ef: int | None
    visited: int
    distance_computations: int
    filter_selectivity: float | None
    elapsed_ms: float

class RecallPoint:
    ef: int
    recall: float
    p50_ms: float
    p95_ms: float

class RecallReport:
    k: int
    sample: int
    exact_p50_ms: float
    points: list[RecallPoint]

class Database:
    """A database file. Changes stay in memory until `save()` or a clean `with` exit."""

    def __init__(self, path: str | PathLike[str]) -> None:
        """Opens an existing database file."""
    @staticmethod
    def create(path: str | PathLike[str]) -> Database: ...
    @staticmethod
    def open_or_create(path: str | PathLike[str]) -> Database: ...
    @property
    def path(self) -> Path: ...
    def create_collection(
        self,
        name: str,
        dim: int,
        metric: Literal["cosine", "l2", "dot"] = "cosine",
        m: int = 16,
        ef_construction: int = 200,
        ef_search: int = 64,
    ) -> Collection: ...
    def collection(self, name: str) -> Collection: ...
    def drop_collection(self, name: str) -> None: ...
    def collection_names(self) -> list[str]: ...
    def save(self) -> None: ...
    def __getitem__(self, name: str) -> Collection: ...
    def __contains__(self, name: str) -> bool: ...
    def __enter__(self) -> Self: ...
    def __exit__(self, exc_type: object, exc_value: object, traceback: object) -> bool: ...

class Collection:
    @property
    def name(self) -> str: ...
    @property
    def dim(self) -> int: ...
    @property
    def metric(self) -> Literal["cosine", "l2", "dot"]: ...
    def upsert(self, id: str, vector: Vector, metadata: Metadata | None = None) -> None: ...
    def upsert_many(
        self,
        ids: Sequence[str],
        vectors: Buffer | Iterable[Vector],
        metadatas: Iterable[Metadata | None] | None = None,
        *,
        threads: int | None = None,
    ) -> int: ...
    def delete(self, id: str) -> bool: ...
    def get(self, id: str) -> Record | None: ...
    def search(
        self,
        query: Vector,
        k: int = 10,
        *,
        ef: int | None = None,
        exact: bool = False,
        filter: Metadata | None = None,
    ) -> list[Hit]: ...
    def explain(
        self,
        query: Vector,
        k: int = 10,
        *,
        ef: int | None = None,
        exact: bool = False,
        filter: Metadata | None = None,
    ) -> SearchReport: ...
    def stats(self) -> dict[str, Any]: ...
    def estimate_recall(
        self,
        sample: int = 100,
        k: int = 10,
        ef_values: Sequence[int] = (16, 32, 64, 128, 256),
        seed: int = 42,
    ) -> RecallReport: ...
    def compact(self) -> int: ...
    def __len__(self) -> int: ...
    def __contains__(self, id: str) -> bool: ...
