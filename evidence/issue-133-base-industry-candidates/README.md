# #133 base.zip research-industry candidate evidence

This directory records a bounded offline scan of the retained official archive. The scan input was `/tmp/official-base.zip`, verified before member reads as 16,099,192 bytes with SHA-256 `39de51e26c0ee8fb3fd970ee870f4cb955b8fbbd7f84d752157f3998e49b71d9`. The independent `/tmp/official-base-latest.zip` copy had the same bytes and SHA and was not analyzed separately.

The frozen rules are in `selection-rules-v1.md`; they were committed before this archive was opened. `scan_base.py` verifies the archive identity, checks every member against the retained 542-member index, scans raw uncompressed bytes for the fixed tokens, and records deterministic member and ZIP offsets, member hashes, and matched bytes in `scan-result.json`. The v1 raw hit records remain valid audit material, but its overall classification is `INVALID_FOR_CONCLUSION` because the implementation let context hits affect the result and v1 did not mechanically define incompatible observations.

## Result

The v1 `scan-result.json` reports `scanned_member_count=542` and an overall `AMBIGUOUS` string; that overall string is retained only as an audit of the flawed implementation and is `INVALID_FOR_CONCLUSION`. Its member/hash/offset/token facts are retained unchanged.

The v2 `scan-result-v2.json` was generated only after `selection-rules-v2.md` was frozen at commit `a23f029cd91e8bfded1dcadd8b921f3da43212f8`. It reports `scanned_member_count=542`, `candidate_classes=["PRIMARY_CLASSIFICATION"]`, `candidate_hits=431`, `context_hits=298`, and overall `FOUND_CANDIDATE`. Context hits do not participate in that classification.

The required probes occur as follows:

- `X4202` and `X420202` occur as raw bytes in `tdxhy.cfg` (member SHA-256 `df7cb7051d0f73a0d1381e9b36eafa33c8f5081d28ddef87cad406dd28ed8ea6`). `X4202` is also the substring of each `X420202` match by the fixed byte-search rule; no truncation or equivalence is asserted.
- `T1205` occurs in `tdxhy.cfg` as another raw token from the retained `688318` line evidence.
- `688318` occurs in `tdxhy.cfg`, `tdxhy2.cfg`, `base.dbf`, `base.map`, and `infoharbor_ex.code`, and also in `block.dat`, `block_fg.dat`, `block_gn.dat`, and `block_zs.dat`. These are context locator hits in v2 and cannot change `FOUND_CANDIDATE`.

The scan found locator candidates (`tdxhy.cfg`, `tdxhy2.cfg`, `base.dbf`, `base.map`, `infoharbor_ex.code`) but did not decode any member or establish code→name, parent/child hierarchy, field meaning, version equivalence, or complete research-industry level-2 coverage. Candidate status is evidence only and does not close #46/#6 or change production readiness. No archive bytes are committed; no network access or production import was used.

The v2 runner is `scan_base_v2.py`; it independently records `industry_hits`, `context_hits`, `candidate_hits`, and `candidate_classes` for every member. Both v1 and v2 results are preserved; v2 is the current classification under the newly frozen executable rules.

The two output files generated from independent runs were byte-identical (SHA-256 `1a0aec42f61be53dc2ec44147d003e5c20f02ebd1e90a1640a99bf24120ee05f`).
