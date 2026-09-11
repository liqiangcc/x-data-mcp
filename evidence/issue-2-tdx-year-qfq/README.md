# Issue #2 TDX year/qfq evidence

Probe date: 2026-09-11, current server, UTC timestamps are in `probe-result.json`.
This is investigation material only. It does not alter the Rust adapter or any
`unknown`/`confirmed` evidence value.

> **Current interpretation correction (2026-09-11): HYPOTHESIS_FIT_ONLY.**
> Historical `CONDITIONAL`/`CONSISTENT` wording and the `±0.005` comparison in
> the files below record a numerical fit under an explicit assumption only.
> The tolerance is not source-defined, annual high/low dates were not verified,
> and the result must not be read as qfq validation or as permission to mark
> evidence `confirmed`. See `official-factor-assessment.md`.

## Fixed source material

- `eltdx==3.1.7`, release tag `v3.1.7`, commit
  `086502a480600595a6fd407be38bc979eefd7c87`:
  <https://github.com/electkismet/eltdx/releases/tag/v3.1.7>
- Publisher documentation at that fixed tag:
  <https://github.com/electkismet/eltdx/blob/v3.1.7/docs/methods/7709-K%E7%BA%BF%E5%91%A8%E6%9C%9F%E7%BA%BF.md>
  - describes `0x052d` K-line/period data and server-side adjustment;
  - maps `year / 1y` to underlying `11/1`;
  - maps `none`, `qfq`, `hfq` to adjustment values `0`, `1`, `2`;
  - states that none/qfq/hfq are returned directly by the `0x052d` server
    interface and distinguishes local `adjustment_factors()` calculation;
  - documents `volume_lots` as lots and `amount` as the decoded amount, while
    not establishing this project's complete currency/precision contract.
- The same facts are indexed in the fixed-tag method reference:
  <https://github.com/electkismet/eltdx/blob/v3.1.7/docs/METHOD_REFERENCE.md>
  (`client.bars.get` examples and period/adjustment parameter description).

## Client implementation boundary

The installed wheel came from the isolated probe environment and reports
`eltdx==3.1.7`. Its `api/bars.py` signature accepts `period`, `adjust`,
`count`, and `include_raw`; the single-page path passes these fields to the
`klines` command, and `api/base.py` passes the command/payload to transport.
This proves the client request mapping only. It is not independent proof that
the server's qfq values have the required adjustment basis, and it contains no
local day-to-year aggregation in this path.

## Probe method and result

`probe.py` made exactly four explicit calls using `TdxClient(timeout=8.0,
probe_timeout=1.2)`, `period="year"`, `count=50`, and `include_raw=True`:

- `sz000001` and `sh600000`;
- once with `adjust=None` (labelled `none`), once with `adjust="qfq"`;
- no application retry and no pagination;
- raw response bytes were hashed with SHA-256; raw payloads and credentials are
  not stored.

The JSON records request/receive UTC timestamps, client/source identity, wire
parameters, parsed period/adjustment metadata, bar count, first/last dates,
boundary OHLC, raw response hashes, and a bounded none-vs-qfq OHLC comparison.

Observed on this run:

- `000001`: both modes `period_name=year`, `period_raw=11`,
  `period_param_raw=1`, 36 bars, `1991-12-31..2026-09-10`; all 36 common-date
  OHLC rows differed.
- `600000`: the same metadata, 28 bars, `1999-12-30..2026-09-10`; all 28
  common-date OHLC rows differed.

## Evidence classification

- Parameter mapping: **PASS** against the fixed-tag publisher documentation and
  parsed response metadata.
- Sample retrieval: **PASS** for these four bounded calls; it is not coverage
  completeness.
- Native year versus local aggregation: **documented PASS at the client/API
  boundary**, but retain the source-side semantic caveat until independently
  reconcilable evidence is available.
- Qfq basis/company-action semantics: **UNKNOWN**. Different none/qfq values
  and an `adjust_mode=qfq` response are not by themselves proof of the
  adjustment basis.
- Coverage, unit/currency, and date-as-of: **UNKNOWN** under Issue #2/#3. A
  count limit, receive time, or non-empty response cannot promote them.
