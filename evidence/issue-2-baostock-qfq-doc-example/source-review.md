# BaoStock qfq documentation-example review

## Scope and source flag check

The downloaded BaoStock 0.9.3 source was reviewed locally before the probe. `security/history.py` accepts and transmits `adjustflag` unchanged. The bundled official demo `demo/demo_A_stock_k_data.py` explicitly comments `adjustflag="2"` as 前复权 and `adjustflag="3"` as 默认不复权; another bundled demo uses `adjustflag="3"` for no adjustment. Therefore both requested flags were supported by existing local source evidence and no flag value was guessed.

Each mode used one fresh anonymous service handshake and one bounded query with an 8-second socket timeout. The query was:

```text
code=sh.600000
start_date=2017-05-24
end_date=2017-05-26
frequency=d
fields=date,code,open,high,low,close,preclose,volume,amount,adjustflag
```

No source-year request, application retry, user credential, system/virtual-environment installation, or full-market request was used.

## Official PDF comparison basis

The supplied BaoStock PDF is [BaoStock 复权因子简介](https://www.baostock.com/helpdocs/pdf/BaoStock%E5%A4%8D%E6%9D%83%E5%9B%A0%E5%AD%90%E7%AE%80%E4%BB%8B.pdf). The comparison uses only its stated `date/code/open/close/preclose` example values:

- no-adjust values for 2017-05-24, 25, 26;
- qfq values `11.681648/11.750007/11.719625` on 2017-05-24 and `11.75/12.93/11.75`, `12.81/12.84/12.93` on the following two dates.

The PDF's factor examples and narrative are documentation evidence. They are not silently treated as a live snapshot or as a production tolerance rule.

## Live response facts

Both sessions returned `error_code=0`, three rows, and the requested fields. Every row's code was `sh.600000`; qfq rows had row-level `adjustflag="2"`, and no-adjust rows had `adjustflag="3"`. The complete raw string rows and UTC/Asia-Shanghai timestamps are in `probe-result.json`.

The no-adjust `date/code/open/close/preclose` values were **live_string_exact** for all nine compared fields. The qfq values were:

| date | live qfq open / close / preclose | PDF open / close / preclose | classification |
|---|---|---|---|
| 2017-05-24 | `8.2023385600 / 8.2503366400 / 8.2290041600` | `11.681648 / 11.750007 / 11.719625` | all `inconsistent` |
| 2017-05-25 | `8.2503330000 / 9.0788770800 / 8.2503330000` | `11.75 / 12.93 / 11.75` | all `inconsistent` |
| 2017-05-26 | `8.9946183600 / 9.0156830400 / 9.0788770800` | `12.81 / 12.84 / 12.93` | all `inconsistent` |

No qfq value was text-equal, Decimal-equal with different formatting, or merely equal under a documentation-display rounding comparison. The mismatch is preserved; the probe does not infer its cause from this sample.

## Layered conclusion

- **No-adjust example:** live data reproduces the supplied official example exactly for the requested comparison fields and dates.
- **qfq example:** live API data does **not** reproduce the supplied PDF qfq values in this bounded run. This is a documented mismatch, not a PASS.
- **Adjustment evidence level:** the run confirms that BaoStock exposes the requested daily qfq mode and returns rows labelled `adjustflag=2`; it does not validate the qfq baseline or make the supplied PDF values a current live oracle. The qfq algorithm/baseline evidence therefore remains **UNKNOWN / unresolved**, subject to investigating snapshot/version or factor-context differences with new authorization and scope.
- A single three-day historical sample cannot establish all corporate-action handling, all historical revisions, coverage, or production correctness. Unit/currency, licensing/production authorization, and long-term stability remain UNKNOWN.
- Under the current product contract, the production yearly path remains local aggregation from a validated daily version; this evidence does not request or reinstate source-year data.
