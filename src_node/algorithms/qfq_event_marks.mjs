// QFQ event-day marks — Node port of src/qfq.rs::event_day_marks.
//
// Semantics mirrored verbatim:
//   - bars are sorted ascending by date (stable sort)
//   - per event (INPUT ORDER preserved — events are NOT sorted):
//       factors normalized FIRST (fail-closed NonFiniteFactor fires even
//       when no prior bar exists — error priority verified against Rust)
//       previous = LAST bar strictly before event.date_raw (ymd < date_raw)
//       displayed_previous_close = cent(chain.apply(raw_close)) via
//       NM-013 displayedPreviousClose, or null
//   - output: {event_date, category, previous_bar_date, displayed_previous_close}
//     — date/category as raw u32/u8, decimal as contract string or null
//
// Typed boundary (REVIEW-NM-014 condition): all inputs validated as exact
// u32/u8/ISO-date BEFORE any arithmetic — malformed values reject instead
// of wrapping via `>>>0`.

import { factors, displayedPreviousClose, decToString, _dec, QfqError }
  from "./qfq_affine.mjs";

const HUNDRED = { m: 100n, s: 0 };
const rawPrice = (cents) => _dec.div({ m: BigInt(cents), s: 0 }, HUNDRED);

export class TypedBoundaryError extends Error {
  constructor(message) {
    super(message);
    this.name = "TypedBoundaryError";
    this.kind = "typed_boundary";
  }
}

const u32 = (v, field) => {
  if (!Number.isInteger(v) || v < 0 || v > 0xffffffff) {
    throw new TypedBoundaryError(`${field} must be u32 integer, got ${v}`);
  }
  return v;
};
const u8 = (v, field) => {
  if (!Number.isInteger(v) || v < 0 || v > 0xff) {
    throw new TypedBoundaryError(`${field} must be u8 integer, got ${v}`);
  }
  return v;
};

export const isoToYmd = (date) => {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(typeof date === "string" ? date : "");
  if (!m) throw new TypedBoundaryError(`bar date must be YYYY-MM-DD, got ${date}`);
  return Number(m[1]) * 10000 + Number(m[2]) * 100 + Number(m[3]);
};

const validateEvent = (e, i) => ({
  c_raw: e.c_raw,
  category: u8(e.category, `events[${i}].category`),
  date_raw: u32(e.date_raw, `events[${i}].date_raw`),
});
const validateBar = (b, i) => {
  if (!Array.isArray(b.ohlc_raw) || b.ohlc_raw.length !== 4 ||
      !b.ohlc_raw.every((v) => Number.isInteger(v) && v >= 0 && v <= 0xffffffff)) {
    throw new TypedBoundaryError(`bars[${i}].ohlc_raw must be four u32 values`);
  }
  return { date: b.date, ohlc_raw: b.ohlc_raw, ymd: isoToYmd(b.date) };
};

/**
 * For each caller-supplied decoded event, find the last raw bar strictly
 * before its event date and compute displayed previous close.
 * @param {{date:string, ohlc_raw:number[]}[]} bars — day_file.parse records
 * @param {{date_raw:number, category:number, c_raw:number[]}[]} events
 * @returns {{category:number, displayed_previous_close:?string,
 *            event_date:number, previous_bar_date:?string}[]} input order
 * @throws {QfqError} non_finite_factor (verbatim Rust text)
 * @throws {TypedBoundaryError} malformed date/count/price input
 */
export function eventDayMarks(bars, events) {
  if (!Array.isArray(bars) || !Array.isArray(events)) {
    throw new TypedBoundaryError("bars and events must be arrays");
  }
  const sorted = bars.map(validateBar).sort((a, b) => a.ymd - b.ymd); // stable
  return events.map((event, i) => {
    const { date_raw, category, c_raw } = validateEvent(event, i);
    const f = factors(c_raw); // fail-closed FIRST — error priority preserved
    let previous = null;
    for (let j = sorted.length - 1; j >= 0; j--) {
      if (sorted[j].ymd < date_raw) { previous = sorted[j]; break; }
    }
    return {
      category,
      displayed_previous_close: previous
        ? decToString(displayedPreviousClose(rawPrice(previous.ohlc_raw[3]), f))
        : null,
      event_date: date_raw,
      previous_bar_date: previous ? previous.date : null,
    };
  });
}
