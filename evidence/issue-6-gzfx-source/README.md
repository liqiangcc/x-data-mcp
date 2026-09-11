# Eastmoney gzfx source-contract evidence

This evidence-only slice records one bounded, unauthenticated read of the public
`data.eastmoney.com/gzfx/list.html` path on 2026-09-11. It does not change Rust
code, claim full-directory coverage, or establish long-term availability.

## Static contract

- Page: `https://data.eastmoney.com/gzfx/list.html`
- Page script: `https://data.eastmoney.com/newstatic/js/gzfx/list.js`
- Production host selected by the script: `//datacenter-web.eastmoney.com/`
- Endpoint: `api/data/v1/get`
- Report: `RPT_VALUEANALYSIS_DET`
- Parameters: `columns=ALL`, `quoteColumns=`, `pageNumber=1`, `pageSize=5`,
  `sortColumns=SECURITY_CODE`, `sortTypes=1`, `source=WEB`, `client=WEB`.
- Page mapping: `result.pages` is the page-count field and `result.data` is the
  row field. The JS maps `pageNumber/pageSize/sortColumns/sortTypes` from the
  table control.
- Filters used by the page: `TRADE_DATE='2026-09-10'` plus the page's explicit
  Shanghai or Shenzhen `TRADE_MARKET` sets. The page's industry selector builds
  `(BOARD_NAME="<selected name>")`.

## Bounded responses

The two raw JSON responses are retained unchanged in this directory:

- `response-shanghai.json`: HTTP 200, `success=true`, `code=0`, `message=ok`,
  `result.count=2317`, `result.pages=464`, 5 returned rows.
- `response-shenzhen.json`: HTTP 200, `success=true`, `code=0`, `message=ok`,
  `result.count=2901`, `result.pages=581`, 5 returned rows.

The responses contain `SECURITY_CODE`, `SECUCODE`, `SECURITY_NAME_ABBR`,
`TRADE_MARKET`, `BOARD_CODE`, `BOARD_NAME`, and `TRADE_DATE`. The returned
sample includes `银行Ⅱ` in both markets and `航空机场` in Shanghai. The HTML
labels the column `所属行业`, lists names including `银行Ⅱ`, `航空机场`, and
`元件`, and states `数据来源：东方财富Choice数据`.

These facts establish a useful server-side candidate for small unauthenticated
catalog reads. They do not prove that `BOARD_NAME` is a stable/complete
二级行业 catalog contract, full coverage, pagination completeness, identity
continuity, or production authorization. Issue #6 AC-12 remains NOT RUN.
