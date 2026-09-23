# NM-023 P2 catalog scope partition — evidence

Baseline `02128b573cc6f7d4173f06124dfc6dce4f5c10b2` (`migration/node-devin`). Node v20.19.2.

## Contents

- `probe-results.jsonl` — 35 rows, all PASS.
- `rust-oracle.jsonl` — expected outputs produced live by `rustc 1.98.1` + `x_data_mcp` rlib (`target/debug/scope_oracle`, not committed): 17 synthetic partition cases + member digests + 3 sampled excluded rows.
- `results.json` — ported function, mirrored semantics, member provenance.
- `SHA256SUMS` — over the three files (excludes itself).

## Reproduction

```sh
node scripts/node_migration_p2/catalog_scope_probe.mjs .
```

Module: `src_node/catalog/scope.mjs` (pure JS, zero deps). Composes unmodified `src_node/parsers/tdxhy.mjs` (NM-021) for the retained member.

## Scope rule (frozen, #168 rule S)

S = { market ∈ {0,1} ∧ t_code ≠ ε ∧ x_code ≠ ε }. Exclusions: `BjMarket` (market 2, any codes) / `BShare` (market 0/1, missing T or X, prefix `200`/`900`) / `Anomalous` (remaining). Disjoint, exhaustive, order-preserving.

## Retained member

`tdxhy.cfg` 150,332B (sha256 `df7cb705…`, MANIFEST-verified zip): 5,230 members + 429 excluded; member digest `c96c914b…`, exclusion digest `ad9cc4ea…`, reconciliation `{total:5659, scope:5230, bj_market:348, b_share:78, anomalous:3}` — all identical to Rust.

## Fail-closed delta (documented)

`reconciliation` throws `ScopeBoundaryError` when `sum != total` — Rust's `debug_assert` is compiled out in release; Node makes the identity check unconditional. Typed-boundary validation of row shapes runs before classification.
