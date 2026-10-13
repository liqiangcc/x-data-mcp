-- Schema 21 latest-only market storage (SPEC-LATEST-ONLY §1, RULINGS A1/A5):
-- every (entity,period,adjustment) keeps exactly one current row; bars live in
-- series_bars; every state transition appends one market_events row inside the
-- same transaction. Applied at the tail of fresh compact init and of the
-- 18->compact upgrade: both paths run on empty market tables, so the schema-19
-- version-manifest tables are dropped, never migrated.
CREATE TABLE market_series(
  series_key INTEGER PRIMARY KEY,
  entity TEXT NOT NULL,
  period INTEGER NOT NULL CHECK(period IN (1,2)),
  adjustment INTEGER NOT NULL CHECK(adjustment=1),
  version_id TEXT NOT NULL UNIQUE,
  content_digest BLOB NOT NULL CHECK(length(content_digest)=32),
  digest_format TEXT NOT NULL CHECK(digest_format='series-fold-v1'),
  record_count INTEGER NOT NULL CHECK(record_count > 0),
  first_day INTEGER NOT NULL,
  last_day INTEGER NOT NULL,
  coverage_status INTEGER NOT NULL CHECK(coverage_status IN (1,2,3)),
  source_binding_digest BLOB NOT NULL REFERENCES source_bindings(source_binding_digest),
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  last_event_seq INTEGER NOT NULL CHECK(last_event_seq > 0),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(entity, period, adjustment),
  CHECK(first_day <= last_day),
  CHECK(version_id = lower(hex(content_digest)))
) STRICT;
-- One row per bar (one row per trade day per series); yearly bars carry the
-- year's last trading day. CHECKs carried over from segment_bars_v19.
CREATE TABLE series_bars(
  series_key INTEGER NOT NULL REFERENCES market_series(series_key),
  trade_day INTEGER NOT NULL,
  open_cents INTEGER NOT NULL,
  high_cents INTEGER NOT NULL,
  low_cents INTEGER NOT NULL,
  close_cents INTEGER NOT NULL,
  volume INTEGER,
  amount TEXT,
  r7_raw INTEGER,
  PRIMARY KEY(series_key, trade_day),
  CHECK(high_cents >= open_cents AND high_cents >= close_cents AND high_cents >= low_cents),
  CHECK(low_cents <= open_cents AND low_cents <= close_cents),
  CHECK(volume IS NULL OR volume >= 0),
  CHECK(r7_raw IS NULL OR r7_raw BETWEEN 0 AND 4294967295)
) STRICT, WITHOUT ROWID;
-- Yearly derivation lineage for period=2 series rows. base_daily_digest is the
-- daily content_digest at derivation time; base_event_seq the daily
-- last_event_seq then (RULINGS A1 stale classification).
CREATE TABLE yearly_series_lineage(
  series_key INTEGER PRIMARY KEY REFERENCES market_series(series_key),
  base_daily_digest BLOB NOT NULL CHECK(length(base_daily_digest)=32),
  base_event_seq INTEGER NOT NULL CHECK(base_event_seq > 0),
  algorithm_version TEXT NOT NULL,
  first_trade_day INTEGER NOT NULL,
  last_trade_day INTEGER NOT NULL,
  as_of_day INTEGER NOT NULL,
  CHECK(first_trade_day <= last_trade_day),
  CHECK(last_trade_day <= as_of_day)
) STRICT;
-- Event log: replaces market_update_results / publication_results /
-- task_publications / result_registry / market_update_delta_clips. The event
-- row and the market_series state row it advances commit in one transaction;
-- `result_id` is the idempotent-replay anchor.
CREATE TABLE market_events(
  seq INTEGER PRIMARY KEY CHECK(seq > 0),
  entity TEXT NOT NULL,
  period INTEGER NOT NULL CHECK(period IN (1,2)),
  kind TEXT NOT NULL CHECK(kind IN
    ('first_publish','append','replace','no_change','metadata','yearly_rederive')),
  outcome INTEGER NOT NULL CHECK(outcome IN (1,2)),
  metadata_changed INTEGER NOT NULL CHECK(metadata_changed IN (0,1)),
  -- C3: 1 when the commit's version evidence differs from the head's in
  -- {source, unit_currency, adjustment_evidence} — the fields yearly
  -- aggregation depends on. 0 for no_change/metadata-without-evidence.
  semantics_changed INTEGER NOT NULL DEFAULT 0 CHECK(semantics_changed IN (0,1)),
  continuity_mode INTEGER CHECK(continuity_mode IN (1,2,3)),
  -- Nominal value only (record_count*32 at append), NOT real prefix byte
  -- count; audit display only, never feed bar math with it.
  prefix_bytes INTEGER,
  old_content_digest BLOB CHECK(old_content_digest IS NULL OR length(old_content_digest)=32),
  new_content_digest BLOB NOT NULL CHECK(length(new_content_digest)=32),
  range_start_day INTEGER,
  range_end_day INTEGER,
  window_digest BLOB CHECK(window_digest IS NULL OR length(window_digest)=32),
  -- yearly_rederive events record the daily base digest they derived from
  -- (audit + verify surface; mirrors yearly_series_lineage.base_daily_digest).
  base_content_digest BLOB CHECK(base_content_digest IS NULL OR length(base_content_digest)=32),
  algorithm_version TEXT,
  source_binding_digest BLOB REFERENCES source_bindings(source_binding_digest),
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  task_id TEXT NOT NULL,
  item_id TEXT NOT NULL,
  task_revision INTEGER NOT NULL,
  result_id TEXT NOT NULL,
  result_digest BLOB NOT NULL CHECK(length(result_digest)=32),
  created_at TEXT NOT NULL,
  UNIQUE(result_id),
  UNIQUE(task_id, item_id),
  FOREIGN KEY(task_id,item_id) REFERENCES task_items(task_id,item_id),
  CHECK(range_start_day IS NULL OR range_end_day IS NULL
        OR range_start_day <= range_end_day),
  CHECK((continuity_mode IN (1,2) AND prefix_bytes IS NOT NULL AND prefix_bytes >= 0)
     OR (continuity_mode = 3 AND prefix_bytes IS NULL)
     OR (continuity_mode IS NULL AND prefix_bytes IS NULL)),
  CHECK((kind='first_publish' AND old_content_digest IS NULL)
     OR (kind<>'first_publish' AND old_content_digest IS NOT NULL)),
  CHECK((outcome=1 AND kind='no_change' AND old_content_digest = new_content_digest)
     OR (outcome=2 AND kind<>'no_change')),
  CHECK((kind='metadata' AND old_content_digest = new_content_digest AND metadata_changed=1)
     OR (kind<>'metadata'))
) STRICT, WITHOUT ROWID;
-- Covering for the startup/closure probes: tail closure (`seq`/`seq DESC`),
-- the per-pair first-event `kind` probe and the digest compare all read this
-- index without touching the table. Also drives the per-pair event
-- iterators in yearly_state/check_entity_chain.
CREATE INDEX market_events_entity_period
  ON market_events(entity, period, seq DESC, new_content_digest, kind);
-- §6.1 stale probe: did this content digest ever become current.
CREATE INDEX market_events_new_digest ON market_events(new_content_digest);

-- Envelope-path scratch: stage() stores the caller-verified candidate row
-- here and commit()/candidate()/staged_matches() bind plan.record to it.
-- Every commit path consumes its candidate inside the commit transaction
-- (commit_series_tx + the replay arms delete by staged_version_id). Rows
-- orphaned by a crashed or cancelled item run are swept by the next
-- stage() once their last stage attempt is older than STAGED_TTL
-- (compact_repo.rs); residual growth is bounded to candidates staged
-- within that window.
CREATE TABLE staged_records(
  version_id TEXT PRIMARY KEY,
  raw TEXT NOT NULL,
  created_at TEXT NOT NULL
) STRICT, WITHOUT ROWID;
-- Backs the TTL sweep's range delete (WHERE created_at < cutoff).
CREATE INDEX staged_records_created_at ON staged_records(created_at);
DROP TABLE market_update_delta_clips;
DROP TABLE task_publications;
DROP TABLE publication_results;
DROP TABLE market_update_results;
DROP TABLE result_registry;
DROP TABLE version_delta_clips;
DROP TABLE version_checkpoint_clips;
DROP TABLE yearly_lineage;
DROP TABLE effective_heads;
DROP TABLE segment_bars;
DROP TABLE bar_segments;
DROP TABLE data_versions;
