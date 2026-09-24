# NM-039 — P3 SQLite schema gate + capability reader

`src_node/storage/schema_gate.mjs` — read-only gate over
`sqlite_adapter`: user_version gate (18=legacy / 19=compact), the
Rust `schema_migrations` checksum chain verbatim (sha256 of each
committed `migrations/tasks/*.sql`), then required tables/indexes/
columns from the parsed migration op chain (DROP/RENAME/ADD COLUMN
modeled; v17 post-migration `DROP TABLE version_segments` mirrored).

## Contents

- `probe-results.jsonl` — 16 rows, all PASS.
- `negative-selftest.jsonl` (exit 1), `results.json` (fixture SHAs),
  `SHA256SUMS`.

## Reproduce

```
node scripts/node_migration_p3/schema_gate_probe.mjs .        # exit 0
node scripts/node_migration_p3/schema_gate_probe.mjs . gate   # selftest, exit 1
```

## Notes

- Every gate path is read-only — DB bytes sha-verified unchanged;
  WAL sidecars (-shm/-wal) are read-protocol artifacts, not mutation.
- Negative coverage: version 0/10/20, checksum tamper, missing row,
  dropped table/column/index — all `unavailable` verbatim Rust text.
- Gate REPORTS schema 19 as compact; it does not assert legacy
  compatibility (Rust `check_schema` refuses 19 on legacy paths).

R1: bytesUnchanged is a full SHA-256 before/after comparison — same-size mutations detected, not size-only (REVIEW-NM-039 R0).
