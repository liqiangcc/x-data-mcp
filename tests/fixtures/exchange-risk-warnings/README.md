# Historical ST/risk-warning fixtures (PR-10)

Offline fixtures for `official_security::risk_warnings`. No test in this
directory performs an HTTP request.

| File | Shape | Provenance |
|---|---|---|
| `szse-name-changes.json` | SZSE `ShowReport` / `SSGSGMXX` / `tab2` envelope (`metadata` + `data`), four rows including one `200xxx` B share | Field names and envelope copied from the public 2026-10-30 response shape; names and dates are a short illustrative chain, not a market snapshot |
| `szse-name-changes-tab1-empty.json` | The same four rows in the real two-block shape: `tab1` with `data: []` and `pagecount=0` first, then `tab2` with the rows | The parser reads the tab the request declared (`TABKEY=tab2`); the empty `tab1` in front does not turn a whole-market table into zero changes |
| `sse-bulletins.json` | SSE `queryCompanyBulletinNew.do` `pageHelp` envelope, five index rows | Field names copied from the public response shape; rows are illustrative: one implementation with an explicit “自…起” date, one revocation, one `*ST` implementation, one proposal that must be ignored, and one B-share row that must be filtered |

The fixtures deliberately cover: an implementation/revocation chain, a
same-day conflict (unit test builds it from `SzNameChange`/events), an unknown
effective day (the parser falls back to `SSEDATE`; a missing `SSEDATE` is
`source_invalid`), a zero-row `unverified_zero` window (PR-2 failure fixture
already covers the envelope), and SH event-level rows coexisting with SZ
interval rows.
