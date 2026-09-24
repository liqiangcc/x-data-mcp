# NM-036 — generation pin lifecycle

Node port of `generation/mod.rs::{pin,pins,unpin,sweep_pins,Pin}`:
per-holder 0-byte markers under `pins/<gid>/`, sorted holder listing,
idempotent unpin, keep-predicate sweep, RAII-style release.

## Contents

- `probe-results.jsonl` — 35 rows, all PASS (19 live Rust oracle +
  7 RAII/symlink/boundary).
- `rust-oracle.jsonl`, `negative-selftest.jsonl` (exit 1), `results.json`.

## Reproduce

```
node scripts/node_migration_p2/generation_pin_probe.mjs .      # exit 0
node scripts/node_migration_p2/generation_pin_probe.mjs . pin  # selftest, exit 1
```

## Notes

- Marker = 0-byte regular file mode 100644 (oracle-verified); live
  marker blocks the future retire precondition.
- Stricter than Rust: holder marker opened `O_NOFOLLOW` (planted
  symlink refused, outside target untouched); surrogate holder rejected.
- `pins()` sorted by UTF-8 bytes (Rust `String::sort`), not UTF-16.
- RAII: explicit `release()`/`Symbol.dispose` — Node 20 has no `using`
  and GC finalization is not a semantic guarantee.

R1: unpin() validates holder before filesystem ops — ../a/b/NUL/surrogate reject not_verified, outside file untouched (REVIEW-NM-036 R0).
