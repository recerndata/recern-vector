# recern-vector-cli

The `recern-vector` command for [Recern Vector](https://github.com/recerndata/recern-vector), a single-file, embedded, inspectable vector database.

> Version 1.0.0: paged format 3, incremental encoding and automatic checkpoints. Readers retain formats 1/2; existing format-2 files stay at 2 until an explicit upgrade with a backup. Mutations commit to WAL; checkpoint before copying a standalone snapshot. Workspace 1.0.0 cannot open format 3.

```sh
cargo install recern-vector-cli

recern-vector init docs.rvec
recern-vector create-collection docs.rvec chunks --dim 384 --metric cosine
recern-vector insert docs.rvec chunks chunks.jsonl   # {"id": "...", "vector": [...], "metadata": {...}} per line
recern-vector query docs.rvec chunks --like chunk-42 -k 5 --filter '{"lang": "en"}' --explain
recern-vector inspect docs.rvec
recern-vector recall docs.rvec chunks --ef 16,32,64,128
recern-vector checkpoint docs.rvec
# For an existing format-2 file only, after backing it up and stopping other writers:
# recern-vector upgrade-storage legacy.rvec
```

Licensed under MIT OR Apache-2.0.
