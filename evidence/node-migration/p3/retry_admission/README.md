# NM-047 — P3 retry admission

`TaskAdmission.admit` with `plan.retry={originalTaskId,expectedRevision,
selectedItems}` — accept_tx RetryGuard branch: validated bound read of
the original, verbatim guard order, stored command_key_json is the
ORIGINAL's key, retry_of bound.

## Contents

- `probe-results.jsonl` — 14 rows, all PASS.
- `negative-selftest.jsonl` (exit 1), `results.json`, `SHA256SUMS`.

## Reproduce

```
node scripts/node_migration_p3/retry_probe.mjs .         # exit 0
node scripts/node_migration_p3/retry_probe.mjs . retry   # selftest, exit 1
```

## Notes

- Guard order: exists -> stored rev -> intent+key (retry_task +
  original key) -> request_id != original's -> per-item selected +
  spec-exact + outcome absent|!success -> count match.
- Replay of a retry request_id compares the durable binding against
  the original's stored key (NM-042 replay rule, retry-aware).
- Scope conflicts still enforced; rejections leave bytes unchanged.

R1: intent compared wholesale (exactly {original_task_id,selected_items}; extra keys reject) — REVIEW-NM-047 R0.
