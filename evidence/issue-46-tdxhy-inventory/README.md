# Issue #46: field-level inventory of retained base.zip members

**FIELD_SEMANTICS_NOT_VERIFIED.** This slice produces mechanical byte evidence
only. It does not claim field semantics, an industry hierarchy, a code→name
contract, an encoding, or a security-range contract, and it does not change
any #46/#6/#71 gate.

## What this slice is

#133 located `tdxhy.cfg` as the PRIMARY_CLASSIFICATION candidate; #135 proved
`tdxhy.cfg`/`tdxhy2.cfg` are `STRUCTURAL_RELATION_AMBIGUOUS` under a
line-level six-digit token rule. This slice goes one level finer:

1. **Field-level inventory** of `tdxhy.cfg` (5,659 records) and `tdxhy2.cfg`
   (gen-2: 6,754 records): delimiter/EOL forms, per-field byte-width
   histograms, distinct counts, charset measurements (ASCII vs high-byte),
   GB18030 strict-decode trials (labeled measurement only), and conformance
   rates against a frozen pattern set.
2. **Field-level attribution** of the #135 ambiguity: every six-digit token is
   classified by the field indices it occupies; code-position tokens are
   tested for univalent (≤1 line per member) correspondence; every
   multi-line token is attributed to its field index.
3. **Record-level machine comparison** for seven fixed samples
   (`688318`, `600000`, `000001`, `600004`, `600006`, `000002`, `000004`)
   across all six members, emitting a per-item PROVEN/UNPROVEN checklist.
4. **Security-identity/scope member inventory**: `infoharbor_ex.code`,
   `infoharbor_ex.name`, `base.dbf` (DBF header + field descriptors +
   per-field stats + GPDM code column), `base.map` (fixed-width split,
   ordered pairing vs base.dbf records), plus code-set joins across all
   members.

## Frozen plan and provenance

`plan.json` was committed before the first full scan (freeze commit precedes
the result commit on this branch). Inputs are gen-2: the official `base.zip`
re-downloaded 2026-09-15 under issue #71 comment-5676627357 authorization —
16,411,232 bytes, SHA-256
`dcdf6b49898926675698263535021aed7ad92b34be82a1a9e8a2a8982bd68bc6`,
Last-Modified `Mon, 14 Sep 2026 10:10:10 GMT`. The gen-1 retained archive
(16,099,192 bytes, `39de51e2…b71d9`) is preserved via
`evidence/issue-6-tdx-dbf-source/official/archive-20260913.json`; the gen-2
member table has the same 542 names, and the 12 members whose content changed
are recorded as findings. `tdxhy.cfg` itself is byte-identical across
generations.

## Evidence levels stay separate

- Official downloaded bytes, member hashes, raw record hex.
- Mechanical observations: field shapes, charset counts, GB18030 decode
  trials, token field positions, code-set cardinalities and overlaps,
  ordered-pairing rates.
- Semantic questions: emitted only under the fixed `UNPROVEN` label set.

## Reproduce offline

Python 3.11+ standard library only:

```sh
python3 -B evidence/issue-46-tdxhy-inventory/scan_tdxhy_inventory.py \
  /path/to/base.zip --output /tmp/issue46.json
python3 -B evidence/issue-46-tdxhy-inventory/scan_tdxhy_inventory.py \
  /path/to/base.zip --check
```

The scanner fails closed on any archive/member/plan hash mismatch. Two runs
from independent checkouts must be byte-identical; the result SHA-256 is
recorded in `determinism.json`.
