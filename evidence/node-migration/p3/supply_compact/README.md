# NM-074 supply_compact — differential evidence (schema-19)

`supply_compact.jsonl` — 35 ops, 0 diffs; also runs as `.migrated` (DIFF_INIT=up).

- Compact-arm coverage: task_publications join, publication_results kind=2
  yearly read (compact_results::read_yearly_audit re-aggregation),
  market_update_results result_id reads under v19, catalog reads.
- commit_range/commit_yearly seed real compact result rows in-scenario.
- Tamper arms corrupt market_update_results.result_digest and
  publication_results.result_digest — verify_* emit the compact
  Integrity messages verbatim.
- task_list/task_facts propagate `MARKET_STORAGE: missing field
  'operation'` on the fixture's `task-pub` stub row (serde CommandKey
  parity — unparseable keys are errors, not skips).
