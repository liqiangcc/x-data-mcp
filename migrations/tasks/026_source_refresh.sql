-- Schema 26 (refresh R2, docs/tasks/issue-446-exchange-refresh.md §5.2-§5.6;
-- owner 2026-11-01 Q5/D-66): the refresh-run record plus the two changes the
-- record depends on.
--
-- 1) `exchange_source_snapshots` keyed by `(snapshot_id, request_fingerprint)`.
--    Schema 25 used `snapshot_id` alone as the primary key, so two different
--    requests that happened to return identical bytes (an empty window is the
--    obvious case) could only keep the first fingerprint: the other windows
--    were never reusable. The response bytes are still one source-store blob
--    per sha; the index row is per (byte identity, request identity) pair, so
--    replay can rebuild exactly the registrations the manifest records.
--    `UNIQUE(source_id, request_fingerprint, snapshot_id)` stays as the
--    read-through lookup key. SQLite cannot alter a primary key, so the table
--    is rebuilt; 025 stays immutable.
--
-- 2) `source_refresh_runs` / `source_refresh_run_sources` /
--    `source_refresh_run_members`: the durable refresh record. A task result
--    JSON cannot carry `verified_through` because the task retention period is
--    undecided (§5.6). `source_refresh_run_sources.complete` says whether the
--    run observed *every* fingerprint of that source (full set) — only a
--    complete, successful run advances the source's `verified_through`, so a
--    daily tail cannot silently satisfy the H=14-day gate that exists to
--    force the weekly full (§5.1/§5.3).
--
--    Members list every page this run froze or confirmed, keyed by
--    `(run_id, source_id, request_fingerprint)`: that is "which snapshot_id
--    this run selected for this request", the input R3's replay selection
--    needs (§5.5). Proposed but unconfirmed pages (a probe page, or a table
--    attempt that failed integrity) are not selected members and are not
--    written here.
--
-- Append-only: 019-025 are unchanged. Fresh-init only (D-08), so the copy
-- below normally sees an empty table; it keeps the step correct for a
-- hand-built schema-25 file opened by the migration chain in tests.

CREATE TABLE exchange_source_snapshots_v26(
  snapshot_id TEXT NOT NULL,
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
  PRIMARY KEY(snapshot_id, request_fingerprint),
  UNIQUE(source_id, request_fingerprint, snapshot_id)
) STRICT;

INSERT INTO exchange_source_snapshots_v26(
  snapshot_id,store_sha256,store_kind,source_id,url,request_fingerprint,
  fetched_at,http_etag,http_last_modified,as_of,size_bytes,row_count,page_count,
  parser_version,basis_status,notes)
  SELECT snapshot_id,store_sha256,store_kind,source_id,url,request_fingerprint,
    fetched_at,http_etag,http_last_modified,as_of,size_bytes,row_count,page_count,
    parser_version,basis_status,notes
  FROM exchange_source_snapshots;

DROP TABLE exchange_source_snapshots;
ALTER TABLE exchange_source_snapshots_v26 RENAME TO exchange_source_snapshots;
CREATE INDEX exchange_source_snapshots_source_as_of
  ON exchange_source_snapshots(source_id, as_of);

CREATE TABLE source_refresh_runs(
  run_id TEXT PRIMARY KEY,
  mode TEXT NOT NULL CHECK(mode IN ('tail','full')),
  status TEXT NOT NULL CHECK(status IN ('running','succeeded','failed')),
  scope TEXT NOT NULL,
  started_at TEXT NOT NULL,
  finished_at TEXT,
  requests INTEGER NOT NULL DEFAULT 0,
  reused INTEGER NOT NULL DEFAULT 0,
  changed INTEGER NOT NULL DEFAULT 0,
  failed_windows INTEGER NOT NULL DEFAULT 0,
  unobserved_windows INTEGER NOT NULL DEFAULT 0,
  verified_through TEXT,
  task_id TEXT,
  detail TEXT
) STRICT;
CREATE INDEX source_refresh_runs_status_finished
  ON source_refresh_runs(status, finished_at);

CREATE TABLE source_refresh_run_sources(
  run_id TEXT NOT NULL REFERENCES source_refresh_runs(run_id) ON DELETE CASCADE,
  source_id TEXT NOT NULL,
  class TEXT NOT NULL CHECK(class IN ('window','table')),
  complete INTEGER NOT NULL CHECK(complete IN (0,1)),
  requests INTEGER NOT NULL,
  snapshots INTEGER NOT NULL,
  changed INTEGER NOT NULL,
  unobserved_windows INTEGER NOT NULL,
  failed_windows INTEGER NOT NULL,
  verified_through TEXT,
  PRIMARY KEY(run_id, source_id)
) STRICT;
CREATE INDEX source_refresh_run_sources_complete
  ON source_refresh_run_sources(source_id, complete);

CREATE TABLE source_refresh_run_members(
  run_id TEXT NOT NULL REFERENCES source_refresh_runs(run_id) ON DELETE CASCADE,
  source_id TEXT NOT NULL,
  request_fingerprint TEXT NOT NULL,
  snapshot_id TEXT NOT NULL,
  page_count INTEGER,
  row_count INTEGER NOT NULL,
  as_of TEXT NOT NULL,
  fetched_at TEXT NOT NULL,
  changed INTEGER NOT NULL CHECK(changed IN (0,1)),
  window_start TEXT,
  window_end TEXT,
  PRIMARY KEY(run_id, source_id, request_fingerprint)
) STRICT;
CREATE INDEX source_refresh_run_members_source
  ON source_refresh_run_members(source_id, run_id);
