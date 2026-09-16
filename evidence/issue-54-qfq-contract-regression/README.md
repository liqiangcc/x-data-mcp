# issue-54-qfq-contract-regression

全冻结矩阵契约回归 — the accepted #167 official-client QFQ contract, written as
an independent deterministic rule, regressed over the complete #159 frozen
event domain (94 events, four securities) + retained gen-2 `.day` members.

Ruling: FG comment-5690661520 — contract is ACCEPTED, no changes, no tuning.
Counterexamples are reported verbatim, never patched.

## Contract under test (frozen in plan.json)

- `C1`=每10股派现(元,含税); `C2`=配股价(元); `C4`=每10股配股数;
  `C3`=每10股送转数 (**indirect** — constrained only via the 000002 chain,
  caveat preserved)
- Input: each C field → `Decimal(str(float))` → `ROUND_HALF_UP` 2dp
- Event reference: `ref = (prev_close - C1/10 + C4*C2/10) / ((10+C3+C4)/10)`;
  displayed 昨结 = `ref_cent` (2dp HALF_UP)
- QFQ chain: `T_i(x) = (x - C1/10 + C4*C2/10) / ((10+C3+C4)/10)` composed
  oldest→newest over events with `event_date > bar_date`, no intermediate
  rounding, anchored at latest bar

## Oracles

- **#167 client transcriptions** (`observations.json`): 6 前复权 bars × OHLC +
  2 event-day 昨结 (600006 7.25, 600004 7.68). Mismatch here = contract failure.
- **#53 frozen BaoStock probe** (`baostock-valid-probe.json`): event-day
  `preclose` de-chained per in-window event (observed-only factor extraction);
  in-window OHLC per bar with `MODEL_DIVERGENCE_BAOSTOCK` attribution when the
  observed multiplicative-cent factor chain reproduces the source row.
- **000004 suspension placeholders**: BaoStock rows with O=H=L=C=preclose,
  volume/amount empty, no `.day` bar → `SUSPENSION_PLACEHOLDER` (#53 口径).

## Files

- `plan.json` — frozen contract, oracles, rules, status taxonomy, acceptance
- `scan_qfq_regression.py` — deterministic scanner (stdlib only)
- `test_scan_qfq_regression.py` — 8 unit tests
- `result.json` — generated (result commit)
- `determinism.json` — two clean-checkout runs, byte-identical
