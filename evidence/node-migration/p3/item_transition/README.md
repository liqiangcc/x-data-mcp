# NM-044 — P3 atomic item start/finish transitions

`TaskAdmission.startItem` / `finishItem` — decide(StartItem/FinishItem)
+ `write_item_row` + `counter_delta` + `task_item_execution_starts`
marker, one transaction each over the NM-043 claim state.

## Contents

- `probe-results.jsonl` — 16 rows, all PASS.
- `negative-selftest.jsonl` (exit 1), `results.json`, `SHA256SUMS`.

## Reproduce

```
node scripts/node_migration_p3/item_transition_probe.mjs .       # exit 0
node scripts/node_migration_p3/item_transition_probe.mjs . item  # selftest, exit 1
```

## Notes

- Shared gate: exists -> stored rev -> terminal -> run-identity
  (needs_verification verbatim) -> arm guards -> guarded CAS.
- `counter_delta` incremental (NOT recount) — pending-1/running+1 on
  start; running-1/finished+1[/succeeded+1] on finish.
- `success with error` verbatim invalid_input; outcome_json stores the
  ItemOutcome serde shape incl. optional evidence.
- Marker row `(task_id,item_id,run_id,start_revision,started_at)` is
  insert-only (triggers forbid update/delete).
