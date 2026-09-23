// NM-023 P2: validate src_node/catalog/scope.mjs against a live Rust
// oracle — partition branches + retained tdxhy.cfg scoped digests.
// Usage: node scripts/node_migration_p2/catalog_scope_probe.mjs <worktree-root>
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { inflateRawSync } from "node:zlib";
import { join } from "node:path";
import { apply, reconciliation, ScopeBoundaryError } from "../../src_node/catalog/scope.mjs";
import { parse as parseTdxhy } from "../../src_node/parsers/tdxhy.mjs";

const ROOT = process.argv[2] || ".";
const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/catalog_scope/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

const emit = (id, status, detail, expected = null, actual = null) =>
  console.log(JSON.stringify({ family: "catalog_scope", id, status, detail, expected, actual }));
const sha256 = (buf) => createHash("sha256").update(buf).digest("hex");

const row = (line, market, code, t_code, x_code) => ({ code, line, market, t_code, x_code });
const CASES = {
  empty: [],
  all_scoped: [row(1, 0, "000001", "T01", "X01"), row(2, 1, "600006", "T0102", "X0102")],
  bj_with_codes: [row(1, 2, "899001", "T01", "X01")],
  bj_missing_codes: [row(1, 2, "899001", null, null)],
  missing_t_only_bshare: [row(1, 0, "200001", null, "X01")],
  missing_t_only_anom: [row(1, 0, "000001", null, "X01")],
  missing_x_only_bshare: [row(1, 1, "900001", "T01", null)],
  missing_x_only_anom: [row(1, 1, "600006", "T01", null)],
  missing_both_bshare: [row(1, 0, "200002", null, null)],
  missing_both_anom: [row(1, 0, "300001", null, null)],
  prefix_boundary_199: [row(1, 0, "199999", null, null)],
  prefix_boundary_200: [row(1, 0, "200000", null, null)],
  prefix_boundary_201: [row(1, 0, "201999", null, null)],
  prefix_boundary_899: [row(1, 1, "899999", null, null)],
  prefix_boundary_900: [row(1, 1, "900000", null, null)],
  prefix_boundary_901: [row(1, 1, "901999", null, null)],
  mixed_order: [
    row(1, 0, "000001", "T01", "X01"), row(2, 2, "899001", "T02", "X02"),
    row(3, 0, "200003", null, "X01"), row(4, 1, "600006", "T01", null),
    row(5, 1, "600007", "T01", "X01"), row(6, 0, "300003", null, null),
  ],
};

for (const o of oracle.filter((r) => !r.id.startsWith("member") && r.id !== "tdxhy_member")) {
  const rows = CASES[o.id];
  const out = apply(rows);
  const rec = reconciliation(out, rows.length);
  const actual = { excluded: out.excluded, members: out.members, reconciliation: rec };
  const expected = { excluded: o.excluded, members: o.members, reconciliation: o.reconciliation };
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  emit(o.id, ok ? "PASS" : "FAIL", ok ? null : "mismatch", expected, actual);
}

// ---------- typed boundary ----------
const bnd = (id, rows, total) => {
  try { const out = apply(rows); reconciliation(out, total ?? rows.length); emit(id, "FAIL", "accepted"); }
  catch (e) { emit(id, e.kind === "typed_boundary" ? "PASS" : "FAIL", `${e.name}(${e.kind}): ${e.message}`); }
};
bnd("reject_row_null", [null]);
bnd("reject_row_array", [[1]]);
bnd("reject_line_zero", [row(0, 0, "000001", "T01", "X01")]);
bnd("reject_line_float", [row(1.5, 0, "000001", "T01", "X01")]);
bnd("reject_market_3", [row(1, 3, "000001", "T01", "X01")]);
bnd("reject_market_neg", [row(1, -1, "000001", "T01", "X01")]);
bnd("reject_code_short", [row(1, 0, "00001", "T01", "X01")]);
bnd("reject_code_num", [row(1, 0, 1, "T01", "X01")]);
bnd("reject_tcode_num", [row(1, 0, "000001", 5, "X01")]);
bnd("reject_xcode_undef", [{ code: "000001", line: 1, market: 0, t_code: "T01" }]);
bnd("reject_rows_string", "x");
{ // reconciliation identity violated -> fail closed
  try { const out = apply(CASES.mixed_order); reconciliation(out, 999); emit("recon_mismatch_reject", "FAIL", "accepted"); }
  catch (e) { emit("recon_mismatch_reject", e.kind === "typed_boundary" ? "PASS" : "FAIL", e.message); }
}

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
const rows = parseTdxhy(member);
const out = apply(rows);
const rec = reconciliation(out, rows.length);
{
  const mh = createHash("sha256");
  for (const m of out.members) mh.update(`${m.line},${m.market},${m.code},${m.t_code},${m.x_code}\n`);
  const eh = createHash("sha256");
  for (const e of out.excluded) eh.update(`${e.line},${e.market},${e.code},${e.class}\n`);
  const bm = oracle.find((r) => r.id === "tdxhy_member");
  const mgot = mh.digest("hex"), egot = eh.digest("hex");
  const ok = out.members.length === bm.members && out.excluded.length === bm.excluded &&
    mgot === bm.members_sha256 && egot === bm.excluded_sha256 &&
    JSON.stringify(rec) === JSON.stringify(bm.reconciliation);
  emit("tdxhy_member", ok ? "PASS" : "FAIL",
    `members=${out.members.length}(${mgot.slice(0,12)}) excluded=${out.excluded.length}(${egot.slice(0,12)})`,
    { members_sha256: bm.members_sha256, excluded_sha256: bm.excluded_sha256, reconciliation: bm.reconciliation },
    { members_sha256: mgot, excluded_sha256: egot, reconciliation: rec });
}
for (const o of oracle.filter((r) => r.id.startsWith("member_ex_"))) {
  const i = Number(o.id.slice(10));
  const ok = JSON.stringify(out.excluded[i]) === JSON.stringify(o.row);
  emit(o.id, ok ? "PASS" : "FAIL", ok ? null : "mismatch", o.row, out.excluded[i]);
}
