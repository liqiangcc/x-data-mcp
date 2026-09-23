# NM-025 P2 catalog pure composition — evidence

Baseline `a289e40fc98e8c30e3a0a2e8899fcd75ce986e6e` (`migration/node-devin`). Node v20.19.2.

## Contents

- `probe-results.jsonl` — 26 rows, all PASS; probe exits nonzero on any FAIL.
- `rust-oracle.jsonl` — live `rustc 1.98.1` + `x_data_mcp` rlib: 10 synthetic outcomes + `real_gen2` full normalized outcome (1,330,712B) + `real_digest`.
- `results.json` — ported semantics, member provenance, counts.
- `SHA256SUMS` — over the three files.

## Reproduction

```sh
node scripts/node_migration_p2/catalog_compose_probe.mjs .
```

Probe self-contains: outer manifest SHA verification (base/gbbq) before extraction, bounded `zip_member.mjs` extraction of `tdxhy.cfg`, `infoharbor_ex.code`, `zhb.zip!incon.dat`, member SHA checks, parse via accepted `tdxhy`/`ex_code`/`incon`/`scope` modules, `compose`, canonical outcome compare + sha256 over identical serialization.

## Real gen-2 result

5,230 scoped → **5,224 named records + 6 nameless** (P1 exclusions); missing_industry = 0, t_code_residuals = 0, t_dictionary_checked = true. Outcome digest `8ddfaba2…` and full normalized JSON byte-identical to live Rust.

## Fixed test metadata

`source = "official-file:deadbeefcafe0001"`, `acquired_at = 2023-11-14T22:13:20+00:00` — labeled test values; does **not** claim actual manifest identity or acquisition time. Completeness count = published members, not market-wide.

## Semantics verified against live Rust

`MissingDictionary(TDXRSHY)` before any member (empty members too); `TDXNHY` absent → `t_dictionary_checked=false`; `BadMarket` before nameless; nameless verbatim with no diagnostics; all-nameless → `snapshot=null`; Missing reasons verbatim; `tdx_research_industry` scheme; ordering preserved.
