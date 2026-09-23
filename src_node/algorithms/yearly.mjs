// Yearly natural-year aggregation — Node port of src/yearly.rs::aggregate.
//
// Semantics mirrored verbatim:
//   - validation order: identity/period/evidence -> empty input/selection ->
//     per-bar scan (year>0, date<=as_of, strictly increasing, OHLC bounds,
//     non-negative volume/amount) -> missing-year -> per-year sums
//   - natural-year contiguous groups (input is strictly ordered)
//   - first-open/max-high/min-low/last-close; date = actual last trading date
//   - year_unfinished = (year == as_of.year)
//   - sum_known: unit unconfirmed OR any None -> unknown (None) BEFORE any
//     overflow consideration; then exact checked_add chain — every partial
//     sum must round-trip (next-sum==value && next-value==sum) else
//     InexactSum. No silent rounding to fit.
//   - lineage: {base_daily_version_id, algorithm_version: local-natural-year-v1,
//     first_trading_date, last_trading_date}
//   - error text byte-identical to AggregationError variants

import { _dec, QfqError } from "./qfq_affine.mjs";

export const ALGORITHM_VERSION = "local-natural-year-v1";

export class AggregationError extends Error {
  constructor(message, kind, extra = {}) {
    super(message);
    this.name = "AggregationError";
    this.kind = kind; // "invalid_input" | "missing_year" | "inexact_sum"
    Object.assign(this, extra);
  }
}
const invalidInput = (reason) =>
  new AggregationError(`invalid fixed QFQ daily input: ${reason}`, "invalid_input", { reason });
const INEXACT = () =>
  new AggregationError("exact decimal sum is not representable", "inexact_sum");

const POW10 = (k) => 10n ** BigInt(k);
const decCmp = (x, y) => {
  const s = Math.max(x.s, y.s);
  const a = x.m * POW10(s - x.s), b = y.m * POW10(s - y.s);
  return a < b ? -1 : a > b ? 1 : 0;
};
const decEq = (x, y) => decCmp(x, y) === 0;
const decMax = (xs) => xs.reduce((a, b) => (decCmp(a, b) >= 0 ? a : b));
const decMin = (xs) => xs.reduce((a, b) => (decCmp(a, b) <= 0 ? a : b));

const confirmed = (e) => e != null && e.confirmed === true;
const ymdOf = (iso) => Number(iso.slice(0, 4) + iso.slice(5, 7) + iso.slice(8, 10));
const yearOf = (iso) => Number(iso.slice(0, 4));

function sumKnown(values, unitConfirmed) {
  if (!unitConfirmed) return null;
  if (values.some((v) => v === null)) return null;
  let sum = { m: 0n, s: 0 };
  for (const value of values) {
    let next;
    try {
      next = _dec.add(sum, value);
    } catch (e) {
      if (e instanceof QfqError && e.kind === "decimal_overflow") throw INEXACT();
      throw e;
    }
    let back1, back2;
    try {
      back1 = _dec.sub(next, sum);
      back2 = _dec.sub(next, value);
    } catch {
      throw INEXACT(); // checked_sub None -> inexact
    }
    if (!decEq(back1, value) || !decEq(back2, sum)) throw INEXACT();
    sum = next;
  }
  return sum;
}

/**
 * Aggregate a fixed, published QFQ daily EffectiveVersion into natural-year
 * records. `daily` mirrors the Rust fields used by aggregate():
 *   {period, yearly_derivation, version_id, symbol, source,
 *    adjustment_evidence:{confirmed,value}, unit_currency:{...evidence},
 *    records:[{date:"YYYY-MM-DD", open..close:Dec, volume:?Dec, amount:?Dec}]}
 * `years`: number[] | null (null = all; empty = reject); `asOf`: "YYYY-MM-DD".
 * Returns {symbol, source, base_daily_version_id, as_of, unit_currency, records}.
 */
export function aggregate(daily, years, asOf) {
  if (daily.period !== "day" || daily.yearly_derivation != null ||
      typeof daily.version_id !== "string" || daily.version_id.trim() === "" ||
      typeof daily.symbol !== "string" || daily.symbol.trim() === "" ||
      typeof daily.source !== "string" || daily.source.trim() === "" ||
      !confirmed(daily.adjustment_evidence) ||
      daily.adjustment_evidence.value !== "qfq" ||
      !confirmed(daily.unit_currency?.price_currency) ||
      !confirmed(daily.unit_currency?.price_unit)) {
    throw invalidInput("identity, period or evidence");
  }
  if (daily.records.length === 0 || (years !== null && years.length === 0)) {
    throw invalidInput("empty input or selection");
  }
  const asOfYmd = ymdOf(asOf);
  const asOfYear = yearOf(asOf);
  const groups = [];
  let previousYmd = null;
  daily.records.forEach((bar, index) => {
    const ymd = ymdOf(bar.date);
    if (yearOf(bar.date) <= 0 || ymd > asOfYmd ||
        (previousYmd !== null && previousYmd >= ymd) ||
        decCmp(bar.high, bar.low) < 0 || decCmp(bar.open, bar.low) < 0 ||
        decCmp(bar.open, bar.high) > 0 || decCmp(bar.close, bar.low) < 0 ||
        decCmp(bar.close, bar.high) > 0 ||
        (bar.volume !== null && bar.volume.m < 0n) ||
        (bar.amount !== null && bar.amount.m < 0n)) {
      throw invalidInput("invalid bar or date order");
    }
    previousYmd = ymd;
    const year = yearOf(bar.date);
    const last = groups[groups.length - 1];
    if (last && last.year === year) last.end = index + 1;
    else groups.push({ year, start: index, end: index + 1 });
  });
  const selected = years === null ? null : [...new Set(years)].sort((a, b) => a - b);
  if (selected !== null) {
    for (const year of selected) {
      if (!groups.some((g) => g.year === year)) {
        throw new AggregationError(
          `requested natural year is absent: ${year}`, "missing_year", { year });
      }
    }
  }
  const records = [];
  for (const { year, start, end } of groups) {
    if (selected !== null && !selected.includes(year)) continue;
    const bars = daily.records.slice(start, end);
    const first = bars[0], last = bars[bars.length - 1];
    records.push({
      bar: {
        amount: sumKnown(bars.map((b) => b.amount),
          confirmed(daily.unit_currency.amount_unit) &&
          confirmed(daily.unit_currency.amount_currency)),
        close: last.close,
        date: last.date,
        high: decMax(bars.map((b) => b.high)),
        low: decMin(bars.map((b) => b.low)),
        open: first.open,
        r7_raw: null,
        volume: sumKnown(bars.map((b) => b.volume),
          confirmed(daily.unit_currency.volume_unit)),
      },
      lineage: {
        algorithm_version: ALGORITHM_VERSION,
        base_daily_version_id: daily.version_id,
        first_trading_date: first.date,
        last_trading_date: last.date,
      },
      year_unfinished: year === asOfYear,
    });
  }
  return {
    as_of: asOf,
    base_daily_version_id: daily.version_id,
    records,
    source: daily.source,
    symbol: daily.symbol,
    unit_currency: daily.unit_currency,
  };
}
