# Issue #2 qfq rule/source review

Review date: 2026-09-11. This material records source findings only; it does
not change the production adapter or promote any evidence status.

## Source roles

### TongdaXin publisher material

- [TongdaXin formula function list](https://help.tdx.com.cn/gspt/docs/markdown/redword/functionlist.html)
  identifies `SPLIT`: type 0 is the stock/bonus ratio and type 1 is the
  per-share dividend (`dividend / 10`); it identifies `DIVFACTOR` as the
  ex-right factor and type 1 as forward adjustment. It also defines `VOL` as
  lots and `AMOUNT` as yuan for formula data.
- [TongdaXin Quant get_market_data](https://help.tdx.com.cn/quant/docs/markdown/mindoc-1ctuhthaq5qmg/mindoc-1h10g60jt68sc.html)
  defines `dividend_type=none/front/back`, says only `none` returns a valid
  `ForwardFactor`, and says returned OHLC is yuan, volume is the smallest
  volume unit, and amount is ten-thousand yuan. It also warns that a returned
  forward factor is carried from an ex-right event and is not directly the
  exact factor for that day.
- The two publisher surfaces describe amount units differently (formula
  function list: yuan; Quant `get_market_data`: ten-thousand yuan). This is
  an unresolved API-surface/unit distinction and is not silently normalized.
- These publisher pages do **not** state the complete cumulative qfq formula,
  the adjustment anchor date, or how a source annual OHLC is constructed from
  intra-year corporate actions. They therefore do not close the full source
  contract.

### eltdx author material and implementation

- Fixed release: [eltdx v3.1.7](https://github.com/electkismet/eltdx/releases/tag/v3.1.7),
  commit `086502a480600595a6fd407be38bc979eefd7c87`.
- [eltdx 7709 K-line documentation](https://github.com/electkismet/eltdx/blob/v3.1.7/docs/methods/7709-K%E7%BA%BF%E5%91%A8%E6%9C%9F%E7%BA%BF.md)
  maps `year/1y` to `11/1`, `qfq` to adjustment value `1`, and says the
  `0x052d` server directly returns none/qfq/hfq. It separately documents
  local `adjustment_factors()` calculation. This is the wrapper author's
  documentation of the protocol/client, not a TongdaXin original qfq contract.
- Installed `eltdx==3.1.7` `api/bars.py` passes `period`, `adjust`, `start`,
  `count`, and `include_raw` to the `klines` transport command. The path has no
  day-to-year aggregation. This establishes request/parse plumbing only.
- The independent [injoyai/tdx algorithm note](https://github.com/injoyai/tdx/blob/master/docs/gbbq_%E9%99%A4%E6%9D%83%E9%99%A4%E6%81%AF%E4%B8%8E%E5%A4%8D%E6%9D%83%E7%AE%97%E6%B3%95.md)
  describes an affine model and claims desktop alignment. It is useful as an
  author implementation hypothesis, not as original TongdaXin evidence.

## Public event inputs used for one conditional check

Target: `000001` / Ping An Bank, annual bar `2025-12-31`.

- [2024 annual distribution notice](https://file.finance.sina.com.cn/211.154.219.97%3A9494/MRGG/CNSESZ_STOCK/2025/2025-6/2025-06-05/11161352.PDF): ex-date 2025-06-12, cash 3.6200 yuan/10 shares = 0.3620/share, no bonus or transfer.
- [2025 interim distribution notice](https://static.cninfo.com.cn/finalpage/2025-09-30/1224693497.PDF): ex-date 2025-10-15, cash 2.3600 yuan/10 shares = 0.2360/share, no bonus or transfer.
- [2025 annual distribution notice](https://disc.static.szse.cn/disc/disk03/finalpage/2026-06-05/bde1eece-e914-4a31-a51d-5de619feb208.PDF): ex-date 2026-06-12, cash 3.6000 yuan/10 shares = 0.3600/share, no bonus or transfer.
- [Shenzhen Stock Exchange formula](https://www.szse.cn/www/investor/knowledge/t20181017_555756.html):
  `[(previous close - cash dividend) + rights price × share-change ratio] ÷ (1 + share-change ratio)`.

Under the cash-only hypothesis, the expected qfq offset for each daily price
before an ex-date is the sum of later cash dividends. The 2025 annual OHLC
must then be the extrema/close of date-varying adjusted daily prices, not one
uniform annual subtraction. The source does not disclose this annual
aggregation rule; this is why the check remains conditional.

## Conditional result

The one new target-sample request is recorded in
`event-reconciliation.json`; no raw payload or credential is stored. The
observed 2025 annual offsets (`-0.96/-0.60/-0.96/-0.36`) fit the conditional
event offsets (`-0.958/-0.596/-0.958/-0.360`) within an explicit ±0.005
half-cent display envelope. This is evidence consistent with date-segmented
cash adjustments, not proof of the TongdaXin qfq contract, full event history,
or universal annual OHLC semantics.
