# Source review: official TDX hsjday package

## Page and JavaScript contract

`vipdata.html` was fetched from `https://www.tdx.com.cn/article/vipdata.html`. The page itself contains the human label `沪深京日线数据完整包` and the literal protocol-relative link `//data.tdx.com.cn/vipdoc/hsjday.zip`; no path was guessed. Its inline load handler fetches `//data.tdx.com.cn/vipdoc/_hsjdayinfo.js`, then displays `HSJDAY_SOFT_SIZE` and `HSJDAY_SOFT_TIME`. The fetched JS said `523.76MB` and `2026-09-11 15:58:57`.

The page describes this as a personal-PC daily-data package and says it includes A/B shares, exchange/sector indexes, repurchases, funds and convertible bonds. That description is not treated as proof that every requested security or field is complete for the x-data-mcp production catalog.

## Download and package evidence

There was exactly one package request, to `https://data.tdx.com.cn/vipdoc/hsjday.zip`, with no credentials, cookies, retry, or parallel download. It returned HTTP 200, no redirect, `application/zip`, `549205235` bytes, Last-Modified `Fri, 11 Sep 2026 07:58:57 GMT`, ETag `"6aa3b4c1-20bc34f3"`, and SHA-256 `611ea3ccc1e64717aebc9a14ce9c8b1ed06526b5fd69bd14275fc376f4f44272`. `unzip -t` reported no errors. The archive has 12,406 `.day` members: 5,942 under `sh`, 5,855 under `sz`, and 609 under `bj`.

## Sample `.day` format boundary

The two actual target members have sizes `204416` and `270464`, both exact multiples of 32. Their records have ordered, valid integer dates from the expected listing-era dates through `2026-09-11`, and the final dates agree with the package update day. This supports a high-confidence fixed-width 32-byte candidate layout for this evidence sample.

For the sample summary only, the parser treats each 32-byte record as five little-endian 32-bit integer words (date and four OHLC words), one raw four-byte amount word, one little-endian volume word, and one trailing raw word. OHLC display strings use the integer `/100` rule and no float. The amount word is retained as raw hex/u32 only; it is **not** assigned a float type or unit from this page-only evidence. This is a structural sample interpretation, not a new production parser contract.

The first/last sample dates and raw records are machine-readable in `samples/day-samples.json`; complete raw sample files are intentionally not committed. No all-security records were parsed.

## What this resolves and what it does not

This evidence resolves the existence of a current, official, server-readable daily package, its exact download identity, package-level directory/member counts, and two concrete `.day` samples. It is useful for #3 daily-source investigation and may inform #6 coverage discussions, but directory membership is not identity validation or licensing.

It does not establish qfq adjustment, a factor chain, corporate-action semantics, unit/currency evidence, completeness across all listed securities, long-term stability, revision behavior, or production authorization. It does not make `.day + gbbq` sufficient for local qfq; the previously recorded opaque `gbbq` format blocker remains independent.
