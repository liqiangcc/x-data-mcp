-- Delta manifests (#259 P0-D): a version's ordered record projection becomes
-- a persistent height-balanced tree of segment clips stored in
-- manifest_nodes. New versions path-copy O(log n) nodes for changed ranges
-- instead of restating every inherited clip, so per-version manifest growth
-- no longer scales with history length. evidence_digest replaces the
-- version_evidence row projection with one content hash; the evidence itself
-- already lives inside envelope_json.
-- Flat clips convert to trees and evidence digests are sealed by the
-- post-migration data step in Rust, inside the same transaction.
CREATE TABLE manifest_nodes(
 node_id INTEGER PRIMARY KEY,
 left_id INTEGER REFERENCES manifest_nodes(node_id),
 right_id INTEGER REFERENCES manifest_nodes(node_id),
 segment_id INTEGER REFERENCES bar_segments(segment_id),
 clip_start TEXT,
 clip_end TEXT,
 first_date TEXT NOT NULL,
 last_date TEXT NOT NULL,
 row_count INTEGER NOT NULL CHECK(row_count>0),
 height INTEGER NOT NULL CHECK(height>0)
) STRICT;
-- manifest_root is a plain id (no REFERENCES): schema-rewind fixtures drop
-- the column, which SQLite forbids while it carries a foreign-key clause.
ALTER TABLE data_versions ADD COLUMN manifest_root INTEGER;
ALTER TABLE data_versions ADD COLUMN evidence_digest TEXT;
