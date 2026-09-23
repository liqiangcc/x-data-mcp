# NM-026 P2 snapshot manifest byte inspection — evidence

Baseline `6c8718d48f3580688e6aa3ee258621cf55fdacb7` (`migration/node-devin`). Node v20.19.2.

## Contents

- `probe-results.jsonl` — 45 rows, all PASS; probe exits nonzero on FAIL.
- `rust-oracle.jsonl` — live `rustc 1.98.1` + `x_data_mcp` rlib: 32 error + 6 ok incl. `real_gen2` full Inspection JSON.
- `results.json` — semantics, provenance, bounded measurement, Node deltas.
- `SHA256SUMS` — over the three files.

## Reproduction

```sh
node scripts/node_migration_p2/snapshot_probe.mjs .
```

Probe builds synthetic fixture dirs under `/dev/shm`, runs all 37 oracle cases (verbatim `invalid`/`mismatch` text; `json`/`io` category with documented message delta), then verifies the committed gen-2 manifest `evidence/issue-3-acceptance-run/snapshot-manifest.json` against retained bytes exposed via same-filesystem **hardlinks** in a mkdtemp private root (cleaned after; sources read-only, SHAs verified vs `manifest-sha256.txt` first).

## Real gen-2 result

`FILE_BYTES_VERIFIED`; roles sorted `[base_industry, day, gbbq]`; all limitation fields verbatim (`NOT_AUTHENTICATED`/`NOT_VERIFIED`/`NOT_RUN`, `production_ready:false`). Bounded measurement: 549,387,306B artifact hashed in ~512ms with ~1.1MiB RSS delta — streaming confirmed; **no performance claim**.

## Error model

`invalid`/`mismatch` texts byte-verbatim (`artifact bytes do not match manifest: Gbbq` — PascalCase role). `json`/`io` category preserved; Node message text differs from serde/io::Error (documented in results.json `node_specific_deltas`).
