# node-p3-compact-write — compact (schema-19) ops write path

Baseline: NM-054 `0946d0d` (accepted with limits). Slice: NM-055. Rust
sources: `src/sqlite_market/compact_repo.rs` (`stage_ops_delta`/`_tx`,
`validate_staged`, `commit_ops_update`/`_tx`, `commit_ops_no_change`/`_tx`,
`update_base_facts`, `base_bars_in_range`, `commit_tx`, `commit_yearly`,
`yearly_no_change_tx`), `src/sqlite_market/compact_results.rs`
(`commit_market_update`/`_tx`, `commit_publication`/`_tx`,
`read_market_update`, `read_publication`, `read_update_audit`,
`read_yearly_audit`, `plan_market_update`, `check_required_heads`,
`check_binding`, `require_item_unused`, `register_result`, `apply_head`,
`finish_item_and_task`, `result_outcome_json`), `compact_versions.rs`
write helpers (`stage_checkpoint`, `stage_delta`, `stage_delta_append`,
`put_evidence`, `put_source_binding`, `insert_segment`,
`insert_version_row`, `mark_validated_delta`, `publish`/`publish_presumed`/
`publish_delta`, `check_draft_static`/`lineage`/`evidence`/`binding`,
`insert_lineage_row`, `recompute_integrity`).

## Files

- `src_node/market/compact_market.mjs` — write-side digest additions:
  `deltaOpsPlain`, `deltaProjectionDigest`, `checkpointProjectionDigest`
  (already), `marketUpdateResultDigest` (`market-update-result-v1`),
  `publicationResultDigest` (`publication-result-v1`), `nowRfc3339`,
  `digestHex`. CompactBar↔Bar conversion accepts serde wire strings
  (Decimal `from_str` path) as well as `{m,s}` decimals.
- `src_node/market/compact_write.mjs` — the ops write surface:
  `insertSegment` (content-addressed dedup + collision check),
  `putEvidence`/`putSourceBinding`, `insertVersionRow`(+`Stats`),
  `stageCheckpoint` (≤512-bar chunking + clip rows + lineage),
  `stageDelta`/`stageDeltaAppend` (full projection vs O(ops) append),
  `stageOpsDelta`/`stageOpsDeltaTx` (draft statics, sort/dedup guards,
  append-only vs full path), `validateStaged` (`mark_validated_delta`),
  `planMarketUpdate`, `commitMarketUpdateTx`/`commitMarketUpdate`,
  `commitPublicationTx`/`commitPublication`, `checkRequiredHeads`,
  `checkBinding`, `requireItemUnused`, `registerResult`, `applyHead`
  (head CAS: keep/missing/version), `finishItemAndTask`,
  `commitOpsUpdate`/`commitOpsNoChange` (+`_tx`),
  `commitTx`/`commitYearly`/`yearlyNoChangeTx`, `updateBaseFacts`,
  `baseBarsInRange`, `readUpdateAudit`/`readYearlyAudit`,
  `readMarketUpdate`/`readPublication`, `receipt`/`yearlyReceipt`/
  `yearlyItemReceipt`/`replayReceipt`.
- `src_node/market/compact_read.mjs` — write-side helpers:
  `requirePublishedFacts`, `publishDelta`, `markValidatedDelta`,
  `resultFacts`, `verifiedEvidenceKind`, `verifiedSourceBinding`,
  `versionKey`, `__*` internals for the writer/harness.
- `src_node/market/market_read.mjs` — `readResult` dispatches compact
  schema-19 to `compact_results::read_update_audit` equivalent;
  `readItemBinding` evidence-marker mismatch is a task-level
  `inconsistent()` → `needs_verification` (Rust `Error::Task` is
  transparent).

## Atomicity contract (verbatim)

- `stage_ops_delta`, `validate_staged` run inside `self.tx` —
  BEGIN IMMEDIATE + COMMIT/ROLLBACK.
- `commit_ops_update`, `commit_ops_no_change`, `commit` (commit_tx),
  `commit_yearly` run under `self.write` — the coordinator executes the
  closure in AUTOCOMMIT. Mid-failure partial writes persist (evidence
  bundles, the registry row); the differential harness proves identical
  partial state on both runners. The `_tx` inner forms take the
  caller-supplied boundary; `commit_market_update`/`commit_publication`
  (used by commit_tx paths) open their own nested IMMEDIATE tx.
- `commit_publication_tx`'s `verify_stored` flag controls the defense
  re-verify (`publish`+`fixed` vs `publish_presumed`); the standalone
  path passes true.

## Differential

`scripts/node_migration_p3/scenarios/compact_write.jsonl` — 42 ops:
base facts + base-bars reads, delta stage (append), dedup reuse across
records (same bars → same content-addressed segment), staged-checkpoint
validate, staged yearly validate (lineage re-aggregation), unvalidated
commit reject, head-moved conflict (partial-write parity), replay
idempotent + divergent, tamper→fail-closed (deleted delta clip →
`delta missing`), task admission/claim/item-start, update commit with
promote, no-change commit, head/read_result/item_receipt reads.

Harness: `DIFF_INIT=19` fixture builds via the real compact write
helpers; `DIFF_NORM_TS=1` normalizes wall-clock fields (created_at,
published_at, integrity_digest recomputed over a fixed stamp); the
compact state digest covers 12 table groups (CV/CVI/CS/CB/CC/CD/CY/CH/
CE/CSB/CTP/CRR/CMU/CMD/CPR).

Harness-caught divergences fixed in-slice:

- `inconsistent()` in `sqlite_tasks` maps to `Error::NeedsVerification`
  (display `ACCEPTANCE_NEEDS_VERIFICATION: <m>`) — the earlier port
  emitted `inconsistent`; probes updated to assert the verbatim code +
  prefix. Compact fixture's seeded `task-pub` exposed it via the
  registry gate.
- `commit_ops_*` autocommit partial-write semantics (above) — the
  comparator now exempts these ops from the error⇒unchanged invariant;
  identical partial state is still digest-compared.
- better-sqlite3 rejects `?N` placeholders → positional `?` everywhere.
- Task ops on schema-19 route through the coordinator-bound handle
  (`SqliteCompactMarket::tasks()`), not the bare schema gate.
