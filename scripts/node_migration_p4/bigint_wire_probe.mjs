// NM-104 — F-001-class audit probe: bigint/u64 values crossing JSON
// serialization boundaries must emit serde's digits, never crash or
// lose precision. Every case asserts the crash-then-fix behavior.
// Usage: node bigint_wire_probe.mjs [--selftest]
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { execFileSync } from "node:child_process";
import { TaskAdmission } from "../../src_node/storage/task_repository.mjs";
import { ser, snapshotCanon, readRequestRecord }
  from "../../src_node/storage/verification_storage.mjs";
import { serValue, parseJsonValue, F64 }
  from "../../src_node/mcp/serde_json.mjs";
import { tU64, tInt, I64_MIN } from "../../src_node/mcp/serde_value.mjs";
import { buildTargetFixture, ensureSkeleton, NOW }
  from "./pubcheck_fixture.mjs";

const TMP = process.env.TMPDIR || "/tmp";
const dir = mkdtempSync(join(TMP, "nm104-"));
const results = [];
const check = (name, fn) => {
  try { fn(); results.push({ name, ok: true }); }
  catch (e) { results.push({ name, ok: false, detail: String(e.stack ?? e) }); }
};
const eq = (a, b, what) => {
  if (a !== b) throw new Error(`${what}: ${JSON.stringify(a)} != ${JSON.stringify(b)}`);
};

const BIG = 2n ** 60n + 1n;            // 1152921504606846977 — > 2^53
const BIG_S = "1152921504606846977";

// ---- L1: ser() leaf — u64 digits, never TypeError ---------------------
check("ser_bigint_leaf", () => {
  eq(ser({ v: BIG }), `{"v":${BIG_S}}`, "ser digits");
});
check("ser_nested_bigint_array", () => {
  eq(ser({ a: [{ x: BIG }, null] }), `{"a":[{"x":${BIG_S}},null]}`);
});
check("ser_f64_leaf", () => {
  eq(ser({ p: new F64(1.5) }), '{"p":1.5}', "F64 leaf");
});

// ---- L2: decode — u64 >2^53 stays exact -------------------------------
check("tU64_bigint_exact", () => {
  const v = tU64(BIG);
  if (v !== BIG) throw new Error(`decode lost digits: ${v}`);
});
check("tU64_small_stays_number", () => {
  eq(tU64(5n), 5, "small u64");
  eq(tU64(5), 5, "number input");
});
check("tU64_overflow_rejected", () => {
  try { tU64(2n ** 64n); throw new Error("accepted 2^64"); }
  catch (e) {
    if (!/invalid value: integer `18446744073709551616`, expected u64/.test(e.message))
      throw e;
  }
});
check("tI64_neg_bigint_exact", () => {
  const v = tInt("i64", I64_MIN, (1n << 63n) - 1n)(-(2n ** 60n) - 1n);
  if (v !== -(2n ** 60n) - 1n) throw new Error("i64 lost digits");
});

// ---- L3: parseJsonValue — u64-exact round-trip ------------------------
check("parse_u64_exact", () => {
  const v = parseJsonValue(`{"bytes":${BIG_S}}`);
  if (v.bytes !== BIG) throw new Error(`parsed ${typeof v.bytes} ${v.bytes}`);
  eq(ser(v), `{"bytes":${BIG_S}}`, "re-encode identical");
});
check("parse_float_f64_lexeme", () => {
  const v = parseJsonValue(`{"x":1.50}`);
  if (!(v.x instanceof F64)) throw new Error("float lost F64 lexeme");
  eq(ser(v), '{"x":1.5}', "serde canonical f64");
});

// ---- L4: storage round-trip — outcome_json evidence bigint ------------
const skeleton = ensureSkeleton(dir);
const dbPath = join(dir, "wire.db");
const fx = buildTargetFixture(dbPath, skeleton);
const repo = fx; // {db, adm, taskId, itemId, path}

check("finishItem_evidence_bigint_write", () => {
  // fixture item is already claimed + running (rev 2, owner "run").
  const t = repo.adm._readBoundTaskValidated(fx.taskId);
  repo.adm.finishItem(fx.taskId, BigInt(t.task.state_revision), "run",
    fx.itemId, { success: true, error: null, evidence: { bytes: BIG } }, NOW);
});
check("outcome_json_digits_exact", () => {
  const row = repo.db.get(
    "SELECT outcome_json FROM task_items WHERE task_id=? AND item_id=?",
    fx.taskId, fx.itemId);
  if (!row.outcome_json.includes(BIG_S)) {
    throw new Error(`stored bytes wrong: ${row.outcome_json}`);
  }
});
check("bound_read_preserves_bigint", () => {
  const s = repo.adm._readBoundTaskValidated(fx.taskId);
  const ev = s.items.find((i) => i.item_id === fx.itemId).outcome.evidence;
  if (ev.bytes !== BIG) throw new Error(`read-back lost digits: ${ev.bytes}`);
});
check("rewrite_reencode_roundtrip_lossless", () => {
  // rewriteItemOutcomes' inner loop: parse stored outcome_json ->
  // re-encode -> must be byte-identical (pre-fix: JSON.parse lost the
  // u64 digits, so the re-encode differed and rewrote corrupt bytes).
  const stored = repo.db.get(
    "SELECT outcome_json FROM task_items WHERE task_id=? AND item_id=?",
    fx.taskId, fx.itemId).outcome_json;
  const reenc = ser(parseJsonValue(stored), "outcome");
  eq(reenc, stored, "rewrite re-encode bytes");
});
check("snapshot_ser_emits_bigint_digits", () => {
  const s = snapshotCanon(repo.adm._readBoundTaskValidated(fx.taskId));
  const out = ser(s, "snapshot");
  if (!out.includes(BIG_S)) throw new Error("snapshot bytes dropped digits");
});

// ---- L5: uncertainty marker previous_result with bigint ---------------
check("uncertainty_marker_bigint_write_path", () => {
  // previous_result is Option<Value> — null under every reachable live
  // state (running/cancel_requested require result_json null), so the
  // bigint arm is only reachable via corrupt seeds. The WRITE path is
  // what must not crash: the marker serializes via ser() — prove the
  // exact digits land and survive the read round-trip.
  const marker = { execution_uncertainty: { item_id: "x", reason: "r",
    previous_result: { answer: BIG }, previous_status: "running" } };
  const bytes = ser(marker);
  if (!bytes.includes(BIG_S)) throw new Error(`marker bytes: ${bytes}`);
  const back = parseJsonValue(bytes);
  if (back.execution_uncertainty.previous_result.answer !== BIG)
    throw new Error("round-trip lost digits");
});

// ---- L6: serValue/serPretty paths (control — already safe) ------------
check("serValue_bigint", () => eq(serValue({ v: BIG }), `{"v":${BIG_S}}`));

// ---- selftest ----------------------------------------------------------
const selftest = process.argv.includes("--selftest");
if (selftest) {
  results.push({ name: "selftest", ok: false,
    detail: "intentional selftest failure" });
}
let fail = 0;
for (const r of results) {
  if (!r.ok) { fail++; console.error(`FAIL ${r.name}: ${r.detail}`); }
  console.log(JSON.stringify({ family: "bigint_wire", id: r.name,
    status: r.ok ? "PASS" : "FAIL", detail: r.detail ?? "" }));
}
console.log(`bigint_wire probe: ${results.length - fail} pass, ${fail} fail, 0 not_run of ${results.length}`);
rmSync(dir, { recursive: true, force: true });
process.exit(fail === 0 ? 0 : 1);
