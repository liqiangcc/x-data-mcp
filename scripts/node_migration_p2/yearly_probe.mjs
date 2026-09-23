// NM-019 P2: validate src_node/algorithms/yearly.mjs against accepted P0
// yearly_aggregate goldens + a live Rust oracle edge matrix.
// Usage: node scripts/node_migration_p2/yearly_probe.mjs <worktree-root>
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { aggregate, AggregationError } from "../../src_node/algorithms/yearly.mjs";
import { _dec } from "../../src_node/algorithms/qfq_affine.mjs";

const ROOT = process.argv[2] || ".";
const vectors = readFileSync(join(ROOT, "evidence/node-migration/p0/numeric/vectors.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse).filter((r) => r.family === "yearly_aggregate");
const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/yearly/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

const emit = (id, status, detail, expected = null, actual = null) =>
  console.log(JSON.stringify({ family: "yearly_aggregate", id, status, detail, expected, actual }));

const dec = _dec.fromStr;
const ev = (value) => ({ confirmed: true, value });
const UNKNOWN = { confirmed: false, value: null };
const bar = (date, o, h, l, c, v, a) => ({
  amount: a, close: c, date, high: h, low: l, open: o, r7_raw: null, volume: v,
});
const version = (records, overrides = {}) => ({
  adjustment_evidence: ev("qfq"),
  period: "day",
  records,
  source: "oracle:synthetic",
  symbol: "600006",
  unit_currency: {
    amount_currency: ev("cny"), amount_unit: ev("yuan"),
    price_currency: ev("cny"), price_unit: ev("yuan"),
    volume_unit: ev("share"),
  },
  version_id: "oracle-daily-v1",
  yearly_derivation: null,
  ...overrides,
});
const fourBars = () => version([
  bar("2023-01-03", dec("10"), dec("11"), dec("9.5"), dec("10.5"), dec("100"), dec("1050")),
  bar("2023-06-30", dec("10.5"), dec("12"), dec("10.4"), dec("11.8"), dec("200"), dec("2360")),
  bar("2024-01-02", dec("11.8"), dec("12.5"), dec("11.6"), dec("12.3"), dec("150"), dec("1845")),
  bar("2024-03-28", dec("12.3"), dec("13"), dec("12.2"), dec("12.9"), dec("180"), dec("2322")),
]);
const recRow = (r) => ({
  amount: r.bar.amount === null ? null : _dec.toString(r.bar.amount),
  close: _dec.toString(r.bar.close),
  date: r.bar.date,
  first_trading_date: r.lineage.first_trading_date,
  high: _dec.toString(r.bar.high),
  last_trading_date: r.lineage.last_trading_date,
  low: _dec.toString(r.bar.low),
  open: _dec.toString(r.bar.open),
  volume: r.bar.volume === null ? null : _dec.toString(r.bar.volume),
  year: Number(r.bar.date.slice(0, 4)),
  year_unfinished: r.year_unfinished,
});

const MAX = dec("79228162514264337593543950335");
const ORACLE_CASES = {
  all_years: [fourBars(), null, "2024-06-01"],
  selected_year_2024: [fourBars(), [2024], "2024-06-01"],
  selected_both: [fourBars(), [2023, 2024], "2024-06-01"],
  empty_selection: [fourBars(), [], "2024-06-01"],
  as_of_before_bars: [fourBars(), null, "2020-01-01"],
  as_of_within_2023: [fourBars(), null, "2023-06-30"],
  adjustment_unknown: [version(fourBars().records, { adjustment_evidence: UNKNOWN }), null, "2024-06-01"],
  adjustment_not_qfq: [version(fourBars().records, { adjustment_evidence: ev("none") }), null, "2024-06-01"],
  volume_unit_unknown: [version(fourBars().records, { unit_currency: { ...fourBars().unit_currency, volume_unit: UNKNOWN } }), null, "2024-06-01"],
  amount_unit_unknown: [version(fourBars().records, { unit_currency: { ...fourBars().unit_currency, amount_unit: UNKNOWN } }), null, "2024-06-01"],
  open_above_high: [version(fourBars().records.map((b, i) => i === 0 ? { ...b, open: dec("11.5") } : b)), null, "2024-06-01"],
  negative_volume: [version(fourBars().records.map((b, i) => i === 0 ? { ...b, volume: dec("-1") } : b)), null, "2024-06-01"],
  duplicate_date: [version(fourBars().records.map((b, i) => i === 1 ? { ...b, date: "2023-01-03" } : b)), null, "2024-06-01"],
  empty_records: [version([]), null, "2024-06-01"],
  sum_exact_max: [version([
    bar("2023-01-03", dec("10"), dec("11"), dec("9"), dec("10"), null, dec("39614081257132168796771975167")),
    bar("2023-06-30", dec("10"), dec("11"), dec("9"), dec("10"), null, _dec.sub(MAX, dec("39614081257132168796771975167"))),
  ]), null, "2024-06-01"],
  sum_precision_loss: [version([
    bar("2023-01-03", dec("10"), dec("11"), dec("9"), dec("10"), null, MAX),
    bar("2023-06-30", dec("10"), dec("11"), dec("9"), dec("10"), null, dec("0.0000000000000000000000000001")),
  ]), null, "2024-06-01"],
  negative_amount: [version(fourBars().records.map((b, i) => i === 0 ? { ...b, amount: dec("-0.01") } : b)), null, "2024-06-01"],
  none_volume_propagates: [version(fourBars().records.map((b, i) => i === 1 ? { ...b, volume: null } : b)), null, "2024-06-01"],
  blank_version_id: [version(fourBars().records, { version_id: "  " }), null, "2024-06-01"],
  already_yearly: [version(fourBars().records, { yearly_derivation: { algorithm_version: "local-natural-year-v1", base_daily_version_id: "x", first_trading_date: "2023-01-03", last_trading_date: "2023-12-29" } }), null, "2024-06-01"],
};

// ---------- golden vectors ----------
const goldenRow = (r) => ({
  amount: r.bar.amount === null ? null : _dec.toString(r.bar.amount),
  close: _dec.toString(r.bar.close), high: _dec.toString(r.bar.high),
  low: _dec.toString(r.bar.low), open: _dec.toString(r.bar.open),
  volume: r.bar.volume === null ? null : _dec.toString(r.bar.volume),
  year: Number(r.bar.date.slice(0, 4)), year_unfinished: r.year_unfinished,
});
const MAX4 = version(fourBars().records.map((b) => ({ ...b, amount: MAX })));
const unsorted = version(fourBars().records.map((b, i) => i === 0 ? fourBars().records[1] : i === 1 ? fourBars().records[0] : b));
const noneAmt = version(fourBars().records.map((b, i) => i === 1 ? { ...b, amount: null } : b));
const GOLD = {
  two_years_valid: [fourBars(), null, "2024-06-01"],
  missing_year_reject: [fourBars(), [2025], "2024-06-01"],
  none_amount_propagates: [noneAmt, null, "2024-06-01"],
  inexact_sum_overflow: [MAX4, null, "2024-06-01"],
  unsorted_reject: [unsorted, null, "2024-06-01"],
};
for (const v of vectors) {
  const [daily, years, asOf] = GOLD[v.id];
  try {
    const s = aggregate(daily, years, asOf);
    const rows = v.id === "none_amount_propagates"
      ? s.records.map((r) => ({ amount: r.bar.amount === null ? null : _dec.toString(r.bar.amount), year: Number(r.bar.date.slice(0, 4)) }))
      : s.records.map(goldenRow);
    const ok = JSON.stringify(rows) === JSON.stringify(v.observed.value);
    emit(v.id, ok ? "PASS" : "FAIL", ok ? null : "mismatch", v.observed.value, rows);
  } catch (e) {
    const ok = e instanceof AggregationError && e.message === v.observed.error;
    emit(v.id, ok ? "PASS" : "FAIL", ok ? null : `got ${e.message}`, v.observed.error, e.message);
  }
}

// ---------- rust-oracle cases ----------
const canon = (v) => JSON.stringify(v, (k, x) =>
  x && typeof x === "object" && !Array.isArray(x)
    ? Object.fromEntries(Object.keys(x).sort().map((kk) => [kk, x[kk]])) : x);
for (const o of oracle) {
  const [daily, years, asOf] = ORACLE_CASES[o.id];
  try {
    const s = aggregate(daily, years, asOf);
    const actual = {
      algorithm_version: s.records[0]?.lineage.algorithm_version ?? "",
      as_of: s.as_of, base_daily_version_id: s.base_daily_version_id,
      records: s.records.map(recRow), source: s.source, symbol: s.symbol,
    };
    const ok = o.status === "ok" && canon(actual) === canon(o.series);
    emit(o.id, ok ? "PASS" : "FAIL", ok ? null : "mismatch", o.series, actual);
  } catch (e) {
    const ok = o.status === "error" && e instanceof AggregationError && e.message === o.error;
    emit(o.id, ok ? "PASS" : "FAIL", ok ? null : `got ${e.name}(${e.kind}): ${e.message}`, o.error, e.message);
  }
}

// ---------- typed-boundary regressions (REVIEW-NM-019) ----------
const bnd = (id, fn) => {
  try { fn(); emit(id, "FAIL", "accepted malformed input"); }
  catch (e) { emit(id, e.kind === "typed_boundary" ? "PASS" : "FAIL", `${e.name}(${e.kind}): ${e.message}`); }
};
const D = fourBars();
bnd("asof_junk", () => aggregate(D, null, "junk"));
bnd("asof_feb30", () => aggregate(D, null, "2023-02-30"));
bnd("asof_month13", () => aggregate(D, null, "2024-13-01"));
bnd("asof_day0", () => aggregate(D, null, "2024-01-00"));
bnd("asof_year0", () => aggregate(D, null, "0000-06-01"));
bnd("asof_partial", () => aggregate(D, null, "2024-6-1"));
bnd("asof_number", () => aggregate(D, null, 20240601));
bnd("asof_null", () => aggregate(D, null, null));
bnd("bar_date_feb30", () => aggregate(version([{ ...D.records[0], date: "2023-02-30" }]), null, "2024-06-01"));
bnd("bar_date_junk", () => aggregate(version([{ ...D.records[0], date: "junk" }]), null, "2024-06-01"));
bnd("bar_date_missing", () => aggregate(version([{ ...D.records[0], date: undefined }]), null, "2024-06-01"));
bnd("years_float", () => aggregate(D, [2024.5], "2024-06-01"));
bnd("years_string", () => aggregate(D, ["2024"], "2024-06-01"));
bnd("years_nan", () => aggregate(D, [NaN], "2024-06-01"));
bnd("years_infinity", () => aggregate(D, [Infinity], "2024-06-01"));
bnd("years_i32_over", () => aggregate(D, [2147483648], "2024-06-01"));
bnd("years_not_array", () => aggregate(D, 2024, "2024-06-01"));
bnd("daily_null", () => aggregate(null, null, "2024-06-01"));
bnd("daily_records_not_array", () => aggregate({ ...D, records: {} }, null, "2024-06-01"));
bnd("dec_field_string", () => aggregate(version([{ ...D.records[0], open: "10" }]), null, "2024-06-01"));
bnd("dec_field_float", () => aggregate(version([{ ...D.records[0], high: { m: 10.5, s: 0 } }]), null, "2024-06-01"));
bnd("dec_scale_over28", () => aggregate(version([{ ...D.records[0], low: { m: 1n, s: 29 } }]), null, "2024-06-01"));
bnd("volume_undefined", () => aggregate(version([{ ...D.records[0], volume: undefined }]), null, "2024-06-01"));
// boundary checks precede Rust-order errors (both fail-closed)
bnd("boundary_precedes_invalid_input", () => aggregate({ ...D, records: [{}] }, null, "junk"));
// valid i32 edge years still reach the Rust layer (missing-year error, not boundary)
try {
  aggregate(D, [-2147483648], "2024-06-01");
  emit("i32_min_year_reaches_rust", "FAIL", "accepted");
} catch (e) {
  emit("i32_min_year_reaches_rust", e.kind === "missing_year" && e.message.includes("-2147483648") ? "PASS" : "FAIL", `${e.kind}: ${e.message}`);
}
try {
  aggregate(D, [2147483647], "2024-06-01");
  emit("i32_max_year_reaches_rust", "FAIL", "accepted");
} catch (e) {
  emit("i32_max_year_reaches_rust", e.kind === "missing_year" ? "PASS" : "FAIL", `${e.kind}: ${e.message}`);
}

// ---------- supplementary: no silent rounding ----------
try {
  // sum 0.05 + 0.05 must stay exact (never 0.1 via float)
  const s = aggregate(version([
    bar("2023-01-03", dec("1"), dec("2"), dec("1"), dec("1"), dec("0.05"), dec("0.05")),
    bar("2023-06-30", dec("1"), dec("2"), dec("1"), dec("1"), dec("0.05"), dec("0.05")),
  ]), null, "2024-06-01");
  const ok = _dec.toString(s.records[0].bar.volume) === "0.10" &&
             _dec.toString(s.records[0].bar.amount) === "0.10";
  emit("exact_decimal_sums", ok ? "PASS" : "FAIL", `${_dec.toString(s.records[0].bar.volume)}/${_dec.toString(s.records[0].bar.amount)}`);
} catch (e) { emit("exact_decimal_sums", "FAIL", e.message); }
