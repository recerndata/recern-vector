"""Private HTTP-v1 codecs; no network client or server is implemented here.

The JSON-text fields preserve int64/uint64 and structural integer/float kinds
across languages whose built-in JSON parser only exposes IEEE-754 doubles.
"""
from __future__ import annotations

import json
import math
import re
import uuid

from .filters import validate_filter
from .model import (
    AlreadyExists, CollectionSpec, Conflict, CorruptData, InvalidArgument,
    MAX_JSON_BYTES, NotFound, PermissionDenied, Record, Unavailable, Unsupported,
    dumps, metadata_value, validated_records,
)

HTTP_API_VERSION = 1
MAX_HTTP_BYTES = 64 * 1024 * 1024
ERROR_TYPES = {
    cls.__name__: cls for cls in (
        InvalidArgument, NotFound, AlreadyExists, Conflict, Unsupported,
        Unavailable, PermissionDenied, CorruptData,
    )
}
ERROR_STATUS_CODES = {
    400: ("InvalidArgument",), 401: ("PermissionDenied",),
    403: ("PermissionDenied",), 404: ("NotFound",),
    405: ("InvalidArgument",), 408: ("Unavailable",),
    409: ("AlreadyExists", "Conflict"),
    413: ("InvalidArgument", "Unsupported"), 415: ("InvalidArgument",),
    422: ("Unsupported",), 429: ("Unavailable",),
    500: ("CorruptData", "Unavailable"), 503: ("Unavailable",),
    504: ("Unavailable",),
}


def _pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def _integer(token):
    value = int(token)
    if not -(2**63) <= value <= 2**64 - 1:
        raise ValueError("integer outside the portable range")
    return value


def _float(token):
    value = float(token)
    if not math.isfinite(value):
        raise ValueError("non-finite JSON number")
    return value


def _constant(_):
    raise ValueError("non-JSON numeric constant")


def _loads(text):
    return json.loads(text, object_pairs_hook=_pairs, parse_int=_integer,
                      parse_float=_float, parse_constant=_constant)


def _json_text(text):
    try:
        if not isinstance(text, str) or len(text.encode("utf-8")) > MAX_JSON_BYTES:
            raise ValueError("JSON text must fit 1 MiB")
        # Validate decoded Unicode, types and depth too. The byte cap applies
        # to both original JSON text and its portable canonical encoding.
        return metadata_value(_loads(text))
    except (ValueError, TypeError, UnicodeError, RecursionError) as exc:
        raise InvalidArgument("invalid portable JSON text") from exc


def metadata_from_wire(text):
    return _json_text(text)


def filter_from_wire(text):
    if text is None:
        return None
    value = _json_text(text)
    if type(value) is not dict:
        raise InvalidArgument("filter_json must encode an object")
    return validate_filter(value)


def filter_to_wire(value):
    value = validate_filter(value)
    return None if value is None else dumps(value)


def record_to_wire(record: Record, spec: CollectionSpec):
    value = validated_records([record], spec)[0]
    return {"id": value.id, "vector": value.vector,
            "metadata_json": dumps(value.metadata)}


def record_from_wire(value, spec: CollectionSpec):
    try:
        if type(value) is not dict:
            raise ValueError()
        # Ignore additional response fields so additive server fields do not
        # break a client. Required fields may never silently receive defaults.
        record = Record(value["id"], value["vector"],
                        metadata_from_wire(value["metadata_json"]))
        return validated_records([record], spec)[0]
    except (KeyError, TypeError, ValueError) as exc:
        raise CorruptData("invalid HTTP record response") from exc


def uint64_from_wire(value):
    if (not isinstance(value, str)
            or re.fullmatch(r"0|[1-9][0-9]{0,19}", value) is None
            or int(value) > 2**64 - 1):
        raise CorruptData("invalid HTTP uint64 response")
    return int(value)


def loads_message(body: bytes, *, max_bytes=MAX_HTTP_BYTES):
    """Decode a received UTF-8 JSON object without normalizing numeric kinds.

    This checks message syntax and primitive values, not operation schemas.
    A transport must enforce the byte limit while reading, before this call.
    """
    try:
        if type(body) is not bytes or len(body) > max_bytes:
            raise ValueError()
        value = _loads(body.decode("utf-8"))
        if type(value) is not dict:
            raise ValueError()

        def check(item, depth=0):
            if depth > 16:
                raise ValueError()
            if type(item) is str:
                item.encode("utf-8")
            elif type(item) is dict:
                for key, child in item.items():
                    check(key, depth + 1)
                    check(child, depth + 1)
            elif type(item) is list:
                for child in item:
                    check(child, depth + 1)
        check(value)
        return value
    except (ValueError, TypeError, UnicodeError, RecursionError) as exc:
        raise CorruptData("invalid HTTP JSON message") from exc


def error_from_wire(status, body):
    """Map a typed server error without disclosing its free-form text.

    Callers must not retry mutations automatically. No response (or an invalid
    response) leaves the submitted write outcome unknown regardless of status.
    """
    try:
        value = loads_message(body)["error"]
        code, reason = value["code"], value["reason"]
        if code not in ERROR_STATUS_CODES.get(status, ()):
            raise ValueError()
        if (not isinstance(reason, str) or len(reason) > 64
                or re.fullmatch(r"[a-z][a-z0-9_]*", reason) is None):
            raise ValueError()
        message = value["message"]
        if not isinstance(message, str) or not 1 <= len(message) <= 512:
            raise ValueError()
        request_id = value["request_id"]
        if not isinstance(request_id, str) or str(uuid.UUID(request_id)) != request_id:
            raise ValueError()
        outcome = value["write_outcome"]
        if outcome not in ("not_applicable", "not_committed", "unknown"):
            raise ValueError()
        result = ERROR_TYPES[code](f"Recern HTTP {status}: {code}")
        result.request_id = request_id
        result.write_outcome = outcome
        return result
    except (KeyError, TypeError, ValueError) as exc:
        raise CorruptData("invalid HTTP error response; write outcome is unknown") from exc
