-- Schema 19 compact market storage (#259): replaces every market-data table
-- that duplicated canonical bars or full envelopes. upgrade_compact proves all
-- business tables empty before applying this; initialize19 applies the same
-- script on already-empty fresh files. No data is migrated. _v19 names are
-- renamed at the end so SQLite rewrites their internal REFERENCES clauses.
CREATE TABLE evidence_bundles_v19(
 evidence_digest BLOB PRIMARY KEY CHECK(length(evidence_digest)=32),
 evidence_kind TEXT NOT NULL CHECK(evidence_kind IN (
   'generation','batch_selection','version','continuity','catalog','yearly')),
 canonical_json TEXT NOT NULL
) STRICT, WITHOUT ROWID;
CREATE TABLE source_bindings_v19(
 source_binding_digest BLOB PRIMARY KEY CHECK(length(source_binding_digest)=32),
 generation_id TEXT NOT NULL,
 generation_manifest_sha256 BLOB NOT NULL CHECK(length(generation_manifest_sha256)=32),
 day_member_sha256 BLOB NOT NULL CHECK(length(day_member_sha256)=32),
 security_event_sha256 BLOB NOT NULL CHECK(length(security_event_sha256)=32),
 evidence_digest BLOB NOT NULL REFERENCES evidence_bundles_v19(evidence_digest)
) STRICT, WITHOUT ROWID;
CREATE TABLE bar_segments_v19(
 segment_key INTEGER PRIMARY KEY,
 segment_digest BLOB NOT NULL UNIQUE CHECK(length(segment_digest)=32),
 entity TEXT NOT NULL,
 period INTEGER NOT NULL CHECK(period IN (1,2)),
 first_day INTEGER NOT NULL,
 last_day INTEGER NOT NULL,
 row_count INTEGER NOT NULL CHECK(row_count BETWEEN 1 AND 512),
 CHECK(first_day <= last_day)
) STRICT;
CREATE TABLE segment_bars_v19(
 segment_key INTEGER NOT NULL REFERENCES bar_segments_v19(segment_key),
 trade_day INTEGER NOT NULL,
 open_cents INTEGER NOT NULL,
 high_cents INTEGER NOT NULL,
 low_cents INTEGER NOT NULL,
 close_cents INTEGER NOT NULL,
 volume INTEGER,
 amount TEXT,
 r7_raw INTEGER,
 PRIMARY KEY(segment_key, trade_day),
 CHECK(high_cents >= open_cents AND high_cents >= close_cents AND high_cents >= low_cents),
 CHECK(low_cents <= open_cents AND low_cents <= close_cents),
 CHECK(volume IS NULL OR volume >= 0),
 CHECK(r7_raw IS NULL OR r7_raw BETWEEN 0 AND 4294967295)
) STRICT, WITHOUT ROWID;
CREATE TABLE data_versions_v19(
 version_key INTEGER PRIMARY KEY,
 version_id TEXT NOT NULL UNIQUE,
 entity TEXT NOT NULL,
 period INTEGER NOT NULL CHECK(period IN (1,2)),
 adjustment INTEGER NOT NULL CHECK(adjustment=1),
 state INTEGER NOT NULL CHECK(state IN (1,2,3)),
 source_binding_digest BLOB NOT NULL REFERENCES source_bindings_v19(source_binding_digest),
 parent_version_key INTEGER REFERENCES data_versions_v19(version_key),
 manifest_mode INTEGER NOT NULL CHECK(manifest_mode IN (1,2)),
 delta_depth INTEGER NOT NULL CHECK(delta_depth BETWEEN 0 AND 256),
 record_count INTEGER NOT NULL CHECK(record_count > 0),
 first_day INTEGER NOT NULL,
 last_day INTEGER NOT NULL,
 coverage_status INTEGER NOT NULL CHECK(coverage_status IN (1,2,3)),
 evidence_digest BLOB NOT NULL REFERENCES evidence_bundles_v19(evidence_digest),
 projection_digest BLOB NOT NULL CHECK(length(projection_digest)=32),
 integrity_digest BLOB CHECK(integrity_digest IS NULL OR length(integrity_digest)=32),
 digest_format TEXT NOT NULL CHECK(digest_format='compact-market-version-v1'),
 created_at TEXT NOT NULL,
 published_at TEXT,
 CHECK(first_day <= last_day),
 CHECK((state IN (1,2) AND published_at IS NULL AND integrity_digest IS NULL)
    OR (state=3 AND published_at IS NOT NULL AND integrity_digest IS NOT NULL)),
 CHECK((manifest_mode=1 AND parent_version_key IS NULL AND delta_depth=0)
    OR (manifest_mode=2 AND parent_version_key IS NOT NULL AND delta_depth BETWEEN 1 AND 256))
) STRICT;
CREATE TABLE version_checkpoint_clips_v19(
 version_key INTEGER NOT NULL REFERENCES data_versions_v19(version_key),
 seq INTEGER NOT NULL CHECK(seq >= 0),
 segment_key INTEGER NOT NULL REFERENCES bar_segments_v19(segment_key),
 clip_start_day INTEGER NOT NULL,
 clip_end_day INTEGER NOT NULL,
 row_count INTEGER NOT NULL CHECK(row_count > 0),
 PRIMARY KEY(version_key,seq),
 CHECK(clip_start_day <= clip_end_day)
) STRICT, WITHOUT ROWID;
CREATE TABLE version_delta_clips_v19(
 version_key INTEGER NOT NULL REFERENCES data_versions_v19(version_key),
 seq INTEGER NOT NULL CHECK(seq >= 0),
 delta_kind INTEGER NOT NULL CHECK(delta_kind IN (1,2)),
 range_start_day INTEGER NOT NULL,
 range_end_day INTEGER NOT NULL,
 segment_key INTEGER REFERENCES bar_segments_v19(segment_key),
 clip_start_day INTEGER,
 clip_end_day INTEGER,
 row_count INTEGER NOT NULL CHECK(row_count >= 0),
 PRIMARY KEY(version_key,seq),
 CHECK(range_start_day <= range_end_day),
 CHECK((delta_kind=1 AND segment_key IS NOT NULL AND clip_start_day IS NOT NULL
        AND clip_end_day IS NOT NULL AND row_count > 0)
    OR (delta_kind=2 AND segment_key IS NULL AND clip_start_day IS NULL
        AND clip_end_day IS NULL AND row_count=0))
) STRICT, WITHOUT ROWID;
CREATE TABLE yearly_lineage_v19(
 version_key INTEGER PRIMARY KEY REFERENCES data_versions_v19(version_key),
 base_daily_version_key INTEGER NOT NULL REFERENCES data_versions_v19(version_key),
 algorithm_version TEXT NOT NULL,
 first_trade_day INTEGER NOT NULL,
 last_trade_day INTEGER NOT NULL,
 as_of_day INTEGER NOT NULL,
 CHECK(first_trade_day <= last_trade_day),
 CHECK(last_trade_day <= as_of_day)
) STRICT;
CREATE TABLE result_registry_v19(
 result_id TEXT PRIMARY KEY,
 result_kind INTEGER NOT NULL CHECK(result_kind IN (1,2))
) STRICT, WITHOUT ROWID;
CREATE TABLE market_update_results_v19(
 result_key INTEGER PRIMARY KEY,
 result_id TEXT NOT NULL UNIQUE REFERENCES result_registry_v19(result_id),
 task_id TEXT NOT NULL,
 item_id TEXT NOT NULL,
 base_version_key INTEGER NOT NULL REFERENCES data_versions_v19(version_key),
 result_version_key INTEGER NOT NULL REFERENCES data_versions_v19(version_key),
 outcome INTEGER NOT NULL CHECK(outcome IN (1,2)),
 range_start_day INTEGER NOT NULL,
 range_end_day INTEGER NOT NULL,
 algorithm_version TEXT NOT NULL,
 source_binding_digest BLOB NOT NULL REFERENCES source_bindings_v19(source_binding_digest),
 continuity_mode INTEGER NOT NULL CHECK(continuity_mode IN (1,2,3)),
 prefix_bytes INTEGER,
 base_projection_digest BLOB NOT NULL CHECK(length(base_projection_digest)=32),
 result_projection_digest BLOB NOT NULL CHECK(length(result_projection_digest)=32),
 delta_digest BLOB CHECK(delta_digest IS NULL OR length(delta_digest)=32),
 evidence_digest BLOB NOT NULL REFERENCES evidence_bundles_v19(evidence_digest),
 task_revision INTEGER NOT NULL,
 result_digest BLOB NOT NULL CHECK(length(result_digest)=32),
 UNIQUE(task_id,item_id),
 FOREIGN KEY(task_id,item_id) REFERENCES task_items(task_id,item_id),
 CHECK(range_start_day <= range_end_day),
 CHECK((continuity_mode IN (1,2) AND prefix_bytes IS NOT NULL AND prefix_bytes>=0)
    OR (continuity_mode=3 AND prefix_bytes IS NULL)),
 CHECK((outcome=1 AND continuity_mode=1 AND base_version_key=result_version_key AND delta_digest IS NULL)
    OR (outcome=2 AND continuity_mode IN (2,3) AND base_version_key<>result_version_key AND delta_digest IS NOT NULL))
) STRICT;
CREATE TABLE market_update_delta_clips_v19(
 result_key INTEGER NOT NULL REFERENCES market_update_results_v19(result_key),
 seq INTEGER NOT NULL CHECK(seq >= 0),
 range_start_day INTEGER NOT NULL,
 range_end_day INTEGER NOT NULL,
 segment_key INTEGER NOT NULL REFERENCES bar_segments_v19(segment_key),
 clip_start_day INTEGER NOT NULL,
 clip_end_day INTEGER NOT NULL,
 row_count INTEGER NOT NULL CHECK(row_count > 0),
 PRIMARY KEY(result_key,seq),
 CHECK(range_start_day <= range_end_day),
 CHECK(clip_start_day <= clip_end_day)
) STRICT, WITHOUT ROWID;
CREATE TABLE publication_results_v19(
 result_key INTEGER PRIMARY KEY,
 publication_id TEXT NOT NULL UNIQUE REFERENCES result_registry_v19(result_id),
 task_id TEXT NOT NULL,
 item_id TEXT NOT NULL,
 publication_kind INTEGER NOT NULL CHECK(publication_kind IN (1,2,3)),
 base_version_key INTEGER REFERENCES data_versions_v19(version_key),
 result_version_key INTEGER NOT NULL REFERENCES data_versions_v19(version_key),
 outcome INTEGER NOT NULL CHECK(outcome IN (1,2)),
 algorithm_version TEXT NOT NULL,
 source_binding_digest BLOB NOT NULL REFERENCES source_bindings_v19(source_binding_digest),
 evidence_digest BLOB NOT NULL REFERENCES evidence_bundles_v19(evidence_digest),
 task_revision INTEGER NOT NULL,
 result_digest BLOB NOT NULL CHECK(length(result_digest)=32),
 UNIQUE(task_id,item_id),
 FOREIGN KEY(task_id,item_id) REFERENCES task_items(task_id,item_id),
 CHECK((outcome=1 AND base_version_key IS NOT NULL AND base_version_key=result_version_key)
    OR (outcome=2 AND (base_version_key IS NULL OR base_version_key<>result_version_key)))
) STRICT;
CREATE TABLE task_publications_v19(
 task_id TEXT NOT NULL REFERENCES tasks(task_id),
 version_key INTEGER NOT NULL UNIQUE REFERENCES data_versions_v19(version_key),
 result_id TEXT NOT NULL UNIQUE REFERENCES result_registry_v19(result_id),
 PRIMARY KEY(task_id,version_key)
) STRICT;
CREATE TABLE effective_heads_v19(
 entity TEXT NOT NULL,
 period INTEGER NOT NULL CHECK(period IN (1,2)),
 adjustment INTEGER NOT NULL CHECK(adjustment=1),
 version_key INTEGER NOT NULL UNIQUE REFERENCES data_versions_v19(version_key),
 PRIMARY KEY(entity,period,adjustment)
) STRICT;
DROP TABLE segment_bars;
DROP TABLE bar_segments;
DROP TABLE version_evidence;
DROP TABLE yearly_bars;
DROP TABLE yearly_lineage;
DROP TABLE task_publications;
DROP TABLE publication_batches;
DROP TABLE effective_heads;
DROP TABLE market_update_results;
DROP TABLE yearly_execution_results;
DROP TABLE manifest_nodes;
DROP TABLE payloads;
DROP TABLE data_versions;
ALTER TABLE evidence_bundles_v19 RENAME TO evidence_bundles;
ALTER TABLE source_bindings_v19 RENAME TO source_bindings;
ALTER TABLE bar_segments_v19 RENAME TO bar_segments;
ALTER TABLE segment_bars_v19 RENAME TO segment_bars;
ALTER TABLE data_versions_v19 RENAME TO data_versions;
ALTER TABLE version_checkpoint_clips_v19 RENAME TO version_checkpoint_clips;
ALTER TABLE version_delta_clips_v19 RENAME TO version_delta_clips;
ALTER TABLE yearly_lineage_v19 RENAME TO yearly_lineage;
ALTER TABLE result_registry_v19 RENAME TO result_registry;
ALTER TABLE market_update_results_v19 RENAME TO market_update_results;
ALTER TABLE market_update_delta_clips_v19 RENAME TO market_update_delta_clips;
ALTER TABLE publication_results_v19 RENAME TO publication_results;
ALTER TABLE task_publications_v19 RENAME TO task_publications;
ALTER TABLE effective_heads_v19 RENAME TO effective_heads;
CREATE INDEX data_versions_entity_period_published
  ON data_versions(entity, period, adjustment, published_at DESC, version_key DESC);
