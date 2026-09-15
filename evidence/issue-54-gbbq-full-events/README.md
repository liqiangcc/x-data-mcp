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

1. Decode every record of the plan-pinned official `gbbq` member
   (gen-1: 4 + 205727 × 29 bytes; gen-2: 4 + 205776 × 29 bytes) with the
   pinned community decoder source, and
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

## Frozen plans and provenance

Two frozen plans are registered in the scanner's `KNOWN_PLANS` hash registry;
each was committed before any evaluation under that plan.

**`plan.json` (generation 1)** — committed before any evaluation (freeze
commit precedes the result commit on this branch). The seven candidate rules
are copied verbatim from the bounded `issue-54-gbbq-decimal/plan.json`; no
candidate was added, removed, or retuned after seeing any data. Its input
pins are the retained gen-1 artifacts:

- retained `gbbq` member: 5,966,087 bytes, SHA-256
  `24f46cecc9b085ee6c0ef126fab33b405b8057747793eb5d25d44e624769ff60`,
  count 205727
- retained `hsjday.zip`: SHA-256
  `611ea3ccc1e64717aebc9a14ce9c8b1ed06526b5fd69bd14275fc376f4f44272`,
  plus the four `.day` member SHA-256 from #53 `selection.json`
- pinned community decoder source `pytdx/reader/gbbq_reader.py`: raw SHA-256
  `e28fae76760b1247a960cab285aa357c2d263b3212e7cabfb3f1ebfcc127d6ba`,
  git blob SHA-1 `0eda4935027ffc79b5c425eff0a70a6d492b21d1`

Gen-1 cross-checks demand the fresh decode reproduce the committed 94 event
records at identical `record_index` + `clear_hex` and the committed 68,642
six-digit category=1 total. The gen-1 retained files were never staged on
the worker machine; `plan.json` pins are preserved unchanged as historical
evidence and are not superseded.

**`plan-v2.json` (generation 2)** — after download authorization (issue #71
comment-5676627357), the official public endpoints returned a regenerated
snapshot dated 2026-09-14 whose hashes differ from every gen-1 pin. Per the
coordinator ruling, this snapshot is pinned as generation-2 input rather
than replacing gen-1 pins:

- `gbbq.zip` 6,358,077 bytes, SHA-256
  `daad6290c4f1cd478d7436b1e7077260a3e3e52cefd7eae58e4f24878fa2d0e9`,
  Last-Modified `Mon, 14 Sep 2026 10:10:10 GMT`; member `gbbq` 5,967,508
  bytes, SHA-256 `4a3e9b862247a8a3fec406cc25b9a5a6ee3303f740780822519c99decaa09d77`,
  count 205776 (+49 vs gen-1)
- `hsjday.zip` 549,387,306 bytes, SHA-256
  `c82f852a7bc2e52c7e89347a1e0d1878b01ce997946c2745f7af323752270892`,
  Last-Modified `Mon, 14 Sep 2026 07:58:56 GMT`; `.day` members 600004 /
  600006 / 000002 each +32 bytes vs gen-1 pins, 000004 unchanged
- decoder source pin unchanged (still matches gen-1)

Gen-2 consistency is presence-based: every committed clear_hex must exist in
the member; `record_index` equality is recorded per event, not assumed; all
divergences (absent events, category-set or cat-1 total deltas, `.day`
member hash diffs vs gen-1) are recorded as findings in `result.json`
`consistency`, never silently dropped.

The candidate rules, formula, `ROUND_HALF_UP` cent reference and
single-event 6dp ratio are identical in both plans — frozen research rules,
diagnostics, not official semantics.

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

Gen-1 retained inputs were never staged on the worker machine. Under the
generation-2 ruling, the scan runs against the `plan-v2.json` pins; the
scanner fails closed on any input hash mismatch and on any unregistered or
drifted plan file. `result.json` and `determinism.json` are produced from
two byte-identical runs.

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
  --plan plan-v2.json \
  --gbbq /path/to/gbbq --decoder-source /path/to/gbbq_reader.py \
  --hsjday /path/to/hsjday.zip --output /tmp/issue54-full.json
python3 -B evidence/issue-54-gbbq-full-events/scan_gbbq_full.py \
  --plan plan-v2.json \
  --gbbq /path/to/gbbq --decoder-source /path/to/gbbq_reader.py \
  --hsjday /path/to/hsjday.zip --check
```

`--plan` defaults to `plan-v2.json`; `--plan plan.json` selects the gen-1
pins should the retained gen-1 files ever be staged.

Two runs must be byte-identical; the result SHA-256 is recorded in
`determinism.json` and the issue comment.

## Boundary

This scan is a histogram + diagnostic comparison, not a contract proof. Even
if one candidate explains every .day-observed previous close, that does not
establish TDX's normalization, rounding order, per-field scale, or cumulative
QFQ arithmetic. `GBBQ_FLOAT32_BUSINESS_DECIMAL_NORMALIZATION_NOT_VERIFIED`,
`TDX_OFFICIAL_QFQ_CONTRACT_NOT_PROVEN`, and all existing gates remain.
