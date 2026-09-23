# NM-015 P2 GBBQ container parser — evidence

Baseline `ed61961ef57341a3fa6f34f8f271a954d747100e` (`migration/node-devin`). Node v20.19.2.

## Contents

- `probe-results.jsonl` — 21 rows, all PASS.
- `rust-oracle.jsonl` — 14 expected outputs/errors produced live by `rustc 1.98.1` + `x_data_mcp` rlib (`target/debug/gbbq_parse_oracle`, not committed).
- `results.json` — ported function, retained-input probe provenance, mirrored semantics.
- `SHA256SUMS` — over the three files (excludes itself).

## Reproduction

```sh
node scripts/node_migration_p2/gbbq_file_probe.mjs .
```

Module: `src_node/parsers/gbbq_file.mjs` (pure JS, zero deps).

## Retained-input probe (read-only)

`/home/box/retained-inputs/official-gbbq-member` — sha256 `4a3e9b862247a8a3fec406cc25b9a5a6ee3303f740780822519c99decaa09d77` (matches MANIFEST.md line 15), 5,966,087 bytes → `record_count=205776`, all records preserved verbatim. Structural only — records remain encrypted bytes, uninterpreted per task scope.
