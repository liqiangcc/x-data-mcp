# NM-040 — P3 read-only task/request repository

`src_node/storage/task_repository.mjs` over the NM-039 gate + NM-038
adapter: getRequest (digest-verified), getTask, getTaskForRequest,
getRequestForTask, keyset-paginated listTasks with explicit owner scope.

## Contents

- `probe-results.jsonl` — 19 rows, all PASS.
- `negative-selftest.jsonl` (exit 1), `results.json`, `SHA256SUMS`.

## Reproduce

```
node scripts/node_migration_p3/task_repository_probe.mjs .       # exit 0
node scripts/node_migration_p3/task_repository_probe.mjs . repo  # selftest, exit 1
```

## Notes

- Open is gated: garbage->corrupt, empty/unknown->unavailable before
  any query; command_digest verified verbatim (Rust read_request).
- i64 counters map to bigint; nulls preserved; no implicit writes —
  DB bytes sha-verified unchanged after reads.
- Owner scoping is an explicit input (`owner` option); limit is
  bounded 1..10000; cursor validated {created_at, task_id}.
