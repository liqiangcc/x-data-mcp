# NM-045 — P3 terminal transitions

`TaskAdmission.requestCancel` / `finishExecution` / `finishCancellation`
— decide arms + `write_task_delta` terminal path: one transaction each.

## Contents

- `probe-results.jsonl` — 15 rows, all PASS.
- `negative-selftest.jsonl` (exit 1), `results.json`, `SHA256SUMS`.

## Reproduce

```
node scripts/node_migration_p3/terminal_transition_probe.mjs .       # exit 0
node scripts/node_migration_p3/terminal_transition_probe.mjs . term  # selftest, exit 1
```

## Notes

- `requestCancel` no-ops on terminal AND already-cancel_requested —
  the terminal no-op precedes the run-identity gate (any run gets Ok).
- Finish: owned || never_started AND zero running items, else
  `needs_verification` "execution has not safely ended" verbatim.
- `result_json` keys serde-sorted; `cancellation_acknowledged:true`
  only via `finish_cancellation` → cancelled; occupancy released on
  terminal; RECOUNT_COUNTERS; rev+1 CAS.

R1: the already-cancel_requested no-op moved after the run-identity gate — a foreign run cancelling a cancelled task gets needs_verification, not silent Ok (REVIEW-NM-045 R0).
