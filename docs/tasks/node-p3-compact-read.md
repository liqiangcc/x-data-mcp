# node-p3-compact-read — compact (schema-19) market read path

Baseline: NM-053 `2db8fc7`. Slice: NM-054. Rust sources:
`src/sqlite_market/compact_repo.rs` (`materialize_checked`, `synthesize`),
`src/sqlite_market/compact_versions.rs` (read side: `materialize`,
`materialize_windowed`, `list`, `fixed`, `verify_staged`,
`verify_published_*`, `verify_stored_lineage`/`verify_lineage`,
`build_identity`, `reconstruct_*`, `leaf_bars_in_range`),
`src/sqlite_market/compact_v19.rs` (segment/clip reads),
`src/compact_market.rs` (digests + typed encodings),
`src/sqlite_market.rs` (`is_compact_schema` dispatch in
`head`/`read_record`/`range_rows`/`versions`).

## Files

- `src_node/market/compact_market.mjs` — primitives: epochDay/epochDate
  (Chrono `num_days_from_ce`, range `[-96465292, 95026601]`),
  periodCode/decodePeriod, canonicalEntity, exact `cents`/`volume`/
  `amount` Decimal→typed conversion (truncate-toward-zero, i64 bounds,
  exact-equality reparse), CompactBar `check`/`toBar`, JCS canonical JSON
  (`sortSer`), `segmentDigest`, `checkpointProjectionDigest`,
  `deltaDigest`/`deltaProjectionDigest`, `evidenceDigest`/`canonicalEvidence`/
  `checkVersionEvidence`/`checkEvidenceFields`, `sourceBindingDigest`,
  `versionIntegrityDigest` (`compact-market-version-v1` domain),
  `canonicalRfc3339` (writer-side canonical gate), `parseRfc3339Utc`
  (chrono `parse_from_rfc3339` → UTC → AutoSi `to_rfc3339`),
  `serdeDateTime` (+00:00→Z wire form).
- `src_node/market/compact_read.mjs` — `readVersionRow(+ById)` (state/
  coverage/digest_format guards verbatim), `readSegment`,
  `checkClipAgainstSegment`/`checkStoredClip`, `readCheckpoint`,
  `readDelta` (delta_digest recompute + depth/chain linkage),
  `leafBarsInRange`, `reconstructVersion`/`reconstructLeaf`,
  `flattenClips`, `applyDelta` (`delta_ops` verbatim: upsert windows,
  bounds, ordering, count/days guards), `verifyVersionEvidence`/
  `readEvidence`/`readSourceBinding` (incl. generation-kind gate +
  `verify_evidence`), `storedLineage`/`verifyStoredLineage`,
  `verifyLineage` (re-aggregates published daily base via
  `algorithms/yearly.mjs`, date-string comparison),
  `buildIdentity` (parent linkage + yearly draft + mode/coverage),
  `verifyPublishedFields`/`verifyPublishedNode`/`verifyNodeCommon`/
  `verifyStaged`/`checkBarsUnits`, `fixedByKey`, `toEffectiveVersion`
  (published_at parse), `unpublishedVersion` (created_at as stamp),
  `materialize`, `materializeChecked` (published ⇒ `!staged`,
  validated ⇒ allowed, staged ⇒ `staged`), `synthesize` (serde-order
  raw; published → slim envelope + stored integrity digest; staged/
  validated → full bars + `RC.legacyDigest`; `compact_source` +
  `compact_parent_version_id`), `compactRangeRows`, `compactVersions`
  (orphan check + list validation + materialize + ordering),
  `headId` (effective_heads version_key join).
- `src_node/market/market_read.mjs` — `isCompactSchema` dispatch:
  `head` → `headId`; `readRecord` → `materializeChecked` +
  yearly-in-daily guard; `rangeRows` → `compactRangeRows`;
  `versions` → `compactVersions`. `decStr` extended to normalize
  Dec `{m,s}` objects (compact bars carry typed Decs). Legacy paths
  unchanged.
- `src/diff_harness.rs` — `build_compact_records` (schema-19 fixture:
  raw `insert_segment`/`put_evidence`/`put_source_binding`/
  `stage_checkpoint` + `recompute_integrity` publish; yearly bars
  derived via `yearly::aggregate` on the materialized base);
  compact-aware `head`/`read_record`/`range`/`versions` ops on a raw
  connection (SqliteMarket read gate rejects >18); `versions` compact
  branch replicates `compact_repo::versions` (orphan + list +
  materialize).
- `scripts/node_migration_p3/diff_runner.mjs` — `exec_sql` error code
  `unavailable` (mapped `Storage`); `read_record` `first`/`last` accept
  plain-string dates (compact `to_bar` emits strings).
- `scripts/node_migration_p3/diff_harness_probe.mjs` — `seed.compact`
  → `DIFF_INIT=19` passthrough.
- Fixtures: `records/compact_daily.json` (5 bars), `compact_daily2.json`
  (6 bars, head), `compact_yearly.json` (`yearly_base: c-day-1`),
  `compact_staged.json` (`|staged` — state=1).
- Scenarios: `compact_read` (17 ops: head×2, fixed/candidate reads,
  versions×2, range paging + after + scope-mismatch, missing-id),
  `compact_tamper` (clip delete → `checkpoint seq not contiguous` fail
  closed; staged candidate read), `compact_tamper_payload`
  (segment_bars delete → `segment row_count mismatch`),
  `compact_tamper_digest` (close_cents drift → `segment digest
  mismatch`; evidence_digest randomblob → `evidence bundle missing`;
  source_bindings delete; evidence_bundles delete),
  `compact_tamper_lineage` (yearly first_trade_day+1 → `yearly lineage
  does not match recomputed aggregation`; head delete → NotFound).

## Verified

- Differential harness (Rust oracle `diff_harness::run` vs
  `diff_runner.mjs` on identical schema-19 bytes): 24 scenarios,
  0 diffs — compact 5 scenarios / 48 ops all MATCH. Stored integrity
  digests emitted by `read_record` are byte-identical across
  implementations (`218c7ed…`, `fd2a6b4…`, `a3f1ce4…` for the three
  published fixture versions).
- Fail-closed tamper parity: clip/segment/row deletes, digest drift,
  evidence/source-binding removal, lineage drift, head removal —
  identical status+code+message on both sides.
- Regressions NM-035..NM-053 probes all PASS; selftest exits 1.

## Known deltas / deferred

- `materialize_windowed` not ported — `range_rows` uses
  `materialize_checked` verbatim (the compact branch of the ported
  `SqliteMarket::range` path); windowed leaf reads belong to the MCP
  query tools slice.
- Chrono `parse_from_rfc3339` failure text is not cloned verbatim —
  kind (`MARKET_INTEGRITY`) and fail-closed parity hold; message
  wording differs for malformed stored timestamps.
- `compact_repo` stage/commit writes, `materialize_windowed`,
  `commit_tx`, MCP query tools: separate slices.
