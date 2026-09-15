-- Durable gap-verification decisions (#155). Rows are never updated,
-- deleted, or backfilled; absence only means no decision is provable
-- (pre-upgrade history has no rows). Only an installed trusted source-gap
-- verifier may produce one; the fact layer stores the sealed decision and
-- never decides which gaps exist.
CREATE TABLE gap_verification_decisions (
 decision_id TEXT PRIMARY KEY CHECK(length(decision_id)>0),
 symbol TEXT NOT NULL CHECK(length(symbol)>0),
 period TEXT NOT NULL CHECK(period='day'),
 start_date TEXT NOT NULL CHECK(date(start_date) IS NOT NULL),
 end_date TEXT NOT NULL CHECK(date(end_date) IS NOT NULL),
 subject_version_id TEXT NOT NULL CHECK(length(subject_version_id)>0),
 subject_version_digest TEXT NOT NULL CHECK(length(subject_version_digest)>0),
 conclusion TEXT NOT NULL CHECK(length(conclusion)>0),
 basis_kind TEXT NOT NULL CHECK(length(basis_kind)>0),
 basis_ref TEXT NOT NULL CHECK(length(basis_ref)>0),
 basis_sha256 TEXT NOT NULL CHECK(length(basis_sha256)>0),
 verifier_id TEXT NOT NULL CHECK(length(verifier_id)>0),
 decided_at TEXT NOT NULL CHECK(length(decided_at)>0),
 decision_sha256 TEXT NOT NULL CHECK(length(decision_sha256)>0),
 CHECK(start_date <= end_date)
) STRICT;
CREATE TRIGGER gap_verification_decisions_no_update
 BEFORE UPDATE ON gap_verification_decisions
 BEGIN SELECT RAISE(ABORT,'gap verification decisions are immutable'); END;
CREATE TRIGGER gap_verification_decisions_no_delete
 BEFORE DELETE ON gap_verification_decisions
 BEGIN SELECT RAISE(ABORT,'gap verification decisions are immutable'); END;
