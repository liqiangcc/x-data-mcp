# NM-021 P2 `tdxhy.cfg` strict line parser — evidence

Baseline `f02178e8625313ed65c4d11dfdb081c1b96bd1e2` (`migration/node-devin`). Node v20.19.2.

## Contents

- `probe-results.jsonl` — 63 rows, all PASS (47 + 16 R1 escaping regressions).
- `rust-oracle.jsonl` — expected outputs produced live by `rustc 1.98.1` + `x_data_mcp` rlib (`target/debug/tdxhy_oracle`, not committed): 37 synthetic cases + member digest + 4 sampled rows.
- `results.json` — ported function, mirrored semantics, member provenance.
- `SHA256SUMS` — over the three files (excludes itself).

## Reproduction

```sh
node scripts/node_migration_p2/tdxhy_probe.mjs .
```

Module: `src_node/parsers/tdxhy.mjs` (pure JS, zero deps).

## Retained member

`tdxhy.cfg` (150,332B, sha256 `df7cb705…`) extracted bounded from MANIFEST-verified `official-base.zip`: 5,659 rows, market histogram {0:2946, 1:2365, 2:348}, row digest `dcd97d9d…` identical to Rust. Raw member not committed.

## R1 Rust `{:?}` escaping (REVIEW-NM-021)

Field values in `BadMarket`/`BadCode`/`BadIndustryCode`/`BadResearchCode` are rendered with Rust `{:?}` — Node now escapes `"`,`\`,`\n`,`\r`,`\t`,`\0`, other control bytes as `\u{hex}` (single quote passes through), verified against live rustc. 16 regressions across f0/f1/f2/f5.
