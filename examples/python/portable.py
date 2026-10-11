"""The same application logic on embedded Recern or a real Qdrant server.

python examples/python/portable.py --path /tmp/portable-demo.rvec
RECERN_QDRANT_API_KEY=... python examples/python/portable.py --qdrant http://127.0.0.1:6335
"""
import argparse
import os

from recern_vector.portable import Client, NotFound, Record


def run(client):
    try:
        merchants = client.collection("portable-demo")
    except NotFound:
        merchants = client.create_collection("portable-demo", dim=3, metric="cosine")
    merchants.upsert_many([
        Record("merchant:001", [0.9, 0.1, 0.2], {"country": "NL", "risk": 0.1}),
        Record("merchant:002", [0.2, 0.8, 0.1], {"country": "JP", "risk": 0.7}),
        Record("merchant:003", [0.8, 0.2, 0.3], {"country": "KR", "risk": 0.2}),
    ])
    hits = merchants.search([0.9, 0.1, 0.2], k=2,
                            filter={"risk": {"$lt": 0.5}, "$or": [{"country": "NL"}, {"country": "KR"}]},
                            exact=True)
    assert [h.id for h in hits] == ["merchant:001", "merchant:003"]
    assert merchants.get("merchant:001").metadata["country"] == "NL"
    print(client.capabilities.backend, [(h.id, round(h.distance, 6)) for h in hits])
    print("Records:", merchants.count())


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group()
    group.add_argument("--path", default="portable-demo.rvec")
    group.add_argument("--qdrant")
    parser.add_argument("--namespace", default="recern_demo")
    args = parser.parse_args()
    client = (Client.qdrant(args.qdrant, api_key=os.getenv("RECERN_QDRANT_API_KEY"), namespace=args.namespace)
              if args.qdrant else Client.embedded(args.path))
    run(client)
