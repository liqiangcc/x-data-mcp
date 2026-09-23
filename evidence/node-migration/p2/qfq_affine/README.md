# NM-013 P2 QFQ affine arithmetic — evidence

Baseline `b60b0ab28bd54c3c4be8323558147eb14d039387` (`migration/node-devin`). Node v20.19.2.

## Contents

- `probe-results.jsonl` — 37 rows, all PASS: 9 accepted P0 goldens + 14 rust-oracle supplementary + 11 decimal-op semantics + 3 fail-closed.
- `rust-oracle.jsonl` — supplementary expected values produced live by `rustc 1.98.1` linking `x_data_mcp` + `rust_decimal 1.43.0` rlibs (`target/debug/qfq_oracle`, not committed).
- `results.json` — ported functions, mirrored Decimal semantics, scope boundary.
- `SHA256SUMS` — over the three files (excludes itself).

## Reproduction

```sh
node scripts/node_migration_p2/qfq_affine_probe.mjs .
```

Module: `src_node/algorithms/qfq_affine.mjs` (pure JS + BigInt, zero deps).

## Key semantics (all verified against the real crate)

- `Dec` = (signed mantissa, scale ≤28); `div` remainder-loop ≤9 digits/iter, cap-28 HALF-EVEN, post-loop `unscale` chunked {8 (2³²-divisibility),4,2,1} — reproduces `10/4="2.50"`, `10/12.50="0.8"` etc.
- `mul`/`add`: zero operands short-circuit (`0.8*0="0"`, `0.0+0="0"` verbatim); rescale overflow HALF-EVEN.
- `from_str` scale>28 HALF_UP; `round_dp(MidpointAwayFromZero)` for business 2dp.
