// QFQ forward-adjust daily traversal — Node port of src/qfq.rs::forward_adjust.
//
// Semantics mirrored verbatim:
//   - events: caller supplies decoded records {date_raw, c_raw[4]}; factors are
//     normalized FIRST (fail-closed NonFiniteFactor), then sorted by date_raw
//     with a STABLE sort (Rust sort_by_key is stable — duplicate dates keep
//     input order, which matters for the compose order)
//   - for each bar, chain = compose over all events with event_date > bar_date
//     (strict), oldest->newest, no intermediate rounding
//   - prices = round_dp(apply(chain, cents/100), 2, MidpointAwayFromZero)
//   - amount_float32_bits / volume_raw / r7_raw pass through verbatim
//   - chain_event_count = number of strictly-later events
//   - out-of-order bar input is handled by a full partition fallback
//     (Rust does the same); .day parser output is already ascending
//   - output prices are Decimal to_string forms (contract strings)
//
// Scope: pure traversal only. Category filtering stays at the caller
// boundary as in Rust; no GBBQ byte decode, no service wiring.

import {
  factors, composeChain, applyAffine, decToString, _dec,
} from "./qfq_affine.mjs";

const HUNDRED = { m: 100n, s: 0 };
const rawPrice = (cents) => _dec.div({ m: BigInt(cents), s: 0 }, HUNDRED);

// "YYYY-MM-DD" -> yyyymmdd int (Rust `ymd` on NaiveDate).
function ymdInt(date) {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
  if (!m) throw new RangeError(`expected ISO date, got ${date}`);
  return Number(m[1]) * 10000 + Number(m[2]) * 100 + Number(m[3]);
}

// partition_point over dated[k0..]: count of entries with date <= barDate.
function partitionPoint(dated, from, barDate) {
  let lo = from, hi = dated.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (dated[mid][0] <= barDate) lo = mid + 1; else hi = mid;
  }
  return lo - from;
}

/**
 * Forward-adjust a .day bar sequence against decoded GBBQ events.
 * @param {{date:string, ohlc_raw:number[], amount_float32_bits:number,
 *          volume_raw:number, r7_raw:number}[]} bars — day_file.parse output
 * @param {{date_raw:number, c_raw:number[]}[]} events — caller-filtered set
 *        (category filtering is the caller's boundary, as in Rust)
 * @returns {object[]} QfqBar contract: date, open/high/low/close decimal
 *          strings, chain_event_count, untouched raw fields
 * @throws {QfqError} non_finite_factor (verbatim Rust error text)
 */
export function forwardAdjust(bars, events) {
  const dated = events.map((e) => [e.date_raw >>> 0, factors(e.c_raw)]);
  dated.sort((a, b) => a[0] - b[0]); // stable — mirrors sort_by_key

  const chains = new Array(dated.length + 1);
  const chainAt = (k) =>
    (chains[k] ??= composeChain(dated.slice(k).map(([, f]) => f)));

  let k = 0;
  let lastBarDate = 0;
  return bars.map((bar) => {
    const barDate = ymdInt(bar.date);
    k = barDate >= lastBarDate
      ? k + partitionPoint(dated, k, barDate)
      : partitionPoint(dated, 0, barDate);
    lastBarDate = barDate;
    const chain = chainAt(k);
    const [open, high, low, close] = bar.ohlc_raw.map(
      (cents) => decToString(_dec.roundAway(applyAffine(chain, rawPrice(cents)), 2)));
    return {
      amount_float32_bits: bar.amount_float32_bits,
      chain_event_count: dated.length - k,
      close,
      date: bar.date,
      high,
      low,
      open,
      r7_raw: bar.r7_raw,
      volume_raw: bar.volume_raw,
    };
  });
}
