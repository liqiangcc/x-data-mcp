# NM-011 P2 .day parser vertical slice — evidence

Baseline `88bc6ebe5068d6d414587cddb5937b4f1edc5445` (`migration/node-devin`). Node v20.19.2.

## Contents

- `probe-results.jsonl` — 13 rows: 4 accepted P0 golden vectors + 9 supplementary structural edges. All PASS, 0 FAIL.
- `results.json` — ported-source reference, mirrored semantics list, scope boundary.
- `SHA256SUMS` — over the two files (excludes itself).

## Reproduction

```sh
node scripts/node_migration_p2/day_probe.mjs .
```

Module under test: `src_node/parsers/day_file.mjs` (pure JS, zero dependencies, sha256 `ba85987c…`).

## Boundary

This is the structural layer only: 32-byte LE frames → `{date, ohlc_raw, amount_float32_bits, volume_raw, r7_raw}` with fail-closed truncation/date/order rejects. f32→decimal projection (NM-007-proven codec) and QFQ/yearly arithmetic are separate later slices.
