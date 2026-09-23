// NM-021 P2: validate src_node/parsers/tdxhy.mjs against a live Rust
// oracle — synthetic edges + retained tdxhy.cfg member digest.
// Usage: node scripts/node_migration_p2/tdxhy_probe.mjs <worktree-root>
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { inflateRawSync } from "node:zlib";
import { join } from "node:path";
import { parse, TdxhyError } from "../../src_node/parsers/tdxhy.mjs";

const ROOT = process.argv[2] || ".";
const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/tdxhy/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

const emit = (id, status, detail, expected = null, actual = null) =>
  console.log(JSON.stringify({ family: "tdxhy", id, status, detail, expected, actual }));
const sha256 = (buf) => createHash("sha256").update(buf).digest("hex");
const B = (s) => Buffer.from(s, "latin1");

const CASES = {
  empty_file: B(""),
  single_valid: B("1|600006|T01|||X0101\r\n"),
  no_trailing_crlf: B("0|000001|T0102|||X01"),
  multi_valid: B("1|600006|T01|||X0101\r\n0|000001|T0102|||X01\r\n2|899001||||\r\n"),
  t_len3: B("1|600006|T01|||\r\n"),
  t_len5: B("1|600006|T0102|||\r\n"),
  t_len7: B("1|600006|T010203|||\r\n"),
  x_len3: B("1|600006||||X01\r\n"),
  x_len7: B("1|600006||||X010203\r\n"),
  both_empty: B("1|600006||||\r\n"),
  non_ascii: Buffer.concat([B("1|600006|T01|||X01\r\n1|600007|T01|||X"), Buffer.from([0xe4, 0xb8, 0xad]), B("1\r\n")]),
  lf_only: B("1|600006|T01|||X01\n1|600007|T01|||X01\n"),
  empty_middle_line: B("1|600006|T01|||X01\r\n\r\n1|600007|T01|||X01\r\n"),
  double_trailing_crlf: B("1|600006|T01|||X01\r\n\r\n"),
  field_count_5: B("1|600006|T01||X01\r\n"),
  field_count_7: B("1|600006|T01|||X01|\r\n"),
  market_3: B("3|600006|T01|||X01\r\n"),
  market_empty: B("|600006|T01|||X01\r\n"),
  market_two_digits: B("10|600006|T01|||X01\r\n"),
  code_5_digits: B("1|60006|T01|||X01\r\n"),
  code_7_digits: B("1|6000060|T01|||X01\r\n"),
  code_letter: B("1|60000a|T01|||X01\r\n"),
  t_len2: B("1|600006|T1|||X01\r\n"),
  t_len4: B("1|600006|T123|||X01\r\n"),
  t_len6: B("1|600006|T12345|||X01\r\n"),
  t_len8: B("1|600006|T1234567|||X01\r\n"),
  t_wrong_letter: B("1|600006|S01|||X01\r\n"),
  t_non_digit: B("1|600006|T0a|||X01\r\n"),
  x_len4: B("1|600006||||X123\r\n"),
  x_wrong_letter: B("1|600006||||T01\r\n"),
  x_non_digit: B("1|600006||||X0a\r\n"),
  f3_nonempty: B("1|600006|T01|x||X01\r\n"),
  f4_nonempty: B("1|600006|T01||y|X01\r\n"),
  precedence_market_first: B("9|abc|z||T|X\r\n"),
  precedence_f3_before_f4: B("1|600006|T01|a|b|X01\r\n"),
  precedence_t_before_reserved: B("1|600006|bad|x||T01\r\n"),
};

for (const o of oracle.filter((r) => !r.id.startsWith("member") && r.id !== "tdxhy_member")) {
  try {
    const rows = parse(CASES[o.id]);
    const ok = o.status === "ok" && JSON.stringify(rows) === JSON.stringify(o.rows);
    emit(o.id, ok ? "PASS" : "FAIL", ok ? null : "mismatch", o.rows, rows);
  } catch (e) {
    const ok = o.status === "error" && e instanceof TdxhyError && e.message === o.error;
    emit(o.id, ok ? "PASS" : "FAIL", ok ? null : `got ${e.name}(${e.kind}): ${e.message}`, o.error, e.message);
  }
}

// ---------- typed boundary ----------
const bnd = (id, v) => {
  try { parse(v); emit(id, "FAIL", "accepted"); }
  catch (e) { emit(id, e.kind === "invalid_input" ? "PASS" : "FAIL", `${e.kind}: ${e.message}`); }
};
bnd("reject_string", "1|600006|T01|||X01\r\n");
bnd("reject_array", [1]);
bnd("reject_null", null);
{ try { parse(new Uint8Array(B("1|600006|T01|||X01\r\n"))); emit("accept_uint8array", "PASS", null); }
  catch (e) { emit("accept_uint8array", "FAIL", e.message); } }

// ---------- retained tdxhy.cfg ----------
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
const member = zipMember(readFileSync(ZIP), "tdxhy.cfg");
emit("member_extract", member.length === 150332 ? "PASS" : "FAIL", `${member.length}B sha256=${sha256(member)}`);
const rows = parse(member);
{
  const h = createHash("sha256");
  for (const r of rows) h.update(`${r.line},${r.market},${r.code},${r.t_code ?? ""},${r.x_code ?? ""}\n`);
  const markets = {};
  for (const r of rows) markets[r.market] = (markets[r.market] ?? 0) + 1;
  const bm = oracle.find((r) => r.id === "tdxhy_member");
  const got = h.digest("hex");
  const ok = rows.length === bm.rows && got === bm.sha256 &&
    JSON.stringify(markets) === JSON.stringify(bm.markets);
  emit("tdxhy_member", ok ? "PASS" : "FAIL",
    `rows=${rows.length} sha256=${got}`, { rows: bm.rows, sha256: bm.sha256, markets: bm.markets }, markets);
}
for (const o of oracle.filter((r) => r.id.startsWith("member_row_"))) {
  const i = Number(o.id.slice(11));
  const ok = JSON.stringify(rows[i]) === JSON.stringify(o.row);
  emit(o.id, ok ? "PASS" : "FAIL", ok ? null : "mismatch", o.row, rows[i]);
}
