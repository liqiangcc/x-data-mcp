# NM-034 — once-only decision/evidence artifacts

Node port of `generation/mod.rs::{write_decision,decision,write_artifact,
artifact}`: exclusive-create writes (O_EXCL|O_NOFOLLOW), fsync file+dir,
sha256 return, null reads, verbatim refusal categories.

## Contents

- `probe-results.jsonl` — 27 rows, all PASS (19 live Rust oracle +
  8 boundary).
- `rust-oracle.jsonl`, `negative-selftest.jsonl` (exit 1), `results.json`.

## Reproduce

```
node scripts/node_migration_p2/generation_artifact_probe.mjs .     # exit 0
node scripts/node_migration_p2/generation_artifact_probe.mjs . art # selftest, exit 1
```

## Notes

- `..` as artifact name resolves to the dir → `artifact_exists` (Rust-verbatim).
- Symlinked target is never opened for writing (O_NOFOLLOW); external
  target bytes verified untouched.
- Reads bounded ≤1MiB (decision aligned with NM-032); artifact reads
  stricter than Rust — documented.
- Reopen proves readable state, not crash durability.
