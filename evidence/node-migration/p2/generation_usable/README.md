# NM-032 — read-only installed-generation usability gate

Node port of `generation/mod.rs::{verify_installed, decision_artifact,
usable}`: a generation is usable only when directory identity ==
sha256(manifest bytes), `inspect` verifies all artifact bytes, and the
recorded decision binds the same generation+manifest with
VERIFIED_COMPATIBLE.

## Contents

- `probe-results.jsonl` — 26 rows, all PASS (21 live Rust oracle +
  5 typed-boundary).
- `rust-oracle.jsonl` — live oracle over synthetic roots + real gen-2.
- `negative-selftest.jsonl` — exit-1 proof.
- `results.json` — ordering, comparison policy, limits.

## Reproduce

```
node scripts/node_migration_p2/generation_usable_probe.mjs .        # exit 0
node scripts/node_migration_p2/generation_usable_probe.mjs . usable # selftest, exit 1
```

## Notes

- Error kinds verbatim: `not_installed` / `corrupt` / `json` /
  `not_consistency_verified` with Rust Display text; deterministic
  inner messages compared verbatim, io/serde inner text by kind+prefix
  (documented text delta).
- serde-struct stage separated from `inspect` so manifest type errors
  map to `json` (not `corrupt`), matching Rust.
- Decision artifact: exact field set, enum-variant and timestamp
  validation; surrogate escapes rejected via strict JSON parse.
- Real gen-2 exercised only as a no-decision negative — no proof
  fabricated.
- Reads bounded: manifest ≤1MiB, decision ≤1MiB.
