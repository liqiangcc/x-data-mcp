-- Schema 25 (M3 PR-1, docs/tasks/issue-446-m3-delisted-industry.md §9.1-§9.4):
-- the five index tables the source store (PR-1a, §9.4.1) rebuilds, plus the
-- `price_limit_rules.min_band_ticks` column approved by the owner on
-- 2026-10-29 for the low-price minimum-band floor (沪市风险警示股前收 ≤0.10 元
-- 按一 tick; decisions 2026-10-29 entry, research report §4.3).
--
-- Structure only: no writer, no replay body and no read surface is added here
-- (those are PR-6 industry, PR-2/PR-8 exchange, PR-8 generation_facts and the
-- limit-v1 algorithm PR respectively). 022/023/024 stay immutable; the only
-- change to a schema-23 table is the appended nullable column, which SQLite
-- can do in place without rebuilding it.
--
-- Write ordering (mandatory, §9.4.1): a source-store blob and its manifest
-- line must be fsynced before the matching index row is written; "DB row
-- present, blob unreadable" is never acceptable and every reader/retirement
-- gate fails closed on it. PR-1 has no write path, so nothing here can violate
-- the order yet — the domain writers listed above must assert it (blob
-- registered before the index INSERT, per domain one transaction). The same
-- applies to `industry_assignments`: a row may only be written after the
-- snapshot it references is present in `industry_snapshots` (the foreign key
-- enforces that half) *and* that snapshot's bytes are in the source store.
--
-- Naming and units follow §9.2: epoch-day INTEGERs are the same unit as
-- `series_bars.trade_day`; `known_at`/`observed_at`/`fetched_at` are ISO8601
-- TEXT because the announcement/source timestamps are second resolution.
-- `store_sha256` names "the blob in the source store" (content-addressed
-- identity, §9.4.1) and is deliberately not called `blob_sha256`; for a
-- `tdxhy.cfg` extraction it must equal `member_sha256`, and the writer/replay
-- assert that (§9.2 note). Store bytes never enter the DB (Q7): these tables
-- are indexes over content-addressed files.

-- ---------------------------------------------------------------------------
-- 1) industry_snapshots: one row per distinct membership content of a
-- generation's `tdxhy.cfg` (content identity = member_sha256). Original bytes
-- are re-parsed from the source store, never stored here.
-- ---------------------------------------------------------------------------
CREATE TABLE industry_snapshots(
  member_sha256 BLOB PRIMARY KEY,
  store_sha256 BLOB NOT NULL,
  base_zip_sha256 BLOB,
  generation_manifest_sha256 BLOB,
  source_kind TEXT NOT NULL CHECK(source_kind IN ('generation','archive','repo_evidence')),
  publisher_last_modified TEXT,
  observed_at TEXT,
  ingested_at TEXT NOT NULL,
  evidence_grade TEXT NOT NULL CHECK(evidence_grade IN ('A','B','C')),
  parser_version TEXT NOT NULL,
  row_count INTEGER NOT NULL,
  scope_member_count INTEGER NOT NULL,
  dictionary_sha256 BLOB,
  notes TEXT
) STRICT;

-- ---------------------------------------------------------------------------
-- 2) industry_assignments: the read-side intervals (§5.1). The key is
-- `(exchange, code)` rather than an entity string because a delisted security
-- may not be in the master table yet; the read model joins and reports
-- `orphan` instead of inventing an entity. `known_at` is NOT NULL on purpose
-- (§9.3): a row without a knowledge time must not be stored, so an event date
-- can never masquerade as `known_at`.
-- ---------------------------------------------------------------------------
CREATE TABLE industry_assignments(
  exchange TEXT NOT NULL CHECK(exchange IN ('sh','sz')),
  code TEXT NOT NULL,
  scheme TEXT NOT NULL CHECK(scheme = 'tdx_research_industry'),
  level INTEGER NOT NULL CHECK(level BETWEEN 1 AND 3),
  industry_code TEXT NOT NULL,
  name TEXT,
  name_status TEXT NOT NULL CHECK(name_status IN ('resolved','unknown_dictionary','not_in_dictionary')),
  effective_from INTEGER NOT NULL,
  effective_to INTEGER,
  known_at TEXT NOT NULL,
  effective_from_basis TEXT NOT NULL
    CHECK(effective_from_basis IN ('publisher_last_modified','acquisition_time','evidence_commit_date')),
  evidence_grade TEXT NOT NULL CHECK(evidence_grade IN ('A','B','C')),
  t_code TEXT,
  snapshot_member_sha256 BLOB NOT NULL REFERENCES industry_snapshots(member_sha256),
  PRIMARY KEY(exchange, code, scheme, effective_from),
  CHECK(effective_to IS NULL OR effective_to > effective_from)
) STRICT;

-- ---------------------------------------------------------------------------
-- 3) exchange_source_snapshots: the index over frozen exchange responses,
-- shared by the five consumers (§6). `snapshot_id` is the response-byte sha256
-- (hex); `store_sha256` is the same byte identity in the source store, an
-- explicit column so nothing has to be inferred from a path.
-- ---------------------------------------------------------------------------
CREATE TABLE exchange_source_snapshots(
  snapshot_id TEXT PRIMARY KEY,
  store_sha256 BLOB NOT NULL,
  store_kind TEXT NOT NULL CHECK(store_kind IN ('http_fetch','archive_import')),
  source_id TEXT NOT NULL,
  url TEXT NOT NULL,
  request_fingerprint TEXT NOT NULL,
  fetched_at TEXT NOT NULL,
  http_etag TEXT,
  http_last_modified TEXT,
  as_of TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  row_count INTEGER NOT NULL,
  page_count INTEGER,
  parser_version TEXT NOT NULL,
  basis_status TEXT NOT NULL
    CHECK(basis_status IN ('approved_source','owner_risk_accepted','not_available')),
  notes TEXT,
  UNIQUE(source_id, request_fingerprint, snapshot_id)
) STRICT;

-- ---------------------------------------------------------------------------
-- 4) generation_facts: rebuildable registration of the derived facts a
-- generation must have before its bytes may be retired (§8.3, §9.4.3). The
-- retirement gate reads the source-store blob by sha; this table is only the
-- accelerated index. An irreproducible fact must carry `evidence_sha256`
-- (enforced by the PR-8 gate, not expressible as a CHECK here because some
-- kinds are reproducible).
-- ---------------------------------------------------------------------------
CREATE TABLE generation_facts(
  generation_manifest_sha256 BLOB NOT NULL,
  role TEXT NOT NULL CHECK(role IN ('base.zip','gbbq.zip','hsjday.zip')),
  fact_kind TEXT NOT NULL,
  fact_digest BLOB NOT NULL,
  evidence_sha256 BLOB,
  parser_version TEXT,
  registered_at TEXT NOT NULL,
  PRIMARY KEY(generation_manifest_sha256, role, fact_kind)
) STRICT;

-- ---------------------------------------------------------------------------
-- 5) retire_exemptions: the explicit, auditable exception path for the
-- retirement gate (§8.3). A blank reason is refused so an exemption cannot be
-- granted silently.
-- ---------------------------------------------------------------------------
CREATE TABLE retire_exemptions(
  generation_manifest_sha256 BLOB NOT NULL,
  missing_kind TEXT NOT NULL,
  reason TEXT NOT NULL CHECK(length(reason) > 0),
  recorded_by TEXT NOT NULL,
  recorded_at TEXT NOT NULL,
  PRIMARY KEY(generation_manifest_sha256, missing_kind)
) STRICT;

-- Read-side index (§9.2 note): interval lookup by code, and source snapshots
-- by source + as_of.
CREATE INDEX industry_assignments_code_from ON industry_assignments(code, effective_from);
CREATE INDEX exchange_source_snapshots_source_as_of ON exchange_source_snapshots(source_id, as_of);

-- ---------------------------------------------------------------------------
-- price_limit_rules.min_band_ticks (owner 2026-10-29): the minimum band width
-- in minimum-tick units, NULL when the rule has no floor. It expresses "沪市
-- 风险警示股前收 ≤0.10 元按一 tick" and the 深市 generic one-tick floor without
-- a second low-price threshold column (decisions 2026-10-29 entry). This PR
-- only adds the column and its storage round-trip: every existing rule row and
-- the rule book stay NULL, and limit-v1 keeps its current algorithm. The
-- appended column is nullable, so the schema-23 table is not rebuilt and no
-- existing row changes.
-- ---------------------------------------------------------------------------
ALTER TABLE price_limit_rules ADD COLUMN min_band_ticks INTEGER
  CHECK(min_band_ticks IS NULL OR min_band_ticks >= 1);
