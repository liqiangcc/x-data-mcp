# Historical ST/risk-warning fixtures (PR-10)

Offline fixtures for `official_security::risk_warnings`. No test in this
directory performs an HTTP request.

| File | Shape | Provenance |
|---|---|---|
| `szse-name-changes.json` | SZSE `ShowReport` / `SSGSGMXX` / `tab2` envelope (`metadata` + `data`), four rows including one `200xxx` B share | Field names and envelope copied from the public 2026-10-30 response shape; names and dates are a short illustrative chain, not a market snapshot |
| `szse-name-changes-tab1-empty.json` | The same four rows in the real two-block shape: `tab1` with `data: []` and `pagecount=0` first, then `tab2` with the rows | The parser reads the tab the request declared (`TABKEY=tab2`); the empty `tab1` in front does not turn a whole-market table into zero changes |
| `sse-bulletins.json` | SSE `queryCompanyBulletinNew.do` `pageHelp` envelope, five flat index rows (`risk-warnings-v2` shape) | Field names copied from the public response shape; rows are illustrative: one implementation with an explicit “自…起” date, one revocation, one `*ST` implementation, one proposal that must be ignored, and one B-share row that must be filtered. Kept as the v2 replay case: the parser must still read a flat snapshot |
| `sse-bulletins-grouped.json` | The real `pageHelp.data` shape since XDM-151: each element is one announcement and its attachments (`[[doc,…],…]`), three groups / six documents | Trimmed from the 2026-11-01 public responses (`risk_warning_2022-04-01_2022-06-30_p1`, read-only forensics for XDM-151): group `8445752887795497` is an implementation plus its audit attachment, `8445752887518267` is a revocation, `8245752881083683` is an application with two attachments and must be ignored. URLs and ids are the publisher's, not a snapshot of the whole window |

The fixtures deliberately cover: an implementation/revocation chain, a
same-day conflict (unit test builds it from `SzNameChange`/events), an unknown
effective day (the parser falls back to `SSEDATE`; a missing `SSEDATE` is
`source_invalid`), a zero-row `unverified_zero` window (PR-2 failure fixture
already covers the envelope), and SH event-level rows coexisting with SZ
interval rows. `sse-bulletins-grouped.json` additionally covers the grouped
row shape (one announcement = body + attachments), which the flat fixture
deliberately does not: the flat shape was what let the v2 parser pass offline
while the live endpoint failed (XDM-151 §8).
