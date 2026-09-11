# BaoStock qfq daily source review

## Scope and execution boundary

Executed 2026-09-11 Asia/Shanghai from a temporary `PYTHONPATH` using public PyPI distributions only. The fixed scope was `sh.600000` and `sz.000001`, daily frequency, `adjustflag=2` (BaoStock's documented forward-adjusted setting). No source-year request was made. The anonymous `baostock.login()` protocol handshake is recorded as a service handshake, not a user-account login.

The initial session used one bounded socket timeout of 8 seconds and no application retry. After connection errors on the long/current requests, each of those target observations was made once in a fresh anonymous session, also with an 8-second socket timeout. The fresh sessions distinguish the successful long-history response from the earlier connection-reuse failure; they are not duplicate fixed-window consistency samples.

## Observed response facts

All values below are observations from `probe-result.json`; numeric-looking fields remain strings in the JSON rows.

| Request | Result | Rows/pages | First..last date |
|---|---|---:|---|
| `sh.600000`, 2025-01-01..2026-09-10 | `error_code=0`, success | 411 / 1 | 2025-01-02..2026-09-10 |
| `sz.000001`, 2025-01-01..2026-09-10 | `error_code=0`, success | 411 / 1 | 2025-01-02..2026-09-10 |
| `sh.600000`, 2025-01-06..2025-01-10, repeat 1/2 | both success | 5 / 1 each | 2025-01-06..2025-01-10 |
| `sz.000001`, 2025-01-06..2025-01-10, repeat 1/2 | both success | 5 / 1 each | 2025-01-06..2025-01-10 |
| `sh.600000`, 1990-01-01..2026-09-10, fresh session | `error_code=0`, success | 6505 / 4 | 1999-11-10..2026-09-10 |
| `sz.000001`, 1990-01-01..2026-09-10, fresh session | `error_code=0`, success | 8651 / 5 | 1991-04-03..2026-09-10 |
| either symbol, 2026-09-11..2026-09-11, fresh session | `error_code=0`, success with zero rows | 0 / 0 | no date returned |

The initial same-session long-history and current-day attempts returned client error `10002007` (`网络接收错误`) after earlier successful calls. They are retained in the machine-readable result as failures, not interpreted as empty data. The fresh-session observations above are the bounded results for those target requests.

The requested fields were `date,code,open,high,low,close,volume,amount,adjustflag`. In successful rows:

- `code` matched the requested `sh.600000` or `sz.000001`.
- Every observed row carried row-level `adjustflag` text `"2"`.
- Observed OHLC text commonly had ten fractional digits; observed amount text had four fractional digits; volume text was integer-shaped. These are response-format observations, not permission to round or infer a stronger source precision contract.
- The client result metadata field `adjustflag` was empty in these responses, while the requested parameter and row-level field were `2`; this client/API metadata discrepancy remains visible in the evidence.
- The two small-window reads for each symbol were equal field-by-field and had equal parsed-response SHA-256. This supports repeatability for those reads at that time only; it is not a long-term guarantee.
- A successful zero-row response for 2026-09-11 does not prove that the market did not trade, that the source is complete, or that the current day is final.

## Official documentation evidence reused from Issue #2

The existing Issue #2 source review records the official BaoStock `stockKData.md` contract at [baostock.com](https://www.baostock.com/mainContent?file=stockKData.md): daily frequency `d` is listed, `adjustflag=2` is documented as forward adjustment, and the documented units are RMB yuan for OHLC/amount and shares for volume; the documentation states four decimal places for price and amount. It also warns that its adjustment method may differ from other systems and that suspended-day records can carry prior-close OHLC with zero volume.

This evidence reuses that already-recorded official documentation and does not claim that a documentation statement alone proves the live response's qfq basis, corporate-action treatment, or production authorization.

## Evidence-layered conclusion

- **PASS — bounded parameter/sample availability:** both fixed samples returned non-empty qfq daily rows with the requested symbols and row-level `adjustflag=2`; fresh long-range observations returned multiple bounded pages and actual first/last dates.
- **PASS — bounded repeat observation:** each sample's fixed small window matched exactly across two reads in the same session.
- **Documentation-supported, not a production approval:** the official documentation supplies the stated daily field/unit descriptions; the response text supplies the observed formatting. Unit/currency evidence should still remain subject to the repository's independent publish gate and source/version policy.
- **UNKNOWN — qfq basis and full corporate-action semantics:** `adjustflag=2` plus row differences/response availability does not independently prove the adjustment baseline, event treatment, or all historical revisions.
- **UNKNOWN — complete historical or market coverage:** the observed starts are `1999-11-10` and `1991-04-03` for these requests, not a claim of listing inception or complete history; no full-market request was made.
- **UNKNOWN — date-as-of/finality:** the last returned date and the zero-row 2026-09-11 query do not establish finality or a universal current-data rule.
- **UNKNOWN — long-term stability, licensing/authorization, production suitability, and Rust integration:** none was tested or authorized here.

The appropriate next step is separate review of whether this bounded daily evidence is sufficient for the repository's daily publish gate. The local yearly path remains the only production yearly path under the current Issue #2 contract; this evidence does not reintroduce a source-year requirement.
