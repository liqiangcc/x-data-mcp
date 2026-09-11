# Static source review

## Pages and backend wiring

The fetched HTML pages were HTTP 200 on 2026-09-11. `list.html` contains the
`股票代码`, `股票简称`, and `所属行业` table headers, an `#filter_hy` selector
with values such as `航空机场`, `银行Ⅱ`, and `元件`, and the footer text
`数据来源：东方财富Choice数据`.

`newstatic/js/gzfx/list.js` is the relevant list-page bundle. Its production
configuration selects `//datacenter-web.eastmoney.com/`; the `gggzTable.loadData`
configuration uses `api/data/v1/get`, `reportName: "RPT_VALUEANALYSIS_DET"`,
`columns: "ALL"`, `quoteColumns: ""`, `source: "WEB"`, and `client: "WEB"`.
The table maps `result.pages` and `result.data`, and maps table controls to
`pageNumber`, `pageSize`, `sortColumns`, and `sortTypes`.

The same bundle defines the market filters exactly as:

```text
sh: (TRADE_MARKET in ("069001001001","069001001006","069001001003"))
sz: (TRADE_MARKET in ("069001002001","069001002002","069001002005"))
```

Its industry filter turns the selected HTML `data-value` into
`(BOARD_NAME="<value>")`. The displayed row mapping includes
`TRADE_DATE`, `SECURITY_CODE`, `SECURITY_NAME_ABBR`, and `BOARD_NAME`; returned
rows also provide `SECUCODE`, `TRADE_MARKET`, and `BOARD_CODE`.

The page supplies a fixed default `gzDate` of `2026-09-10` in the fetched HTML.
The probe used that page-provided date and did not infer a different date.

## Request result and limits

Exactly two new data requests were made, directly to the endpoint identified by
the bundle, without cookies or credentials. They used `pageNumber=1` and
`pageSize=5`, one with the page's Shanghai filter and one with its Shenzhen
filter. Both returned HTTP 200 and JSON `success=true`, `code=0`, `message=ok`.
The raw requests, metadata, and response hashes are in `probe-result.json` and
the two `response-*.json` files.

The 5-row samples prove that this server currently returns security identity
fields and industry fields for both market filters. They do not prove that the
list is a complete current沪深 A-share directory, that `BOARD_NAME` is formally
the required Eastmoney secondary-industry taxonomy in a durable API contract,
that all pages can be read, or that records remain stable over time. No paging
loop was run. The old pushguest/push2 route was not used.

Therefore this is a promising #6 source candidate for a bounded follow-up, not
an AC-12 real-source/full-directory acceptance or a production source approval.
