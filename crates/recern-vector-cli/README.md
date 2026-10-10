# recern-vector-cli

The `recern-vector` command for [Recern Vector](https://github.com/recerndata/recern-vector), a single-file, embedded, inspectable vector database.

> Version 0.2.0: stable format 2, with compatibility for format-1 databases. Mutating commands commit to WAL; checkpoint before copying a standalone database file.

```sh
cargo install recern-vector-cli

recern-vector init docs.rvec
recern-vector create-collection docs.rvec chunks --dim 384 --metric cosine
recern-vector insert docs.rvec chunks chunks.jsonl   # {"id": "...", "vector": [...], "metadata": {...}} per line
recern-vector query docs.rvec chunks --like chunk-42 -k 5 --filter '{"lang": "en"}' --explain
recern-vector inspect docs.rvec
recern-vector recall docs.rvec chunks --ef 16,32,64,128
recern-vector checkpoint docs.rvec
```

Licensed under MIT OR Apache-2.0.
