-- Schema 27 (M3-2 PR-B, issue-509 §8/§12): factor facts become survivable
-- without turning them into QFQ input. Two additions to the existing
-- `adjustment_events` table:
--
-- * category 11 (ETF share conversion) is stored next to category 1. The
--   price chain still uses category 1 only (D-70 Q3): a category-11 row is a
--   fact, not an applied factor.
-- * `basis_status` says which evidence tier the row carries. A-share
--   category-1 rows are `verified` (M1 semantics, unchanged); ETF category-11
--   rows are `pending` because only three events are price-anchored so far.
--
-- SQLite cannot widen a CHECK, so the table is rebuilt with the identical
-- columns/index plus the new column. Existing rows copy as `verified`; an
-- A-share rebuild therefore keeps every value and digest input unchanged.
CREATE TABLE adjustment_events_v27(
  entity TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
  effective_day INTEGER NOT NULL,
  category INTEGER NOT NULL CHECK(category IN (1,11)),
  c_bits_1 INTEGER NOT NULL CHECK(c_bits_1 BETWEEN 0 AND 4294967295),
  c_bits_2 INTEGER NOT NULL CHECK(c_bits_2 BETWEEN 0 AND 4294967295),
  c_bits_3 INTEGER NOT NULL CHECK(c_bits_3 BETWEEN 0 AND 4294967295),
  c_bits_4 INTEGER NOT NULL CHECK(c_bits_4 BETWEEN 0 AND 4294967295),
  c1 TEXT NOT NULL,
  c2 TEXT NOT NULL,
  c3 TEXT NOT NULL,
  c4 TEXT NOT NULL,
  basis_status TEXT NOT NULL DEFAULT 'verified' CHECK(basis_status IN ('verified','pending')),
  source_binding_digest BLOB NOT NULL REFERENCES source_bindings(source_binding_digest),
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  known_at TEXT,
  PRIMARY KEY(entity, ordinal)
) STRICT, WITHOUT ROWID;
INSERT INTO adjustment_events_v27(entity,ordinal,effective_day,category,
    c_bits_1,c_bits_2,c_bits_3,c_bits_4,c1,c2,c3,c4,basis_status,
    source_binding_digest,evidence_digest,known_at)
  SELECT entity,ordinal,effective_day,category,
    c_bits_1,c_bits_2,c_bits_3,c_bits_4,c1,c2,c3,c4,'verified',
    source_binding_digest,evidence_digest,known_at
  FROM adjustment_events;
DROP TABLE adjustment_events;
ALTER TABLE adjustment_events_v27 RENAME TO adjustment_events;
CREATE INDEX adjustment_events_entity_day
  ON adjustment_events(entity, effective_day, ordinal);
