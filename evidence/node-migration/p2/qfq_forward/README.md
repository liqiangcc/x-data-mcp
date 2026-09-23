# NM-014 P2 QFQ forward-adjust traversal — evidence

Baseline `6344d667f1610693dd79b7b8d7895c8ace1aff6c` (`migration/node-devin`). Node v20.19.2.

## Contents

- `probe-results.jsonl` — 15 rows, all PASS: 3 accepted P0 goldens + 8 rust-oracle edge cases + 4 fail-closed/structural.
- `rust-oracle.jsonl` — edge-case expected values produced live by `rustc 1.98.1` + `x_data_mcp` rlib (`target/debug/qfq_fwd_oracle`, not committed).
- `results.json` — ported function, mirrored semantics, output shape, scope boundary.
- `SHA256SUMS` — over the three files (excludes itself).

## Reproduction

```sh
node scripts/node_migration_p2/qfq_forward_probe.mjs .
```

Module: `src_node/algorithms/qfq_forward.mjs` (uses NM-013 `qfq_affine.mjs` Dec engine; pure JS+BigInt, zero deps).

## Edge coverage

Unsorted events (stable sort), duplicate event dates (both apply, input order), event with no same-day bar, event before/after all bars, unordered bar input (partition fallback), raw-field pass-through, non-finite factor at index 0/3 with verbatim error text, malformed bar date reject, empty bars → [].
