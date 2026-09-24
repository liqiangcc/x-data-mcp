# P4 evidence — early bins (NM-060)

Golden-vector differential of the three early standalone CLI bins —
`inspect_day`, `inspect_snapshot`, `consistency_check` — between the Rust
binaries (`target/debug/*`, oracle) and the Node ports
(`src_node/bin/*.mjs`). Same inputs, byte-compare of stdout, stderr and
exit code.

## Reproduce

```bash
cd /home/box/x-data-mcp-node-w2
cargo build --bin inspect_day --bin inspect_snapshot --bin consistency_check
TMPDIR=/dev/shm node scripts/node_migration_p4/early_bins_probe.mjs \
  --out evidence/node-migration/p4/early_bins        # exit 0
TMPDIR=/dev/shm node scripts/node_migration_p4/early_bins_probe.mjs --selftest
                                                     # must exit 1
```

The probe materializes `.day` binaries, manifest mutations and a
>1 MiB manifest into `$TMPDIR` at run time. Committed fixtures:

- `fixtures/early_bins/cc/*.json` — 78 consistency inputs (decision
  shaping + serde decode-error classes).
- `fixtures/early_bins/snapshot/` — synthetic 3-file input dir +
  manifest.json (sha256/bytes computed over the fixture bytes).
- Repo inputs reused: `evidence/issue-166-windows-official-behavior/
  raw/files/sh600004.day`, `evidence/issue-131-file-set-consistency/
  input.json`, `evidence/issue-4-update-gen3-acceptance/
  snapshot-manifest-gen3.json`.
- External: `/home/box/retained-inputs/gen3` (marked `not_run` if absent).

## Result summary (this run)

- 123 cases: **122 pass, 0 fail, 1 documented divergence, 0 not_run**.
- `snapshot-real-gen3` hashed the full retained gen-3 set
  (~578 MB) on both sides — identical `Inspection` JSON.
- stderr modes: `exact` byte-compare except three malformed-manifest
  cases marked `category` — the ported `snapshot` manifest parser keeps
  its own diagnostic text (accepted delta from the earlier slice; both
  sides emit `manifest JSON invalid:` prefix, exit 1, empty stdout).

## Documented divergence (not a parity break)

`cc-divergence-u64max`: `bytes` field > 2^53. Rust accepts full u64 and
prints a decision; the ported `checkFact`
(`src_node/governance/file_consistency.mjs`) gates on
`Number.isSafeInteger`, so Node exits 1 with `files[i].bytes invalid`.
The boundary is in the governance slice (outside this lane); the probe
asserts the current Node behavior explicitly so a future fix flips the
check to parity.

## Files

- `results.json` — summary + per-case records.
- `results.jsonl` — one record per scenario case.
- `SHA256SUMS` — checksums of this directory's artifacts.
