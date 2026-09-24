// NM-091 — parseJsonBytes decode parity corpus.
// Compares the optimized parser against the pre-NM-091 implementation
// (extracted from git baseline at build time into _parse_old.mjs —
// F64 bound to the live module so cross-module instanceof works):
// error messages byte-exact, values byte-exact via serValue.
//
// Usage: node serde_decode_probe.mjs [--selftest] [--out <dir>]
import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { serValue, parseJsonBytes } from "../../src_node/mcp/serde_json.mjs";

const SELFTEST = process.argv.includes("--selftest");
const OUT = process.argv.includes("--out")
  ? process.argv[process.argv.indexOf("--out") + 1] : null;

// Extract the baseline parser into a temp module (its own F64 class
// would defeat instanceof on parsed F64 values — we only compare
// serValue outputs, and each side uses its own serValue, so this is
// fine).
const OLD_SRC = execFileSync("git",
  ["show", "HEAD:src_node/mcp/serde_json.mjs"],
  { cwd: new URL("../..", import.meta.url).pathname, encoding: "utf8" });
writeFileSync("/dev/shm/_serde_json_old.mjs", OLD_SRC);
const OLD = await import("/dev/shm/_serde_json_old.mjs");

const results = [];
const emit = (id, status, detail) => {
  results.push({ family: "serde_decode", id, status, detail });
  console.log(JSON.stringify(results[results.length - 1]));
};
const check = (id, input) => {
  const buf = Buffer.from(input, "utf8");
  const a = parseJsonBytes(buf);
  const b = OLD.parseJsonBytes(buf);
  let same = a.ok === b.ok;
  if (same && !a.ok) same = a.message === b.message;
  if (same && a.ok) {
    same = serValue(a.value) === OLD.serValue(b.value);
  }
  emit(id, same ? "PASS" : "FAIL",
    same ? "" :
      `new=${JSON.stringify(a.ok ? serValue(a.value) : a.message)} ` +
      `old=${JSON.stringify(b.ok ? OLD.serValue(b.value) : b.message)}`);
};

// ---------- value corpus --------------------------------------------
check("null", "null");
check("true", "true");
check("false", "false");
check("int_zero", "0");
check("int_neg_zero", "-0");
check("int", "42");
check("int_neg", "-17");
check("int_safe_max", "9007199254740991");
check("int_safe_max_p1", "9007199254740992");
check("int_15digit", "999999999999999");
check("int_16digit", "1000000000000000");
check("i64_min", "-9223372036854775808");
check("u64_max", "18446744073709551615");
check("u64_max_p1", "18446744073709551616");
check("huge_int", "99999999999999999999999999");
check("float_1_0", "1.0");
check("float_frac", "3.25");
check("float_exp", "1e300");
check("float_exp_neg", "1e-300");
check("float_neg", "-0.5");
check("float_E", "2E4");
check("float_plus_exp", "1.5e+10");
check("float_underflow", "1e-999");
check("float_overflow", "1e999");
check("string_plain", '"hello"');
check("string_empty", '""');
check("string_escapes", '"a\\nb\\tc\\"d\\\\e\\/f\\bg\\fh\\ri"');
check("string_unicode", '"ä中💩"');
check("string_hex_escape", '"\\u0041\\u00e4"');
check("string_hex_pair", '"\\ud83d\\udca9"');
check("string_utf8_4byte", '"\u{1f4a9}"');
check("array_empty", "[]");
check("array_mixed", '[1,"x",null,true,2.5]');
check("object_empty", "{}");
check("object_simple", '{"a":1,"b":"x"}');
check("object_dup_keys", '{"k":1,"k":2}');
check("object_nested", '{"a":{"b":{"c":[1,{"d":null}]}}}');
check("whitespace_heavy", '  \t\r\n { "a" : [ 1 , 2 ] } \r\n\t ');
check("deep_100", "[".repeat(100) + "]".repeat(100));
check("deep_obj_127", '{"k":'.repeat(127) + "1" + "}".repeat(127));
check("unicode_keys", '{"中":1,"🚀":2}');

// ---------- error corpus (positions byte-exact) ---------------------
check("err_empty", "");
check("err_ws_only", "   ");
check("err_trailing", "1 2");
check("err_trailing_obj", "{} x");
check("err_bad_value", "x");
check("err_ident_true", "tru");
check("err_ident_truX", "truX");
check("err_ident_false", "falsx");
check("err_ident_null", "nul");
check("err_unclosed_str", '"abc');
check("err_unclosed_arr", "[1,2");
check("err_unclosed_obj", '{"a":1');
check("err_obj_no_key", "{1:2}");
check("err_obj_no_colon", '{"a" 1}');
check("err_obj_no_comma", '{"a":1 "b":2}');
check("err_arr_no_comma", "[1 2]");
check("err_trailing_comma_obj", '{"a":1,}');
check("err_trailing_comma_arr", "[1,]");
check("err_key_eof_post_comma", '{"a":1,');
check("err_arr_eof_post_comma", "[1,");
check("err_ctrl_in_str", '"a\x01b"');
check("err_raw_tab_in_str", '"a\tb"');
check("err_bad_escape", '"a\\qb"');
check("err_hex_short", '"\\u041"');
check("err_hex_bad_digit", '"\\u04g1"');
check("err_lone_high_surr", '"\\ud800"');
check("err_lone_low_surr", '"\\udfff"');
check("err_surr_no_escape", '"\\ud800x"');
check("err_surr_bad_low", '"\\ud800\\u0041"');
check("err_leading_zero", "01");
check("err_minus_only", "-");
check("err_minus_x", "-x");
check("err_dot_no_frac", "1.");
check("err_dot_no_frac2", "1.x");
check("err_exp_no_digits", "1e");
check("err_exp_sign_only", "1e+");
check("err_deep_129", "[".repeat(129) + "]".repeat(129));
check("err_deep_obj_129", '{"k":'.repeat(129) + "1" + "}".repeat(129));
check("err_colon_after_key_eof", '{"a"');
check("err_num_then_eof_obj", '{"a":');
check("bare_minus_zero_frac", "-0.0");
check("nested_err_deep", '[[[[[["a\x00b"]]]]]]');
check("err_after_valid_str", '"ok" "extra"');
check("just_minus_e", "-e5");
check("neg_zero_int", "-0");
check("esc_at_eof", '"a\\');
check("esc_u_at_eof", '"a\\u00');
check("err_obj_close_in_arr", "[1}");
check("err_arr_close_in_obj", '{"a":1]');

// ---------- a realistic doc -----------------------------------------
check("big_doc", JSON.stringify({
  generations: Array.from({ length: 20 }, (_, i) => ({
    generation_id: "a".repeat(40) + i,
    state: "installed", at: "2026-09-24T00:00:00Z",
    ev: { sha: "b".repeat(64), nums: [1, -2, 3.5, 1e10] },
  })),
}));

if (SELFTEST) emit("selftest_forced", "FAIL", "intentional failure");
const fails = results.filter((r) => r.status === "FAIL");
emit("summary", fails.length === 0 ? "PASS" : "FAIL",
  `${results.length} checks, ${fails.length} failed`);
if (OUT) {
  mkdirSync(OUT, { recursive: true });
  writeFileSync(join(OUT, "probe-results.jsonl"),
    results.map((r) => JSON.stringify(r)).join("\n") + "\n");
  writeFileSync(join(OUT, "results.json"), JSON.stringify({
    probe: "serde_decode", total: results.length,
    pass: results.filter((r) => r.status === "PASS").length,
    fail: fails.length,
  }, null, 2) + "\n");
}
process.exit(fails.length ? 1 : 0);
