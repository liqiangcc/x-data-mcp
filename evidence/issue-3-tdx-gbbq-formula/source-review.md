# Source review and bounded result

## Fixed community sources

The fixed `pytdx` decoder uses the 29-byte encrypted-record structure and
decodes category 1 as four float32 values. Its comments identify reverse-
engineering blog sources, so this is community protocol evidence, not an
official TDX contract: [pytdx `gbbq_reader.py` at the fixed commit](https://github.com/wxyzZ/pytdx/blob/0d6f80618e1bae0f8e48552e597af492f3640f5d/pytdx/reader/gbbq_reader.py).

An independent implementation assigns the four category-1 values in order
as `Fenhong`, `Peigujia`, `Songzhuangu`, and `Peigu`; its model comments give
the same Chinese field meanings. The source and blob hashes are recorded in
`source-provenance.json`: [injoyai `model_gbbq.go` at the fixed commit](https://github.com/injoyai/tdx/blob/39b30f44477dad4a783921de5284b055bf347e88/protocol/model_gbbq.go).
This agreement supports `FIELD_MAPPING_COMMUNITY_CONSISTENT`, not official
semantic authorization.

The fixed mootdx sample computes:

```text
preclose=(prev_close*10-fenhong+peigu*peigujia)/(10+peigu+songzhuangu)
```

and derives an adjustment series from reverse cumulative
`next_preclose/current_close`: [mootdx `sample/fq.py` at the fixed commit](https://github.com/mootdx/mootdx/blob/e99ae34382d970c68654c6d17c45512e728f130d/sample/fq.py).
It is also community evidence, not an official TDX formula contract.

## One-event offline calculation

The self-contained `event-input.json` copies only the PR #49 decoded clear
record for `600000`, category `1`,
`2017-05-25` has float32 raw bits
`0x40000000, 0x00000000, 0x40400000, 0x00000000`, diagnostic values
`2.0, 0.0, 3.0, 0.0`. Applying the independently observed mapping gives:

The input records origin PR #49 HEAD
`2583d4606c12d91c4da7cf5a3686f53d3358f431`, source-file raw SHA256
`6664a14b997d367cc07761705e0e1eaab0751906d4064557e11ca009467c93f3`, and
canonical JSON SHA256
`c23b84e7d42cf750411cbd280de7c3a4ae9f7d3c0d798e1a769a3f4c46d4dfec`.
Only this event is copied, so a clean checkout of PR #50 does not require
PR #49.

```text
fenhong=2.0, peigujia=0.0, songzhuangu=3.0, peigu=0.0
previous close (2017-05-24) = 15.47
reference preclose = 11.746153846153846153846153846...
single-event ratio = 0.7592859629058724081348515737656...
```

The official `sh600000.day` records are retained only in `/tmp`; the
evidence records their raw hex and integer prices. The three rows are:

| date | open | high | low | close |
| --- | ---: | ---: | ---: | ---: |
| 2017-05-24 | 15.38 | 15.52 | 15.21 | 15.47 |
| 2017-05-25 | 11.75 | 12.93 | 11.72 | 12.93 |
| 2017-05-26 | 12.81 | 12.91 | 12.54 | 12.84 |

Using 2017-05-26 as a temporary local anchor, the pre-event open/close/
preclose values match the old BaoStock PDF at two-decimal display precision.
The exact Decimal values do not all match, and the local event ratio differs
from the PDF's displayed `0.759535`; this is `EVENT_FORMULA_NUMERICALLY_COMPATIBLE`
only within that explicitly bounded display-precision comparison.

## Relationship to existing BaoStock evidence

The existing PR #43 evidence reports current live qfq/no-adjust ratios of
`0.533312` on 2017-05-24 and `0.702156` on 2017-05-25/26. Those are not the
single-event ratio `0.7592859629...`; they reflect a different later/current
anchor. The old PDF's `0.759535` remains compatible only at its documented
display precision in the existing evidence and is not treated as the current
qfq truth. No later factor chain is inferred here.

## Layered conclusion and blockers

- `FIELD_MAPPING_COMMUNITY_CONSISTENT`
- `EVENT_FORMULA_NUMERICALLY_COMPATIBLE`
- `CURRENT_ANCHORED_QFQ_NOT_PROVEN`
- category numeric semantics, complete factor chain, all corporate actions,
  historical revisions, and `.day + gbbq` production sufficiency:
  `NOT_VERIFIED`

This one-event calculation does not prove current-anchored QFQ, all-history
coverage, or production correctness. It does not elevate community mappings
to an official TDX contract.
