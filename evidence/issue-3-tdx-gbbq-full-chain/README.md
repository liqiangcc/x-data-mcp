# 600000 full category-1 event-chain evidence

This is an offline evidence-only reconstruction against the current-anchor
question in Issue #3. It reuses only the merged TDX gbbq/hsjday evidence and
the merged BaoStock #42/#43 evidence; it makes no network request and changes
no production code.

The official `sh600000.day` member contains 6,388 records through
2026-09-11. The merged 600000 gbbq target summary contains 27 category-1
records from 2000-07-06 through 2026-07-16. All 27 have an earlier trading-day
close and therefore resolve; none is silently skipped.

For each event, the fixed community mapping is C1/C2/C3/C4 =
`fenhong/peigujia/songzhuangu/peigu`, and the fixed formula is evaluated with
Decimal. A daily factor is the product of event ratios whose dates are
strictly later than that daily date, so the event date itself excludes its own
ratio. Three chains are recorded:

- unrounded theoretical reference preclose;
- a diagnostic `ROUND_HALF_UP` cent-rounded reference preclose;
- an evidence-driven diagnostic/output-compatibility variant that rounds each
  cumulative factor to 6 decimal places with `ROUND_HALF_UP` before applying it
  to OHLC. This is not a TDX or BaoStock contract.

The cent-rounded chain is not an official TDX or exchange rounding contract.

Results:

- `UNROUNDED_FULL_CHAIN_CURRENT_QFQ_MISMATCH`
- `CENT_ROUNDED_DIAGNOSTIC_FULL_CHAIN_MISMATCH`
- `BAOSTOCK_OUTPUT_COMPATIBLE_WITH_CENT_REFERENCE_6DP_FACTOR`
- `CURRENT_ANCHORED_QFQ_RECONSTRUCTION_NOT_PROVEN`

The cent-rounded chain is close in the 2017 ratio checks (within the fixed
diagnostic `1e-6` threshold) and matches selected prices to two-decimal
display precision, but exact required gates fail. The 6dp output variant
matches all 12 2017 OHLC fields and all 1,644 OHLC fields in the 411-row
2025-01-02..2026-09-10 fixed-history window exactly. The full machine gate
results and all event records are in `event-chain-analysis.json`; official
TDX semantics and production sufficiency remain unverified.
