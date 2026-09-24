# NM-056 — market tail: gaps + yearly_results + read_facts + publication_checks + backend open

Same JSONL scenario vocabulary as NM-049+; both runners execute the
real storage surfaces — Rust via `diff_harness.rs` (#[cfg(test)],
rustup 1.98.1), Node via `diff_runner.mjs` against the ported modules.

## Contents

- `probe-results.jsonl` — 27 scenarios PASS (market_tail 52 ops,
  compact_tail 38 ops, 0 diffs).
- `negative-selftest.jsonl` (exit 1), `results.json`, `SHA256SUMS`.

## Reproduce

    node scripts/node_migration_p3/diff_harness_probe.mjs .        # exit 0
    node scripts/node_migration_p3/diff_harness_probe.mjs . diff   # selftest, exit 1

## Notes

- `DIFF_CHECK=1` seeds the publication-check fixture on both schemas
  (legacy: committed all_recorded pair; compact: failed-item
  not_applicable pair).
- The legacy arm of `read_facts` is exercised via a `start_collection`
  task owning both `publication_batches` rows.
