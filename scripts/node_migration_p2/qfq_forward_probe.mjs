// NM-014 P2: validate src_node/algorithms/qfq_forward.mjs against accepted
// P0 qfq_forward_adjust goldens + rust-oracle edge cases.
// Usage: node scripts/node_migration_p2/qfq_forward_probe.mjs <worktree-root>
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { forwardAdjust } from "../../src_node/algorithms/qfq_forward.mjs";
import { QfqError } from "../../src_node/algorithms/qfq_affine.mjs";

const ROOT = process.argv[2] || ".";
const vectors = readFileSync(join(ROOT, "evidence/node-migration/p0/numeric/vectors.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse)
  .filter((r) => r.family === "qfq_forward_adjust");
const rustOracle = readFileSync(join(ROOT, "evidence/node-migration/p2/qfq_forward/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

const emit = (id, status, detail, expected = null, actual = null) =>
  console.log(JSON.stringify({ family: "qfq_forward_adjust", id, status, detail, expected, actual }));

// bars matching the P0 oracle's raw_rec calls
const bar = (yyyymmdd, ohlc, amount = 0, vol = 0, r7 = 0x10000) => ({
  amount_float32_bits: amount,
  date: `${String(yyyymmdd).slice(0, 4)}-${String(yyyymmdd).slice(4, 6)}-${String(yyyymmdd).slice(6)}`,
  ohlc_raw: ohlc,
  r7_raw: r7,
  volume_raw: vol,
});
const BARS = [
  bar(20240102, [1500, 1520, 1495, 1510], 0, 1000),
  bar(20240103, [1512, 1530, 1508, 1525], 0, 2000),
  bar(20240104, [1000, 1010, 990, 1005], 0, 3000),
];
const ev = (date_raw, c_raw) => ({ date_raw, c_raw });

const G5 = (r) => ({ chain_event_count: r.chain_event_count, close: r.close, date: r.date, high: r.high, low: r.low, open: r.open });
const FULL = (r) => ({ amount_float32_bits: r.amount_float32_bits, chain_event_count: r.chain_event_count, close: r.close, date: r.date, high: r.high, low: r.low, open: r.open, r7_raw: r.r7_raw, volume_raw: r.volume_raw });

// ---------- golden vectors ----------
for (const v of vectors) {
  if (v.id === "one_dividend_event") {
    const actual = forwardAdjust(BARS, [ev(v.input.event_date, v.input.c_raw)]).map(G5);
    const ok = JSON.stringify(actual) === JSON.stringify(v.observed.value);
    emit(v.id, ok ? "PASS" : "FAIL", ok ? null : "mismatch", v.observed.value, actual);
  } else if (v.id === "empty_events_identity") {
    const actual = forwardAdjust(BARS, []).map((r) => r.close);
    const ok = JSON.stringify(actual) === JSON.stringify(v.observed.value);
    emit(v.id, ok ? "PASS" : "FAIL", ok ? null : "mismatch", v.observed.value, actual);
  } else if (v.id === "nan_factor_reject") {
    try {
      forwardAdjust(BARS, [ev(20240104, v.input.c_raw.map((x) => x === "NaN" ? NaN : x))]);
      emit(v.id, "FAIL", "accepted NaN factor");
    } catch (e) {
      const ok = e instanceof QfqError && e.message === v.observed.error;
      emit(v.id, ok ? "PASS" : "FAIL", ok ? null : `wrong error: ${e.message}`, v.observed.error, e.message);
    }
  }
}

// ---------- rust-oracle edge cases (live rustc + real rlib) ----------
const orc = Object.fromEntries(rustOracle.map((r) => [r.id, r]));
const e1 = ev(20240104, [2.0, 5.0, 0.0, 1.0]);
const e2 = ev(20240103, [0.5, 0.0, 2.5, 0.0]);
// field-order-independent compare (oracle rows use a different key order)
const canon = (o) => JSON.stringify(Object.fromEntries(Object.keys(o).sort().map((k) => [k, o[k]])));
const check = (id, actual) => {
  const want = orc[id].value;
  const ok = JSON.stringify(actual.map((o) => canon(o))) ===
             JSON.stringify(want.map((o) => canon(o)));
  emit(id, ok ? "PASS" : "FAIL", ok ? null : "mismatch", want, actual);
};
check("unsorted_events", forwardAdjust(BARS, [e1, e2]).map(FULL));
check("duplicate_event_dates", forwardAdjust(BARS, [e2, ev(20240103, [1.0, 0.0, 0.0, 0.0])]).map(FULL));
check("event_no_same_day_bar", forwardAdjust(BARS, [ev(20240105, [2.0, 5.0, 0.0, 1.0])]).map(FULL));
check("event_before_all_bars", forwardAdjust(BARS, [ev(20240101, [2.0, 5.0, 0.0, 1.0])]).map(FULL));
check("event_after_all_bars", forwardAdjust(BARS, [ev(20240110, [2.0, 5.0, 0.0, 1.0])]).map(FULL));
check("unordered_bars", forwardAdjust([BARS[2], BARS[1], BARS[0]], [e1]).map(FULL));
check("two_events_two_bars", forwardAdjust(BARS, [e2, e1]).map(FULL));
check("raw_fields_passthrough", forwardAdjust([bar(20240102, [1500, 1520, 1495, 1510], 0x4d0f4406, 999999, 0xdeadbeef)], [e1]).map(FULL));

// ---------- structural/fail-closed edges ----------
try {
  forwardAdjust(BARS, [ev(20240104, [NaN, 0, 0, 0]), e1]);
  emit("nonfinite_among_valid_reject", "FAIL", "accepted NaN among valid events");
} catch (e) {
  emit("nonfinite_among_valid_reject",
    e.message === "GBBQ factor 0 is not a finite decimal input" ? "PASS" : "FAIL", e.message);
}
try {
  forwardAdjust(BARS, [ev(20240104, [0, 0, 0, NaN])]);
  emit("nonfinite_factor_index3", "FAIL", "accepted NaN at index 3");
} catch (e) {
  emit("nonfinite_factor_index3",
    e.message === "GBBQ factor 3 is not a finite decimal input" && e.index === 3 ? "PASS" : "FAIL", e.message);
}
try {
  forwardAdjust([{ ...BARS[0], date: "not-a-date" }], []);
  emit("invalid_bar_date_reject", "FAIL", "accepted malformed date");
} catch (e) {
  emit("invalid_bar_date_reject", e instanceof RangeError ? "PASS" : "FAIL", `${e.name}: ${e.message}`);
}
emit("empty_bars", JSON.stringify(forwardAdjust([], [e1])) === "[]" ? "PASS" : "FAIL", "empty input -> []");
