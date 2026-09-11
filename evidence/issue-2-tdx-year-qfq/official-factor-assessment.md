# Official factor availability assessment

Review date: 2026-09-11

This assessment uses the existing evidence branch at `5fceca6ac590094dd72809e173682c7970eaabbc` and reuses the previously captured non-sensitive 000001/600000 `none`/`qfq` evidence. No new market-data probe was run.

## Official interface and boundary

| Question | Evidence | Conclusion |
| --- | --- | --- |
| Which official interface exposes a factor? | TDX Quant documents `tqcenter.get_market_data(field_list, stock_list, period, start_time, end_time, count, dividend_type, fill_data)`. Its documented fields include `ForwardFactor`; `dividend_type=none` is the mode that returns it. | This is a TDX Quant interface, not the 7709 request exposed by `eltdx`. |
| What is the documented factor basis? | The official Quant documentation says `ForwardFactor` is carried from an ex-right/ex-dividend event until the next event, is not the exact factor for that day, and cannot directly calculate a qfq price. | It is not sufficient by itself to claim a complete cumulative qfq formula or anchor. |
| What does 7709 prove? | The fixed `eltdx` v3.1.7 7709 notes document year as wire period `11/1`, qfq as adjustment `1`, and state that none/qfq/hfq are returned by the server. | This proves the eltdx adapter parameter mapping only; it does not turn the separate Quant `ForwardFactor` documentation into a 7709 contract. |
| Product and execution environment | TDX Quant examples initialize `tq` and require its market-data environment. The current probe environment contains `eltdx==3.1.7`, but `tqcenter` is not installed and no TDX Quant/desktop/account environment was found. | No official factor output can be obtained directly in the current Linux/eltdx environment. |

The original TDX public formula list separately documents `SPLIT` and `DIVFACTOR`, but it does not, in the material reviewed here, specify the complete cumulative qfq anchor or the annual OHLC construction used by 7709. The official Quant product's `front`/`ForwardFactor` terminology must therefore not be mapped to `eltdx` `adjust=qfq` or treated as proof of 7709 semantics.

## Correction to the prior reconciliation strength

The prior `±0.005` comparison envelope was an analysis assumption (a half-cent display-scale envelope), not a precision or tolerance stated by a TDX source. The reviewed official material mentions price units/formatting but does not establish the annual 7709 output rounding rule or an evidence-backed tolerance for this comparison.

The annual response did not provide the dates contributing the reported high and low. Those dates were not independently verified. The observed offsets and the event inputs can be numerically fitted to the simple cash-dividend subtraction hypothesis, but that is not validation of the qfq rule, its anchor, or annual high/low construction. The prior result is consequently downgraded to `HYPOTHESIS_FIT_ONLY`, not “condition consistent” evidence.

## Blocker and minimum next material

No authorized factor sample was available, so no factor was added and no production code or `unknown` value was changed. The smallest useful next material is one authorized TDX Quant official response/export for `000001` around one ex-right/ex-dividend event, using `dividend_type=none`, containing the raw OHLC plus `ForwardFactor`, with product/client version and the documented factor anchor/transition semantics. Separately, confirming annual OHLC requires an official rule or output that identifies the daily records contributing to annual open/high/low/close. Credentials and secrets must not be stored.

Sources reviewed:

- [TDX original formula function list](https://help.tdx.com.cn/gspt/docs/markdown/redword/functionlist.html)
- [TDX Quant `get_market_data` and `ForwardFactor` documentation](https://help.tdx.com.cn/quant/docs/markdown/mindoc-1ctuhthaq5qmg/mindoc-1h10g60jt68sc.html)
- [TDX Quant field/type dictionary](https://help.tdx.com.cn/quant/docs/markdown/Dict.html)
- [TDX Quant tdxpy notes](https://help.tdx.com.cn/quant/docs/markdown/mindoc-tdxpy.html)
- [eltdx v3.1.7 7709 notes](https://github.com/electkismet/eltdx/blob/v3.1.7/docs/methods/7709-K%E7%BA%BF%E5%91%A8%E6%9C%9F%E7%BA%BF.md)
