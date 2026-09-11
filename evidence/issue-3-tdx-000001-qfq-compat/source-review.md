# 000001 offline review

The verifier consumes the merged #49 `target-summary.json` records for
`000001` and does not re-decode or reorder them. The fixed mapping is
C1/C2/C3/C4 = `fenhong/peigujia/songzhuangu/peigu`. All arithmetic is Decimal;
raw `.day` prices are integer values divided by 100.

The frozen output rule is:

```text
reference_cent = ROUND_HALF_UP(reference, 0.01)
event_ratio = reference_cent / previous_raw_close
factor(d) = ROUND_HALF_UP(product(event_ratio where event_date > d), 0.000001)
local_ohlc = raw_day_ohlc * factor(d)
```

The actual member SHA is recorded in `source-provenance.json`. It has 8,452
records from 1991-04-03 through 2026-09-11. There are 32 category-1 events
from 1990-03-01 through 2026-06-12. The first event is explicitly retained as
`unresolved` because no earlier `.day` trading record exists; it is not
silently discarded. The other 31 events resolve to a strict prior trading
date.

The merged #42 `sz.000001` fixed_history has 411 source rows from
2025-01-02 through 2026-09-10. Every date is present in the official member,
and all 1,644 open/high/low/close comparisons are exact under the fixed 6dp
factor rule. Each of those 411 rows also has four non-zero OHLC quotients that
round to one common 6dp factor. The 2026-06-12 latest event is before the
2026-09-11 anchor, whose factor is 1.000000.

The acceptance conclusion nevertheless remains
`BAOSTOCK_000001_OUTPUT_MISMATCH_WITH_FIXED_TDX_LOCAL_QFQ_RULE` because the
predeclared rule requires zero unresolved events. This is not a TDX or
BaoStock official QFQ contract; `TDX_OFFICIAL_QFQ_CONTRACT_NOT_PROVEN` and
production sufficiency `NOT_VERIFIED` remain.
