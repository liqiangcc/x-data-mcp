# NM-019 P2 yearly natural-year aggregation — evidence

Baseline `47d9efeac96e91093a283cf6f39b86e48d84e77e` (`migration/node-devin`). Node v20.19.2.

## Contents

- `probe-results.jsonl` — 61 rows, all PASS: 5 goldens + 20 rust-oracle + 1 exact-sum + 35 typed-boundary regressions (R1 + R2).
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

## R1 typed-boundary fix (REVIEW-NM-019)

`aggregate` previously accepted `asOf="junk"` / `"2023-02-30"` (NaN/unchecked fragments skipped the `date>as_of` reject). Now: strict calendar validation for `asOf` and every bar date, i32-year and array checks for `years`, object/records/Dec-shape validation for `daily` — `YearlyTypedBoundaryError` precedes the Rust validation order. Valid i32 edge years (±2³¹) still reach the Rust layer (`MissingYear`), not the boundary.

## R2 96-bit mantissa bound (REVIEW-NM-019-R1)

`isDec` accepted `{m: 2n**96n}` — no Rust `Decimal` can represent it. Now `|m| <= 2^96-1` enforced for every Dec input in both signs (`-MAX96` = `Decimal::MIN` accepted, verified aggregated; `±(2^96)` rejected in open/high/low/close/volume/amount). 8 reject cases + 1 exact-bound accept.
