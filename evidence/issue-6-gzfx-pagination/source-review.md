# Pagination source-contract review

The #44 page wiring remains the source of truth for this bounded test:
`https://datacenter-web.eastmoney.com/api/data/v1/get`, report
`RPT_VALUEANALYSIS_DET`, `columns=ALL`, empty `quoteColumns`, `source=WEB`,
`client=WEB`, `sortColumns=SECURITY_CODE`, `sortTypes=1`, the exact page 1
market filters, and `pageNumber/pageSize` pagination. The corrected requests
changed only `pageNumber` and used `pageSize=5`.

## Preflight

`preflight.json` was generated with zero network requests. It parsed filters and
common parameters from `origin/main:evidence/issue-6-gzfx-source/` and compared
them with the merged #44 `probe.py`. Both filter diffs and common-parameter
diffs were empty. This specifically guards against the prior shell error where
`TRADE_DATE='2026-09-10'` was sent without its single quotes.

## Boundary observations

For both market filters, the reused page 1 `count/pages` satisfy the page-size
arithmetic, page 2 has five rows and no code overlap with page 1, and the
actual last page has exactly the row count derived from `count`. The next page
returns a successful HTTP transport response but business `success=false`,
`code=9201`, `返回数据为空`, with no `result` metadata. Rows preserve the
identity/industry fields `SECURITY_CODE`, `SECUCODE`, `TRADE_MARKET`,
`BOARD_CODE`, and `BOARD_NAME`.

All successful page responses had a different top-level `version` from page 1;
the out-of-range responses had no version. This is reported as an observation
only. A version difference is not treated as proof of a changed dataset, and a
version match would not prove a snapshot contract.

This gives evidence for the observed boundary and count arithmetic in two
bounded market-filtered samples. It does not establish that every page is
reachable, that pages remain stable during a traversal, that the counts are a
complete沪深 A-share directory, or that the displayed `BOARD_NAME` is a durable
and complete二级行业 contract. The earlier malformed six-request batch is
classified only as `operator_request_construction_error` and is excluded from
the source conclusion.
