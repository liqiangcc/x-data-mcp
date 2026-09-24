// NM-109 — byte-identity golden for envelope pack/expand + record_check.
// Exercises the three surfaces over real fixtures + edge inputs and
// emits a canonical JSONL dump; identical bytes pre/post-change is the
// parity bar. Usage: node envelope_golden.mjs <workdir>
import fs from "node:fs";
import { join } from "node:path";
import { execFileSync } from "node:child_process";
import {
  compactBarFromBar, compactBarToBar, segmentDigest,
} from "../../src_node/market/compact_market.mjs";
import { check as recordCheck, legacyDigest } from "../../src_node/market/record_codec.mjs";

const dir = process.argv[2];

// Same bar fixture as bench.mjs envelope_pack/expand (100 bars).
const bars = Array.from({ length: 100 }, (_, i) => ({
  date: { s: `2024-01-${String((i % 28) + 1).padStart(2, "0")}`, y: 2024, mo: 1, d: (i % 28) + 1 },
  open: `${(10 + i / 100).toFixed(2)}`,
  high: `${(11 + i / 100).toFixed(2)}`,
  low: `${(9 + i / 100).toFixed(2)}`,
  close: `${(10.5 + i / 100).toFixed(2)}`,
  volume: "100000", amount: "1050000.00", r7_raw: 7 }));
// Edge inputs exercising every arm: non-2dp, exponent forms, negatives,
// integrality, ordering violations, missing/odd fields.
const edge = [
  { date: { s: "2024-02-30", y: 2024, mo: 2, d: 30 }, open: "1.00", high: "2", low: "1", close: "1.5", volume: null, amount: null, r7_raw: null }, // epochDate rejects Feb30? (daysFromCe accepts — check via check())
  { date: "2024-03-01", open: "10.505", high: "11", low: "9", close: "10.5", volume: "5", amount: "1e3", r7_raw: null },   // non-2dp
  { date: "2024-03-02", open: "-1.00", high: "2", low: "-2", close: "0.5", volume: "-1", amount: "-5", r7_raw: null },   // negatives
  { date: "2024-03-03", open: "1e2", high: "1.00e2", low: "99", close: "100.00", volume: "100000.0", amount: "0", r7_raw: 0 }, // exponent+frac volume
  { date: "2024-03-04", open: "0.00", high: "0.01", low: "0.00", close: "0.005", volume: "1.5", amount: "1.0000", r7_raw: null }, // frac volume
  { date: "2024-03-05", open: "1e30", high: "1e30", low: "1e30", close: "1e30", volume: "9e18", amount: "7.9e28", r7_raw: null },  // overflow-ish
  { date: "2024-03-06", open: "10.00", high: "5.00", low: "9.00", close: "10.00", volume: "1", amount: "1", r7_raw: null },       // OHLC violation
  { date: "2024-03-07", open: "10", high: "11", low: "9", close: "10", volume: "1", amount: "01.5", r7_raw: null },              // non-canonical amount
  { date: "2024-03-08", open: "10", high: "11", low: "9", close: "10", volume: "1", amount: "-0.5", r7_raw: null },              // neg amount
  { date: "2024-03-09", open: "10", high: "11", low: "9", close: "10", volume: "1", amount: "1.50", r7_raw: null },              // trailing-zero frac
];
const serBig = (v) => JSON.stringify(v, (_, x) => typeof x === "bigint" ? `n${x}` : x);
const rows = [];
const emit = (op, fn) => {
  try { rows.push({ op, ok: serBig(fn()) }); }
  catch (e) { rows.push({ op, err: `${e.name}:${e.kind ?? ""}:${e.message}` }); }
};
bars.forEach((b, i) => {
  emit(`pack:${i}`, () => compactBarFromBar(b, "Day"));
});
const packed = bars.map((b) => compactBarFromBar(b, "Day"));
packed.forEach((c, i) => emit(`expand:${i}`, () => compactBarToBar(c, "Day")));
edge.forEach((b, i) => {
  emit(`pack_edge:${i}`, () => compactBarFromBar(b, "Day"));
  emit(`pack_edge_year:${i}`, () => compactBarFromBar(b, "Year"));
});
// expand on synthetic compact rows incl. malformed
[
  { trade_day: 19737n, open_cents: 1000n, high_cents: 1100n, low_cents: 900n, close_cents: 1050n, volume: 100000n, amount: "1050000", r7_raw: 7 },
  { trade_day: 19737n, open_cents: 1000n, high_cents: 500n, low_cents: 900n, close_cents: 1050n, volume: null, amount: null, r7_raw: null },   // OHLC viol
  { trade_day: 999999999999n, open_cents: 1n, high_cents: 1n, low_cents: 1n, close_cents: 1n, volume: null, amount: null, r7_raw: null },        // date range
  { trade_day: 19737n, open_cents: 1n, high_cents: 1n, low_cents: 1n, close_cents: 1n, volume: -1n, amount: "x", r7_raw: null },                // neg vol + bad amt
  { trade_day: 19737n, open_cents: 1n, high_cents: 1n, low_cents: 1n, close_cents: 1n, volume: null, amount: "1.50", r7_raw: 3 },               // non-canon amt
  { trade_day: 19737n, open_cents: 1n, high_cents: 1n, low_cents: 1n, close_cents: 1n, volume: null, amount: "x", r7_raw: null },                 // malformed amt -> .m TypeError
  { trade_day: 19737n, open_cents: 1n, high_cents: 1n, low_cents: 1n, close_cents: 1n, volume: null, amount: "-5", r7_raw: null },                // neg stored amt
  { trade_day: 19737n, open_cents: 1n, high_cents: 1n, low_cents: 1n, close_cents: 1n, volume: null, amount: "1e3", r7_raw: null },               // exponent amt -> null
].forEach((c, i) => emit(`expand_edge:${i}`, () => compactBarToBar(c, "Day")));
emit("seg", () => segmentDigest("sh.600000", "Day", packed.slice(0, 10)).toString("hex"));

// record_check over the real issue79 fixtures (valid + invalid).
const recSrc = JSON.parse(fs.readFileSync(
  new URL("../../tests/fixtures/issue79/legacy-records.json", import.meta.url), "utf8"));
recSrc.forEach((r, i) => {
  emit(`check:${i}`, () => {
    const out = recordCheck(r.record);
    return { version_id: out.version.version_id, bars: out.batch.bars.length,
      validation: out.batch ? out.version.validation : null };
  });
  emit(`digest:${i}`, () => legacyDigest(r.record));
});
fs.writeFileSync(join(dir, "golden.jsonl"),
  rows.map((r) => JSON.stringify(r)).join("\n") + "\n");
console.log(`golden: ${rows.length} rows`);
