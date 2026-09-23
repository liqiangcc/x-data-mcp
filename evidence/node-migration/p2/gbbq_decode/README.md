# NM-016 P2 GBBQ decrypt + event decode — evidence

Baseline `a7c75a19eb4bd9c5de9d2df3f222b73d2a0662d7` (`migration/node-devin`). Node v20.19.2.

## Contents

- `probe-results.jsonl` — 19 rows, all PASS.
- `rust-oracle.jsonl` — expected outputs produced live by `rustc 1.98.1` + `x_data_mcp` rlib (`target/debug/gbbq_decode_oracle`, not committed): 4 synthetic encrypt→decode round-trips (incl. 2 invalid-UTF-8 code + empty code), 7 member sample events, full-corpus digest + category histogram.
- `results.json` — ported functions, digest serialization, mirrored semantics.
- `SHA256SUMS` — over the three files (excludes itself).

## Reproduction

```sh
node scripts/node_migration_p2/gbbq_decode_probe.mjs .
```

Module: `src_node/parsers/gbbq_decode.mjs` (pure JS, u32 wrapping via `>>>0`, zero deps). Key table is the 4176-byte pinned table extracted verbatim from `src/gbbq.rs`.

## Retained-corpus verification

`/home/box/retained-inputs/official-gbbq-member` (sha256 `4a3e9b86…`, MANIFEST-verified): **all 205,776 events decoded and digested** — sha256 `a33ff825b118de4b7dc6ade4889443db411bef54c01f25b29fdbc5b720ff1adc` identical to Rust over the documented per-event line serialization; category histogram identical; 7 sampled events field-identical incl. `c_raw_bits`.
