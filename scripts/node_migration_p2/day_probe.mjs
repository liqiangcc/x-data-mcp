// NM-011 P2: validate src_node/parsers/day_file.mjs against accepted P0
// day_parse golden vectors + supplementary structural edges.
// Usage: node scripts/node_migration_p2/day_probe.mjs <worktree-root>
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { parse, DayFileError } from "../../src_node/parsers/day_file.mjs";

const ROOT = process.argv[2] || ".";
const vectors = readFileSync(join(ROOT, "evidence/node-migration/p0/numeric/vectors.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse)
  .filter((r) => r.family === "day_parse");

const emit = (id, status, detail, expected = null, actual = null) =>
  console.log(JSON.stringify({ family: "day_parse", id, status, detail, expected, actual }));

const words = (...ws) => { const b = Buffer.alloc(ws.length * 4); ws.forEach((w, i) => b.writeUInt32LE(w >>> 0, i * 4)); return b; };

function check(vector, makeBytes) {
  try {
    const actual = parse(makeBytes());
    const ok = vector.observed.status === "ok" &&
      JSON.stringify(actual) === JSON.stringify(vector.observed.value);
    emit(vector.id, ok ? "PASS" : "FAIL",
      ok ? null : "value mismatch", vector.observed.value, actual);
  } catch (e) {
    const match = vector.observed.status === "error" && vector.observed.error === e.message;
    emit(vector.id, match ? "PASS" : "FAIL",
      match ? null : `expected ${JSON.stringify(vector.observed)} got ${e.name}: ${e.message}`,
      vector.observed.error, e.message);
  }
}

for (const v of vectors) {
  if (v.id === "two_valid_records") {
    check(v, () => Buffer.from(v.input.hex, "hex"));
  } else if (v.id === "truncated_31_bytes") {
    check(v, () => Buffer.alloc(31));
  } else if (v.id === "invalid_month_13") {
    check(v, () => words(20241301, 0, 0, 0, 0, 0, 0, 0));
  } else if (v.id === "non_increasing_date") {
    check(v, () => words(20240103, 0, 0, 0, 0, 0, 0, 0, 20240102, 0, 0, 0, 0, 0, 0, 0));
  } else {
    emit(v.id, "FAIL", `no probe mapping for golden vector ${v.id}`);
  }
}

// ---- supplementary structural edges (not golden rows) ----
const sup = (id, fn, want) => {
  try { emit(id, "FAIL", `expected reject, parsed: ${JSON.stringify(fn())}`); }
  catch (e) { emit(id, e instanceof DayFileError && want.test(e.message) ? "PASS" : "FAIL",
      `${e.kind}: ${e.message}`); }
};
sup("sup_year_zero_reject", () => parse(words(0, 0, 0, 0, 0, 0, 0, 0)), /invalid calendar date 0 at record 0/);
sup("sup_equal_dates_reject", () => parse(words(20240102, 0, 0, 0, 0, 0, 0, 0, 20240102, 0, 0, 0, 0, 0, 0, 0)), /non-increasing date at record 1/);
sup("sup_feb30_reject", () => parse(words(20240230, 0, 0, 0, 0, 0, 0, 0)), /invalid calendar date 20240230 at record 0/);
const supOk = (id, fn) => {
  try { emit(id, "PASS", null, null, fn()); }
  catch (e) { emit(id, "FAIL", `unexpected reject: ${e.message}`); }
};
supOk("sup_leap_feb29_accept", () => { const r = parse(words(20240229, 0, 0, 0, 0, 0, 0, 0)); if (r[0].date !== "2024-02-29") throw new Error("wrong date"); return r[0].date; });
supOk("sup_empty_file", () => { const r = parse(Buffer.alloc(0)); if (r.length !== 0) throw new Error("expected []"); return r.length; });
sup("sup_truncation_precedence", () => parse(Buffer.alloc(33)), /byte count 33 is not divisible by 32/);
sup("sup_day_zero_reject", () => parse(words(20240100, 0, 0, 0, 0, 0, 0, 0)), /invalid calendar date 20240100 at record 0/);
sup("sup_month_zero_reject", () => parse(words(20240015, 0, 0, 0, 0, 0, 0, 0)), /invalid calendar date 20240015 at record 0/);
// kind discrimination for programmatic consumers
try { parse(Buffer.alloc(1)); } catch (e) {
  emit("sup_error_kind_discrimination", e.kind === "truncated" && e.name === "DayFileError" ? "PASS" : "FAIL",
    `kind=${e.kind} name=${e.name}`);
}
