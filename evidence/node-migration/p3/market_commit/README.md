# NM-053 — publish commit transaction (commit_range)

`src_node/market/market_commit.mjs` — `commit_range` (replay →
publication-id uniqueness → base re-verify → no_change vs
commit_publication → audit payloads → market_update_results →
read-back) and `commit_publication` (replay → reuse → candidate
validated → base digest → heads CAS → binding+finish_task →
publication/task_publications → guarded publish → promote upsert →
persist_decided → read_projection verify). `decideHead` +
`persistDecided` exported from `task_repository.mjs` (the ported task
write path — not reimplemented).

## Differential

`commit_range` op with plan JSON (command + record/base fixture +
required_heads/promote/base_digest + audit + payloads); emits receipt +
11-table symbol-normalized digest — byte-identical commits proven.

## Reproduce

```
node scripts/node_migration_p3/diff_harness_probe.mjs .        # exit 0
node scripts/node_migration_p3/diff_harness_probe.mjs . diff   # selftest, exit 1
```

18 scenarios, 180 ops, 0 diffs.
