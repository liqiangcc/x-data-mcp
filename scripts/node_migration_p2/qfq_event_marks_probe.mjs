// NM-017 P2: validate src_node/algorithms/qfq_event_marks.mjs against a
// live Rust oracle, plus typed-boundary coverage incl. the NM-014
// forward-adjust condition (now enforced + tested).
// Usage: node scripts/node_migration_p2/qfq_event_marks_probe.mjs <worktree-root>
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { eventDayMarks, TypedBoundaryError } from "../../src_node/algorithms/qfq_event_marks.mjs";
import { forwardAdjust } from "../../src_node/algorithms/qfq_forward.mjs";
import { QfqError } from "../../src_node/algorithms/qfq_affine.mjs";

const ROOT = process.argv[2] || ".";
const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/qfq_event_marks/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

const emit = (id, status, detail, expected = null, actual = null) =>
  console.log(JSON.stringify({ family: "qfq_event_marks", id, status, detail, expected, actual }));

const bar = (yyyymmdd, ohlc) => ({
  amount_float32_bits: 0,
  date: `${String(yyyymmdd).slice(0, 4)}-${String(yyyymmdd).slice(4, 6)}-${String(yyyymmdd).slice(6)}`,
  ohlc_raw: ohlc, r7_raw: 0x10000, volume_raw: 0,
});
const BARS = [bar(20240102, [1500, 1520, 1495, 1510]), bar(20240103, [1512, 1530, 1508, 1525]), bar(20240104, [1000, 1010, 990, 1005])];
const ev = (date_raw, category, c_raw) => ({ date_raw, category, c_raw });
const E1 = ev(20240104, 1, [2.0, 5.0, 0.0, 1.0]);

const CASES = {
  valid_single: [BARS, [E1]],
  no_prior_bar: [BARS, [ev(20240101, 1, [2.0, 5.0, 0.0, 1.0])]],
  event_between_bars: [BARS, [ev(20240103, 1, [2.0, 5.0, 0.0, 1.0])]],
  event_after_all: [BARS, [ev(20240110, 1, [2.0, 5.0, 0.0, 1.0])]],
  duplicate_dates: [BARS, [E1, ev(20240104, 2, [0.5, 0.0, 2.5, 0.0])]],
  unsorted_events_order_preserved: [BARS, [ev(20240110, 1, [2.0, 5.0, 0.0, 1.0]), ev(20240103, 2, [1.0, 0.0, 0.0, 0.0]), ev(20240101, 3, [0.5, 0.0, 0.0, 0.0])]],
  unsorted_bars: [[BARS[2], BARS[1], BARS[0]], [E1]],
  nonfinite_with_prior_bar: [BARS, [ev(20240104, 1, [NaN, 0, 0, 0])]],
  nonfinite_no_prior_bar: [BARS, [ev(20240101, 1, [0, NaN, 0, 0])]],
  empty_bars: [[], [E1]],
  empty_events: [BARS, []],
  nonfinite_index3: [BARS, [ev(20240104, 1, [0, 0, 0, Infinity])]],
  event_same_day_as_first_bar: [BARS, [ev(20240102, 1, [2.0, 5.0, 0.0, 1.0])]],
};
for (const o of oracle) {
  const [bars, events] = CASES[o.id];
  try {
    const actual = eventDayMarks(bars, events);
    const ok = o.status === "ok" && JSON.stringify(actual) === JSON.stringify(o.value);
    emit(o.id, ok ? "PASS" : "FAIL", ok ? null : "mismatch", o.value, actual);
  } catch (e) {
    const ok = o.status === "error" && e instanceof QfqError && e.message === o.error;
    emit(o.id, ok ? "PASS" : "FAIL", ok ? null : `got ${e.name}: ${e.message}`, o.error, e.message);
  }
}

// ---------- typed boundary: event_day_marks ----------
const bnd = (id, fn, wantKind) => {
  try { fn(); emit(id, "FAIL", "accepted malformed input"); }
  catch (e) { emit(id, e.kind === wantKind ? "PASS" : "FAIL", `${e.name}(${e.kind}): ${e.message}`); }
};
bnd("reject_negative_date", () => eventDayMarks(BARS, [ev(-1, 1, [0, 0, 0, 0])]), "typed_boundary");
bnd("reject_float_date", () => eventDayMarks(BARS, [ev(20240104.5, 1, [0, 0, 0, 0])]), "typed_boundary");
bnd("reject_string_date", () => eventDayMarks(BARS, [ev("20240104", 1, [0, 0, 0, 0])]), "typed_boundary");
bnd("reject_oversize_date", () => eventDayMarks(BARS, [ev(4294967296, 1, [0, 0, 0, 0])]), "typed_boundary");
bnd("reject_u8_category", () => eventDayMarks(BARS, [ev(20240104, 256, [0, 0, 0, 0])]), "typed_boundary");
bnd("reject_bad_ohlc", () => eventDayMarks([bar(20240102, [1500.5, 0, 0, 0])], [E1]), "typed_boundary");
bnd("reject_bad_bar_date", () => eventDayMarks([{ ...BARS[0], date: "2024-1-2" }], [E1]), "typed_boundary");
bnd("reject_nonarray_bars", () => eventDayMarks("x", [E1]), "typed_boundary");
// error priority: structural boundary checks run before factor checks
// (fail-closed either way — document precedence)
bnd("boundary_before_factors", () => eventDayMarks(BARS, [ev(-1, 1, [NaN, 0, 0, 0])]), "typed_boundary");

// ---------- NM-014 forward-adjust typed-boundary follow-up ----------
bnd("fwd_reject_negative_date", () => forwardAdjust(BARS, [ev(-1, 1, [0, 0, 0, 0])]), undefined);
bnd("fwd_reject_float_date", () => forwardAdjust(BARS, [ev(20240104.5, 1, [0, 0, 0, 0])]), undefined);
bnd("fwd_reject_string_date", () => forwardAdjust(BARS, [ev("20240104", 1, [0, 0, 0, 0])]), undefined);
bnd("fwd_reject_oversize_date", () => forwardAdjust(BARS, [ev(4294967296, 1, [0, 0, 0, 0])]), undefined);
bnd("fwd_reject_bad_ohlc", () => forwardAdjust([bar(20240102, [1500.5, 0, 0, 0])], [E1]), undefined);
// valid u32 boundary values still accepted
{
  const out = forwardAdjust(BARS, [ev(0, 1, [0, 0, 0, 0]), ev(4294967295, 1, [0, 0, 0, 0])]);
  emit("fwd_u32_edge_values_accept", Array.isArray(out) && out.length === 3 ? "PASS" : "FAIL", "u32 edge dates accepted");
  const out2 = eventDayMarks(BARS, [ev(0, 1, [0, 0, 0, 0])]);
  emit("marks_u32_edge_accept", out2.length === 1 && out2[0].event_date === 0 ? "PASS" : "FAIL", "date_raw=0 accepted");
}
