# NM-048 — P3 verification storage

Verification storage surface: `stopped_write_checks` audit receipts
(readAudit/readReceipt incl. validate_audit replay + AlreadyVerified
recursion), both `validate_task_link` registries wired into the bound
read (inspection first), `validate_terminal` both copies,
`validate_absent_request`/`validate_task_request`, runtime
registration + `claimWithRuntimeGuard` (triple `verify_binding`), and
the `verification_runtime.mjs` Linux guard (flock + FileIdentity +
statfs local gate + boot_id + descriptor publish).

## Contents

- `probe-results.jsonl` — 10 rows, all PASS.
- `negative-selftest.jsonl` (exit 1), `results.json`, `SHA256SUMS`.

## Reproduce

```
node scripts/node_migration_p3/verification_probe.mjs .       # exit 0
node scripts/node_migration_p3/verification_probe.mjs . ver   # selftest, exit 1
```

## Notes

- Completes the validate_task_link registries deferred at NM-046 R1
  (inspection + verification sides; publication_checks remains
  deferred to its own P3 slice).
- Audit bytes: sha256 column check, association binding, prepare()
  replay comparison; snapshot equality via canonical serde-order
  serialization.
- Runtime guard: tmpfs/ext4/xfs/btrfs only — overlayfs refused
  (not_verified); busy/held flock proven; triple verify_binding.
