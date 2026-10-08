#!/usr/bin/env bash
# The recern-vector command line in one minute.
#
#   cargo install recern-vector-cli
#   ./quickstart.sh
set -euo pipefail
cd "$(dirname "$0")"
db=$(mktemp -d)/fruits.rvec

recern-vector init "$db"
recern-vector create-collection "$db" fruits --dim 4 --metric cosine

# One JSON object per line: {"id": ..., "vector": [...], "metadata": {...}}
recern-vector insert "$db" fruits fruits.jsonl

echo; echo "# Nearest neighbours of a stored record"
recern-vector query "$db" fruits --like apple -k 3

echo; echo "# Nearest neighbours of a vector, with a metadata filter, and how the query ran"
recern-vector query "$db" fruits --vector '[0.5, 0.5, 0.0, 0.2]' -k 3 \
  --filter '{"price": {"$lt": 1.0}}' --explain

echo; echo "# Index structure, memory and reachability"
recern-vector inspect "$db"

echo; echo "# Recall of the index against exact search"
recern-vector recall "$db" fruits --sample 8 -k 3 --ef 4,16

echo; echo "# Delete, then rebuild without the deleted record"
recern-vector delete "$db" fruits banana
recern-vector compact "$db" fruits
