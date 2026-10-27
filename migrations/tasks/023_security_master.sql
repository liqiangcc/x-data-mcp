-- Schema 23 (M2 按时点证券主表, docs/tasks/issue-391-m2-security-master.md §9):
-- the parsed-fact layer for identity/listing/delisting, code reuse, names,
-- capital events, risk warnings and suspensions, plus the trading-rule tables
-- (§8.1). Appended at the tail of fresh compact init and of the 18->compact
-- upgrade; both paths run on empty business tables (D-08: archive + fresh init,
-- never an in-place data migration).
--
-- Structure only: no writer, no derivation and no read surface is added here,
-- and no schema-22 file is rewritten in place. `COMPACT_SCHEMA_VERSION` moves
-- 22 -> 23, so a schema-22 file is refused exactly like any other foreign
-- schema (`check_schema`), and a schema-22 binary refuses a schema-23 file by
-- the same `version > COMPACT_SCHEMA_VERSION` guard (AC-M2-14).
--
-- Naming: epoch-day integers (`INTEGER`, days since 1970-01-01) are the same
-- unit as `series_bars.trade_day`; every `known_at`/`announced_at` is ISO8601
-- TEXT because the announcement sources are second-resolution (§9.2 note 3).
-- Absent knowledge is NULL, never a sentinel: a NULL `known_at` means "the
-- 知晓时间 is unknown" and point-in-time reads must exclude such rows and count
-- them (§5.4, §12.2).

-- ---------------------------------------------------------------------------
-- derived_dataset_versions: M1 PR-4 shipped a closed `dataset_kind` CHECK
-- ('adjustment_events','trading_calendar'), so the six M2 dataset kinds need a
-- rebuild (§9.3). SQLite cannot alter a CHECK, hence create + copy + drop +
-- rename; the copy carries the old values verbatim and the new vocabulary is a
-- strict superset, so no schema-22 row can be relabelled or lost. Keeping the
-- list closed is deliberate: an unregistered kind must fail closed instead of
-- becoming a free-form label.
-- ---------------------------------------------------------------------------
CREATE TABLE derived_dataset_versions_v23(
  dataset_kind TEXT NOT NULL CHECK(dataset_kind IN (
    'adjustment_events','trading_calendar',
    'security_master','security_names','capital_events','risk_warnings',
    'suspensions','price_limit_rules')),
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
INSERT INTO derived_dataset_versions_v23(dataset_kind,dataset_key,version_id,
    content_digest,digest_format,record_count,first_day,last_day,
    algorithm_version,source_binding_digest,evidence_digest,task_id,item_id,
    created_at,updated_at)
  SELECT dataset_kind,dataset_key,version_id,content_digest,digest_format,
    record_count,first_day,last_day,algorithm_version,source_binding_digest,
    evidence_digest,task_id,item_id,created_at,updated_at
  FROM derived_dataset_versions;
DROP TABLE derived_dataset_versions;
ALTER TABLE derived_dataset_versions_v23 RENAME TO derived_dataset_versions;

-- ---------------------------------------------------------------------------
-- 1) security_master: one row per (exchange, code, effective interval). The
-- row identity is (exchange, code, effective_from) — the issuer key is an
-- ordinary column (Q2), because a code can be reused by another issuer and the
-- interval is what separates the two rows. `issuer_resolution` says how the
-- row was attributed: by issuer key, by code+date only, or ambiguous
-- (code reuse without a proven boundary, Q3).
-- Listing/delisting dates keep the second source as a `_tdx` column and a
-- `*_conflict` flag instead of silently picking one: the exchange list is
-- authoritative, the TDX `SSDATE`/`infoharbor` value is the fallback, and a
-- disagreement is recorded, not resolved. `*_source='unknown'` with a date
-- present is rejected — an unattributed date is not knowledge (§4.2, §4.3).
-- `arrangement_start`/`arrangement_end` are the 退市整理期 bounds (end is the
-- delisting-effective day, #401) and are only meaningful together with
-- `delisting_reason_class='arrangement'`.
-- ---------------------------------------------------------------------------
CREATE TABLE security_master(
  exchange TEXT NOT NULL CHECK(exchange IN ('sh','sz','bj')),
  code TEXT NOT NULL,
  effective_from INTEGER NOT NULL,
  effective_to INTEGER,
  instrument_class INTEGER NOT NULL CHECK(instrument_class IN (0,1,2)),
  issuer_key TEXT,
  issuer_resolution TEXT NOT NULL CHECK(issuer_resolution IN ('issuer_key','code_date','ambiguous')),
  listing_date INTEGER,
  listing_date_tdx INTEGER,
  listing_source TEXT NOT NULL CHECK(listing_source IN ('exchange_list','tdx_ssdate','unknown')),
  b_listing_date INTEGER,
  delisting_date INTEGER,
  delisting_date_tdx INTEGER,
  delisting_source TEXT NOT NULL CHECK(delisting_source IN ('exchange_list','tdx_arrangement','unknown')),
  delisting_reason_class TEXT NOT NULL CHECK(delisting_reason_class IN ('arrangement','absorption','other','unknown')),
  arrangement_start INTEGER, arrangement_end INTEGER,
  listing_conflict INTEGER NOT NULL CHECK(listing_conflict IN (0,1)),
  delisting_conflict INTEGER NOT NULL CHECK(delisting_conflict IN (0,1)),
  known_at TEXT,
  source_binding_digest BLOB NOT NULL REFERENCES source_bindings(source_binding_digest),
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  PRIMARY KEY(exchange, code, effective_from),
  CHECK(effective_to IS NULL OR effective_to > effective_from),
  CHECK(listing_date IS NULL OR listing_source <> 'unknown'),
  CHECK(delisting_date IS NULL OR delisting_source <> 'unknown'),
  -- The 整理期 end may be unknown (#401: an empty end is 'unknown', not
  -- 'still in the arrangement period'), so only the order is constrained.
  CHECK(arrangement_start IS NULL OR arrangement_end IS NULL
        OR arrangement_start <= arrangement_end)
) STRICT;

-- ---------------------------------------------------------------------------
-- 2) code_migrations: one issuer changed code (sz.300114 -> sz.302132,
-- 2025-02-17). `certainty='candidate'` is a lead that must not be used for
-- deduplication; only 'confirmed' rows collapse two code histories into one
-- issuer (otherwise a backtest doubles the returns, §19 note 7).
-- ---------------------------------------------------------------------------
CREATE TABLE code_migrations(
  from_exchange TEXT NOT NULL, from_code TEXT NOT NULL,
  to_exchange TEXT NOT NULL, to_code TEXT NOT NULL,
  effective_from INTEGER NOT NULL,
  certainty TEXT NOT NULL CHECK(certainty IN ('confirmed','candidate')),
  source_kind TEXT NOT NULL, source_record_id TEXT, known_at TEXT,
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  PRIMARY KEY(from_exchange, from_code, effective_from),
  CHECK(from_exchange IN ('sh','sz','bj') AND to_exchange IN ('sh','sz','bj'))
) STRICT;

-- ---------------------------------------------------------------------------
-- 3) security_name_history: interval rows, never a current-value snapshot.
-- `certainty='unbounded'` marks a row whose end is unknown (the source only
-- proves "from this day on"); `source_kind='unknown'` keeps a name whose
-- provenance is unresolved instead of guessing. XD/XR prefixes must be
-- normalised before a name lands here (#396) — that is a writer rule, not a
-- CHECK.
-- ---------------------------------------------------------------------------
CREATE TABLE security_name_history(
  exchange TEXT NOT NULL, code TEXT NOT NULL,
  effective_from INTEGER NOT NULL, effective_to INTEGER,
  name TEXT NOT NULL,
  certainty TEXT NOT NULL CHECK(certainty IN ('interval','unbounded')),
  known_at TEXT,
  source_kind TEXT NOT NULL CHECK(source_kind IN ('szse_name_history','gp29_marker','exchange_snapshot','unknown')),
  source_record_id TEXT,
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  PRIMARY KEY(exchange, code, effective_from),
  CHECK(exchange IN ('sh','sz','bj')),
  CHECK(effective_to IS NULL OR effective_to > effective_from)
) STRICT;

-- ---------------------------------------------------------------------------
-- 4) capital_events: the parsed 股本 facts (cat 2/3/9/10 only, R-3). Two
-- representations per row on purpose: `*_bits` are the source's f32 values
-- verbatim (immutable fact) and `float_after`/`total_after` are the derived
-- 股 counts (万股 2dp -> 股, exact); a test binds `to_shares(bits)` to the
-- stored integer (§9.2 note 2). `prev_is_lagged`/`suspicious` are quality
-- flags: the status function skips `suspicious=1` unless explicitly relaxed
-- (§5.2), and a lagged 前值 never becomes a state.
-- `known_at` is NULL for gbbq (no announcement day) — never inferred.
-- ---------------------------------------------------------------------------
CREATE TABLE capital_events(
  exchange TEXT NOT NULL, code TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
  effective_day INTEGER NOT NULL,
  category INTEGER NOT NULL CHECK(category IN (2,3,9,10)),
  float_before_bits INTEGER NOT NULL CHECK(float_before_bits BETWEEN 0 AND 4294967295),
  total_before_bits INTEGER NOT NULL CHECK(total_before_bits BETWEEN 0 AND 4294967295),
  float_after_bits INTEGER NOT NULL CHECK(float_after_bits BETWEEN 0 AND 4294967295),
  total_after_bits INTEGER NOT NULL CHECK(total_after_bits BETWEEN 0 AND 4294967295),
  float_after INTEGER NOT NULL CHECK(float_after >= 0),
  total_after INTEGER NOT NULL CHECK(total_after >= 0),
  prev_is_lagged INTEGER NOT NULL DEFAULT 1 CHECK(prev_is_lagged IN (0,1)),
  suspicious INTEGER NOT NULL DEFAULT 0 CHECK(suspicious IN (0,1)),
  known_at TEXT,
  source_binding_digest BLOB NOT NULL REFERENCES source_bindings(source_binding_digest),
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  PRIMARY KEY(exchange, code, ordinal),
  CHECK(exchange IN ('sh','sz','bj'))
) STRICT;
CREATE INDEX capital_events_code_day ON capital_events(exchange, code, effective_day, ordinal);

-- ---------------------------------------------------------------------------
-- 5) risk_warning_intervals: the historical ST state machine (§6.3). The raw
-- source status code is kept next to the normalised vocabulary so a mapping
-- decision is auditable; `marker_only_unresolved` and `unknown` are real
-- states, not "normal" (fail-closed: an unresolved marker must not be read as
-- "no risk warning"). Production rows require #308; the table exists so the
-- shape is fixed before the data is admitted.
-- ---------------------------------------------------------------------------
CREATE TABLE risk_warning_intervals(
  exchange TEXT NOT NULL, code TEXT NOT NULL,
  effective_from INTEGER NOT NULL, effective_to INTEGER,
  raw_status_code TEXT NOT NULL,
  normalized_status TEXT NOT NULL CHECK(normalized_status IN (
    'normal','other_risk_warning','delisting_risk_warning','delisting_arrangement',
    'terminated_or_delisted','suspended','marker_only_unresolved','unknown')),
  announced_at TEXT, known_at TEXT,
  source_kind TEXT NOT NULL CHECK(source_kind IN (
    'szse_name_history','exchange_announcement','gp29_marker','unresolved')),
  source_record_id TEXT, source_document_sha256 TEXT, source_generation_id TEXT,
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  PRIMARY KEY(exchange, code, effective_from),
  CHECK(exchange IN ('sh','sz','bj')),
  CHECK(effective_to IS NULL OR effective_to > effective_from)
) STRICT;

-- ---------------------------------------------------------------------------
-- 6) suspension_intervals: one row per interval, `certainty='unresolved_gap'`
-- for a run of missing bars with no announcement (Q6), `missing_days` counts
-- the missing trading days inside the interval. `confirmed` rows come from an
-- admitted announcement source; until then the gap rows are the only rows and
-- they never mean "suspended" (§7.1, AC-M2-8).
-- ---------------------------------------------------------------------------
CREATE TABLE suspension_intervals(
  exchange TEXT NOT NULL, code TEXT NOT NULL,
  start_day INTEGER NOT NULL, end_day INTEGER NOT NULL,
  certainty TEXT NOT NULL CHECK(certainty IN ('confirmed','unresolved_gap')),
  missing_days INTEGER NOT NULL CHECK(missing_days > 0),
  reason TEXT NOT NULL CHECK(reason IN ('exchange_announcement','gp29_resume','missing_bar_no_event')),
  known_at TEXT, source_kind TEXT NOT NULL, source_record_id TEXT,
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  PRIMARY KEY(exchange, code, start_day),
  CHECK(exchange IN ('sh','sz','bj')),
  CHECK(end_day >= start_day)
) STRICT;

-- ---------------------------------------------------------------------------
-- 7) Trading rules (§8.1, separation point 6): band, side, rounding and the
-- minimum tick are DATA keyed by effective date, never code constants. The
-- uniqueness anchor is the rule's scope plus `effective_from`; the derivation
-- requires exactly one hit per date and fails closed on zero or two hits
-- (`rule_missing`/`rule_ambiguous`), so `basis_status='pending'` rows exist to
-- record a known rule whose text is not yet admitted — they are excluded from
-- derivation, never used as a default (§8.5).
-- `band_bp` is basis points (1000 = 10%); `band_side='none'` (no price limit)
-- must carry NULL, and a bounded side must carry a value.
-- ---------------------------------------------------------------------------
CREATE TABLE price_limit_rules(
  rule_id TEXT PRIMARY KEY,
  market TEXT NOT NULL CHECK(market IN ('sh','sz','bj','all')),
  board TEXT NOT NULL CHECK(board IN ('main','gem','star','bse','all')),
  status_class TEXT NOT NULL CHECK(status_class IN ('normal','st','star_st','delisting_arrangement','all')),
  listing_phase TEXT NOT NULL CHECK(listing_phase IN ('first_day','first_5_days','seasoned','all')),
  band_bp INTEGER CHECK(band_bp IS NULL OR band_bp >= 0),
  band_side TEXT NOT NULL CHECK(band_side IN ('both','up_only','none')),
  rounding TEXT NOT NULL CHECK(rounding IN ('half_up','floor','ceil')),
  effective_from INTEGER NOT NULL,
  effective_to INTEGER,
  basis_ref TEXT NOT NULL CHECK(length(basis_ref) > 0),
  basis_status TEXT NOT NULL CHECK(basis_status IN ('verified','pending')),
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  UNIQUE(market, board, status_class, listing_phase, effective_from),
  CHECK(effective_to IS NULL OR effective_to > effective_from),
  CHECK((band_side = 'none' AND band_bp IS NULL)
     OR (band_side <> 'none' AND band_bp IS NOT NULL))
) STRICT;

-- `tick_units` is the minimum price step in units of the rule's own
-- `price_scale` (A 股: 1 unit at scale 2 = 0.01 元); the scale is a column so a
-- future instrument with a different quantum does not need a code branch.
CREATE TABLE min_tick_rules(
  rule_id TEXT PRIMARY KEY,
  market TEXT NOT NULL CHECK(market IN ('sh','sz','bj','all')),
  board TEXT NOT NULL CHECK(board IN ('main','gem','star','bse','all')),
  effective_from INTEGER NOT NULL,
  effective_to INTEGER,
  tick_units INTEGER NOT NULL CHECK(tick_units > 0),
  price_scale INTEGER NOT NULL CHECK(price_scale BETWEEN 0 AND 6),
  basis_ref TEXT NOT NULL CHECK(length(basis_ref) > 0),
  basis_status TEXT NOT NULL CHECK(basis_status IN ('verified','pending')),
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  UNIQUE(market, board, effective_from),
  CHECK(effective_to IS NULL OR effective_to > effective_from)
) STRICT;
