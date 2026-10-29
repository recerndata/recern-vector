"""Contract/codec checks, deliberately independent of a running HTTP server.

The portable library still needs its built native extension for the existing
engine/fixture checks. No mocks here are presented as server conformance.
"""
import copy
import json
from pathlib import Path

import pytest
from jsonschema import Draft202012Validator, FormatChecker, ValidationError
from openapi_spec_validator import validate as validate_openapi

from recern_vector.portable import Client, CollectionSpec, CorruptData, InvalidArgument, Record
from recern_vector.portable import _wire
from recern_vector.portable.filters import validate_filter
from recern_vector.portable.model import dumps, validated_records

ROOT = Path(__file__).resolve().parents[2]
API = json.loads((ROOT / "api/openapi.json").read_text(encoding="utf-8"))
CASES = json.loads((ROOT / "api/fixtures/http-v1.json").read_text(encoding="utf-8"))
PORTABLE = json.loads((ROOT / "crates/recern-vector/tests/fixtures/portable-v1.json").read_text(encoding="utf-8"))


def resolve(value):
    while "$ref" in value:
        assert value["$ref"].startswith("#/")
        target = API
        for part in value["$ref"][2:].split("/"):
            target = target[part.replace("~1", "/").replace("~0", "~")]
        value = target
    return value


def validator(schema):
    # References remain rooted at the complete OpenAPI components dictionary.
    return Draft202012Validator({**schema, "components": API["components"]}, format_checker=FormatChecker())


def validate(schema_name, instance):
    validator({"$ref": "#/components/schemas/" + schema_name}).validate(instance)


def assert_nested_wire_values(value):
    if isinstance(value, dict):
        for key, item in value.items():
            if key == "metadata_json":
                metadata = _wire.metadata_from_wire(item)
                validate("JsonValue", metadata)
            elif key == "filter_json":
                parsed = _wire.filter_from_wire(item)
                if parsed is not None:
                    validate("Filter", parsed)
            elif key in ("sequence", "count"):
                _wire.uint64_from_wire(item)
            else:
                assert_nested_wire_values(item)
    elif isinstance(value, list):
        for item in value:
            assert_nested_wire_values(item)


def test_openapi_is_valid_and_local_references_resolve():
    validate_openapi(API)

    def visit(value):
        if isinstance(value, dict):
            if "$ref" in value:
                resolve(value)
            for child in value.values():
                visit(child)
        elif isinstance(value, list):
            for child in value:
                visit(child)
    visit(API)
    for schema in API["components"]["schemas"].values():
        Draft202012Validator.check_schema(schema)


def test_every_operation_is_authenticated_bounded_and_has_typed_errors():
    assert API["security"] == [{"BearerAuth": []}]
    assert API["components"]["securitySchemes"]["BearerAuth"]["scheme"] == "bearer"
    operations = set()
    writes = set()
    for path, methods in API["paths"].items():
        # Keeping names and arbitrary IDs out of URL templates is intentional.
        assert "{" not in path
        for method, operation in methods.items():
            assert operation["operationId"] not in operations
            operations.add(operation["operationId"])
            assert operation.get("security", API["security"]) == ([] if path == "/health/live" else [{"BearerAuth": []}])
            assert any(resolve(p)["name"] == "X-Recern-Timeout-Ms" for p in operation["parameters"])
            if operation["x-write-operation"]:
                assert method == "post"
                writes.add(operation["operationId"])
                for status, response in operation["responses"].items():
                    if status.startswith("2"):
                        schema = resolve(resolve(response)["content"]["application/json"]["schema"])
                        assert "commit" in schema["required"]
            for status in API["x-error-status-codes"]:
                assert resolve(operation["responses"][status])["x-error-codes"] == API["x-error-status-codes"][status]
            for response in operation["responses"].values():
                response = resolve(response)
                assert "X-Request-ID" in response["headers"]
                assert response["headers"]["Cache-Control"]["schema"]["const"] == "no-store"
    assert writes == {"createCollection", "dropCollection", "upsertRecords", "deleteRecord"}
    assert operations == {case_operation(case)["operationId"] for case in CASES["cases"]}


def case_operation(case):
    return API["paths"][case["path"]][case["method"]]


@pytest.mark.parametrize("case", CASES["cases"], ids=lambda c: c["name"])
def test_wire_examples_match_their_http_operation(case):
    operation = case_operation(case)
    if case["request"] is not None:
        schema = operation["requestBody"]["content"]["application/json"]["schema"]
        validator(schema).validate(case["request"])
        assert_nested_wire_values(case["request"])
    response = resolve(operation["responses"][str(case["status"])])
    validator(response["content"]["application/json"]["schema"]).validate(case["response"])
    assert_nested_wire_values(case["response"])
    assert _wire.loads_message(dumps(case["response"]).encode("utf-8")) == case["response"]
    if case["status"] >= 400:
        error = _wire.error_from_wire(case["status"], dumps(case["response"]).encode())
        assert type(error).__name__ == case["response"]["error"]["code"]
        assert error.write_outcome == case["response"]["error"]["write_outcome"]


@pytest.mark.parametrize("case", CASES["invalid_requests"])
def test_unknown_request_fields_and_wrong_structures_are_rejected(case):
    with pytest.raises(ValidationError):
        validate(case["schema"], case["value"])


def test_http_and_portable_limits_error_categories_and_defaults_agree():
    assert API["x-contract-status"] == "draft"
    assert API["info"]["version"] == "0.4.0-draft.1"
    assert _wire.HTTP_API_VERSION == 1
    assert {int(k): tuple(v) for k, v in API["x-error-status-codes"].items()} == _wire.ERROR_STATUS_CODES
    assert set(API["components"]["schemas"]["Error"]["properties"]["error"]["properties"]["code"]["enum"]) == set(_wire.ERROR_TYPES)
    defaults = next(c["response"]["limits"] for c in CASES["cases"] if c["name"] == "discovery")
    assert defaults["default_timeout_ms"] <= defaults["max_timeout_ms"]
    assert defaults["max_request_bytes"] == _wire.MAX_HTTP_BYTES == defaults["max_response_bytes"]
    assert defaults["max_batch"] == defaults["max_page_size"] == 256
    spec = CollectionSpec("c", 2)
    assert (spec.metric, spec.quantization) == ("cosine", "f32")


@pytest.mark.parametrize("case", PORTABLE["filters"])
def test_shared_filter_fixtures_round_trip_without_semantic_changes(case):
    encoded = _wire.filter_to_wire(case["filter"])
    parsed = _wire.filter_from_wire(encoded)
    validate("Filter", parsed)
    assert dumps(parsed) == dumps(case["filter"])


@pytest.mark.parametrize("value", PORTABLE["invalid_filters"])
def test_shared_invalid_filters_fail_schema_and_codec(value):
    with pytest.raises(ValidationError):
        validate("Filter", value)
    with pytest.raises(InvalidArgument):
        _wire.filter_from_wire(dumps(value))


@pytest.mark.parametrize("metric", ["cosine", "l2", "dot"])
def test_http_encoding_matches_real_embedded_filter_results(tmp_path, metric):
    client = Client.embedded(tmp_path / "http-codec.rvec")
    collection = client.create_collection("collection/世界", 2, metric)
    wire_records = [_wire.record_to_wire(Record(**value), collection.spec) for value in PORTABLE["records"]]
    # Actually parse the outer JSON, as a transport would, then decode metadata.
    message = _wire.loads_message(dumps({"records": wire_records}).encode("utf-8"))
    decoded = [_wire.record_from_wire(value, collection.spec) for value in message["records"]]
    collection.upsert_many(decoded)
    for case in PORTABLE["filters"]:
        hits = collection.search([.6, .8], k=20,
            filter=_wire.filter_from_wire(_wire.filter_to_wire(case["filter"])), exact=True)
        assert {h.id for h in hits} == set(case["ids"])
    for value in PORTABLE["records"]:
        stored = collection.get(value["id"])
        assert dumps(stored.metadata) == dumps(value["metadata"])


def test_metadata_preserves_uint64_and_structural_number_kinds():
    metadata = {"max": 2**64 - 1, "min": -(2**63), "array": [1, 1.0], "obj": {"v": 1.0}}
    wire = _wire.record_to_wire(Record("line\n/東京\x00", [1, 0], metadata), CollectionSpec("x", 2))
    assert isinstance(wire["metadata_json"], str)
    decoded = _wire.record_from_wire(json.loads(dumps(wire)), CollectionSpec("x", 2))
    assert type(decoded.metadata["array"][0]) is int
    assert type(decoded.metadata["array"][1]) is float
    assert decoded.metadata["max"] == 2**64 - 1
    assert dumps(decoded.metadata) == dumps(metadata)
    validate("Record", wire)
    assert decoded.id == "line\n/東京\x00"


@pytest.mark.parametrize("text", ['{"a":1,"a":2}', '{"x":{"a":1,"a":2}}',
    'NaN', 'Infinity', '-Infinity', '1e400', '18446744073709551616', '-9223372036854775809',
    '"\\ud800"', '{"\\ud800":1}', '"\ud800"', '\ufeff{}', '[1,]', ''])
def test_invalid_json_text_is_not_silently_normalized(text):
    with pytest.raises(InvalidArgument):
        _wire.metadata_from_wire(text)


def test_json_depth_utf8_and_canonical_limits():
    assert _wire.metadata_from_wire('[' * 64 + '0' + ']' * 64)
    with pytest.raises(InvalidArgument):
        _wire.metadata_from_wire('[' * 65 + '0' + ']' * 65)
    boundary = '"' + 'a' * (1048576 - 2) + '"'
    assert len(_wire.metadata_from_wire(boundary)) == 1048576 - 2
    for text in (boundary + ' ', '"' + '界' * 349526 + '"'):
        with pytest.raises(InvalidArgument):
            _wire.metadata_from_wire(text)
    # The raw token is small but its canonical encoding exceeds the byte cap.
    many_floats = '[' + ','.join(['0e0'] * 262145) + ']'
    with pytest.raises(InvalidArgument):
        _wire.metadata_from_wire(many_floats)
    validate_filter({"x": {"$eq": {"$unknown": 1}}})
    validate("Filter", {"x": {"$eq": {"$unknown": 1}}})
    assert _wire.filter_from_wire(None) is None
    for text in ('null', '[]', 'false', '{"$or":[]}', ' ' * 1048577):
        with pytest.raises(InvalidArgument):
            _wire.filter_from_wire(text)


def test_runtime_checks_constraints_not_expressible_as_schema():
    # JSON Schema treats 1.0 as a mathematical integer. The portable model
    # deliberately rejects the float token for dimensions and other integers.
    validate("CollectionSpec", {"name": "x", "dim": 1.0})
    with pytest.raises(InvalidArgument):
        CollectionSpec("x", 1.0)
    name = '界' * 86
    validate("CollectionName", name)  # Character count is not UTF-8 byte count.
    with pytest.raises(InvalidArgument):
        CollectionSpec(name, 2)
    validate("RecordInput", {"id": "x", "vector": [0, 0]})
    for vector in ([0, 0], [1], [float('nan'), 1], [True, 1], [1e20, 0]):
        with pytest.raises(InvalidArgument):
            _wire.record_to_wire(Record("x", vector), CollectionSpec("x", 2))
    with pytest.raises(InvalidArgument):
        validated_records([Record("same", [1,0]), Record("same", [0,1])], CollectionSpec("x",2))


@pytest.mark.parametrize("value", [0, 1.0, True, None, '', '01', '+1', '-1', '1.0', ' 1',
                                  '18446744073709551616', '99999999999999999999'])
def test_uint64_never_rounds_or_accepts_out_of_range_values(value):
    with pytest.raises(CorruptData):
        _wire.uint64_from_wire(value)


def test_uint64_boundaries_and_additive_response_fields():
    assert _wire.uint64_from_wire('0') == 0
    assert _wire.uint64_from_wire('18446744073709551615') == 2**64-1
    wire = {"id": "", "vector": [1,0], "metadata_json": 'null', "future": True}
    validate("Record", wire)
    assert _wire.record_from_wire(wire, CollectionSpec("x",2)).id == ''
    del wire["metadata_json"]
    with pytest.raises(CorruptData):
        _wire.record_from_wire(wire, CollectionSpec("x",2))


@pytest.mark.parametrize("body", [b'[]', b'null', b'{} {}', b'{"x":NaN}', b'{"x":1e400}',
    b'{"a":1,"a":2}', b'{"x":"\xff"}', b'\xef\xbb\xbf{}', b'{"x":"\\ud800"}',
    b'{"x":18446744073709551616}', b'{' * 1000, b''])
def test_malformed_outer_messages_are_errors(body):
    with pytest.raises(CorruptData):
        _wire.loads_message(body)


def test_message_size_and_envelope_depth_are_bounded():
    assert _wire.loads_message(b'{}', max_bytes=2) == {}
    with pytest.raises(CorruptData):
        _wire.loads_message(b'{ }', max_bytes=2)
    with pytest.raises(CorruptData):
        _wire.loads_message(b'{"x":' * 17 + b'0' + b'}' * 17)


def test_bad_error_status_pairings_and_bodies_are_not_success():
    body = next(c["response"] for c in CASES["cases"] if c["name"] == 'error-504-Unavailable')
    body = copy.deepcopy(body)
    body["error"]["message"] = "sensitive credential, local path and metadata"
    error = _wire.error_from_wire(504, dumps(body).encode())
    assert 'sensitive' not in str(error)
    assert error.write_outcome == 'unknown'
    for status, value in [(200, body), (401, body), (599, body), (504, {}),
        (504, {"error": {**body["error"], "write_outcome": "committed"}}),
        (504, {"error": {**body["error"], "request_id": body["error"]["request_id"].upper()}}),
        (504, {"error": {**body["error"], "request_id": "not-a-uuid"}})]:
        with pytest.raises(CorruptData):
            _wire.error_from_wire(status, dumps(value).encode())
