# Issue #54: full-domain GBBQ category=1 scan (extended bounded experiment)

**GBBQ_FLOAT32_BUSINESS_DECIMAL_NORMALIZATION_NOT_VERIFIED**.
**TDX_OFFICIAL_QFQ_CONTRACT_NOT_PROVEN**. No production normalization contract
or runtime change is introduced. #54 remains open; the official-file gate
remains closed. Compatibility with a diagnostic candidate is not official
semantics. All #53 gates are unchanged: the aggregate remains
`CROSS_MARKET_VALIDATION_HAS_MISMATCH`.

## What this slice is

`evidence/issue-54-gbbq-decimal/` evaluated the frozen candidates on a bounded
nine-event matrix with a first-N cap. This slice removes the cap and extends to
the retained domain, still evidence-only and offline:

1. Decode every record of the retained official `gbbq` member
   (4 + 205727 × 29 bytes) with the pinned community decoder source, and
   exhaust **all** category=1 records into C1..C4 raw-bits/float32
   distribution histograms: distinct value counts, nonzero-field combination
   patterns, and per-symbol coverage.
2. For every category=1 event of the fixed #53 four securities
   (sh.600004, sh.600006, sz.000002, sz.000004 — 22+28+36+8 = 94 events, no
   reselection), evaluate all seven frozen candidate decimal interpretations
   against the retained official `.day` previous bar and event-date bar.
3. Report per-candidate machine comparison vs the .day observed previous
   close, and the list of events where frozen candidates disagree on the
   cent-rounded reference (discriminating events).

## Frozen plan and provenance

`plan.json` was committed before any evaluation (freeze commit precedes the
result commit on this branch). The seven candidate rules are copied verbatim
from the bounded `issue-54-gbbq-decimal/plan.json`; no candidate was added,
removed, or retuned after seeing any data. The formula, `ROUND_HALF_UP` cent
reference and single-event 6dp ratio are the same frozen research rules —
diagnostics, not official semantics.

Inputs are pinned by hash inside `plan.json` and re-verified by the scanner
before producing output:

- retained `gbbq` member: 5,966,087 bytes, SHA-256
  `24f46cecc9b085ee6c0ef126fab33b405b8057747793eb5d25d44e624769ff60`
- retained `hsjday.zip`: SHA-256
  `611ea3ccc1e64717aebc9a14ce9c8b1ed06526b5fd69bd14275fc376f4f44272`,
  plus the four `.day` member SHA-256 from #53 `selection.json`
- pinned community decoder source `pytdx/reader/gbbq_reader.py`: raw SHA-256
  `e28fae76760b1247a960cab285aa357c2d263b3212e7cabfb3f1ebfcc127d6ba`,
  git blob SHA-1 `0eda4935027ffc79b5c425eff0a70a6d492b21d1`
- committed #53 `selected-events.json` / `selection.json` SHA-256; the fresh
  full decode must reproduce the committed 94 event records exactly
  (`record_index` + `clear_hex`) and the committed 68,642 six-digit
  category=1 total.

## Evidence levels stay separate

- Official downloaded file bytes and member hashes: retained source artifacts
  from #47/#48/#53.
- Record decode, C1/C2/C3/C4 field names/units and category interpretation:
  inherited **community reverse-engineering / research assumptions**, not an
  official contract.
- Decimal and reference calculations: reproducible diagnostic experiment only.
- Official client/API numeric behavior: **NOT_RUN**; no client execution,
  login or Key.

## Status

**INPUTS_NOT_STAGED** in the execution environment as of the freeze commit:
the retained `gbbq` member, `hsjday.zip`, and the pinned decoder source are
not committed to git and were not present on the worker machine. The scanner
fails closed on any input hash mismatch. `result.json` and
`determinism.json` are produced only after the retained inputs are staged.

Self-test coverage runs without the inputs:

```sh
python3 -B -m unittest discover -s evidence/issue-54-gbbq-full-events -p 'test_*.py'
```

It replays the known 600006 half-cent split between candidates, the
unresolved-previous-close retention, and the histogram/combination/coverage
aggregations on synthetic records, plus the frozen plan-hash pin.

## Reproduce offline (once inputs are staged)

Python 3.11+ standard library only, no production dependency:

```sh
python3 -B evidence/issue-54-gbbq-full-events/scan_gbbq_full.py \
  --gbbq /path/to/gbbq --decoder-source /path/to/gbbq_reader.py \
  --hsjday /path/to/hsjday.zip --output /tmp/issue54-full.json
python3 -B evidence/issue-54-gbbq-full-events/scan_gbbq_full.py \
  --gbbq /path/to/gbbq --decoder-source /path/to/gbbq_reader.py \
  --hsjday /path/to/hsjday.zip --check
```

Two runs must be byte-identical; the result SHA-256 is recorded in
`determinism.json` and the issue comment.

## Boundary

This scan is a histogram + diagnostic comparison, not a contract proof. Even
if one candidate explains every .day-observed previous close, that does not
establish TDX's normalization, rounding order, per-field scale, or cumulative
QFQ arithmetic. `GBBQ_FLOAT32_BUSINESS_DECIMAL_NORMALIZATION_NOT_VERIFIED`,
`TDX_OFFICIAL_QFQ_CONTRACT_NOT_PROVEN`, and all existing gates remain.
