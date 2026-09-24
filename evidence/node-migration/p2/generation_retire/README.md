# NM-037 — evidence-only generation byte retirement

Node port of `generation/mod.rs::retire_bytes`: removes only the three
required raw artifacts (base.zip/gbbq.zip/hsjday.zip), retains manifest,
decision, evidence and the generation dir, fsyncs the directory.

## Contents

- `probe-results.jsonl` — 12 rows, all PASS (9 live Rust oracle +
  3 symlink/pin boundary).
- `rust-oracle.jsonl`, `negative-selftest.jsonl` (exit 1), `results.json`.

## Reproduce

```
node scripts/node_migration_p2/generation_retire_probe.mjs .         # exit 0
node scripts/node_migration_p2/generation_retire_probe.mjs . retire  # selftest, exit 1
```

## Notes

- Removal via native `unlinkat` on the fd-pinned generation dir —
  ancestor swap can't redirect; unlink removes a symlinked artifact as
  the link, never the outside target.
- Missing artifacts idempotent (ENOENT); directory-as-artifact -> io.
- Does NOT check active/pin/authorization — caller-held preconditions,
  same as Rust.
