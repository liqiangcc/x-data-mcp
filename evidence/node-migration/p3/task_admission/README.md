# NM-042 — P3 atomic task admission + scope occupancy

`TaskAdmission` in `src_node/storage/task_repository.mjs` ports
`accept_tx`: one transaction — replay/conflict -> orphan + five
registry integrity gates -> digest gate -> scope RangeConflict ->
task/items/scopes/occupancy/request rows. task_id via
`'task-'+hex(randomblob(16))`; pending/rev-0/owner-NULL; occupancy at
revision 0.

## Contents

- `probe-results.jsonl` — 9 rows, all PASS.
- `negative-selftest.jsonl` (exit 1), `results.json`, `SHA256SUMS`.

## Reproduce

```
node scripts/node_migration_p3/task_admission_probe.mjs .      # exit 0
node scripts/node_migration_p3/task_admission_probe.mjs . adm  # selftest, exit 1
```

## Notes

- Writable open is explicit + gate-first (NM-041 R1 ordering).
- Any failure rolls back all rows — verified on conflict, overlap,
  malformed plans, and a forced PK violation.
- Retry path and worker transitions are outside this slice.
