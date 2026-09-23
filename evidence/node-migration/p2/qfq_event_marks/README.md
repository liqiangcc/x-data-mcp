# NM-017 P2 QFQ event-day marks — evidence

Baseline `588d19981faf8c2c80b801786a247f8775bbfe61` (`migration/node-devin`). Node v20.19.2.

## Contents

- `probe-results.jsonl` — 29 rows, all PASS.
- `rust-oracle.jsonl` — 13 expected outputs produced live by `rustc 1.98.1` + `x_data_mcp` rlib (`target/debug/marks_oracle`, not committed).
- `results.json` — ported function, mirrored semantics, typed-boundary coverage.
- `SHA256SUMS` — over the three files (excludes itself).

## Reproduction

```sh
node scripts/node_migration_p2/qfq_event_marks_probe.mjs .
```

Module: `src_node/algorithms/qfq_event_marks.mjs` (uses NM-013 `qfq_affine.mjs`; pure JS, zero deps).

## NM-014 typed-boundary follow-up

REVIEW-NM-014 recorded that `forwardAdjust` coerced `date_raw` via `>>> 0`. This task **enforces and tests** the boundary in both modules: `date_raw`/`category`/`ohlc_raw`/bar-date rejects with `RangeError`/`TypedBoundaryError` instead of wrapping; u32 edge values (0, 2³²−1) still accepted; NM-014 probe regression 15/15 PASS.
