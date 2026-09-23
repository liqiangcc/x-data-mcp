# NM-012 P2 amount f32 projection — evidence

Baseline `b6e710febb780e4d982916de36d86e3c3215368f` (`migration/node-devin`). Node v20.19.2.

## Contents

- `probe-results.jsonl` — 25 rows: 6 accepted P0 goldens + 12 Rust-verified boundary cases + 7 input-validation rejects. All PASS.
- `results.json` — ported expression, verification method, mirrored semantics.
- `SHA256SUMS` — over the two files (excludes itself).

## Reproduction

```sh
node scripts/node_migration_p2/amount_f32_probe.mjs .
```

Module: `src_node/codecs/amount_f32.mjs` (pure JS, zero deps, sha256 `08812ba2…`).

## Verification method note

Golden rows compare byte-exactly against the accepted P0 vectors. Boundary rows (f32 max/min-denormal/min-normal/-0/exponent-range/inf/NaN) have expected values produced by `rustc 1.98.1` + the real `rust_decimal` rlib executed on this box during NM-012 — not hand-derived.
