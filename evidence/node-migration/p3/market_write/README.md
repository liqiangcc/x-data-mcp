# NM-052 — market write path: stage + record inserts + receipt reads

`src_node/market/market_write.mjs` — `stage(raw)` (compat::check →
BEGIN IMMEDIATE → base_record → validate_base_identity → insert_record
→ COMMIT), `envelope::pack` ($segments/$payload/annual_bars markers +
shared-evidence extraction), `manifest::build` AVL write side,
`write_day_segments` base-piece sharing.

`market_read.mjs` additions — `read_result` / `item_receipt` /
`range_receipt` over `market_update_results` (digest check,
`require_refs`, association, base/result re-read, task binding,
publication receipt match) + `read_item_binding` port.

## Differential

New ops `stage`/`read_result`/`range_receipt`/`item_receipt`/
`lookup_task`. `stage` emits a symbol-normalized digest over all 11
market tables — byte-identical staging proven across both runners.

## Reproduce

```
node scripts/node_migration_p3/diff_harness_probe.mjs .        # exit 0
node scripts/node_migration_p3/diff_harness_probe.mjs . diff   # selftest, exit 1
```

17 scenarios, 149 ops, 0 diffs.
