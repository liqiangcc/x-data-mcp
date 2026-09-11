# 000001 independent fixed-6dp output compatibility evidence

This is an offline, evidence-only recheck of the frozen PR #51 rule. It uses
the merged 000001 GBBQ target summary, the retained official
`sz\\lday\\sz000001.day` member, and the merged #42 BaoStock fixed_history
rows. No new data request, download, package installation, or business-code
change was made.

The rule is unchanged: category-1 C1/C2/C3/C4 maps to
`fenhong/peigujia/songzhuangu/peigu`; reference preclose is rounded HALF_UP
to 0.01; event ratios are multiplied for strictly later event dates; the
cumulative factor is rounded HALF_UP to 0.000001 before multiplying raw `.day`
OHLC with Decimal.

Results:

- 32 category-1 events, 1990-03-01..2026-06-12;
- 31 resolved, 1 unresolved: the 1990-03-01 event has no earlier `.day`
  trading record because this member starts at 1991-04-03;
- #42 `sz.000001` fixed_history: 411/411 dates, 1,644/1,644 OHLC exact,
  no first mismatch;
- all 411 rows infer one consistent 6dp factor from their four non-zero OHLC
  quotients;
- 2026-09-11 anchor factor is 1.000000.

Because the frozen acceptance rule requires `unresolved=0`, the bounded
conclusion is `BAOSTOCK_000001_OUTPUT_MISMATCH_WITH_FIXED_TDX_LOCAL_QFQ_RULE`,
despite the exact 411-row output comparison. This remains compatibility
evidence only: `TDX_OFFICIAL_QFQ_CONTRACT_NOT_PROVEN` and production
sufficiency `NOT_VERIFIED`.
