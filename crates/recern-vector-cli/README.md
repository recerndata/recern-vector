# recern-vector-cli

The `recern-vector` command for [Recern Vector](https://github.com/recerndata/recern-vector), a single-file, embedded, inspectable vector database.

> Prototype (0.0.x): the file format and commands will change between releases.

```sh
cargo install recern-vector-cli

recern-vector init docs.rvec
recern-vector create-collection docs.rvec chunks --dim 384 --metric cosine
recern-vector insert docs.rvec chunks chunks.jsonl   # {"id": "...", "vector": [...], "metadata": {...}} per line
recern-vector query docs.rvec chunks --like chunk-42 -k 5 --filter '{"lang": "en"}' --explain
recern-vector inspect docs.rvec
recern-vector recall docs.rvec chunks --ef 16,32,64,128
```

Licensed under MIT OR Apache-2.0.
