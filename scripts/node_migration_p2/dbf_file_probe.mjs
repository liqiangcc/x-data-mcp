// NM-020 P2: validate src_node/parsers/dbf_file.mjs against a live Rust
// oracle — synthetic edges + retained base.dbf member digest.
// Usage: node scripts/node_migration_p2/dbf_file_probe.mjs <worktree-root>
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { inflateRawSync } from "node:zlib";
import { join } from "node:path";
import { parse, DbfError, fieldBytes } from "../../src_node/parsers/dbf_file.mjs";

const ROOT = process.argv[2] || ".";
const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/dbf_file/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

const emit = (id, status, detail, expected = null, actual = null) =>
  console.log(JSON.stringify({ family: "dbf_file", id, status, detail, expected, actual }));
const sha256 = (buf) => createHash("sha256").update(buf).digest("hex");

// fixture builder — mirrors the oracle's mk()
const mk = (ver, fields, recs) => {
  const declared = 1 + fields.reduce((a, f) => a + f[2], 0);
  const rlen = Math.max(declared, ...recs.map((r) => r.length));
  const hlen = 33 + 32 * fields.length;
  const b = Buffer.alloc(hlen + rlen * recs.length);
  b[0] = ver; b[1] = 125; b[2] = 9; b[3] = 14;
  b.writeUInt32LE(recs.length, 4); b.writeUInt16LE(hlen, 8); b.writeUInt16LE(rlen, 10);
  b[29] = 0x57;
  fields.forEach(([name, ty, len, dec], i) => {
    const o = 32 + 32 * i;
    b.write(name, o, "latin1"); b[o + 11] = ty; b[o + 16] = len; b[o + 17] = dec;
  });
  b[hlen - 1] = 0x0d;
  recs.forEach((r, i) => Buffer.from(r).copy(b, hlen + i * rlen));
  return b;
};

const view = (f) => ({
  code_page: f.code_page,
  fields: f.fields.map((d) => ({ decimals: d.decimals, length: d.length, name: d.name, type: d.field_type })),
  header_length: f.header_length,
  last_update: f.last_update,
  record_count: f.record_count,
  record_length: f.record_length,
  records: f.records.length,
  version: f.version,
});

const CASES = {
  zero_fields_zero_records: mk(0x03, [], []),
  one_field_zero_records: mk(0x03, [["CODE", 0x43, 10, 0]], []),
  two_fields_one_record: mk(0x83, [["CODE", 0x43, 6, 0], ["PRICE", 0x4e, 8, 2]], [Buffer.from(" 600006123.45", "latin1")]),
  deleted_marker: mk(0x03, [["X", 0x43, 4, 0]], [Buffer.from("*abcd", "latin1"), Buffer.from(" efgh", "latin1")]),
  vendor_tail: mk(0x30, [["A", 0x43, 2, 0]], [Buffer.from(" abTAIL!", "latin1")]),
  unsupported_version: mk(0x02, [["A", 0x43, 2, 0]], []),
  short_file: Buffer.from([0x03, 0, 0]),
  empty_file: Buffer.alloc(0),
  header_len_too_small: (() => { const b = mk(0x03, [["A", 0x43, 2, 0]], []); b.writeUInt16LE(20, 8); return b; })(),
  record_len_zero: (() => { const b = mk(0x03, [["A", 0x43, 2, 0]], []); b.writeUInt16LE(0, 10); return b; })(),
  header_beyond_file: (() => { const b = mk(0x03, [["A", 0x43, 2, 0]], []); b.writeUInt16LE(500, 8); return b; })(),
  missing_terminator: (() => { const b = mk(0x03, [["A", 0x43, 2, 0]], []); b[b.readUInt16LE(8) - 1] = 0x1a; return b; })(),
  zero_length_field: mk(0x03, [["A", 0x43, 0, 0]], []),
  fields_exceed_record: (() => { const b = mk(0x03, [["A", 0x43, 5, 0]], []); b.writeUInt16LE(3, 10); return b; })(),
  body_truncated: (() => { const b = mk(0x03, [["A", 0x43, 2, 0]], [Buffer.from(" ab", "latin1")]); return b.subarray(0, b.length - 1); })(),
  surplus_tail_ok: (() => { const b = mk(0x03, [["A", 0x43, 2, 0]], [Buffer.from(" ab", "latin1")]); return Buffer.concat([b, Buffer.from([0x1a, 0xee])]); })(),
  descriptor_ragged: (() => { const b = mk(0x03, [["A", 0x43, 2, 0]], []); const n = Buffer.alloc(40); b.copy(n, 0, 0, 40); n.writeUInt16LE(40, 8); n[39] = 0x0d; return n; })(),
  utf8_lossy_name: (() => { const b = mk(0x03, [["X", 0x43, 3, 0]], []); b.set([0xef, 0xbf, 0xbd, 0xff], 32); return b; })(),
  name_no_nul_11: mk(0x03, [["ABCDEFGHIJK", 0x43, 3, 0]], []),
};

for (const o of oracle.filter((r) => !r.id.startsWith("base"))) {
  try {
    const f = parse(CASES[o.id]);
    const actual = view(f);
    const expected = { code_page: o.code_page, fields: o.fields, header_length: o.header_length,
      last_update: o.last_update, record_count: o.record_count, record_length: o.record_length,
      records: o.records, version: o.version };
    const ok = o.status === "ok" && JSON.stringify(actual) === JSON.stringify(expected);
    emit(o.id, ok ? "PASS" : "FAIL", ok ? null : "mismatch", expected, actual);
  } catch (e) {
    const ok = o.status === "error" && e instanceof DbfError && e.message === o.error;
    emit(o.id, ok ? "PASS" : "FAIL", ok ? null : `got ${e.name}(${e.kind}): ${e.message}`, o.error, e.message);
  }
}

// ---------- typed boundary ----------
const bnd = (id, v) => {
  try { parse(v); emit(id, "FAIL", "accepted"); }
  catch (e) { emit(id, e.kind === "invalid_input" ? "PASS" : "FAIL", `${e.kind}: ${e.message}`); }
};
bnd("reject_string", "x");
bnd("reject_array", [1, 2]);
bnd("reject_null", null);
bnd("reject_number", 42);
{ // Uint8Array input also accepted (typed boundary)
  try { parse(new Uint8Array(mk(0x03, [], []))); emit("accept_uint8array", "PASS", null); }
  catch (e) { emit("accept_uint8array", "FAIL", e.message); }
}

// ---------- field_bytes semantics ----------
{
  const f = parse(mk(0x83, [["CODE", 0x43, 6, 0], ["PRICE", 0x4e, 8, 2]], [Buffer.from(" 600006123.45", "latin1")]));
  const r = f.records[0];
  const h = (u) => u === undefined ? null : Buffer.from(u).toString("hex");
  const got = [h(fieldBytes(f.fields, r.raw, 0)), h(fieldBytes(f.fields, r.raw, 1)), h(fieldBytes(f.fields, r.raw, 2)), h(fieldBytes(f.fields, r.raw, 99))];
  // marker ' ' consumed; raw = "600006" + "123.45" + 2 zero tail bytes
  const want = [Buffer.from("600006", "latin1").toString("hex"), Buffer.from("123.45\0\0", "latin1").toString("hex"), null, null];
  emit("field_bytes_slices", JSON.stringify(got) === JSON.stringify(want) ? "PASS" : "FAIL", null, want, got);
}

// ---------- retained base.dbf ----------
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
const member = zipMember(readFileSync(ZIP), "base.dbf");
emit("member_extract", member.length === 4270189 ? "PASS" : "FAIL", `${member.length}B sha256=${sha256(member)}`);
const bm = oracle.find((r) => r.id === "base_member");
const bf = parse(member);
{
  const h = createHash("sha256");
  for (const r of bf.records) { h.update(Buffer.from([r.deleted ? 1 : 0])); h.update(r.raw); }
  const fh = createHash("sha256");
  for (const d of bf.fields) fh.update(`${d.name},${d.field_type},${d.length},${d.decimals};`);
  const ok = bf.version === bm.version && bf.record_count === bm.record_count &&
    bf.header_length === bm.header_length && bf.record_length === bm.record_length &&
    bf.code_page === bm.code_page && bf.fields.length === bm.fields &&
    h.digest("hex") === bm.records_sha256 && fh.digest("hex") === bm.fields_sha256;
  emit("base_member", ok ? "PASS" : "FAIL",
    `v${bf.version} recs=${bf.record_count} hdr=${bf.header_length} rlen=${bf.record_length} cp=${bf.code_page} fields=${bf.fields.length}`,
    { records_sha256: bm.records_sha256, fields_sha256: bm.fields_sha256 });
}
for (const o of oracle.filter((r) => r.id.startsWith("base_fb_"))) {
  const i = Number(o.id.slice(8));
  const got = fieldBytes(bf.fields, bf.records[0].raw, i);
  const hex = got === undefined ? "null" : Buffer.from(got).toString("hex");
  emit(o.id, hex === o.hex ? "PASS" : "FAIL", null, o.hex, hex);
}
