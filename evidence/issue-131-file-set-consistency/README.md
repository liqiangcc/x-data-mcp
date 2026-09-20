# issue-131-file-set-consistency

Compatibility audit of the pinned official retained input set
(`base_industry` + `day` + `gbbq`, snapshot generation 2026-09-20) for the
`FILE_SET_CONSISTENCY_NOT_VERIFIED` gate (contract
`official-file-consistency-v1`, `src/file_consistency.rs`).

## What this evidence asserts

The exact pinned file set was **exercised together across the full published
domain with verified outcomes** — not inferred from matching URLs, dates,
ETags, or common hashes:

- **Byte integrity re-verified**: each retained file was re-hashed from bytes
  and equals its manifest sha256 (the manifest itself is pinned by sha256 in
  `plan.json`).
- **Sole-input binding**: all 10,443 published day `data_versions` carry
  `snapshot_inputs` binding exactly the manifest `day` + `gbbq` archive
  sha256s — zero violations.
- **Full-domain exercise**: all 5,221 effective day heads are versions
  published from this set; no combination failure was observed in the
  exercised domain (112,414 category-1 GBBQ events applied; 243,822
  non-category-1 records excluded uninterpreted — both summed over all
  10,443 day versions; see `result.json` for exact counts).
- **base exercised**: the catalog publication `catalog-1789885204014663569`
  carries `source = official-file:<manifest_sha256[:16]>` and
  `acquired_at` equal to base's `acquisition_completed_at`.

## Artifacts

- `plan.json` — frozen inputs (manifest sha256 pin), checks, scope limits
- `audit.py` — deterministic stdlib-only audit; streams `compat_json` rows
- `result.json` — generated audit outcome (`audit_digest` covers the core facts)
- `compatibility-evidence.json` — the `CompatibilityEvidence` artifact
  (`member_bindings` = the three manifest sha256s; `valid_window` = the closed
  acquisition interval; set revision pins the manifest itself)
- `input.json` — generated `consistency_check` input (FileFacts + evidence)
- `decision.json` — verbatim `consistency_check input.json` output:
  `VERIFIED_COMPATIBLE`, zero reasons
- `determinism.json` — two runs byte-identical

## Scope limits (read before relying on this)

- This evidence asserts **composability of this exact pinned set** for this
  snapshot generation. It says nothing about vendor authorization — that
  remains #71 / `FILE_ACCESS_USE_NOT_VERIFIED`, an independent gate.
- A new download generation (different bytes) requires a new manifest and a
  fresh audit; this artifact does not transfer.
- Applied/excluded event counts are read verbatim from stored
  `adjustment_evidence`; this audit does not decode GBBQ records itself.
- Whether these facts suffice to retire `FILE_SET_CONSISTENCY_NOT_VERIFIED`
  is a Final Gate decision, not made by this package.
