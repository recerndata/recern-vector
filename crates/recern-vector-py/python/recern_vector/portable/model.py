"""Version 1 of the backend-independent data and error contract."""
from __future__ import annotations

import base64
import json
import math
import struct
from dataclasses import dataclass
from typing import Any, Literal

API_VERSION = 1
PROFILE = "recern-vector/1"
MAX_BATCH = 256
MAX_JSON_BYTES = 1_048_576
Metric = Literal["cosine", "l2", "dot"]


class PortableError(Exception):
    """No unsuccessful operation is represented as an empty successful result."""


class InvalidArgument(PortableError, ValueError):
    pass


class NotFound(PortableError, LookupError):
    pass


class AlreadyExists(PortableError):
    pass


class Conflict(PortableError):
    pass


class Unsupported(PortableError):
    pass


class Unavailable(PortableError):
    """A failed write may have committed; read it back or replay its logical IDs."""


class PermissionDenied(PortableError):
    pass


class CorruptData(PortableError):
    pass


@dataclass(frozen=True)
class CollectionSpec:
    name: str
    dim: int
    metric: Metric = "cosine"
    quantization: Literal["f32", "int8"] = "f32"

    def __post_init__(self):
        validate_text(self.name, "collection name", 255, nonempty=True)
        integer(self.dim, "dim", 1, 65_536)
        if self.metric not in ("cosine", "l2", "dot"):
            raise InvalidArgument("metric must be cosine, l2 or dot")
        if self.quantization not in ("f32", "int8"):
            raise InvalidArgument("quantization must be f32 or int8")


@dataclass(frozen=True)
class Record:
    id: str
    vector: list[float]
    metadata: Any = None


@dataclass(frozen=True)
class Hit:
    id: str
    distance: float
    metadata: Any = None


@dataclass(frozen=True)
class Page:
    records: list[Record]
    next_cursor: str | None


@dataclass(frozen=True)
class Capabilities:
    backend: str
    profile: str = PROFILE
    atomic_batches: bool = False
    consistent_export: bool = False
    quantization: tuple[str, ...] = ("f32",)
    max_batch: int = MAX_BATCH
    exact_search: bool = True
    # Qdrant pages observe the server at each request, not a global snapshot.
    snapshot_pages: bool = False


def integer(value, label, low, high):
    if type(value) is not int or not low <= value <= high:
        raise InvalidArgument(f"{label} must be an integer from {low} to {high}")
    return value


def validate_text(value, label="id", limit=65_536, *, nonempty=False):
    if not isinstance(value, str):
        raise InvalidArgument(f"{label} must be a string")
    try:
        size = len(value.encode("utf-8"))
    except UnicodeError as e:
        raise InvalidArgument(f"{label} must be valid Unicode") from e
    if size > limit or (nonempty and not size):
        raise InvalidArgument(f"{label} exceeds its size limit or is empty")
    return value


def json_value(value, depth=0):
    if depth > 64:
        raise InvalidArgument("JSON nesting exceeds 64")
    if value is None or type(value) is bool:
        return value
    if type(value) is str:
        return validate_text(value, "JSON string", MAX_JSON_BYTES)
    if type(value) is int:
        if not -(2**63) <= value <= 2**64 - 1:
            raise InvalidArgument("JSON integers must fit signed/unsigned 64 bits")
        return value
    if type(value) is float and math.isfinite(value):
        return value
    if type(value) is list:
        return [json_value(v, depth + 1) for v in value]
    if type(value) is dict:
        return {validate_text(k, "JSON key", MAX_JSON_BYTES): json_value(v, depth + 1) for k, v in value.items()}
    raise InvalidArgument("metadata must contain finite JSON values")


def dumps(value):
    return json.dumps(value, ensure_ascii=False, allow_nan=False, sort_keys=True, separators=(",", ":"))


def metadata_value(value):
    result = json_value(value)
    if len(dumps(result).encode("utf-8")) > MAX_JSON_BYTES:
        raise InvalidArgument("metadata exceeds 1 MiB")
    return result


def vector_value(vector, spec):
    try:
        values = list(vector)
        if len(values) != spec.dim:
            raise InvalidArgument(f"expected {spec.dim} dimensions, got {len(values)}")
        if any(isinstance(v, (bool, str, bytes)) for v in values):
            raise InvalidArgument("vectors must contain numbers")
        result = [struct.unpack("<f", struct.pack("<f", float(v)))[0] for v in values]
    except InvalidArgument:
        raise
    except (TypeError, ValueError, OverflowError, struct.error) as e:
        raise InvalidArgument("vector must contain finite float32 numbers") from e
    if not all(math.isfinite(v) for v in result):
        raise InvalidArgument("vector must contain finite float32 numbers")
    norm = math.fsum(v * v for v in result)
    # Bound both norms so squared-L2 and dot accumulations cannot overflow f32.
    if norm > 3.4028234663852886e38 / 8:
        raise InvalidArgument("vector magnitude exceeds the portable float32 limit")
    if spec.metric == "cosine" and norm < 1.1754943508222875e-38:
        raise InvalidArgument("cosine requires a nonzero, normal float32 norm")
    return result


def validated_records(records, spec):
    result, ids = [], set()
    try:
        records = iter(records)
    except TypeError as e:
        raise InvalidArgument("upsert_many expects an iterable of Record instances") from e
    for record in records:
        if len(result) == MAX_BATCH:
            raise InvalidArgument(f"a batch may contain at most {MAX_BATCH} records")
        if not isinstance(record, Record):
            raise InvalidArgument("upsert_many expects Record instances")
        id = validate_text(record.id)
        if id in ids:
            raise InvalidArgument("duplicate logical ID in one batch")
        ids.add(id)
        result.append(Record(id, vector_value(record.vector, spec), metadata_value(record.metadata)))
    return result


def encode_cursor(context, offset):
    return base64.urlsafe_b64encode(dumps([PROFILE, context, offset]).encode()).decode()


def decode_cursor(cursor, context):
    if cursor is None:
        return None
    try:
        if not isinstance(cursor, str) or len(cursor) > 4096:
            raise ValueError()
        profile, saved, offset = json.loads(base64.b64decode(cursor, altchars=b"-_", validate=True))
        if profile != PROFILE or saved != context:
            raise ValueError()
        return offset
    except (ValueError, TypeError, UnicodeError) as e:
        raise InvalidArgument("cursor belongs to another collection, revision or backend") from e
