# NM-051 — market read path (legacy schema-18 backend)

`src_node/market/market_read.mjs` — `head`/`read_projection`/`read_record`
(`candidate`/`fixed`), `envelope::expand` (`segments`/`records`/`payload:`/
`annual_bars:` refs + shared-evidence inflation), manifest clip read,
`base_record`/`validate_base` via the NM-ported `yearly::aggregate`,
`range` (validate + head default + scope check + limit+1 `next_after`),
`versions` (orphan check + ordering + filter), `is_compact_schema`
fail-closed unsupported.

Fixture DBs are built by the REAL Rust `stage()` path
(`diff_harness::build_market_fixture`) — segment-backed daily +
payload-backed yearly, published with consistent
publication/task_publication/effective_heads rows. Scenario `sql`
tampers run on per-runner copies.

## Reproduce

```
node scripts/node_migration_p3/diff_harness_probe.mjs .        # exit 0
node scripts/node_migration_p3/diff_harness_probe.mjs . diff   # selftest, exit 1
```

6 market-read scenarios, 37 ops, 0 diffs.
