// NM-013 P2: validate src_node/algorithms/qfq_affine.mjs against accepted
// P0 goldens (gbbq_factor_normalize + qfq_affine) plus rust-oracle cases
// generated live via rustc + real x_data_mcp/rust_decimal rlibs.
// Usage: node scripts/node_migration_p2/qfq_affine_probe.mjs <worktree-root>
import { readFileSync } from "node:fs";
import { join } from "node:path";
import {
  normalizeFactor, factors, eventTransform, composeChain, applyAffine,
  displayedPreviousClose, decToString, _dec,
} from "../../src_node/algorithms/qfq_affine.mjs";

const ROOT = process.argv[2] || ".";
const vectors = readFileSync(join(ROOT, "evidence/node-migration/p0/numeric/vectors.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);
const rustOracle = readFileSync(join(ROOT, "evidence/node-migration/p2/qfq_affine/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

const emit = (family, id, status, detail, expected = null, actual = null) =>
  console.log(JSON.stringify({ family, id, status, detail, expected, actual }));

const d = _dec.fromStr;
const f32 = (bits) => { const v = new DataView(new ArrayBuffer(4)); v.setUint32(0, bits); return v.getFloat32(0); };

// ---------- golden: gbbq_factor_normalize ----------
for (const v of vectors.filter((r) => r.family === "gbbq_factor_normalize")) {
  const raw = v.input.f32 === null ? NaN : v.input.f32; // null encodes non-finite in JSON
  const actual = normalizeFactor(raw);
  const actualStr = actual === null ? null : decToString(actual);
  const ok = actualStr === v.observed.value;
  emit("gbbq_factor_normalize", v.id, ok ? "PASS" : "FAIL",
    ok ? null : "mismatch", v.observed.value, actualStr);
}

// ---------- golden: qfq_affine ----------
for (const v of vectors.filter((r) => r.family === "qfq_affine")) {
  let actual;
  if (v.id === "event_transform") {
    const t = eventTransform(v.input.c.map(d));
    actual = { a: decToString(t.a), b: decToString(t.b) };
  } else if (v.id === "compose_chain_two_events") {
    const ch = composeChain(v.input.events.map((e) => e.map(d)));
    actual = { a: decToString(ch.a), b: decToString(ch.b) };
  } else if (v.id === "displayed_previous_close") {
    actual = decToString(displayedPreviousClose(d(v.input.prev_close), v.input.c.map(d)));
  }
  const ok = JSON.stringify(actual) === JSON.stringify(v.observed.value);
  emit("qfq_affine", v.id, ok ? "PASS" : "FAIL",
    ok ? null : "mismatch", v.observed.value, actual);
}

// ---------- rust-oracle supplementary (live rustc + real rlib) ----------
const orc = Object.fromEntries(rustOracle.map((r) => [r.id, r]));
const orcCheck = (id, actualObj, wantObj, keys) => {
  const ok = keys.every((k) => actualObj[k] === wantObj[k]);
  emit("rust_oracle", id, ok ? "PASS" : "FAIL", ok ? null : "mismatch", wantObj, actualObj);
};
orcCheck("ev_all_zero", (() => { const t = eventTransform(["0", "0", "0", "0"].map(d)); return { a: decToString(t.a), b: decToString(t.b) }; })(), orc.ev_all_zero, ["a", "b"]);
orcCheck("ev_cash_only", (() => { const t = eventTransform(["1.00", "0", "0", "0"].map(d)); return { a: decToString(t.a), b: decToString(t.b) }; })(), orc.ev_cash_only, ["a", "b"]);
orcCheck("ev_terminating_denominator", (() => { const t = eventTransform(["0.50", "0", "2.50", "0"].map(d)); return { a: decToString(t.a), b: decToString(t.b) }; })(), orc.ev_terminating_denominator, ["a", "b"]);
orcCheck("ev_negative_factor", (() => { const t = eventTransform(["-1.00", "0", "0", "0"].map(d)); return { a: decToString(t.a), b: decToString(t.b) }; })(), orc.ev_negative_factor, ["a", "b"]);
orcCheck("chain_single", (() => { const ch = composeChain([["0", "0", "2.50", "0"].map(d)]); return { a: decToString(ch.a), b: decToString(ch.b) }; })(), orc.chain_single, ["a", "b"]);
orcCheck("chain_three", (() => { const ch = composeChain([["0", "0", "2.50", "0"].map(d), ["1.00", "0", "0", "1.00"].map(d), ["0.20", "0", "0", "0"].map(d)]); return { a: decToString(ch.a), b: decToString(ch.b) }; })(), orc.chain_three, ["a", "b"]);
orcCheck("chain_empty", (() => { const ch = composeChain([]); return { a: decToString(ch.a), b: decToString(ch.b) }; })(), orc.chain_empty, ["a", "b"]);
for (const [id, prev, c] of [
  ["dpc_half_tie_up", "10.00", ["0.20", "0", "0", "0"]],
  ["dpc_terminating", "10.00", ["0", "0", "2.50", "0"]],
  ["dpc_apply_chain", "10.00", ["1.00", "0", "0", "1.00"]],
]) {
  orcCheck(id, { v: decToString(displayedPreviousClose(d(prev), c.map(d))) }, orc[id], ["v"]);
}
for (const [id, bits] of [["nf_nan", 0x7fc00000], ["nf_inf", 0x7f800000], ["nf_neg_half_up", f32bits(-0.005)], ["nf_1004", f32bits(1.005)]]) {
  const r = normalizeFactor(f32(bits));
  orcCheck(id, { v: r === null ? null : decToString(r) }, orc[id], ["v"]);
}
function f32bits(v) { const dv2 = new DataView(new ArrayBuffer(4)); dv2.setFloat32(0, v); return dv2.getUint32(0); }

// ---------- rust_decimal op-level semantics checks ----------
const op = (id, got, want) => emit("decimal_semantics", id, got === want ? "PASS" : "FAIL", got === want ? null : "mismatch", want, got);
op("div_10_4", decToString(_dec.div(d("10"), d("4"))), "2.50");
op("div_10_2", decToString(_dec.div(d("10"), d("2"))), "5");
op("div_10_5_00", decToString(_dec.div(d("10"), d("5.00"))), "2");
op("div_1_40", decToString(_dec.div(d("1"), d("40"))), "0.025");
op("mul_0_5_2", decToString(_dec.mul(d("0.5"), d("2"))), "1.0");
op("mul_rescale_half_even", decToString(_dec.mul(d("0.9090909090909090909090909091"), d("0.8333333333333333333333333333"))), "0.7575757575757575757575757576");
op("add_scale_align", decToString(_dec.add(d("10.00"), d("0"))), "10.00");
op("add_zero_shortcircuit_1", decToString(_dec.add(d("0.0"), d("0"))), "0");
op("add_zero_shortcircuit_2", decToString(_dec.add(d("0.00"), d("0.0"))), "0.0");
op("mul_zero_shortcircuit", decToString(_dec.mul(d("0.8"), d("0"))), "0");
op("neg_zero_display", decToString(d("-0")), "0");

// ---------- fail-closed behavior ----------
try {
  factors([1.0, NaN, 0, 0]);
  emit("fail_closed", "non_finite_factor_reject", "FAIL", "accepted NaN factor");
} catch (e) {
  emit("fail_closed", "non_finite_factor_reject",
    e.message === "GBBQ factor 1 is not a finite decimal input" && e.kind === "non_finite_factor" ? "PASS" : "FAIL",
    e.message);
}
try {
  factors([Infinity, 0, 0, 0]);
  emit("fail_closed", "inf_factor_reject", "FAIL", "accepted inf factor");
} catch (e) {
  emit("fail_closed", "inf_factor_reject", e.index === 0 && e.kind === "non_finite_factor" ? "PASS" : "FAIL", e.message);
}
emit("fail_closed", "empty_chain_identity", (() => {
  const ch = composeChain([]);
  const x = d("12.34");
  return decToString(applyAffine(ch, x)) === "12.34" ? "PASS" : "FAIL";
})());
