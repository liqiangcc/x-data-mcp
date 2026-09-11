# Official TDX Shanghai/Shenzhen/Beijing daily package evidence

Evidence-only verification of the package linked by the official [TDX personal market-data page](https://www.tdx.com.cn/article/vipdata.html). No Rust/business code, database import, or full-market record parsing is included.

## Auditable source location

The page labels the row `沪深京日线数据完整包` and contains the literal link:

```html
<a href="//data.tdx.com.cn/vipdoc/hsjday.zip">
```

The same page loads `//data.tdx.com.cn/vipdoc/_hsjdayinfo.js`; the fetched JS returned:

```js
window.HSJDAY_SOFT_SIZE="523.76MB";
window.HSJDAY_SOFT_TIME="2026-09-11 15:58:57";
```

The page says the package is for the personal PC client, contains Shanghai/Shenzhen/Beijing daily data, and should be downloaded after the update date includes the desired day. Page/JS bodies and headers are preserved under `source-page/`.

## Single download

`https://data.tdx.com.cn/vipdoc/hsjday.zip` returned HTTP 200 without a redirect. The response had `Content-Type: application/zip`, `Content-Length: 549205235`, `Last-Modified: Fri, 11 Sep 2026 07:58:57 GMT`, and ETag `"6aa3b4c1-20bc34f3"`. Actual bytes were `549205235`; SHA-256 was:

`611ea3ccc1e64717aebc9a14ce9c8b1ed06526b5fd69bd14275fc376f4f44272`

The exact request/receive times are in `download/hsjday.download-meta`. The ZIP passed `unzip -t`; the complete 12,406-member manifest with compressed/uncompressed sizes, CRCs, and member timestamps is `download/hsjday.members.json`.

## Package-level statistics

All 12,406 members have the `.day` extension. Uncompressed/compressed totals by actual top-level directory:

| directory | members | compressed bytes | uncompressed bytes |
|---|---:|---:|---:|
| `sh` | 5,942 | 287,314,739 | 485,122,496 |
| `sz` | 5,855 | 252,221,937 | 449,748,256 |
| `bj` | 609 | 8,229,441 | 14,925,888 |

This is direct evidence of package membership/counts, not proof of complete exchange coverage or production authorization.

## Two samples

The actual members were `sh\\lday\\sh600000.day` and `sz\\lday\\sz000001.day`.

| sample | bytes | records | first date | last date | SHA-256 |
|---|---:|---:|---|---|---|
| `sh600000.day` | 204,416 | 6,388 | 1999-11-10 | 2026-09-11 | `bc6815ceafa9cdcd24c68df4d8ec43accfc3e724f5d34d2c7d827a5b12ccac8f` |
| `sz000001.day` | 270,464 | 8,452 | 1991-04-03 | 2026-09-11 | `8db365d39beaa0e2178717adcdc1e411f63295f87c647b8bf6aa56d9762dfc7b` |

Both lengths divide exactly by 32. The sample parser preserves raw record bytes, integer date/OHLC/volume words, and the raw amount word. It renders the OHLC integer words as `/100` decimal strings without floating point; it deliberately does not decode the amount word because the fetched official page/package does not publish that field schema. Full first/last raw records are in `samples/day-samples.json`.

The package is a strong technical candidate for a daily-file input and materially improves the #3 source-location/coverage evidence. It does not prove qfq, factor semantics, unit/currency evidence, revision behavior, licensing, or production readiness. The separate `gbbq` parsing blocker remains unchanged.
