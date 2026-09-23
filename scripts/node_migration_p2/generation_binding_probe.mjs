// NM-031 P2: validate governance/generation_binding.mjs against a live
// Rust oracle (uniform_binding Ok/BindingRefusal). Pure-object fixtures.
// Exit nonzero on FAIL. Usage: node generation_binding_probe.mjs <root> [selftest]
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { uniformBinding, GenerationBindingInputError } from "../../src_node/governance/generation_binding.mjs";

const ROOT = process.argv[2] || ".";
const SELFTEST = process.argv[3] || null;
const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/generation_binding/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

let failures = 0;
const emit = (id, status, detail, expected = null, actual = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ family: "generation_binding", id, status, detail, expected, actual }));
};

const K = "source_generation_id";
const with_ = (v) => ({ item_id: "i", target: {}, progress_unit: "x", frozen_inputs: { [K]: v }, scope_keys: [] });
const without = () => ({ item_id: "i", target: {}, progress_unit: "x", frozen_inputs: {}, scope_keys: [] });
const itemF = (f) => ({ item_id: "i", target: {}, progress_unit: "x", frozen_inputs: f, scope_keys: [] });

const CASES = {
  no_items: () => [],
  one_missing: () => [without()],
  one_null: () => [with_(null)],
  one_nonstr: () => [with_(5)],
  one_bool: () => [with_(true)],
  one_obj: () => [with_({ x: 1 })],
  uniform_id: () => [with_("gen-a"), with_("gen-a")],
  diff_ids: () => [with_("gen-a"), with_("gen-b")],
  present_then_missing: () => [with_("gen-a"), without()],
  missing_then_present: () => [without(), with_("gen-a")],
  present_then_null: () => [with_("gen-a"), with_(null)],
  null_then_present: () => [with_(null), with_("gen-a")],
  all_missing: () => [without(), without()],
  all_null: () => [with_(null), with_(null)],
  missing_and_null: () => [without(), with_(null)],
  perm_aba: () => [with_("a"), with_("b"), with_("a")],
  perm_missing_mid: () => [with_("g"), without(), with_("g")],
  empty_string: () => [with_(""), with_("")],
  frozen_nonstr: () => [itemF("not-object"), itemF("not-object")],
  frozen_nonstr_mixed: () => [itemF("not-object"), with_("g")],
  unicode_id: () => [with_("gen-中😀"), with_("gen-中😀")],
};

for (const o of oracle) {
  try {
    const got = uniformBinding(CASES[o.id]());
    if (o.status === "ok") {
      const ok = got.ok === true && got.generation_id === o.generation_id;
      emit(o.id, ok ? "PASS" : "FAIL", ok ? `ok ${JSON.stringify(o.generation_id)}` : "mismatch", o, got);
    } else {
      const ok = got.ok === false && got.refusal === o.refusal;
      emit(o.id, ok ? "PASS" : "FAIL", ok ? `refused ${o.refusal}` : "mismatch", o.refusal, got);
    }
  } catch (e) {
    emit(o.id, "FAIL", `threw ${e.name}: ${e.message}`);
  }
}

// ---------- typed boundary (Node-only; serde-unrepresentable) ----------
const bnd = (id, fn) => {
  try { fn(); emit(id, "FAIL", "accepted malformed input"); }
  catch (e) { emit(id, e instanceof GenerationBindingInputError ? "PASS" : "FAIL", `${e.name}: ${e.message.slice(0, 80)}`); }
};
bnd("reject_items_notarray", () => uniformBinding({}));
bnd("reject_item_nonobj", () => uniformBinding(["x"]));
bnd("reject_item_no_frozen", () => uniformBinding([{ item_id: "i" }]));
bnd("reject_frozen_undefined", () => uniformBinding([itemF(undefined)]));
bnd("reject_frozen_nan", () => uniformBinding([itemF({ [K]: NaN })]));
bnd("reject_frozen_fn", () => uniformBinding([itemF({ [K]: () => "g" })]));
bnd("reject_id_surrogate", () => uniformBinding([with_("g\ud800")]));
bnd("reject_key_surrogate", () => uniformBinding([itemF({ ["a\ud800"]: "x", [K]: "g" })]));
bnd("reject_frozen_class", () => uniformBinding([itemF(new Map())]));

// accessor getters must reject WITHOUT invocation (side-effect counter)
let getterCalls = 0;
const gcount = (v) => () => { getterCalls++; return v; };
{
  const f = {}; Object.defineProperty(f, K, { get: gcount("gen-evil"), enumerable: true });
  try { uniformBinding([itemF(f)]); emit("getter_binding", "FAIL", "getter produced binding"); }
  catch (e) { emit("getter_binding", e instanceof GenerationBindingInputError ? "PASS" : "FAIL", `${e.name}`); }
  const it = {}; Object.defineProperty(it, "frozen_inputs", { get: gcount({ [K]: "gen-evil" }), enumerable: true });
  try { uniformBinding([it]); emit("getter_frozen_inputs", "FAIL", "getter produced frozen_inputs"); }
  catch (e) { emit("getter_frozen_inputs", e instanceof GenerationBindingInputError ? "PASS" : "FAIL", `${e.name}`); }
  const nested = { [K]: { get x() { getterCalls++; return "y"; } } };
  try { uniformBinding([itemF(nested)]); emit("getter_nested", "FAIL", "nested getter accepted"); }
  catch (e) { emit("getter_nested", e instanceof GenerationBindingInputError ? "PASS" : "FAIL", `${e.name}`); }
  const setter = {}; Object.defineProperty(setter, K, { set(v) { getterCalls++; }, enumerable: true });
  try { uniformBinding([itemF(setter)]); emit("setter_binding", "FAIL", "setter accepted"); }
  catch (e) { emit("setter_binding", e instanceof GenerationBindingInputError ? "PASS" : "FAIL", `${e.name}`); }
  const sym = { [K]: "gen-s", [Symbol("k")]: 1 };
  try { uniformBinding([itemF(sym)]); emit("symbol_key", "FAIL", "symbol key accepted"); }
  catch (e) { emit("symbol_key", e instanceof GenerationBindingInputError ? "PASS" : "FAIL", `${e.name}`); }
  const hidden = {}; Object.defineProperty(hidden, K, { value: "gen-h", enumerable: false });
  try { uniformBinding([itemF(hidden)]); emit("nonenum_binding", "FAIL", "non-enumerable binding accepted"); }
  catch (e) { emit("nonenum_binding", e instanceof GenerationBindingInputError ? "PASS" : "FAIL", `${e.name}`); }
  emit("getters_never_invoked", getterCalls === 0 ? "PASS" : "FAIL", `getterCalls=${getterCalls}`);
}

// prototype-safety: inherited names can never manufacture a binding
{
  // frozen_inputs object inheriting source_generation_id: custom
  // prototype is not JSON-representable -> typed reject (fail closed);
  // the inherited binding is never consulted.
  try {
    uniformBinding([itemF(Object.create({ [K]: "evil" }))]);
    emit("proto_key_absent", "FAIL", "inherited binding accepted");
  } catch (e) {
    emit("proto_key_absent", e instanceof GenerationBindingInputError ? "PASS" : "FAIL", `${e.name}`);
  }
  // plain object with own '__proto__' data key (JSON.parse shape) binds normally
  const jp = JSON.parse('{"__proto__":"x","source_generation_id":"gen-p"}');
  const got1b = uniformBinding([itemF(jp), itemF(jp)]);
  emit("dunder_proto_own_key", got1b.ok === true && got1b.generation_id === "gen-p" ? "PASS" : "FAIL", "own __proto__ data key inert", "gen-p", got1b);
  // null-prototype frozen map works like a plain JSON object
  const f = Object.create(null);
  f[K] = "gen-x";
  const got2 = uniformBinding([itemF(f), itemF(f)]);
  emit("null_proto_ok", got2.ok === true && got2.generation_id === "gen-x" ? "PASS" : "FAIL", "null-prototype frozen map binds", "gen-x", got2);
  // own 'constructor' key is just data — inert, doesn't affect binding
  const got3 = uniformBinding([itemF({ constructor: "g", [K]: "gen-y" }), itemF({ [K]: "gen-y" })]);
  emit("constructor_key_ignored", got3.ok === true && got3.generation_id === "gen-y" ? "PASS" : "FAIL", "own 'constructor' key is inert", "gen-y", got3);
}

// selftest
if (SELFTEST === "binding") {
  const got = uniformBinding([with_("g"), with_("g")]);
  emit("selftest_binding", got.ok === false ? "PASS" : "FAIL",
    got.ok === false ? null : "expected refusal on uniform input — STOP");
}

process.exitCode = failures === 0 ? 0 : 1;
