// yearly.rs::aggregate_key + aggregate_memoized — verbatim port of the
// memo layer consumed by this module's validation paths (validate_base
// facts, runner re-aggregation). The memo lives at module scope like
// Rust's thread_local RefCell — same "compute once per key, 16384 cap
// with full clear" semantics.
import { createHash } from "node:crypto";

const MEMO = new Map();
const MEMO_CAP = 16_384;

/** aggregate_key: sha256 over the verbatim payload layout. */
export const aggregateKey = (baseDigestHex, years, asOf) => {
  const parts = [];
  parts.push(Buffer.from("x-data-aggregate-v1\0", "latin1"));
  parts.push(Buffer.from(baseDigestHex, "utf8"));
  parts.push(Buffer.from([0]));
  const b = Buffer.alloc(4);
  b.writeInt32BE(daysFromCe(asOf));
  parts.push(b);
  for (const y of years) {
    const p = Buffer.alloc(4);
    p.writeInt32BE(y);
    parts.push(p);
  }
  return createHash("sha256").update(Buffer.concat(parts)).digest();
};

// chrono NaiveDate::num_days_from_ce on "YYYY-MM-DD"
const daysFromCe = (s) => {
  const y = Number(s.slice(0, 4)), mo = Number(s.slice(5, 7)), d = Number(s.slice(8, 10));
  const a = Math.floor((14 - mo) / 12);
  const yy = y + 4800 - a;
  const mm = mo + 12 * a - 3;
  const jdn = d + Math.floor((153 * mm + 2) / 5) + 365 * yy +
    Math.floor(yy / 4) - Math.floor(yy / 100) + Math.floor(yy / 400) - 32045;
  return jdn - 1721425;
};

/** aggregate_memoized: cached Rc<AnnualSeries> — Node returns the plain
 *  shared object (single-threaded; Rc aliasing is observational only). */
export const aggregateMemoized = (key, compute) => {
  const hit = MEMO.get(key.toString("hex"));
  if (hit !== undefined) return hit;
  const series = compute();
  if (MEMO.size >= MEMO_CAP) MEMO.clear();
  MEMO.set(key.toString("hex"), series);
  return series;
};
