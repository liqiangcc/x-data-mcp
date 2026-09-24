// NM-090 — serValue byte-writer equivalence corpus.
// Asserts byte-identity between the new byte-level serValue/serValueBytes
// and the pre-NM-090 recursive implementation (verbatim copy below,
// bound to this module's F64/serF64 so instanceof checks work) across
// the NM-081 equivalence cases plus adversarial escape/surrogate/
// key-order edges.
//
// Usage: node serde_writer_probe.mjs [--selftest] [--out <dir>]
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { serValue, serValueBytes, serF64, F64, parseJsonBytes }
  from "../../src_node/mcp/serde_json.mjs";

const SELFTEST = process.argv.includes("--selftest");
const OUT = process.argv.includes("--out")
  ? process.argv[process.argv.indexOf("--out") + 1] : null;

// ---- pre-NM-090 reference implementation (git-show baseline) -------
const keyCmpOld = (a, b) =>
  Buffer.compare(Buffer.from(a, "utf8"), Buffer.from(b, "utf8"));
const serValueOld = (v) => {
  if (v === null || v === undefined) return "null";
  if (v instanceof F64) return serF64(v.v);
  if (typeof v === "bigint") return v.toString();
  if (typeof v === "number") return Number.isInteger(v) ? String(v) : serF64(v);
  if (typeof v === "string") return JSON.stringify(v);
  if (typeof v === "boolean") return v ? "true" : "false";
  if (typeof v.toJSON === "function") return serValueOld(v.toJSON());
  if (Array.isArray(v)) return `[${v.map(serValueOld).join(",")}]`;
  const keys = Object.keys(v).sort(keyCmpOld);
  return `{${keys.map((k) => `${JSON.stringify(k)}:${serValueOld(v[k])}`).join(",")}}`;
};

const results = [];
const emit = (id, status, detail) => {
  results.push({ family: "serde_writer", id, status, detail });
  console.log(JSON.stringify(results[results.length - 1]));
};
const check = (id, v) => {
  const a = serValue(v);
  const b = serValueOld(v);
  const bytes = serValueBytes(v);
  const asBuf = Buffer.from(a, "utf8");
  const ok = a === b && Buffer.compare(bytes, asBuf) === 0;
  emit(id, ok ? "PASS" : "FAIL",
    ok ? `${a.length}B` : `new=${JSON.stringify(a)} old=${JSON.stringify(b)}`);
};

// ---------- NM-081 equivalence corpus (14 cases) --------------------
check("f64_integral", new F64(1.0));
check("f64_neg_zero", new F64(-0));
check("f64_e300", new F64(1e300));
check("f64_e_minus300", new F64(1e-300));
check("bigint", 9007199254740993n);
check("bigint_max_u64", 18446744073709551615n);
check("toJSON", { v: 1, toJSON: () => ({ z: [new F64(2.5), "x"] }) });
check("nested", { a: { b: { c: [1, { d: null }] } } });
check("key_order_ä", { "z": 1, "ä": 2, "a": 3 });
check("key_order_nonbmp", { "💩": 1, "z": 2, "￿": 3 });
check("c0_escapes", "\x00\x01\x07\x08\x09\x0a\x0b\x0c\x0d\x1f");
check("quote_backslash", '"\\');
check("plain_int", 42);
check("plain_string", "hello");

// ---------- adversarial edges ---------------------------------------
check("all_c0", Array.from({ length: 32 }, (_, i) => String.fromCharCode(i)).join(""));
check("del_boundary", "\x1f\x20\x7f\u0080");
check("nonbmp_surrogate_pair", "💩x\u{1f4a9}");
check("lone_surrogate_lead", "\ud800");
check("lone_surrogate_trail", "\udfff");
check("surrogate_before_bmp", { "😀": 1, "퟿": 2, "": 3, "퟿x": 4 });
check("empty_containers", { e: [], o: {}, s: "", n: null });
check("deep_nest", (() => { let v = 1; for (let i = 0; i < 200; i++) v = [v]; return v; })());
check("deep_obj", (() => { let v = {}; for (let i = 0; i < 200; i++) v = { k: v }; return v; })());
check("number_edges", [0, -0, 1, -1, 2 ** 53 - 1, -(2 ** 53 - 1), 2 ** 53, 0.5]);
check("f64_edges", [new F64(0), new F64(1e15), new F64(1e16), new F64(1e-5), new F64(1e-6),
  new F64(123456789.123), new F64(5e-324)]);
check("mixed_doc", { arr: [1, "a\nb", null, { k: "v", K: "w" }], num: new F64(0.001) });
check("toJSON_in_array", [1, { toJSON: () => "replaced" }, 3]);
check("undefined_leaf", undefined);
check("unicode_keys_mixed", { "中": 1, "z": 2, "𐀀": 3, "a": 4 });
check("escape_all_quotes", '""""');
check("control_after_ascii", "abc\x00def");
check("emoji_key_vs_ascii", { "🚀": 1, "zzzz": 2, "~~~~": 3 });

// ---------- roundtrip: parse(serValue(v)) reserializes identically --
{
  const doc = { gens: [{ id: "a".repeat(44), st: "installed",
    at: "2026-09-24T00:00:00Z", ev: { sha: "b".repeat(64) } }] };
  const s = serValue(doc);
  const back = serValue(parseJsonBytes(Buffer.from(s, "utf8")).value);
  emit("roundtrip_stability", s === back ? "PASS" : "FAIL",
    s === back ? "" : `${s} != ${back}`);
}

if (SELFTEST) emit("selftest_forced", "FAIL", "intentional failure");
const fails = results.filter((r) => r.status === "FAIL");
emit("summary", fails.length === 0 ? "PASS" : "FAIL",
  `${results.length} checks, ${fails.length} failed`);
if (OUT) {
  mkdirSync(OUT, { recursive: true });
  writeFileSync(join(OUT, "probe-results.jsonl"),
    results.map((r) => JSON.stringify(r)).join("\n") + "\n");
  writeFileSync(join(OUT, "results.json"), JSON.stringify({
    probe: "serde_writer", total: results.length,
    pass: results.filter((r) => r.status === "PASS").length,
    fail: fails.length,
  }, null, 2) + "\n");
}
process.exit(fails.length ? 1 : 0);
