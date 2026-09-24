# NM-057 — update-task commit surface (RangeRepository)

Ports the update-task commit surface for the compact (schema-19)
backend — the `RangeRepository`-equivalent trait surface plus the typed
`commit_range` variants and the update batch commit — and exercises the
NM-055 ported-but-unexercised result/commit functions.

## Modules

- `src_node/market/compact_write.mjs` — `commitRangeTx` (replay →
  base Published → required heads → continuity evidence →
  `commitMarketUpdate`), `commitRangeOps`/`commitRangeOpsTyped`/
  `commitRangeNoChangeTyped` (typed ops commit + no-change arm),
  `commitUpdateBatch` (one write tx: claim → per-item start → ops
  staging → commit → item finish; content failures recorded as item
  outcomes), `commitTx`/`commitPublication`/`commitMarketUpdate`,
  `commitYearly`/`yearlyNoChangeTx`, `receipt`/`yearlyReceipt`/
  `yearlyItemReceipt`, `typedReplayReceipt`.
- `src_node/storage/task_repository.mjs` — `readBoundSnapshot`
  (`{head, items}` decision snapshot for the batch/light-transition
  path) + `transitionLightTx` usage; `rewriteItemOutcomes` replicates
  `write_items_delta`'s whole-snapshot arm (head-level changes
  re-encode stored `outcome_json` in serde `ItemOutcome` field order).

## Differential

New ops in both runners (`src/diff_harness.rs` /
`scripts/node_migration_p3/diff_runner.mjs`):

- `commit_range_ops`/`commit_range_ops_typed`/`commit_range_no_change_typed`/
  `commit_update_batch` — compact path; legacy emits the trait-default
  `None` shape verbatim.
- `commit_range` — dispatched to the compact backend when the fixture
  is schema-19 (`@candidate`/`@base` record markers materialize through
  the backend itself so `stored == plan.record` byte-compares).
- `commit_publication` (`MarketRepository::commit` → `commit_tx`),
  `commit_yearly` (`YearlyRepository::commit_yearly` →
  `commit_tx`/`yearly_no_change_tx`), `yearly_receipt`,
  `yearly_item_receipt`, `receipt`.

Comparator now compares **all** emitted fields (except runner-local
`seq`/`op`/`status`) plus the state digest — previously digest/receipt
fields were emitted but not compared.

## Reproduce

```
node scripts/node_migration_p3/diff_harness_probe.mjs .        # exit 0
node scripts/node_migration_p3/diff_harness_probe.mjs . diff   # selftest, exit 1
```

28/28 scenarios PASS; `compact_commit_surface` 64 ops, 0 diffs.
