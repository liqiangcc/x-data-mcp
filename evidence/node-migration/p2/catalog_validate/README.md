# NM-029 — pure catalog candidate validation

Node port of `catalog.rs::validate_snapshot` + `validate_record`:
accepts a serde-shaped candidate (NM-028 output) + optional previous
CatalogVersion; returns `{accepted}` or `{accepted:false,
reason:<serde RejectReason>}`.

## Contents

- `probe-results.jsonl` — 57 rows, all PASS (34 live Rust oracle + 23
  typed-boundary rejects).
- `rust-oracle.jsonl` — oracle via `CatalogService::refresh` reason serde.
- `negative-selftest.jsonl` — exit-1 proof.
- `results.json` — semantics, matrix, limits.

## Reproduce

```
node scripts/node_migration_p2/catalog_validate_probe.mjs .           # exit 0
node scripts/node_migration_p2/catalog_validate_probe.mjs . validate  # selftest, exit 1
```

## Notes

- Rust `trim` uses Unicode `White_Space` (not JS `trim` — U+FEFF is NOT
  blank in Rust; verified via oracle `feff_source`/`feff_symbol`).
- RejectReason serde external tagging verbatim, incl. unit variant
  `"EmptySnapshot"` as bare string and `SecurityKey` Debug escaping
  (`中` literal, `﻿` → `\u{feff}`, `"` → `\"`).
- Disappeared members sorted by (exchange ordinal, UTF-8 symbol).
- Pure/read-only; no publish, DB, completeness inference, or proof.
