// NM-022 P2: validate src_node/parsers/ex_code.mjs against a live Rust
// oracle — GB18030 edges + retained infoharbor_ex.code member digest.
// Usage: node scripts/node_migration_p2/ex_code_probe.mjs <worktree-root>
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { inflateRawSync } from "node:zlib";
import { join } from "node:path";
import { parse, ExCodeError } from "../../src_node/parsers/ex_code.mjs";

const ROOT = process.argv[2] || ".";
const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/ex_code/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

const emit = (id, status, detail, expected = null, actual = null) =>
  console.log(JSON.stringify({ family: "ex_code", id, status, detail, expected, actual }));
const sha256 = (buf) => createHash("sha256").update(buf).digest("hex");
const gb = (s) => Buffer.from(s, "utf8"); // probe fixtures are pre-encoded GB18030 bytes
// fixture strings here are written as GB18030-equivalent bytes via explicit
// hex where needed; Chinese chars below are encoded to GB18030 by iconv-less
// construction — instead embed the GB18030 byte sequences directly.
const gb18030 = (s) => {
  // encode UTF-8 fixture string to GB18030 bytes using Node's iconv? Node
  // has no GB18030 ENCODER — build from oracle-verified byte sequences.
  throw new Error("use explicit byte fixtures");
};
// Explicit GB18030 byte fixtures (verified against encoding_rs GB18030.encode):
const hx = (h) => Buffer.from(h.replace(/\s/g, ""), "hex");
// 平安银行 = C6 BD B0 B2 D2 F8 D0 D0; 平安 = C6 BD B0 B2; 万科 = CD F2 BF C6;
// 万 科Ａ = CD F2 20 BF C6 A3 C1; 东风汽车 = B6 AB B7 E7 C6 FB B3 B5;
// 平银 = C6 BD D2 F8; PAYH/VANKE/DFQC/abc/def/k/x/y/P/Q/R/V are ASCII.
const CASES = {
  empty_file: Buffer.alloc(0),
  single_valid: hx("3030303030317cc6bdb0b2d2f8d0d07cc6bdb0b2d2f8d0d02c504159480d0a"),
  four_fields: hx("3030303030327ccdf220bfc6a3c17c56414e4b452ccdf2bfc6417c65787472610d0a"),
  no_trailing_crlf: hx("3030303030317cc6bdb0b2d2f8d0d07c50415948"),
  multi_valid: hx("3030303030317cc6bdb0b2d2f8d0d07c504159480d0a3030303030327ccdf220bfc6a3c17c56414e4b450d0a3630303030367cb6abb7e7c6fbb3b57c444651430d0a"),
  decode_ascii_single: hx("62"),
  decode_lone_trail: hx("bf"),
  decode_truncated_lead: hx("3030303030317c6162637c6b0d0a81"),
  decode_ff: hx("3030303030317c6162637c6b0d0afffe"),
  decode_efbbbf: hx("efbbbf3030303030317c6162637c6b0d0a"),
  decode_bad4: hx("3030303030317c6162637c6b0d0a95328 2ff".replace(/ /g,"")),
  lf_only: hx("3030303030317c6162637c6b0a3030303030327c6465667c6b0a"),
  empty_middle_line: hx("3030303030317cc6bdb0b27c500d0a0d0a3030303030327ccdf2bfc67c560d0a"),
  double_trailing_crlf: hx("3030303030317cc6bdb0b27c500d0a0d0a"),
  field_count_2: hx("3030303030317cc6bdb0b20d0a"),
  field_count_1: hx("3030303030310d0a"),
  code_5_digits: hx("30303030317cc6bdb0b27c500d0a"),
  code_7_digits: hx("303030303031327cc6bdb0b27c500d0a"),
  code_letter: hx("3030303030617cc6bdb0b27c500d0a"),
  code_with_pipe_extra: hx("3030303030317cc6bdb0b27c507c517c520d0a"),
  empty_name: hx("3030303030317c7c500d0a"),
  duplicate_code: hx("3030303030317cc6bdb0b27c500d0a3030303030327ccdf2bfc67c560d0a3030303030317cc6bdd2f87c500d0a"),
  duplicate_adjacent: hx("3030303030317cc6bdb0b27c500d0a3030303030317cc6bdd2f87c500d0a"),
  precedence_decode_first: hx("3030303030317cc6bdb0b27c500d0aff"),
  esc_code_quote: hx("3030303030227cc6bdb0b27c500d0a"),
  esc_code_backslash: hx("303030305c307cc6bdb0b27c500d0a"),
  esc_code_tab: hx("3030303009307cc6bdb0b27c500d0a"),
  esc_code_nul: hx("3030303000307cc6bdb0b27c500d0a"),
};

for (const o of oracle.filter((r) => !r.id.startsWith("member") && r.id !== "excode_member")) {
  try {
    const rows = parse(CASES[o.id]);
    const ok = o.status === "ok" && JSON.stringify(rows) === JSON.stringify(o.rows);
    emit(o.id, ok ? "PASS" : "FAIL", ok ? null : "mismatch", o.rows, rows);
  } catch (e) {
    const ok = o.status === "error" && e instanceof ExCodeError && e.message === o.error;
    emit(o.id, ok ? "PASS" : "FAIL", ok ? null : `got ${e.name}(${e.kind}): ${e.message}`, o.error, e.message);
  }
}

// ---------- typed boundary ----------
const bnd = (id, v) => {
  try { parse(v); emit(id, "FAIL", "accepted"); }
  catch (e) { emit(id, e.kind === "invalid_input" ? "PASS" : "FAIL", `${e.kind}: ${e.message}`); }
};
bnd("reject_string", "000001|a|b\r\n");
bnd("reject_null", null);
{ try { parse(new Uint8Array(CASES.single_valid)); emit("accept_uint8array", "PASS", null); }
  catch (e) { emit("accept_uint8array", "FAIL", e.message); } }

// ---------- retained infoharbor_ex.code ----------
const ZIP = "/home/box/retained-inputs/official-base.zip";
emit("manifest_zip", sha256(readFileSync(ZIP)) === "dcdf6b49898926675698263535021aed7ad92b34be82a1a9e8a2a8982bd68bc6" ? "PASS" : "FAIL", "official-base.zip sha256");
function zipMember(buf, name) {
  const eocd = buf.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06]));
  const cdCount = buf.readUInt16LE(eocd + 10);
  let off = buf.readUInt32LE(eocd + 16);
  for (let i = 0; i < cdCount; i++) {
    const method = buf.readUInt16LE(off + 10);
    const compSize = buf.readUInt32LE(off + 20);
    const nameLen = buf.readUInt16LE(off + 28), extraLen = buf.readUInt16LE(off + 30), cmtLen = buf.readUInt16LE(off + 32);
    const lho = buf.readUInt32LE(off + 42);
    const entry = buf.subarray(off + 46, off + 46 + nameLen).toString("latin1");
    if (entry === name) {
      const lNameLen = buf.readUInt16LE(lho + 26), lExtraLen = buf.readUInt16LE(lho + 28);
      const data = buf.subarray(lho + 30 + lNameLen + lExtraLen, lho + 30 + lNameLen + lExtraLen + compSize);
      return method === 0 ? data : inflateRawSync(data);
    }
    off += 46 + nameLen + extraLen + cmtLen;
  }
  throw new Error(`member not found: ${name}`);
}
const member = zipMember(readFileSync(ZIP), "infoharbor_ex.code");
emit("member_extract", member.length === 182145 ? "PASS" : "FAIL", `${member.length}B sha256=${sha256(member)}`);
const rows = parse(member);
{
  const h = createHash("sha256");
  for (const r of rows) h.update(`${r.line},${r.code},${r.name}\n`);
  const bm = oracle.find((r) => r.id === "excode_member");
  const got = h.digest("hex");
  emit("excode_member", rows.length === bm.rows && got === bm.sha256 ? "PASS" : "FAIL",
    `rows=${rows.length} sha256=${got}`, { rows: bm.rows, sha256: bm.sha256 });
}
for (const o of oracle.filter((r) => r.id.startsWith("member_row_"))) {
  const i = Number(o.id.slice(11));
  const ok = JSON.stringify(rows[i]) === JSON.stringify(o.row);
  emit(o.id, ok ? "PASS" : "FAIL", ok ? null : "mismatch", o.row, rows[i]);
}
