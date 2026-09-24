# NM-043 — P3 atomic task claim CAS

`TaskAdmission.claim(taskId, expectedRevision, runId, now)` — one
transaction: task exists -> pending && owner-NULL guard -> guarded
`UPDATE tasks ... WHERE task_id=? AND state_revision=?` (in-DB CAS;
`changes!=1` -> `revision_conflict`). Items and occupancy rows
untouched (Rust stamps claims at read time from the live revision).

## Contents

- `probe-results.jsonl` — 11 rows, all PASS.
- `negative-selftest.jsonl` (exit 1), `results.json`, `SHA256SUMS`.

## Reproduce

```
node scripts/node_migration_p3/task_claim_probe.mjs .        # exit 0
node scripts/node_migration_p3/task_claim_probe.mjs . claim  # selftest, exit 1
```

## Notes

- Typed errors verbatim: `STATE_REVISION_CONFLICT`, `TASK_NOT_FOUND`,
  `empty run identity`.
- No mutation on any rejection — DB sha verified unchanged.
- No item/cancel/retry transitions in this slice.
