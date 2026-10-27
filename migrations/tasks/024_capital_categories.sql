-- Schema 24 (M2 PR-2, issue-391 §5.1; D-50 R-3 revision): D-50 admits gbbq
-- categories 5, 7 and 8 as capital facts at the OWNER_RISK_ACCEPTED tier, so
-- the schema-23 `category IN (2,3,9,10)` CHECK is too narrow. SQLite cannot
-- alter a CHECK, hence create + copy + drop + rename. 023 stays immutable
-- (D-42 Q1); this is the only schema-23 object that changes.
--
-- Only the category vocabulary changes. Every column, default, foreign key,
-- primary key and the secondary index are copied verbatim; the source's
-- `*_bits` stay the immutable audit representation, while `float_after`/
-- `total_after` and `effective_day` remain the only state inputs (D-50: only
-- the after value and event day are used; the before value is audit only).
CREATE TABLE capital_events_v24(
  exchange TEXT NOT NULL, code TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
  effective_day INTEGER NOT NULL,
  category INTEGER NOT NULL CHECK(category IN (2,3,5,7,8,9,10)),
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
INSERT INTO capital_events_v24(exchange,code,ordinal,effective_day,category,
    float_before_bits,total_before_bits,float_after_bits,total_after_bits,
    float_after,total_after,prev_is_lagged,suspicious,known_at,
    source_binding_digest,evidence_digest)
  SELECT exchange,code,ordinal,effective_day,category,
    float_before_bits,total_before_bits,float_after_bits,total_after_bits,
    float_after,total_after,prev_is_lagged,suspicious,known_at,
    source_binding_digest,evidence_digest
  FROM capital_events;
DROP TABLE capital_events;
ALTER TABLE capital_events_v24 RENAME TO capital_events;
CREATE INDEX capital_events_code_day ON capital_events(exchange, code, effective_day, ordinal);
