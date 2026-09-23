# NM-022 P2 `infoharbor_ex.code` strict GB18030 parser — evidence

Baseline `926a9068b9925262d9bca94dc8fb26a51408aed3` (`migration/node-devin`). Node v20.19.2.

## Contents

- `probe-results.jsonl` — 38 rows, all PASS.
- `rust-oracle.jsonl` — expected outputs produced live by `rustc 1.98.1` + `x_data_mcp`/`encoding_rs` rlibs (`target/debug/excode_oracle`, not committed): 28 synthetic cases + member digest + 4 sampled rows.
- `results.json` — ported function, GB18030 decoder equivalence, member provenance.
- `SHA256SUMS` — over the three files (excludes itself).

## Reproduction

```sh
node scripts/node_migration_p2/ex_code_probe.mjs .
```

Module: `src_node/parsers/ex_code.mjs` (pure JS; strict GB18030 via `TextDecoder('gb18030', {fatal:true})` — same WHATWG spec as `encoding_rs`).

## Decoder equivalence (verified against encoding_rs)

Rejects identically: lone trail `0xbf`, truncated lead, `0xff`, `EF BB BF`, bad 4-byte `95 32 82 FF`. Decodes identically: 2-byte (中, 平安银行…) and 4-byte sequences — confirmed end-to-end by the identical 5,571-row member digest `124bc22d…`.

## Retained member

`infoharbor_ex.code` (182,145B, sha256 `79eed0f6…`) bounded from MANIFEST-verified `official-base.zip`. Raw bytes not committed.
