# Security name-history fixtures (PR-12, S-6)

Offline fixtures for `official_security::security_names`. No test in this
directory performs an HTTP request. They are parsed by the SZSE row parser
PR-10 already owns (`official_security::risk_warnings::parse_sz_page`); this
directory adds rows, not a second parser.

| File | Shape | What it proves |
|---|---|---|
| `szse-chain.json` | SZSE `ShowReport` / `SSGSGMXX` / `tab2` envelope (`metadata` + `data`), four rows: a three-change chain for `000004` plus one `200xxx` B-share row | A change chain becomes two closed intervals and one `unbounded` last interval; the B-share row is skipped, not turned into a name |
| `szse-chain-tab1-empty.json` | The same four rows in the real two-block shape: `tab1` (暂停上市) with `data: []` and `pagecount=0` first, then `tab2` (简称变更) with the rows | Replay reads the block the request declared (`TABKEY=tab2`), not the first block with a `data` array: the empty `tab1` no longer rebuilds zero rows |
| `szse-xd-xr.json` | Three rows: an `XD` marker row for `000002`, a real rename for `000002`, and an `XR` marker row for `000001` | XD/XR prefixes are normalised before a name lands; a marker-only row links the chain and opens no interval |
| `szse-same-day.json` | Two changes for one code on one day | Same-day multiple changes fail closed (`source_invalid`), no row is written |
| `szse-empty.json` | Envelope with `data: []` and `recordcount: 0` | A zero-row table is *not* an empty name history: zero rows are assembled with only the `not_observed` windows, and the live item refuses to write them |

Field names and the envelope shape are copied from the public 2026-10-30
response (the same shape PR-10 froze); names and dates are a short illustrative
chain, not a market snapshot.
