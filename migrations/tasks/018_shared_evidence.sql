-- Task-level shared evidence digest: generation/batch-scale facts (e.g. a
-- resolved default selection's evidence object) are stored once inside
-- frozen_work_json instead of being copied into every item's frozen
-- inputs. This column carries the digest the per-item evidence markers
-- must equal, so item rows stay O(1) verifiable without decoding the
-- shared blob.
ALTER TABLE tasks ADD COLUMN evidence_digest TEXT;
