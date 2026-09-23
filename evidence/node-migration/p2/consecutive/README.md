# NM-030 — pure consecutive_direction algorithm

Node port of `metrics::consecutive::evaluate` + `ratio_change`
(exact-decimal comparisons, one half-even rounding at scale 12).

## Contents

- `probe-results.jsonl` — 78 rows, all PASS (52 live Rust oracle
  incl. full Outcome serde + INVALID_MARKET_COMMAND errors +
  ratio_change strings; 26 typed-boundary rejects).
- `rust-oracle.jsonl` — live oracle.
- `negative-selftest.jsonl` — exit-1 proof.
- `results.json` — semantics, matrix, limits.

## Reproduce

```
node scripts/node_migration_p2/consecutive_probe.mjs .             # exit 0
node scripts/node_migration_p2/consecutive_probe.mjs . consecutive # selftest, exit 1
```

## Notes

- Decimal via shared `_dec` engine (qfq_affine): mantissa/scale BigInt,
  exact aligned comparison, Display verbatim (`close` strings).
- `ratio_change` verbatim: coefficient scale alignment, 512-bit
  budget, magnitude quotient + single half-even step, `-` only when
  nonzero, `padStart(13)` split at len-12.
- Typed boundary rejects malformed dates/decimals/enums/params.
- Pure/read-only — no completion-provenance or coverage inference.

R1 additions (REVIEW-NM-030): manual Gregorian calendar replaces
Date.UTC (years 0001-0099 compute; leap rules verified year 4/400/2000,
1900/2100 Feb-29 reject); `completed_only`/`price_field`/`equal_policy`/
`gap_policy` defaults apply to absent keys only — explicit null rejects.
