"""Public-API contract probes also run against installed historical SDKs."""
import pytest

from recern_vector.portable import Client, Record
from test_http_contract import PORTABLE
from test_server import BIN, KEY, Server

pytestmark = pytest.mark.skipif(not BIN, reason="set RECERN_TEST_SERVER_BIN")


@pytest.mark.parametrize("metric", ["cosine", "l2", "dot"])
def test_released_client_keeps_record_filter_and_page_semantics(tmp_path, metric):
    server = Server(tmp_path / metric)
    try:
        with Client.remote(server.url, api_key=KEY) as client:
            collection = client.create_collection("contract/東京", 2, metric)
            records = [Record(r["id"], r["vector"], r["metadata"]) for r in PORTABLE["records"]]
            assert collection.upsert_many(records) == len(records)
            assert collection.count() == len(records)
            for record in records:
                stored = collection.get(record.id)
                assert stored.id == record.id
                assert stored.metadata == record.metadata
            # Numeric kinds matter even when Python equality considers 1 == 1.0.
            assert type(collection.get("doc/東京").metadata["arr"][0]) is int
            assert type(collection.get("18446744073709551615").metadata["arr"][0]) is float
            for case in PORTABLE["filters"]:
                hits = collection.search([1, 0], k=10, exact=True, filter=case["filter"])
                assert {hit.id for hit in hits} == set(case["ids"])
                assert [hit.distance for hit in hits] == sorted(hit.distance for hit in hits)
            found, cursor = [], None
            while True:
                page = collection.page(limit=2, cursor=cursor)
                found.extend(record.id for record in page.records)
                cursor = page.next_cursor
                if cursor is None:
                    break
            assert len(found) == len(set(found)) == len(records)
            assert set(found) == {record.id for record in records}
            assert collection.delete("")
            assert not collection.delete("")
            assert collection.get("") is None
            assert collection.count() == len(records) - 1
    finally:
        server.stop()
