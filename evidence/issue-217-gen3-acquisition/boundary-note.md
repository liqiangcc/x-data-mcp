# Issue #71 FILE_ACCESS_USE boundary decision

Operator: record the approved scope, frequency and manner for the
long-running server to acquire and use official files. Verbatim; no
inference.

## What was observed (facts only)

- `gbbq.zip` / `base.zip`: plain HTTPS GET to `www.tdx.com.cn`, 200,
  `application/zip`, no authentication, no cookie challenge.
- `hsjday.zip`: `data.tdx.com.cn` is behind TencentEdgeOne. The first
  request returned a 985-byte JS cookie challenge
  (`hsjday-challenge-response.html` preserved verbatim); retrying with
  the cookies a browser would compute returned the real 550MB zip with
  `EO-Cache-Status: HIT`. Subsequent fetches serve the zip directly.
- No license, terms-of-service, or rate-limit document was obtained
  that authorizes (or forbids) automated/long-running-server use of
  these endpoints. The TDX client itself shows an 扩展市场行情
  disclaimer on entering extended-market functions
  (`extended-market-disclaimer-2.png`), unrelated to file download.
- TQ (TdxQuant) access required the official client running + logged
  in; no separate credential or token is involved.

## Boundary status

**NOT OBTAINED.** Public downloadability ≠ authorization. This package
does not claim a #71 decision; it only records the access facts above.
