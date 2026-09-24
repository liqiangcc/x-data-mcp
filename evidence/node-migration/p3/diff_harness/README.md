# NM-049 — differential harness: Rust oracle vs Node diff_runner

One JSONL scenario vocabulary interpreted by BOTH the real Rust
`SqliteTasks`/`task_control` APIs (`src/diff_harness.rs`, #[cfg(test)],
env `DIFF_SCENARIO`/`DIFF_DB`/`DIFF_OUT`, rustup 1.98.1) and the Node
`TaskAdmission` port (`scripts/node_migration_p3/diff_runner.mjs`).
`diff_compare.mjs` diffs per-op `{seq,op,status,code,message,task,
unchanged}` rows after symbolic-id normalization.

## Contents

- `probe-results.jsonl` — 8 scenarios, all MATCH (59 ops, 0 diffs).
- `negative-selftest.jsonl` (exit 1), `results.json`, `SHA256SUMS`.
- `scenarios/` — under `scripts/node_migration_p3/scenarios/`.

## Reproduce

```
node scripts/node_migration_p3/diff_harness_probe.mjs .        # exit 0
node scripts/node_migration_p3/diff_harness_probe.mjs . diff   # selftest, exit 1
```

## Caught-divergence proof

`run_identity_gate` re-run with the NM-043 R1 gate reverted →
comparator reports `status error!=ok` DIFF. Restored and re-verified.
The harness also caught one live divergence this round:
`InvalidRetrySelection` is a unit variant — Node detail messages
replaced with verbatim `INVALID_RETRY_SELECTION`.
