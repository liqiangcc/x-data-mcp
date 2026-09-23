# NM-019 P2 yearly natural-year aggregation — evidence

Baseline `47d9efeac96e91093a283cf6f39b86e48d84e77e` (`migration/node-devin`). Node v20.19.2.

## Contents

- `probe-results.jsonl` — 26 rows, all PASS: 5 accepted P0 goldens + 20 live rust-oracle cases + 1 supplementary exact-sum.
- `rust-oracle.jsonl` — expected outputs produced live by `rustc 1.98.1` + `x_data_mcp` rlib (`target/debug/yearly_oracle`, not committed).
- `results.json` — ported function, mirrored semantics, scope boundary.
- `SHA256SUMS` — over the three files (excludes itself).

## Reproduction

```sh
node scripts/node_migration_p2/yearly_probe.mjs .
```

Module: `src_node/algorithms/yearly.mjs` (uses NM-013 `qfq_affine.mjs` Dec engine; pure JS+BigInt, zero deps).

## Exactness guarantee

`sum_known` never silently rounds: unit unconfirmed or any `None` → field `null` (presence pass before overflow), then every `checked_add` partial sum must satisfy `next-sum==value && next-value==sum` — any precision loss or overflow → `exact decimal sum is not representable`. Verified against the real crate incl. exact `Decimal::MAX` boundary.
