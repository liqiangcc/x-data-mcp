-- Durable proven-gap facts. Rows are never updated, deleted, or backfilled;
-- absence only means no gap is provable (pre-upgrade history has no rows).
-- A covering row supersedes a gap for default selection without erasing it.
CREATE TABLE proven_gaps (
 gap_id TEXT PRIMARY KEY CHECK(length(gap_id)>0),
 symbol TEXT NOT NULL CHECK(length(symbol)>0),
 period TEXT NOT NULL CHECK(period IN ('day','year')),
 start_date TEXT NOT NULL CHECK(date(start_date) IS NOT NULL),
 end_date TEXT NOT NULL CHECK(date(end_date) IS NOT NULL),
 evidence_kind TEXT NOT NULL CHECK(length(evidence_kind)>0),
 evidence_id TEXT NOT NULL CHECK(length(evidence_id)>0),
 evidence_digest TEXT NOT NULL CHECK(length(evidence_digest)>0),
 observed_at TEXT NOT NULL CHECK(length(observed_at)>0),
 CHECK(start_date <= end_date)
) STRICT;
CREATE TABLE gap_coverings (
 gap_id TEXT NOT NULL REFERENCES proven_gaps(gap_id),
 version_id TEXT NOT NULL REFERENCES data_versions(version_id),
 covered_at TEXT NOT NULL CHECK(length(covered_at)>0),
 PRIMARY KEY(gap_id,version_id)
) STRICT;
CREATE TRIGGER proven_gaps_no_update
 BEFORE UPDATE ON proven_gaps
 BEGIN SELECT RAISE(ABORT,'proven gap facts are immutable'); END;
CREATE TRIGGER proven_gaps_no_delete
 BEFORE DELETE ON proven_gaps
 BEGIN SELECT RAISE(ABORT,'proven gap facts are immutable'); END;
CREATE TRIGGER gap_coverings_no_update
 BEFORE UPDATE ON gap_coverings
 BEGIN SELECT RAISE(ABORT,'gap covering bindings are immutable'); END;
CREATE TRIGGER gap_coverings_no_delete
 BEFORE DELETE ON gap_coverings
 BEGIN SELECT RAISE(ABORT,'gap covering bindings are immutable'); END;
