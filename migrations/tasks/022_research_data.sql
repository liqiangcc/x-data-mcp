-- Schema 22 (M1 research data, docs/tasks/issue-383-m1-research-data.md §3):
-- the adjustment (口径) and price_scale (精度) dimensions become series
-- properties, and the price columns stop claiming a unit.
--
-- Applied at the tail of fresh compact init and of the 18->compact upgrade.
-- Both paths run on empty market tables (D-08: archive + fresh init, never an
-- in-place data migration), so every table below is rebuilt with
-- "create new + copy + drop + rename" purely to change CHECK/UNIQUE clauses
-- (SQLite cannot alter them). The copy statements carry the old values
-- verbatim: a populated schema-21 table would fail closed here instead of
-- being silently relabelled (digest_format stays 'series-fold-v1', which the
-- new CHECK rejects). Nothing is migrated.

-- evidence_bundles: 'derived' joins the kind vocabulary (calendar / factor
-- set / index unit-and-scope evidence, §3.9, §6.3).
CREATE TABLE evidence_bundles_v22(
 evidence_digest BLOB PRIMARY KEY CHECK(length(evidence_digest)=32),
 evidence_kind TEXT NOT NULL CHECK(evidence_kind IN (
   'generation','batch_selection','version','continuity','catalog','yearly',
   'derived')),
 canonical_json TEXT NOT NULL
) STRICT, WITHOUT ROWID;
INSERT INTO evidence_bundles_v22(evidence_digest,evidence_kind,canonical_json)
  SELECT evidence_digest,evidence_kind,canonical_json FROM evidence_bundles;
DROP TABLE evidence_bundles;
ALTER TABLE evidence_bundles_v22 RENAME TO evidence_bundles;

-- market_series: adjustment 0=不复权 / 1=前复权; instrument_class
-- 0=股票 / 1=指数 / 2=ETF (reserved, no rows in M1); price_scale is the
-- denominator exponent of the integer prices in series_bars.
CREATE TABLE market_series_v22(
  series_key INTEGER PRIMARY KEY,
  entity TEXT NOT NULL,
  period INTEGER NOT NULL CHECK(period IN (1,2)),
  adjustment INTEGER NOT NULL CHECK(adjustment IN (0,1)),
  instrument_class INTEGER NOT NULL CHECK(instrument_class IN (0,1,2)),
  price_scale INTEGER NOT NULL CHECK(price_scale BETWEEN 0 AND 6),
  version_id TEXT NOT NULL UNIQUE,
  content_digest BLOB NOT NULL CHECK(length(content_digest)=32),
  digest_format TEXT NOT NULL CHECK(digest_format='series-fold-v2'),
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
  CHECK(version_id = lower(hex(content_digest))),
  -- Yearly series exist only in the forward-adjusted 口径; raw stays daily.
  CHECK(period = 1 OR adjustment = 1)
) STRICT;
-- A schema-21 row carries digest_format='series-fold-v1' and is rejected by
-- the CHECK above: the v2 digest binds 口径/精度 into the seed, so relabelling
-- a v1 digest as v2 would be a silent identity change.
INSERT INTO market_series_v22(series_key,entity,period,adjustment,
    instrument_class,price_scale,version_id,content_digest,digest_format,
    record_count,first_day,last_day,coverage_status,source_binding_digest,
    evidence_digest,last_event_seq,created_at,updated_at)
  SELECT series_key,entity,period,adjustment,0,2,version_id,content_digest,
    digest_format,record_count,first_day,last_day,coverage_status,
    source_binding_digest,evidence_digest,last_event_seq,created_at,updated_at
  FROM market_series;
DROP TABLE market_series;
ALTER TABLE market_series_v22 RENAME TO market_series;

-- series_bars: prices are integers interpreted through
-- market_series.price_scale (元 / 点), so the columns no longer name 分.
-- volume_quantum reserves the per-bar volume precision decided by the owner
-- (#381 option 1: 手-day bars are 100-share quanta); M1 writes NULL.
CREATE TABLE series_bars_v22(
  series_key INTEGER NOT NULL REFERENCES market_series(series_key),
  trade_day INTEGER NOT NULL,
  open_units INTEGER NOT NULL,
  high_units INTEGER NOT NULL,
  low_units INTEGER NOT NULL,
  close_units INTEGER NOT NULL,
  volume INTEGER,
  amount TEXT,
  r7_raw INTEGER,
  volume_quantum INTEGER CHECK(volume_quantum IS NULL OR volume_quantum > 0),
  PRIMARY KEY(series_key, trade_day),
  CHECK(high_units >= open_units AND high_units >= close_units AND high_units >= low_units),
  CHECK(low_units <= open_units AND low_units <= close_units),
  CHECK(volume IS NULL OR volume >= 0),
  CHECK(r7_raw IS NULL OR r7_raw BETWEEN 0 AND 4294967295)
) STRICT, WITHOUT ROWID;
INSERT INTO series_bars_v22(series_key,trade_day,open_units,high_units,
    low_units,close_units,volume,amount,r7_raw,volume_quantum)
  SELECT series_key,trade_day,open_cents,high_cents,low_cents,close_cents,
    volume,amount,r7_raw,NULL
  FROM series_bars;
DROP TABLE series_bars;
ALTER TABLE series_bars_v22 RENAME TO series_bars;

-- yearly_series_lineage: the derived-from 口径 becomes explicit so a future
-- raw-daily re-aggregation cannot reuse a forward lineage row (§3.5).
CREATE TABLE yearly_series_lineage_v22(
  series_key INTEGER PRIMARY KEY REFERENCES market_series(series_key),
  base_daily_digest BLOB NOT NULL CHECK(length(base_daily_digest)=32),
  base_event_seq INTEGER NOT NULL CHECK(base_event_seq > 0),
  base_adjustment INTEGER NOT NULL DEFAULT 1 CHECK(base_adjustment IN (0,1)),
  base_instrument_class INTEGER NOT NULL DEFAULT 0
    CHECK(base_instrument_class IN (0,1,2)),
  algorithm_version TEXT NOT NULL,
  first_trade_day INTEGER NOT NULL,
  last_trade_day INTEGER NOT NULL,
  as_of_day INTEGER NOT NULL,
  CHECK(first_trade_day <= last_trade_day),
  CHECK(last_trade_day <= as_of_day)
) STRICT;
-- Schema 21 yearly rows were derived from the forward-adjusted stock daily
-- series, so the carried base 口径 is (1, 0) by definition.
INSERT INTO yearly_series_lineage_v22(series_key,base_daily_digest,
    base_event_seq,base_adjustment,base_instrument_class,algorithm_version,
    first_trade_day,last_trade_day,as_of_day)
  SELECT series_key,base_daily_digest,base_event_seq,1,0,algorithm_version,
    first_trade_day,last_trade_day,as_of_day
  FROM yearly_series_lineage;
DROP TABLE yearly_series_lineage;
ALTER TABLE yearly_series_lineage_v22 RENAME TO yearly_series_lineage;

-- market_events: one item now audits one event per 口径, so the uniqueness
-- anchor becomes (task_id, item_id, adjustment); result_id stays globally
-- unique. The covering index carries adjustment so the startup closure probe
-- stays two index seeks per series.
CREATE TABLE market_events_v22(
  seq INTEGER PRIMARY KEY CHECK(seq > 0),
  entity TEXT NOT NULL,
  period INTEGER NOT NULL CHECK(period IN (1,2)),
  adjustment INTEGER NOT NULL CHECK(adjustment IN (0,1)),
  kind TEXT NOT NULL CHECK(kind IN
    ('first_publish','append','replace','no_change','metadata','yearly_rederive')),
  outcome INTEGER NOT NULL CHECK(outcome IN (1,2)),
  metadata_changed INTEGER NOT NULL CHECK(metadata_changed IN (0,1)),
  semantics_changed INTEGER NOT NULL DEFAULT 0 CHECK(semantics_changed IN (0,1)),
  continuity_mode INTEGER CHECK(continuity_mode IN (1,2,3)),
  prefix_bytes INTEGER,
  old_content_digest BLOB CHECK(old_content_digest IS NULL OR length(old_content_digest)=32),
  new_content_digest BLOB NOT NULL CHECK(length(new_content_digest)=32),
  range_start_day INTEGER,
  range_end_day INTEGER,
  window_digest BLOB CHECK(window_digest IS NULL OR length(window_digest)=32),
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
  UNIQUE(task_id, item_id, adjustment),
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
-- Every schema-21 event belongs to the forward-adjusted series by definition.
INSERT INTO market_events_v22(seq,entity,period,adjustment,kind,outcome,
    metadata_changed,semantics_changed,continuity_mode,prefix_bytes,
    old_content_digest,new_content_digest,range_start_day,range_end_day,
    window_digest,base_content_digest,algorithm_version,source_binding_digest,
    evidence_digest,task_id,item_id,task_revision,result_id,result_digest,
    created_at)
  SELECT seq,entity,period,1,kind,outcome,metadata_changed,semantics_changed,
    continuity_mode,prefix_bytes,old_content_digest,new_content_digest,
    range_start_day,range_end_day,window_digest,base_content_digest,
    algorithm_version,source_binding_digest,evidence_digest,task_id,item_id,
    task_revision,result_id,result_digest,created_at
  FROM market_events;
DROP TABLE market_events;
ALTER TABLE market_events_v22 RENAME TO market_events;
CREATE INDEX market_events_entity_period
  ON market_events(entity, period, adjustment, seq DESC, new_content_digest, kind);
CREATE INDEX market_events_new_digest ON market_events(new_content_digest);

-- ---------------------------------------------------------------------------
-- New tables (M1 PR-4, issue-383 §3.6/§3.7). Both are new in schema 22, so
-- they are created rather than rebuilt: no schema-21 row can exist for them.
-- ---------------------------------------------------------------------------

-- adjustment_events: the parsed factor facts (解析事实层, separation point 2).
-- `c_bits_*` are the source's f32 factor values verbatim (immutable fact);
-- `c1..c4` are the derived 2dp business decimals and are re-derivable from
-- the bits (`gbbq-cat1-2dp-v1`), which a test binds.
-- `effective_day` uses the same epoch-day integer as `series_bars.trade_day`.
-- `known_at` is the 知晓时间 (separation point 3): the source carries only the
-- ex-rights date, so M1 always writes NULL = unknown. No file mtime or other
-- proxy is ever substituted.
CREATE TABLE adjustment_events(
  entity TEXT NOT NULL,
  -- Decode order within the entity: same-day events stay distinguishable.
  ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
  effective_day INTEGER NOT NULL,
  -- Only the verified category is stored; other categories are counted into
  -- the evidence bundle and never interpreted (§3.6 item 3).
  category INTEGER NOT NULL CHECK(category = 1),
  c_bits_1 INTEGER NOT NULL CHECK(c_bits_1 BETWEEN 0 AND 4294967295),
  c_bits_2 INTEGER NOT NULL CHECK(c_bits_2 BETWEEN 0 AND 4294967295),
  c_bits_3 INTEGER NOT NULL CHECK(c_bits_3 BETWEEN 0 AND 4294967295),
  c_bits_4 INTEGER NOT NULL CHECK(c_bits_4 BETWEEN 0 AND 4294967295),
  c1 TEXT NOT NULL,
  c2 TEXT NOT NULL,
  c3 TEXT NOT NULL,
  c4 TEXT NOT NULL,
  source_binding_digest BLOB NOT NULL REFERENCES source_bindings(source_binding_digest),
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  known_at TEXT,
  PRIMARY KEY(entity, ordinal)
) STRICT, WITHOUT ROWID;
CREATE INDEX adjustment_events_entity_day
  ON adjustment_events(entity, effective_day, ordinal);

-- derived_dataset_versions: identity + audit for the non-bar datasets, so an
-- export manifest can name "the calendar" or "this entity's factor set" with
-- a stable version id (§3.7). Business rows stay in their own typed tables.
-- `digest_format` is the digest algorithm name ('adjustment-events-v1' for
-- factor sets, §3.10); `algorithm_version` is the derivation label
-- ('gbbq-cat1-2dp-v1'). `version_id` is the content digest in hex, exactly as
-- `market_series.version_id`.
CREATE TABLE derived_dataset_versions(
  dataset_kind TEXT NOT NULL CHECK(dataset_kind IN ('adjustment_events','trading_calendar')),
  -- Factor set = the entity; the calendar = 'exchange_union'.
  dataset_key TEXT NOT NULL,
  version_id TEXT NOT NULL UNIQUE,
  content_digest BLOB NOT NULL CHECK(length(content_digest)=32),
  digest_format TEXT NOT NULL,
  record_count INTEGER NOT NULL CHECK(record_count > 0),
  first_day INTEGER NOT NULL,
  last_day INTEGER NOT NULL,
  algorithm_version TEXT NOT NULL,
  source_binding_digest BLOB NOT NULL REFERENCES source_bindings(source_binding_digest),
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  task_id TEXT NOT NULL,
  item_id TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY(dataset_kind, dataset_key),
  CHECK(first_day <= last_day),
  CHECK(version_id = lower(hex(content_digest)))
) STRICT;
