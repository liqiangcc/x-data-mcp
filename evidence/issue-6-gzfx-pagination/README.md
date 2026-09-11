# Eastmoney gzfx pagination boundary evidence

This evidence-only slice is based on the merged #44 contract and the latest
`origin/main` at `7252fac4fef30c855fb328dab8a96c7a60bacff2`. It reuses #44 page 1
responses and records exactly six corrected, unauthenticated requests with
`pageSize=5`: Shanghai pages 2/464/465 and Shenzhen pages 2/581/582.

The zero-network preflight parsed the two filter strings from the merged
machine evidence and compared them with the #44 reproduction template. The
field diff was empty; the date retained single quotes and each market code in
`TRADE_MARKET in (...)` retained double quotes. Only `pageNumber` changed.

The previous six `9501` responses are retained only as an operator audit fact:
the shell had stripped the date quotes. They are not used as source behavior or
as the conclusion of this evidence.

## Corrected results

- Shanghai page 2: HTTP 200, `success=true`, `code=0`, `version=6611258b...`,
  5 rows (`600009` through `600015`).
- Shanghai page 464: HTTP 200, `success=true`, `code=0`, `version=1ae0209d...`,
  2 rows (`688981`, `689009`).
- Shanghai page 465: HTTP 200, `success=false`, `code=9201`, message `返回数据为空`,
  0 rows, no version.
- Shenzhen page 2: HTTP 200, `success=true`, `code=0`, `version=f1af2d17...`,
  5 rows (`000009` through `000014`).
- Shenzhen page 581: HTTP 200, `success=true`, `code=0`, `version=10050f468...`,
  1 row (`302132`).
- Shenzhen page 582: HTTP 200, `success=false`, `code=9201`, message `返回数据为空`,
  0 rows, no version.

For the reused page 1 counts, `ceil(2317/5)=464` and `ceil(2901/5)=581`.
The derived final-page row counts are 2 and 1, matching the actual final pages.
Page 2 had no `SECURITY_CODE` overlap with page 1 in either market. The
last-plus-one requests returned HTTP 200 with business empty-data `9201`; this
behavior is recorded, not assumed universally.

Page 2 and final-page response versions differed from their respective page 1
versions. This is only an observation; it is not interpreted as a snapshot
guarantee or as proof of data change. No full traversal, coverage assertion,
long-term stability, identity continuity, or licensing/production conclusion is
made. AC-12 remains NOT RUN.
