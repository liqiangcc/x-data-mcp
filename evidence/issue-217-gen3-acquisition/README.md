# Issue #217 — gen-3 official snapshot acquisition + TQ oracle (Windows)

Executed the merged spec `docs/tasks/issue-217-gen3-acquisition.md` on
2026-09-20 with the official TDX V7.73 client at `C:\new_tdx64`.

## Contents

- `acquisition.json` — runner envelope: file metadata (URL, UTC
  timestamps, status, Last-Modified, ETag, bytes, sha256, full
  response headers, per-attempt records) + embedded
  `issue54.tq-oracle.v1` oracle (verbatim dates/values as served).
- `tq-raw/` — 21 verbatim `GetTdxDataStr` JSON responses (type=4
  dividend factors + type=1 kline `none`/`front` for all 7 frozen
  securities).
- `matrix.json` — `oracle_compare.py compare` output. DIAGNOSTIC_ONLY.
- `gen2-vs-gen3-frozendiff.json` — gen-3 gbbq member decoded with the
  pinned community decoder (source sha256 `e28fae76…` verified), frozen
  7-security event diff vs gen-2 oracle-inputs.
- `hsjday-challenge-response.html` — verbatim 985-byte TencentEdgeOne
  anti-bot interstitial returned on the first hsjday.zip request.
- `extended-market-disclaimer-2.png` — client 扩展市场行情 disclaimer
  observed during the session (#71 context).
- `environment.json`, `boundary-note.md`, `result.json`,
  `scripts/verify_issue217.py` (41 checks, all PASS).

## S1 file facts (gen-3 vs gen-2 pins — all changed)

| role | gen-3 sha256 | bytes | Last-Modified | gen-2 |
|---|---|---|---|---|
| gbbq.zip | `502236d9…63e3` | 6,337,960 | Fri, 18 Sep 2026 10:14:43 GMT | `daad6290…d0e9` 6,358,077B |
| hsjday.zip | `563797f0…c64` | 550,106,783 | Fri, 18 Sep 2026 07:58:50 GMT | `c82f852a…0892` ~549MB |
| base.zip | `56c35745…bec` | 15,963,181 | Fri, 18 Sep 2026 12:59:10 GMT | `dcdf6b49…8bc6` |

gbbq member: 206,034 records (sha256 `da33cb67…bf38`) vs gen-2 205,776
→ **+258 records**. Frozen-set diff: exactly one new event,
`sz.000001 @2026-09-24 cat1` — cross-validated by the TQ oracle, which
serves the same event (`bonus=2.49`). No removals, no field changes in
the frozen set. This is a real changed-generation input for the
changed-update acceptance path.

## S2 TQ oracle

All 7 securities captured (`generator.kind = tq_api`):

| symbol | divid events | none bars | front bars |
|---|---|---|---|
| sh.600004 | 22 | 5,632 | 5,632 |
| sh.600006 | 28 | 6,469 | 6,469 |
| sz.000001 | 33 | 8,457 | 8,457 |
| sz.000002 | 36 | 8,481 | 8,481 |
| sz.000004 | 0 | 0 | 0 (delisted — expected) |
| sz.000035 | 9 | 6,754 | 6,754 |
| sh.688318 | 8 | 1,552 | 1,552 |

Note: `sz.000001`/`sz.000035` initially returned only the current bar —
TQ serves klines from the client's local `vipdoc` cache. Opening each
security's daily chart and entering training mode (function 47) made
the client download full history (`.day` files appeared), after which
TQ served 8,457 / 6,754 bars.

## S3 consistency metadata

Each file carries URL/status/Last-Modified/ETag/bytes/sha256/headers —
but **no set identity or revision binding the three roles together**
was obtainable. Per `official-file-consistency-v1`, matching URLs or
dates do not establish compatibility → `FILE_SET_CONSISTENCY:
NOT_VERIFIED` (honest, not inferred).

## S4 boundary

`boundary-note.md`: observed access facts only; no authorization
evidence obtained → **NOT OBTAINED**, no #71 claim made.

## Runner fixes applied vs merged #224 version

`scripts/gen3_acquire.py` required four fixes to run honestly:
`norm_code` produced `SH600006` (API returns empty; official format is
`600006.SH`), `tq.initialize()` was never called, `count=0` did not
retrieve full history (front is response-range-sensitive), and raw
responses were only `repr()`-truncated — now verbatim `tq-raw/` files
plus preserved decimal strings. Added EdgeOne challenge handling for
data.tdx.com.cn (cookie replay, verbatim challenge retained).
