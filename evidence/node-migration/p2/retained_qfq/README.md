# NM-018 P2 retained `.day` + GBBQ + QFQ integration — evidence

Baseline `e65e9694266c82d1ba6e3141144b706656668f2f` (`migration/node-devin`). Node v20.19.2. **One symbol only — no whole-market claim.**

## Contents

- `probe-results.jsonl` — 115 rows, all PASS.
- `rust-oracle.jsonl` — expected outputs produced live by `rustc 1.98.1` + `x_data_mcp` rlib on the same input bytes (`target/debug/nm018_oracle`, not committed): selection, raw digest, fq digest, 72 sampled bars, 36 marks.
- `results.json` — symbol mapping rationale, digests, serializations, scope.
- `SHA256SUMS` — over the three files (excludes itself).

## Reproduction

```sh
node scripts/node_migration_p2/retained_qfq_probe.mjs .
```

## Slice

`sz\lday\sz000002.day` (271,328B extracted bounded from MANIFEST-verified zip) + `official-gbbq-member` (MANIFEST-verified) → 8,479 raw records (`c482596a…`) → 36 category-1 events (of 112 for market=0/code=000002; non-cat-1 uninterpreted) → 8,479 QFQ bars digest **`836c9117…` identical to Rust** + 36 event-day marks identical.

Mapping rationale: all 000002 events in the member are market=0 → `sz` member; `sh000002.day` has no matching events — unambiguous.
