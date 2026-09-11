# TDX GBBQ event and QFQ formula evidence

This is an evidence-only, offline analysis for Issue #3. It reuses the
already downloaded official `gbbq` member and `hsjday.zip`, plus the
committed BaoStock evidence from PR #43. The minimal decoded event input is
copied from PR #49 with its source commit and canonical/raw hashes recorded;
the full PR #49 evidence is not required. No TDX or market-data request was
made in this slice, and no production Rust code is changed.

The fixed event is `600000`, category `1`, dated `2017-05-25`. Two community
implementations agree on the category-1 field order used here:
`fenhong, peigujia, songzhuangu, peigu`. The decoded values are `2.0, 0.0,
3.0, 0.0`. The mootdx community formula gives a Decimal reference preclose
of `11.746153846153846153846153846...` and a single-event ratio of
`0.7592859629058724081348515737656...`.

Using `2017-05-26` as a temporary local anchor (not a current-anchor QFQ
calculation), the three pre-event open/close/preclose values agree with the
old PDF example at displayed two-decimal precision. Exact decimals do not
all agree, and the local event ratio is not the current BaoStock ratio.

Layered result:

- `FIELD_MAPPING_COMMUNITY_CONSISTENT`
- `EVENT_FORMULA_NUMERICALLY_COMPATIBLE` — bounded to this event and displayed
  two-decimal comparison only
- `CURRENT_ANCHORED_QFQ_NOT_PROVEN`
- category semantics and `.day + gbbq` production sufficiency remain
  `NOT_VERIFIED`

See `event-input.json` for the self-contained decoded event and provenance,
and `event-analysis.json` for raw record hex, integer prices, Decimal
calculations, and the comparison data. `analyze_event.py` is stdlib-only and
was run twice with identical output SHA256
`5a653a0fa4b30f2b3270448089b42b43758049ba8de965c153090efca2a2c98f`.
