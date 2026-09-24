// NM-058 MCP transport differential probe.
//
// Oracle: the real Rust `mcp_server` binary (target/debug, built via the
// pinned rustup toolchain) fed the same .lines scenario over stdin —
// black-box byte comparison of stdout, no Rust source changes needed.
// Node side: mcp_transport_runner.mjs -> runStdio + stub handler.
//
// Emits one JSONL verdict per check:
//   - scenario line N: PASS/FAIL (byte-equal response lines)
//   - serde unit vectors (float echo formats, parse-error strings)
//   - eof_lines / utf8_lines framing checks
//   - tools_list_bytes / summary
// exit 0 iff all PASS. `diff` selftest arg appends an intentional FAIL and
// exits 1.
import { execFileSync, execSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { parseJsonBytes, serValue, F64 } from "../../src_node/mcp/serde_json.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const ROOT = process.argv[2] ? join(process.cwd(), process.argv[2]) : join(here, "..", "..");
const SELFTEST = process.argv.includes("diff");
const CARGO = process.env.CARGO || join(process.env.HOME, ".cargo/bin/cargo");
const SERVER = join(ROOT, "target/debug/mcp_server");
const results = [];
const emit = (id, status, detail) => {
  results.push({ family: "mcp_transport", id, status, detail });
};
const line = (r) => JSON.stringify(r);

// --- build oracle -------------------------------------------------------
execSync(`${CARGO} build --bin mcp_server`, { cwd: ROOT, stdio: "pipe" });
const store = mkdtempSync(join(tmpdir(), "nm58-store-"));
const env = { ...process.env, X_DATA_MCP_STORE: store };

const runSide = (cmd, args, bytes, cwd) => {
  try {
    const stdout = execFileSync(cmd, args, { input: bytes, env, cwd, stdio: ["pipe", "pipe", "pipe"] });
    return { code: 0, stdout };
  } catch (e) {
    return { code: e.status ?? 1, stdout: e.stdout ?? Buffer.alloc(0), stderr: e.stderr };
  }
};

// --- main scenario: byte-for-byte stdout diff ---------------------------
const scenarioPath = join(here, "scenarios/mcp_transport.lines");
const scenario = readFileSync(scenarioPath);
const rust = runSide(SERVER, [], scenario, ROOT);
const node = runSide("node", [join(here, "mcp_transport_runner.mjs")], scenario, ROOT);
const rustLines = rust.stdout.toString("utf8").split("\n").filter((l) => l.length);
const nodeLines = node.stdout.toString("utf8").split("\n").filter((l) => l.length);
const inLines = scenario.toString("utf8").split("\n").filter((l) => l.length);

emit("scenario_input", "PASS", `${inLines.length} request lines`);
emit("rust_exit", rust.code === 0 ? "PASS" : "FAIL", `exit ${rust.code}`);
emit("node_exit", node.code === 0 ? "PASS" : "FAIL", `exit ${node.code}`);
emit(
  "line_count",
  rustLines.length === nodeLines.length ? "PASS" : "FAIL",
  `rust=${rustLines.length} node=${nodeLines.length}`,
);
const n = Math.max(rustLines.length, nodeLines.length);
let diffs = 0;
for (let i = 0; i < n; i++) {
  const ok = rustLines[i] === nodeLines[i];
  if (!ok) diffs++;
  emit(`line_${i + 1}`, ok ? "PASS" : "FAIL",
    ok ? `${rustLines[i].length}B` : `rust=${JSON.stringify((rustLines[i] ?? "").slice(0, 120))} node=${JSON.stringify((nodeLines[i] ?? "").slice(0, 120))}`);
}
emit("byte_parity", diffs === 0 && rustLines.length === nodeLines.length ? "PASS" : "FAIL",
  `${diffs} diffs over ${n} output lines`);

// --- framing: no trailing newline ---------------------------------------
const eofScenario = readFileSync(join(here, "scenarios/mcp_transport_eof.lines"));
const eofR = runSide(SERVER, [], eofScenario, ROOT);
const eofN = runSide("node", [join(here, "mcp_transport_runner.mjs")], eofScenario, ROOT);
emit("eof_lines", eofR.code === eofN.code && Buffer.compare(eofR.stdout, eofN.stdout) === 0
  ? "PASS" : "FAIL", `exit rust=${eofR.code} node=${eofN.code}`);

// --- framing: invalid UTF-8 aborts both ---------------------------------
const u8Scenario = readFileSync(join(here, "scenarios/mcp_transport_utf8.lines"));
const u8R = runSide(SERVER, [], u8Scenario, ROOT);
const u8N = runSide("node", [join(here, "mcp_transport_runner.mjs")], u8Scenario, ROOT);
emit("utf8_lines", u8R.code !== 0 && u8N.code !== 0 && Buffer.compare(u8R.stdout, u8N.stdout) === 0
  ? "PASS" : "FAIL", `exit rust=${u8R.code} node=${u8N.code}`);

// --- serde unit vectors (golden, extracted from the oracle) -------------
const vectors = [
  // [line, expected message] — serde_json::from_str error strings
  ["", "EOF while parsing a value at line 1 column 0"],
  ["{", "EOF while parsing an object at line 1 column 1"],
  ["[1,", "EOF while parsing a value at line 1 column 3"],
  ['{"a":1,', "EOF while parsing a value at line 1 column 7"],
  ['{"a":1', "EOF while parsing an object at line 1 column 6"],
  ['{"a":1,"b"', "EOF while parsing an object at line 1 column 10"],
  ['{"a":"\\u12', "EOF while parsing a string at line 1 column 10"],
  ["x", "expected value at line 1 column 1"],
  ['{"a":1,}', "trailing comma at line 1 column 8"],
  ["[1,]", "trailing comma at line 1 column 4"],
  ['{"a":1 "b":2}', "expected `,` or `}` at line 1 column 8"],
  ["[1 2]", "expected `,` or `]` at line 1 column 4"],
  ["{a:1}", "key must be a string at line 1 column 2"],
  ['{"a":nul}', "expected ident at line 1 column 9"],
  ['{"a": tru}', "expected ident at line 1 column 10"],
  ['{"a":01}', "invalid number at line 1 column 7"],
  ['{"a":1e}', "invalid number at line 1 column 8"],
  ['{"a":1e309}', "number out of range at line 1 column 10"],
  ['{"a":1}}', "trailing characters at line 1 column 8"],
  ['{"a" 1}', "expected `:` at line 1 column 6"],
  ['{"a":"\\uD800\\uD800"}', "lone leading surrogate in hex escape at line 1 column 18"],
  ['{"a":"\\udc00"}', "lone leading surrogate in hex escape at line 1 column 12"],
  ['{"a":"\\uD800x"}', "unexpected end of hex escape at line 1 column 13"],
  ['{"a":"b\\q"}', "invalid escape at line 1 column 9"],
  ['{"a":"b\\x01c"}', "invalid escape at line 1 column 9"],
  ['{"a":"b\x01c"}', "control character (\\u0000-\\u001F) found while parsing a string at line 1 column 8"],
];
let vpass = 0;
for (const [input, want] of vectors) {
  const got = parseJsonBytes(input);
  const ok = !got.ok && got.message === want;
  if (ok) vpass++;
  else emit(`vec_${JSON.stringify(input).slice(0, 30)}`, "FAIL", `want ${want} got ${got.ok ? "ok" : got.message}`);
}
emit("parse_vectors", vpass === vectors.length ? "PASS" : "FAIL", `${vpass}/${vectors.length}`);

const f64s = [
  [1.0, "1.0"], [-0.0, "-0.0"], [0.0, "0.0"], [1e300, "1e+300"], [1e21, "1e+21"],
  [1e16, "1e+16"], [1e15, "1000000000000000.0"], [0.000001, "1e-6"],
  [0.00001, "0.00001"], [1.2e-5, "0.000012"], [0.1, "0.1"], [200.0, "200.0"],
  [123456789.123, "123456789.123"], [5e-324, "5e-324"], [1e-999, "0.0"],
  [1234567890123456.0, "1234567890123456.0"], [12345678901234568.0, "1.2345678901234568e+16"],
  [3.141592653589793, "3.141592653589793"], [0.30000000000000004, "0.30000000000000004"],
];
let fpass = 0;
for (const [v, want] of f64s) {
  const got = serValue(new F64(v));
  if (got === want) fpass++;
  else emit(`f64_${v}`, "FAIL", `want ${want} got ${got}`);
}
emit("f64_vectors", fpass === f64s.length ? "PASS" : "FAIL", `${fpass}/${f64s.length}`);

// int echo: BigInt + safe ints
const echo = serValue({ v: parseJsonBytes('{"v":9007199254740993}').value.v });
emit("bigint_echo", echo === '{"v":9007199254740993}' ? "PASS" : "FAIL", echo);

// --- summary -------------------------------------------------------------
const fails = results.filter((r) => r.status === "FAIL").length;
emit("summary", fails === 0 ? "PASS" : "FAIL", `${results.length} checks, ${fails} failed`);
if (SELFTEST) emit("selftest_forced", "FAIL", "intentional failure for negative selftest");

for (const r of results) console.log(line(r));
rmSync(store, { recursive: true, force: true });
process.exit(results.some((r) => r.status === "FAIL") ? 1 : 0);
