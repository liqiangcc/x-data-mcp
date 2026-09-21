-- Compact record storage (#259): data_versions drops the duplicated
-- compat_json bulk in favor of envelope_json with content-addressed refs.
-- segment_bars keeps one canonical bar representation (exact JSON) keyed by
-- integer surrogate segment ids; payloads holds shared arrays once.
-- Legacy rows are converted by the post-migration data step in Rust, which
-- fills bar_json/envelope_json before this transaction commits.
CREATE TABLE payloads(
 payload_id TEXT PRIMARY KEY,
 payload_json TEXT NOT NULL
) STRICT;
CREATE TABLE data_versions_v16(
 version_id TEXT PRIMARY KEY,
 symbol TEXT NOT NULL,
 period TEXT NOT NULL CHECK(period IN ('day','year')),
 adjustment TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('staging','validated','published')),
 source TEXT NOT NULL,
 published_at TEXT NOT NULL,
 record_count INTEGER NOT NULL,
 unique_count INTEGER NOT NULL,
 duplicate_count INTEGER NOT NULL,
 coverage_status TEXT NOT NULL,
 coverage_reason TEXT NOT NULL,
 first_date TEXT NOT NULL,
 last_date TEXT NOT NULL,
 envelope_json TEXT NOT NULL,
 content_digest TEXT NOT NULL,
 digest_format TEXT NOT NULL
) STRICT;
INSERT INTO data_versions_v16
 SELECT version_id,symbol,period,adjustment,state,source,published_at,record_count,unique_count,duplicate_count,coverage_status,coverage_reason,first_date,last_date,compat_json,content_digest,digest_format
 FROM data_versions ORDER BY rowid;
CREATE TABLE bar_segments_v16(
 segment_id INTEGER PRIMARY KEY,
 symbol TEXT NOT NULL,
 first_date TEXT NOT NULL,
 last_date TEXT NOT NULL,
 row_count INTEGER NOT NULL CHECK(row_count>0)
) STRICT, WITHOUT ROWID;
INSERT INTO bar_segments_v16
 SELECT rowid,symbol,first_date,last_date,row_count FROM bar_segments ORDER BY rowid;
CREATE TABLE segment_bars_v16(
 segment_id INTEGER NOT NULL REFERENCES bar_segments_v16(segment_id),
 trade_date TEXT NOT NULL,
 bar_json TEXT NOT NULL,
 PRIMARY KEY(segment_id,trade_date)
) STRICT, WITHOUT ROWID;
INSERT INTO segment_bars_v16
 SELECT n.segment_id,s.trade_date,''
 FROM segment_bars s
 JOIN bar_segments o ON o.segment_id=s.segment_id
 JOIN bar_segments_v16 n ON n.segment_id=o.rowid;
CREATE TABLE version_segments_v16(
 version_id TEXT NOT NULL REFERENCES data_versions_v16(version_id),
 seq INTEGER NOT NULL CHECK(seq>=0),
 segment_id INTEGER NOT NULL REFERENCES bar_segments_v16(segment_id),
 clip_start TEXT NOT NULL,
 clip_end TEXT NOT NULL,
 PRIMARY KEY(version_id,seq)
) STRICT, WITHOUT ROWID;
INSERT INTO version_segments_v16
 SELECT vs.version_id,vs.seq,n.segment_id,vs.clip_start,vs.clip_end
 FROM version_segments vs
 JOIN bar_segments o ON o.segment_id=vs.segment_id
 JOIN bar_segments_v16 n ON n.segment_id=o.rowid;
DROP TABLE version_segments;
DROP TABLE segment_bars;
DROP TABLE bar_segments;
DROP TABLE data_versions;
ALTER TABLE data_versions_v16 RENAME TO data_versions;
ALTER TABLE bar_segments_v16 RENAME TO bar_segments;
ALTER TABLE segment_bars_v16 RENAME TO segment_bars;
ALTER TABLE version_segments_v16 RENAME TO version_segments;
