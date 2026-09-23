# NM-020 P2 DBF structural container parser — evidence

Baseline `fa004a093aa727c65d4e15abcc9e759d0e21f646` (`migration/node-devin`). Node v20.19.2.

## Contents

- `probe-results.jsonl` — 68 rows, all PASS.
- `rust-oracle.jsonl` — expected outputs produced live by `rustc 1.98.1` + `x_data_mcp` rlib (`target/debug/dbf_oracle`, not committed): 19 synthetic cases + retained-member header/digest + 40 `base_fb_*` field-byte rows.
- `results.json` — ported function, mirrored semantics, member provenance.
- `SHA256SUMS` — over the three files (excludes itself).

## Reproduction

```sh
node scripts/node_migration_p2/dbf_file_probe.mjs .
```

Module: `src_node/parsers/dbf_file.mjs` (pure JS, zero deps). Includes a Rust-exact `from_utf8_lossy` implementation (per-sequence U+FFFD with `error_len` resync — deliberately **not** WHATWG `TextDecoder`, whose resync differs on overlong/surrogate-prefix sequences).

## Retained member

`base.dbf` (4,270,189B, sha256 `6e6480bb…`) extracted bounded from MANIFEST-verified `official-base.zip`: version 0x03, 8,875 records, 40 fields, header 1313, record_length 481, code_page 0x7a. Full-record digest `ff2fba6d…` and field-descriptor digest `a26db445…` identical to Rust; all 40 first-record `field_bytes` slices identical. Raw member not committed.
