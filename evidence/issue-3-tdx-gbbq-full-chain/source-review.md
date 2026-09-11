# Review of the full-chain reconstruction

## Fixed inputs and interpretation

The analysis uses the merged #49 decoded target summary for all 600000
records with category `1`, and the official `sh600000.day` member. It does not
decode a fresh file or request data. The record fields are not reinterpreted:
the merged #50 community-consistent order is C1/C2/C3/C4 =
`fenhong/peigujia/songzhuangu/peigu`.

All 27 category-1 events are included. For each event the script finds the
latest `.day` date strictly before the event date. The resulting unresolved
count is zero. The event formula is:

```text
reference=(prev_close*10-fenhong+peigu*peigujia)/(10+peigu+songzhuangu)
ratio=reference/prev_close
```

For a daily date `d`, the current-anchor factor is the Decimal product of
ratios for events with `event_date > d`; the event date therefore excludes its
own ratio. The latest event is 2026-07-16, so the factor on 2026-07-16 and
later is 1 under this rule.

The second chain rounds `reference` to 0.01 with `ROUND_HALF_UP` before the
division. This is a diagnostic for the earlier 11.746... versus 11.75
observation only; it is explicitly **not** an official TDX or exchange rule.

## Gate policy

The machine output has separate gates for both chains. Each chain records:

- unresolved count and latest-event/anchor semantics;
- 2017-05-24/25/26 factor ratio exact checks and fixed `1e-6` diagnostic checks;
- 2017 OHLC exact checks and separate two-decimal display checks;
- selected 2026-07-15/16/17 and 2026-09-10 OHLC exact/display checks;
- the 2017-05-24 factor divided by the 2017-05-25 factor versus the event
  ratio, both exact and diagnostic tolerance.

Only unresolved=0, anchor semantics, all exact ratio/price gates, and exact
quotient gates are `required_for_full_match`. Diagnostic tolerance or 2dp
display agreement cannot create a MATCH.

## Results

There are 27 resolved events, latest 2026-07-16. The unrounded factor is
`0.533076243456588643...` on 2017-05-24 and
`0.702075725746919070...` on 2017-05-25/26. BaoStock's exact ratios from
the stored rows are `0.533312` and `0.702156`; the unrounded chain therefore
fails exact and `1e-6` ratio gates. Its 2017 price checks do not pass exact;
some are only 2dp matches.

The cent-rounded factor is
`0.533311952713758227...` on 2017-05-24 and
`0.702156247530369342...` on 2017-05-25/26. Those ratios are within the fixed
`1e-6` diagnostic threshold but are not exact. Selected recent dates after
the latest event (`2026-07-17`, `2026-09-10`) match the stored BaoStock OHLC
rows exactly with factor 1; 2026-07-15 and the 2017 rows fail exact gates.

The BaoStock 2017 factor quotient is
`0.759534918166333407...`. It differs from the unrounded event ratio
`0.759285962905872408...` by about `0.000248955260...`; it differs from the
cent-rounded diagnostic ratio `0.759534583063994828...` by about
`0.000000335102...`, within the diagnostic threshold but not exact.

Therefore the persisted conclusions are:

- `UNROUNDED_FULL_CHAIN_CURRENT_QFQ_MISMATCH`;
- `CENT_ROUNDED_DIAGNOSTIC_FULL_CHAIN_MISMATCH`;
- `CURRENT_ANCHORED_QFQ_RECONSTRUCTION_NOT_PROVEN`.

This does not establish that `.day + gbbq` is insufficient in every possible
implementation; it records that these fixed community mapping/formula
variants do not pass the predeclared exact full-chain gates. No rounding,
factor, or tolerance was tuned to obtain a pass.
