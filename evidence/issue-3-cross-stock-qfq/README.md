# Cross-market fixed-rule QFQ evidence

This evidence-only slice validates the already frozen PR #51/#52 local QFQ
rule against four deterministic, cross-market samples other than `600000`
and `000001`. Selection is made from the retained official TDX `gbbq` and
`hsjday.zip` artifacts before any BaoStock result is inspected. The BaoStock
part is a bounded daily-qfq comparison only: one anonymous session query per
selected security, no full-market traversal, and no source-year request.

The frozen Decimal rule is not changed:

```text
category=1; C1/C2/C3/C4 = fenhong/peigujia/songzhuangu/peigu
reference = (previous_close*10-fenhong+peigu*peigujia)
            /(10+peigu+songzhuangu)
reference_cent = ROUND_HALF_UP(reference, 0.01)
event_ratio = reference_cent / previous_close
factor(d) = ROUND_HALF_UP(product(event_ratio where event_date > d), 0.000001)
local_qfq_OHLC = raw_day_OHLC / 100 * factor(d)
```

`selection.json` is the pre-query selection record. `selected-events.json`
contains only category-1 records for the four selected securities; it is not
a dump of the full GBBQ stream. Large ZIP and `.day` files remain outside
the repository.

## Selection incident and correction

The first local selection used only six-digit `.day` member names and selected
`sh.000002`, `sh.000003`, `sz.000004`, and `sz.000006`. This was a
`PRE_PROBE_SELECTION_BUG`: the proxy was too broad for the requested SH/SZ
A-share scope. One bounded BaoStock query was made for each of those four
securities before the correction; the request metadata and row counts are
preserved in `pre-probe-selection-bug.json`, with
`excluded_from_validation_conclusions=true`. Their raw rows are not copied or
reused.

The corrected selection applies the explicit bounded A-share code-range proxy
before querying: SH `600/601/603/605/688/689`; SZ `000/001/002/003/300/301`,
while excluding `600000` and `000001`. It also requires a matching official
`.day` member, category-1 history, and at least one bar in the fixed window.
The proxy does not prove full current listing status, which remains
`NOT_VERIFIED`. The fixed corrected selection is `sh.600004`, `sh.600006`,
`sz.000002`, and `sz.000004`, two per market, and was written before the
valid BaoStock probe.

## Corrected probe and validation

Each corrected security received one fresh anonymous BaoStock qfq-daily query
for `2025-01-01..2026-09-10`, with no source-year request. The corrected
selection was not changed after these requests:

| security | rows / dates | category-1 events | relevant unresolved | OHLC exact | factor consistency | result |
| --- | --- | ---: | ---: | ---: | ---: | --- |
| `sh.600004` | 411 / 2025-01-02..2026-09-10 | 22 | 0 | 1644/1644 | 411/411 | compatible |
| `sh.600006` | 411 / 2025-01-02..2026-09-10 | 28 | 0 | 1128/1644 | 411/411 | mismatch |
| `sz.000002` | 411 / 2025-01-02..2026-09-10 | 36 | 0 | 1644/1644 | 411/411 | compatible |
| `sz.000004` | 368 / 2025-01-02..2026-07-13 | 8 | 0 | 1324/1472 over 331 matched dates; 37 source dates absent from `.day` | 331/331 | mismatch |

The `sh.600006` mismatch is classified as a GBBQ numeric-semantics diagnostic,
not repaired by changing the frozen rule. Its 2025-07-17 category-1 event has
C1 raw bits `0x3d4ccccd`, decoder diagnostic
`0.05000000074505806`, and C2/C3/C4 zero. The previous raw close on
2025-07-16 is `7.25`. The frozen diagnostic Decimal produces reference
`7.244999999925494194`, cent value `7.24`, ratio
`0.998620689655...`, and output factor `0.998621`; this leaves 129 dates
(2025-01-02..2025-07-16) and 516 OHLC mismatches. The persisted BaoStock
rows independently infer `1.000000` for all four OHLC quotients on all 129
dates. Treating the diagnostic value as business Decimal `0.05` is only a
counterfactual: reference `7.245` -> `7.25`, ratio `1`, which explains the
source factor but is `GBBQ_FLOAT32_BUSINESS_DECIMAL_NORMALIZATION_NOT_VERIFIED`
and not an official contract.

`sz.000004` has no OHLC mismatch on its 331 matched dates: the result is
`MATCHED_TRADING_BAR_QFQ_EXACT` with 331 dates / 1324 fields. The source also
returned 37 rows absent from the official member. All 37 are verified
suspension placeholders (`open=high=low=close=preclose`, `volume=''`,
`amount=''`), in clusters 2025-04-29 (1 row) and 2026-04-28..2026-06-22
(36 rows). This is
`BAOSTOCK_SUSPENSION_PLACEHOLDER_NOT_PRESENT_IN_TDX_DAY`, not a numeric QFQ
mismatch, but the stock remains incompatible under the original all-source-
dates-present gate.

The aggregate conclusion is
`CROSS_MARKET_VALIDATION_HAS_MISMATCH`, not a four-stock pass. The incident
probe is excluded from every count and conclusion. `TDX_OFFICIAL_QFQ_CONTRACT_NOT_PROVEN`
and full-market production sufficiency `NOT_VERIFIED` remain.

This is compatibility evidence only. It does not prove the TDX official QFQ
contract, full-market coverage, licensing, long-term stability, or production
sufficiency.
