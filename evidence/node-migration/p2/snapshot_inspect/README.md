# NM-026 P2 snapshot manifest byte inspection — evidence

Baseline `6c8718d48f3580688e6aa3ee258621cf55fdacb7` (`migration/node-devin`). Node v20.19.2.

## Contents

- `probe-results.jsonl` — 60 rows, all PASS; probe exits nonzero on FAIL. `negative-retained-sha.jsonl` — selftest proof (exit 1).
- `rust-oracle.jsonl` — live `rustc 1.98.1` + `x_data_mcp` rlib: 40 error + 13 ok incl. `real_gen2` full Inspection JSON.
- `results.json` — semantics, provenance, bounded measurement, Node deltas.
- `SHA256SUMS` — over the three files.

## Reproduction

```sh
node scripts/node_migration_p2/snapshot_probe.mjs .
```

Probe builds synthetic fixture dirs under `/dev/shm`, runs all 37 oracle cases (verbatim `invalid`/`mismatch` text; `json`/`io` category with documented message delta), then verifies the committed gen-2 manifest `evidence/issue-3-acceptance-run/snapshot-manifest.json` against retained bytes exposed via same-filesystem **hardlinks** in a mkdtemp private root (mkdtemp-owned, exit-handler cleanup; sources read-only, SHAs streamed-verified vs `manifest-sha256.txt` first).

## Real gen-2 result

`FILE_BYTES_VERIFIED`; roles sorted `[base_industry, day, gbbq]`; all limitation fields verbatim (`NOT_AUTHENTICATED`/`NOT_VERIFIED`/`NOT_RUN`, `production_ready:false`). Bounded measurement: 549,387,306B artifact streamed-hashed ~517ms; whole-process VmHWM 64.3MiB (incl. streamed retained pins) — **no performance claim**.

## Error model

`invalid`/`mismatch` texts byte-verbatim (`artifact bytes do not match manifest: Gbbq` — PascalCase role). `json`/`io` category preserved; message text differs from serde/io::Error (documented).

R1 additions (REVIEW-NM-026): serde Option omission → null; strictJson duplicate-key rejection; fatal UTF-8 decode; chrono-strict RFC3339 (space/lowercase separators, required offset <24h, calendar strict, leap `:60`, ns 3-digit groups); mkdtemp + exit-handler cleanup; streaming retained pin hash; VmHWM measurement; `retained_sha` selftest exit-1 proof.
