# NM-050 — market record codec (compat::check + publish validators)

`src_node/market/record_codec.mjs` — `legacy_digest` (byte-exact vs
issue79 fixtures), `check` (verbatim gate order: digest → version parse
→ batch parse → 100k budget → dedup → unit/currency → adjustment →
identity cross-checks), serde-faithful typed parse of
`EffectiveVersion`/`MarketDataBatch`/`Bar`/`Provenance`/evidence types,
Decimal value semantics, NaiveDate validity.

Differential coverage via the NM-049 harness `check_record` op —
`market_records` scenario: 14 ops, 0 diffs (valid daily/null/yearly +
every guard rejection + serde-shape negatives + over-budget).

## Reproduce

```
node scripts/node_migration_p3/diff_harness_probe.mjs .        # exit 0
node scripts/node_migration_p3/diff_harness_probe.mjs . diff   # selftest, exit 1
```

Fixture provenance: `tests/fixtures/issue79/legacy-records.json`
(committed; digests byte-verified) plus rehashed mutations committed
under `scripts/node_migration_p3/records/`; the 100001-row overbudget
record is generated at probe time.
