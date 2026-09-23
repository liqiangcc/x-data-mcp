# NM-031 — pure generation binding decision

Node port of `data_supply/generation.rs::{binding_of, uniform_binding}`:
resolves `source_generation_id` from frozen item inputs exactly once;
missing/null/mixed/no-items are refusals, never fallbacks.

## Contents

- `probe-results.jsonl` — 34 rows, all PASS (21 live Rust oracle +
  9 typed-boundary + 4 prototype-safety).
- `rust-oracle.jsonl` — live oracle (BindingRefusal Debug verbatim).
- `negative-selftest.jsonl` — exit-1 proof.
- `results.json` — semantics, matrix, limits.

## Reproduce

```
node scripts/node_migration_p2/generation_binding_probe.mjs .         # exit 0
node scripts/node_migration_p2/generation_binding_probe.mjs . binding # selftest, exit 1
```

## Notes

- `frozen_inputs` is a serde `Value` — non-object means key absent
  (Missing), not a type error; present non-string means Null refusal.
- Mixed precedence: every binding compared to the first before the
  first is interpreted.
- `Ok("")` is reachable for a uniform empty string — `Store::usable`
  remains the separate later gate; the pure contract is unchanged.
- Own-property lookup only (`Object.hasOwn`); objects with custom
  prototypes (inherited bindings) are typed-boundary rejects.
