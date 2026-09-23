# NM-028 — catalog source candidate assembly

Node port of the pre-publication part of
`catalog_tdx::source::TdxCatalogExecutor::run`: scope gate -> bounded
manifest read (1MiB+1) -> `snapshot::inspect` -> anchored members
(`base.zip!tdxhy.cfg`, `base.zip!infoharbor_ex.code`,
`gbbq.zip!zhb.zip!incon.dat`, 64MiB bound) -> parsers -> scope +
reconciliation -> compose -> in-memory candidate + evidence.

## Contents

- `probe-results.jsonl` — 8 rows, all PASS; probe exits nonzero on FAIL.
- `rust-oracle.jsonl` — live Rust oracle replicating `run` pre-publish.
- `negative-selftest.jsonl` — selftest proof (exit 1).
- `results.json` — semantics, provenance, deltas.

## Reproduce

```
node scripts/node_migration_p2/catalog_source_probe.mjs .         # exit 0
node scripts/node_migration_p2/catalog_source_probe.mjs . source  # selftest, exit 1
```

## Real gen-2

`manifest b8a0d7b9…` + retained zips (hardlinks, read-only):
`source=official-file:b8a0d7b9aec1a270`, **5,224 records**, full
serde-shape candidate identical to live Rust; evidence:
6 `reject_candidate` nameless exclusions, 0 missing industry,
0 t-code residuals, `t_dictionary_checked=true`, scope partition
{5659, 5230, 348, 78, 3}.

## Limits

Byte inspection never implies cross-file compatibility / origin
authorization / production readiness; no proof insertion. Inner
io/zip error text differs from Rust `Display` (categories + member
names + parser texts verbatim). Synthetic `source` normalized
(manifest-bound); real gen-2 verifies it verbatim.
