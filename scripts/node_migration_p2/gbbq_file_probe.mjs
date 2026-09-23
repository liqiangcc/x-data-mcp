// NM-015 P2: validate src_node/parsers/gbbq_file.mjs against a live Rust
// oracle + a bounded read-only probe of the retained official GBBQ member.
// Usage: node scripts/node_migration_p2/gbbq_file_probe.mjs <worktree-root>
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { join } from "node:path";
import { parse, GbbqFileError, RECORD_BYTES } from "../../src_node/parsers/gbbq_file.mjs";

const ROOT = process.argv[2] || ".";
const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/gbbq_file/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

const emit = (id, status, detail, expected = null, actual = null) =>
  console.log(JSON.stringify({ family: "gbbq_file_parse", id, status, detail, expected, actual }));

const hx = (hex) => Buffer.from(hex, "hex");
const recHex = (r) => Buffer.from(r).toString("hex");

// ---------- Rust-oracle cases (generated live via rustc + x_data_mcp rlib) ----------
const INPUTS = {
  header_0_bytes: hx(""),
  header_1_byte: hx("01"),
  header_2_bytes: hx("0102"),
  header_3_bytes: hx("010203"),
  empty_file_valid: hx("00000000"),
  one_record: hx("01000000" + "aa".repeat(29)),
  three_records: hx("03000000" + "11".repeat(29) + "22".repeat(29) + "33".repeat(29)),
  body_short_28: hx("01000000" + "aa".repeat(28)),
  body_surplus_30: hx("01000000" + "aa".repeat(30)),
  count2_body1: hx("02000000" + "aa".repeat(29)),
  count0_with_body: hx("00000000" + "aa".repeat(29)),
  huge_count_short: hx("ffffffffaa"),
  count_0x80000000: hx("00000080" + "aa".repeat(29)),
  record_bytes_verbatim: hx("01000000" + "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d"),
};
for (const o of oracle) {
  const input = INPUTS[o.id];
  try {
    const got = parse(input);
    const actual = { record_count: got.record_count, records: got.records.map(recHex) };
    const want = { record_count: o.record_count, records: o.records };
    const ok = o.status === "ok" && JSON.stringify(actual) === JSON.stringify(want);
    emit(o.id, ok ? "PASS" : "FAIL", ok ? null : "mismatch", want, actual);
  } catch (e) {
    const ok = o.status === "error" && e instanceof GbbqFileError && e.message === o.error;
    emit(o.id, ok ? "PASS" : "FAIL", ok ? null : `expected ${JSON.stringify(o)} got ${e.name}: ${e.message}`, o.error, e.message);
  }
}

// ---------- fail-closed input-type rejects (no coercion) ----------
for (const [id, bad] of [
  ["reject_string", "01000000"], ["reject_array", [1, 0, 0, 0]],
  ["reject_number", 4], ["reject_null", null], ["reject_object", { length: 4 }],
]) {
  try {
    parse(bad);
    emit(id, "FAIL", `accepted ${typeof bad}`);
  } catch (e) {
    emit(id, e instanceof GbbqFileError && e.kind === "invalid_input" ? "PASS" : "FAIL",
      `${e.kind}: ${e.message}`);
  }
}

// ---------- no-allocation-on-declared-count ----------
{
  const before = process.memoryUsage().heapUsed;
  try { parse(hx("ffffffff" + "aa".repeat(29))); } catch { /* LengthMismatch expected */ }
  const grew = process.memoryUsage().heapUsed - before;
  emit("huge_count_no_alloc", grew < 16 * 1024 * 1024 ? "PASS" : "FAIL",
    `heap growth ${grew} bytes for count=2^32-1`);
}

// ---------- bounded read-only probe of retained official member ----------
{
  const member = "/home/box/retained-inputs/official-gbbq-member";
  const buf = readFileSync(member);
  const sha = createHash("sha256").update(buf).digest("hex");
  try {
    const f = parse(buf);
    const ok = f.record_count === (buf.length - 4) / RECORD_BYTES && f.records.length === f.record_count;
    emit("retained_member_parse", ok ? "PASS" : "FAIL",
      `sha256=${sha} count=${f.record_count} records=${f.records.length}`,
      null, { sha256: sha, record_count: f.record_count, bytes: buf.length });
  } catch (e) {
    emit("retained_member_parse", "FAIL", `${e.kind}: ${e.message} (sha256=${sha})`);
  }
}
