# NM-035 — active-generation pointer read + conditional swap

Node port of `generation/mod.rs::{active,activate,ActivePointer}`:
CAS on expected_previous (null = absent), verbatim conflict text,
serde_json::to_vec_pretty bytes, tmp+fsync+rename+dir-fsync ordering.

## Contents

- `probe-results.jsonl` — 16 rows, all PASS (11 live Rust oracle +
  5 lease/boundary).
- `rust-oracle.jsonl`, `negative-selftest.jsonl` (exit 1), `results.json`.

## Reproduce

```
node scripts/node_migration_p2/generation_active_probe.mjs .     # exit 0
node scripts/node_migration_p2/generation_active_probe.mjs . act # selftest, exit 1
```

## Notes

- Oracle-verified serde details: `schema` is a plain String (wrong
  values accepted); `previous_generation_id` absent → null.
- Pointer bytes identical to Rust `to_vec_pretty` (byte array compared).
- Caller-held exclusion proven via real kernel flock (cross-process
  EWOULDBLOCK), not an in-process flag.
- Reopen proves readability, not crash durability.
