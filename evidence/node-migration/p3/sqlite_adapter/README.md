# NM-038 — P3 SQLite driver-neutral adapter

`src_node/storage/sqlite_adapter.mjs` over the pinned
`better-sqlite3@11.10.0` (NM-006). Explicit modes, safeIntegers
default-ON, typed `StorageError` codes, WAL/checkpoint, busy mapping.

## Contents

- `probe-results.jsonl` — 24 rows, all PASS.
- `negative-selftest.jsonl` (exit 1), `results.json` (fixture + lockfile
  SHA pinned), `SHA256SUMS`.

## Reproduce

```
node scripts/node_migration_p3/sqlite_adapter_probe.mjs .      # exit 0
node scripts/node_migration_p3/sqlite_adapter_probe.mjs . sql  # selftest, exit 1
```

## Notes

- Modes: `readonly` / `readwrite` (both fileMustExist) / `create` —
  only `create` may create a file; verified no file appears on missing.
- Readonly open verified byte-unchanged (sha before/after) — no
  implicit schema init on empty.db.
- Driver gated at load to 11.10.0 + Node>=20; package-lock pinned.

R1: checkpoint(mode) strict whitelist PASSIVE|FULL|RESTART|TRUNCATE — injection-shaped modes reject invalid_input before pragma (REVIEW-NM-038 R0).
