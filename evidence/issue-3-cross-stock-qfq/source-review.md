# Cross-market review

The sample selection was fixed from local official artifacts before inspecting
any valid BaoStock response. It decoded category-1 records for the retained
GBBQ member, intersected them with official `hsjday` SH/SZ `.day` members, and
required at least one bar in `2025-01-01..2026-09-10`. The corrected identity
proxy is SH `600/601/603/605/688/689` and SZ `000/001/002/003/300/301`, with
`600000` and `000001` excluded. This is a bounded code-range proxy only;
complete current-listing status is `NOT_VERIFIED`.

The first selection omitted the code-prefix proxy and admitted
`sh.000002`, `sh.000003`, `sz.000004`, and `sz.000006`. That process error is
recorded as `PRE_PROBE_SELECTION_BUG`. One query per invalid candidate was
already made; its metadata is preserved separately and explicitly excluded.
No invalid raw rows are used by the corrected selection or validation.

The corrected selection is `sh.600004`, `sh.600006`, `sz.000002`, and
`sz.000004`. Each received one fresh anonymous BaoStock daily query with
`frequency=d`, `adjustflag=2`, and the fixed date window. The raw string
responses are in `baostock-valid-probe.json`; the per-security summaries are
in `validation/`.

The verifier applies the PR #51/#52 rule without modification: C1/C2/C3/C4
maps to `fenhong/peigujia/songzhuangu/peigu`, previous close is the strict
prior `.day` close, reference uses Decimal and HALF_UP cent rounding, factors
use strict `event_date > bar_date` and HALF_UP six-decimal rounding, and final
OHLC has no extra rounding. Pre-history events remain outside the available
bar domain; relevant unresolved events gate compatibility.

`sh.600004` and `sz.000002` are exact across all 1,644 OHLC fields. For
`sh.600006`, the fixed 2025-07-17 category-1 event is C1 bits `0x3d4ccccd`,
diagnostic `0.05000000074505806`, with C2/C3/C4 zero. The actual previous
trading date is 2025-07-16 and its raw close is `7.25`. Direct Decimal use of
the diagnostic tail gives reference `7.244999999925494194`, HALF_UP cent
reference `7.24`, ratio `0.998620689655...`, and factor `0.998621`. The 129
dates before that event have 516 OHLC mismatches; the first is 2025-01-02
open, local `7.34985056` versus source `7.3600000000`. Existing source rows
show all four OHLC source/raw quotients round to `1.000000` on all 129 dates.
The diagnostic counterfactual normalizes C1 to business Decimal `0.05`, giving
reference `7.245`, cent reference `7.25`, and ratio `1`; it is marked
`GBBQ_FLOAT32_BUSINESS_DECIMAL_NORMALIZATION_NOT_VERIFIED` /
`COUNTERFACTUAL_EXPLAINS_SOURCE_FACTOR`, not promoted to a contract or PASS.

`sz.000004` has 1,324/1,324 exact OHLC fields on 331 matched dates, with no
numeric mismatch there, but 37 source dates are absent from the official
`.day` member. The saved rows verify all 37 as placeholders with
`open=high=low=close=preclose`, `volume=''`, and `amount=''`. Source-row-order
clusters are 2025-04-29 (1) and 2026-04-28..2026-06-22 (36). This is recorded
as `MATCHED_TRADING_BAR_QFQ_EXACT` plus
`BAOSTOCK_SUSPENSION_PLACEHOLDER_NOT_PRESENT_IN_TDX_DAY`; the stock still
fails the original all-source-dates-present gate. All quotients inferred from
non-zero OHLC agree at six decimals for every matched row.

The cross-market result is `CROSS_MARKET_VALIDATION_HAS_MISMATCH`. This does
not authorize changing the rule or promote it to an official TDX QFQ contract;
`TDX_OFFICIAL_QFQ_CONTRACT_NOT_PROVEN` and full-market production sufficiency
remain `NOT_VERIFIED`.
