// NM-012 P2: validate src_node/codecs/amount_f32.mjs against accepted P0
// amount_f32_projection goldens + Rust-verified boundary cases.
// Usage: node scripts/node_migration_p2/amount_f32_probe.mjs <worktree-root>
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { projectAmountF32, AmountF32Error } from "../../src_node/codecs/amount_f32.mjs";

const ROOT = process.argv[2] || ".";
const vectors = readFileSync(join(ROOT, "evidence/node-migration/p0/numeric/vectors.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse)
  .filter((r) => r.family === "amount_f32_projection");

const emit = (id, status, detail, expected = null, actual = null) =>
  console.log(JSON.stringify({ family: "amount_f32_projection", id, status, detail, expected, actual }));

// ---- golden vectors ----
for (const v of vectors) {
  try {
    const actual = projectAmountF32(parseInt(v.input.f32_bits, 16));
    const ok = v.observed.status === "ok" && actual === v.observed.value;
    emit(v.id, ok ? "PASS" : "FAIL", ok ? null : "value mismatch", v.observed.value, actual);
  } catch (e) {
    emit(v.id, "FAIL", `unexpected throw: ${e.message}`, v.observed, String(e));
  }
}

// ---- Rust-verified boundary cases (from live rust_decimal checks) ----
// expected values produced by rustc + real rust_decimal rlib on this box.
const rustChecked = [
  ["rust_f32max_overflow_none", 0x7f7fffff, null],                      // 3.4e38 -> mantissa > 2^96 -> None
  ["rust_min_denormal_underflow", 0x00000001, "0.0000000000000000000000000000"], // scale 51 -> rounds to 0 at 28dp
  ["rust_min_normal_underflow", 0x00800000, "0.0000000000000000000000000000"],   // ~1.18e-38 -> 0
  ["rust_neg_zero", 0x80000000, "0"],                                  // "-0" -> Decimal(0) -> "0"
  ["rust_expansion_1e20", 0x60bfc0cd, "110538152453251000000"],          // positional, JS non-exp too
  ["rust_expansion_8e20", 0x622d42b8, "799023579338492600000"],
  ["rust_expansion_1e29_none", 0x6fa81fe4, null],                      // JS e-notation; expanded -> 30 digits -> None
  ["rust_expansion_1e_10", 0x2edbe6ff, "0.0000000001000000013351432"], // JS e-notation; expanded
  ["rust_small_1e_5", 0x3727c5ac, "0.000009999999747378752"],
  ["rust_1e8", 0x4cbebc20, "100000000"],
  ["rust_neg_inf", 0xff800000, null],
  ["rust_quiet_nan", 0x7fc00001, null],
];
for (const [id, bits, want] of rustChecked) {
  try {
    const actual = projectAmountF32(bits);
    emit(id, actual === want ? "PASS" : "FAIL", actual === want ? null : "mismatch", want, actual);
  } catch (e) { emit(id, "FAIL", `unexpected throw: ${e.message}`, want, String(e)); }
}

// ---- fail-closed input validation ----
const rejects = [
  ["reject_negative", -1], ["reject_overflow", 0x100000000],
  ["reject_fraction", 1.5], ["reject_string", "0x4d0f4406"],
  ["reject_nan_input", NaN], ["reject_undefined", undefined], ["reject_bool", true],
];
for (const [id, bad] of rejects) {
  try {
    projectAmountF32(bad);
    emit(id, "FAIL", "accepted invalid input", "throw", "returned");
  } catch (e) {
    emit(id, e instanceof AmountF32Error ? "PASS" : "FAIL",
      e instanceof AmountF32Error ? null : `wrong error type: ${e.name}`, "AmountF32Error", e.name);
  }
}
