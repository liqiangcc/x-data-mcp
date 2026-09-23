# NM-027 — file_consistency::validate

Node port of the pure cross-file consistency decision. Compatibility is
established ONLY by an explicit audited proof binding roles -> hashes /
bound source revision+identity / revision window; verified bytes,
matching URLs/dates/ETags never infer it.

## Contents

- `probe-results.jsonl` — 36 rows, all PASS; probe exits nonzero on FAIL.
- `rust-oracle.jsonl` — live `rustc 1.98.1` + `x_data_mcp` rlib: 28 cases
  (5 VERIFIED_COMPATIBLE / 14 NOT_VERIFIED / 9 CONFLICT); `real_131`
  decision is byte-identical to committed
  `evidence/issue-131-file-set-consistency/decision.json`.
- `results.json` — semantics, provenance, deltas.

## Reproduce

```
node scripts/node_migration_p2/file_consistency_probe.mjs .     # exit 0
node scripts/node_migration_p2/file_consistency_probe.mjs . decision  # selftest, exit 1
```

## Real evidence

- **#131** `input.json` -> Node decision identical to committed
  `decision.json` (VERIFIED_COMPATIBLE, pinned set only — no broader
  claim per Final Gate).
- **gen-2**: NM-026 manifest facts + `compatibility:null` ->
  `NOT_VERIFIED` / `missing_compatibility_evidence` — fact-only, no
  proof inferred.
- **gen-3**: `consistency-{input,decision}.json` in
  `evidence/issue-217-gen3-acquisition/` are 0-byte placeholders —
  NOT_RUN, flagged in results.json.

## Shared helper

`src_node/governance/ts.mjs` extracted from `snapshot.mjs`
(`parseTs`/`nowKey`, single ordering scale incl. leap seconds).
NM-026 regression rerun: 72/72 PASS unchanged.

## Notable oracle-verified semantics

- File order for sort/dedup is the full FileFact order (Option None <
  Some); exact duplicates dedup, divergent duplicates ->
  `conflicting_duplicate` CONFLICT.
- Noncanonical file hash produces `invalid_file_hash` AND
  `hash_mismatch` (binding compare still runs) -> CONFLICT.
- Noncanonical BINDING hash -> `incomplete_compatibility_evidence`
  (checked before binding compare) -> NOT_VERIFIED.
- Reasons sorted by serialized `{"code":..,"role":..}` string;
  conflict reasons dominate status regardless of missing/incomplete
  proof.
