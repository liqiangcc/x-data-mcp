# NM-046 — P3 recovery surface

`TaskAdmission.requireVerification` / `reconcile` / `candidates` —
decide RequireVerification + Reconcile arms, `reconcile_tx`,
`candidates_tx` with read-stamped claims.

## Contents

- `probe-results.jsonl` — 12 rows, all PASS.
- `negative-selftest.jsonl` (exit 1), `results.json`, `SHA256SUMS`.

## Reproduce

```
node scripts/node_migration_p3/recovery_probe.mjs .       # exit 0
node scripts/node_migration_p3/recovery_probe.mjs . rec   # selftest, exit 1
```

## Notes

- `requireVerification` binds owner run on running/cancel_requested
  task with a named running item; `execution_uncertainty` result_json
  serde-sorted keys, previous_* verbatim; item row untouched.
- `reconcile` marks foreign-accepted/foreign-owned non-terminal tasks
  needs_verification WITHOUT a progress stamp; consistent rows no-op.
- `candidates` returns accepted=run owner-NULL pending/cancel_requested
  rows ordered created_at,task_id; claims stamped live revision.
