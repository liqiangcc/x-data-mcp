# NM-033 — immutable generation staging and install

Node port of `generation/mod.rs::{create_stage, staged_manifest,
install_staged, installed_ids}`: stage once per request, verify all
bytes against the canonical manifest, derive the ID from
sha256(manifest bytes), refuse an existing identity, rename +
fsync-dir.

## Contents

- `probe-results.jsonl` — 30 rows, all PASS (19 live Rust oracle +
  10 request_id/boundary + 1 durability observation).
- `rust-oracle.jsonl` — live oracle incl. reopen/installed-ids rows.
- `negative-selftest.jsonl` — exit-1 proof.
- `results.json` — ordering, durability notes, limits.

## Reproduce

```
node scripts/node_migration_p2/generation_store_probe.mjs .       # exit 0
node scripts/node_migration_p2/generation_store_probe.mjs . store # selftest, exit 1
```

## Notes

- Error kinds verbatim: `staging` / `unverified_stage` / `json` /
  `not_verified` / `already_installed` with Rust Display text.
- Stricter than Rust (documented): `request_id` boundary rejects
  `.`/`..`/slashes/NUL/non-wellformed before any mutation.
- Durability: same-fs rename + `fsync(generations dir)`; reopen rows
  show post-rename state; no in-process crash-fault claim.
- Stage bytes preserved after failed install (`stage_kept_after_fail`).

R1 (REVIEW-NM-033): UTF-8 byte sort for installed_ids (unicode case),
atomic renameat2(RENAME_NOREPLACE) via committed N-API source (built at
probe time; .node uncommitted regenerable), strict ENOENT-only absence,
atomic exclusive mkdir for create_stage.
