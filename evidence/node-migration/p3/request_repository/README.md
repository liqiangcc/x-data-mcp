# NM-041 — P3 request idempotency primitive

`src_node/storage/request_repository.mjs` — `admit(requestId,
commandJson, outcome)` in one transaction: unique `request_id`, exact
byte replay -> `reused:true`, same-ID different-command -> `conflict`
(`REQUEST_CONFLICT`, no mutation). Stored digest re-verified on read.

## Contents

- `probe-results.jsonl` — 16 rows, all PASS.
- `negative-selftest.jsonl` (exit 1), `results.json`, `SHA256SUMS`.

## Reproduce

```
node scripts/node_migration_p3/request_repository_probe.mjs .      # exit 0
node scripts/node_migration_p3/request_repository_probe.mjs . req  # selftest, exit 1
```

## Notes

- Open requires explicit `mode:"readwrite"|"create"`; schema gate
  must pass first.
- Outcome binding shape validated before SQL; the DB CHECK is the
  second layer — a forced violation rolls back with no row.
- Digest = sha256(command bytes), computed at admission; byte-equality
  replay is stricter-equal to Rust's decoded-command comparison for
  canonical JSON.

R1: gate runs before the writable open — create mode on missing/garbage/uninitialized paths rejects with no artifact and unchanged bytes (REVIEW-NM-041 R0 / TASK-NM-041-R1).
