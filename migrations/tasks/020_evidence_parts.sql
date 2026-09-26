-- #259 P0-D: generation-common evidence sub-documents.
-- A `version` evidence bundle mixes per-version facts (coverage reason)
-- with fields that are identical for every version resolved from one
-- generation (unit/currency, adjustment evidence). Those fields are stored
-- once in this table keyed by their content digest, and the parent row in
-- `evidence_bundles` carries an `$evref` marker that readers re-inline.
-- Digests and canonical bytes are unchanged: this table is a storage-level
-- dedup, not a format change.
CREATE TABLE evidence_parts(
  part_digest BLOB PRIMARY KEY CHECK(length(part_digest)=32),
  canonical_json TEXT NOT NULL
) STRICT, WITHOUT ROWID;
