// NM-024 P2: validate src_node/parsers/incon.mjs against a live Rust
// oracle — synthetic sections/helpers + official-client-deb member.
// Usage: node scripts/node_migration_p2/incon_probe.mjs <worktree-root>
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { parse, InconError, codeLevel, researchLevel2, sectionMap } from "../../src_node/parsers/incon.mjs";

const ROOT = process.argv[2] || ".";
const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/incon/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

const emit = (id, status, detail, expected = null, actual = null) =>
  console.log(JSON.stringify({ family: "incon", id, status, detail, expected, actual }));
const sha256 = (buf) => createHash("sha256").update(buf).digest("hex");
const hx = (h) => Buffer.from(h.replace(/\s/g, ""), "hex");

// GB18030 fixture bytes (iconv-verified):
// 农、林、牧、渔业 = c5a9a1a2c1d6a1a2c4c1d2b5d2e6b5b5b5b5b5b5? — built via hex
// A|农、林、牧、渔业 -> 417cc5a9a1a2c1d6a1a2c4c1d2b5d2e6b5b5? — use python-generated hex:
// 农 c5a9 、 a1a2 林 c1d6 牧 c4c1 渔 d3e6 业 d2b5 中 d6d0
const CASES = {
  empty_file: Buffer.alloc(0),
  header_only: hx("235345430d0a"),
  single_section: hx("235a4a4848590d0a417cc5a9a1a2c1d6a1a2c4c1a1a2d3e6d2b50d0a4130317cc5a9d2b50d0a"),
  multi_section: hx("2353310d0a30317c6e616d65310d0a2353320d0a30327c6e616d65320d0a"),
  extra_columns: hx("235345430d0a30317c6e616d657c63337c63347c63350d0a30327c6e320d0a"),
  trailing_empty_section: hx("2353310d0a30317c6e616d650d0a2323232323230d0a"),
  empty_section_name: hx("230d0a30317c6e616d650d0a"),
  dup_same_section: hx("23530d0a30317c610d0a30327c620d0a30317c630d0a"),
  dup_across_sections_ok: hx("2353310d0a30317c610d0a2353320d0a30317c620d0a"),
  entry_before_header: hx("30317c6e616d650d0a23530d0a"),
  empty_middle_line: hx("23530d0a30317c610d0a0d0a30327c620d0a"),
  malformed_1_field: hx("23530d0a30316f6e6c790d0a"),
  empty_code: hx("23530d0a7c6e616d650d0a"),
  nonascii_code: hx("23530d0a30d6d07c6e616d650d0a"),
  empty_name: hx("23530d0a30317c0d0a"),
  decode_bad: hx("23530d0a30317c610d0a95328 2ff".replace(/ /g,"")),
  precedence_decode: hx("23530d0aff"),
  precedence_fields_before_code: hx("23530d0ad6d00d0a"),
  lf_only: hx("23530d0a30317c610a30327c620a"),
  double_trailing_crlf: hx("23530d0a30317c610d0a0d0a"),
  dup_esc_quote: hx("23530d0a30227c610d0a30227c620d0a"),
  dup_esc_section: hx("23535c780d0a30317c610d0a30317c620d0a"),
};

for (const o of oracle.filter((r) => !r.id.startsWith("helper_") && !r.id.startsWith("secmap") && r.id !== "incon_member")) {
  try {
    const sections = parse(CASES[o.id]);
    const ok = o.status === "ok" && JSON.stringify(sections) === JSON.stringify(o.sections);
    emit(o.id, ok ? "PASS" : "FAIL", ok ? null : "mismatch", o.sections, sections);
  } catch (e) {
    const ok = o.status === "error" && e instanceof InconError && e.message === o.error;
    emit(o.id, ok ? "PASS" : "FAIL", ok ? null : `got ${e.name}(${e.kind}): ${e.message}`, o.error, e.message);
  }
}

// ---------- helpers ----------
for (const o of oracle.filter((r) => r.id.startsWith("helper_"))) {
  const code = o.id.slice(7);
  const cl = codeLevel(code), rl = researchLevel2(code);
  const ok = cl === o.code_level && rl === o.research_level2;
  emit(o.id, ok ? "PASS" : "FAIL", ok ? null : `code_level=${cl} rl2=${JSON.stringify(rl)}`,
    { code_level: o.code_level, research_level2: o.research_level2 },
    { code_level: cl, research_level2: rl });
}

// ---------- typed boundary ----------
const bnd = (id, v) => {
  try { parse(v); emit(id, "FAIL", "accepted"); }
  catch (e) { emit(id, e.kind === "invalid_input" ? "PASS" : "FAIL", `${e.kind}: ${e.message}`); }
};
bnd("reject_string", "#S\r\n01|a\r\n");
bnd("reject_null", null);
{ try { parse(new Uint8Array(CASES.single_section)); emit("accept_uint8array", "PASS", null); }
  catch (e) { emit("accept_uint8array", "FAIL", e.message); } }

// ---------- deb member (official client package — separate source, not
// the absent gen-2 base.zip member; extraction reproducible) ----------
const DEB = "/home/box/retained-inputs/official-client-com.tdx.tdxcfv_7.64_amd64.deb";
const MANIFEST = "/home/box/retained-inputs/manifest-sha256.txt";
const MEMBER_PATH = "./opt/apps/com.tdx.tdxcfv/files/tc/incon.dat";
{
  // verify deb sha256 against manifest entry
  const debSha = sha256(readFileSync(DEB));
  const manifest = readFileSync(MANIFEST, "utf8").split("\n").filter(Boolean)
    .map((l) => { const [h, ...p] = l.trim().split(/\s+/); return { h, p: p.join(" ") }; });
  const entry = manifest.find((e) => e.p === "official-client-com.tdx.tdxcfv_7.64_amd64.deb");
  emit("deb_manifest_sha", entry !== undefined && entry.h === debSha ? "PASS" : "FAIL",
    `deb sha256=${debSha} manifest=${entry?.h ?? "absent"}`);
}
const member = (() => {
  const r = spawnSync("python3", [join(ROOT, "scripts/node_migration_p2/deb_member_extract.py"), DEB, MEMBER_PATH],
    { encoding: "buffer", maxBuffer: 32 * 1024 * 1024 });
  if (r.status !== 0) throw new Error(`extractor failed (${r.status}): ${r.stderr}`);
  return r.stdout;
})();
emit("member_extract", member.length === 65853 && sha256(member) === "ca941e57a21d0717ee6108ac3ccfee5fab85289e26ee154d89b3e1cbbe8bc6c9" ? "PASS" : "FAIL",
  `deb->${MEMBER_PATH} ${member.length}B sha256=${sha256(member)} (separate source: official client deb, NOT gen-2 base.zip — member absent there)`);
const sections = parse(member);
{
  const h = createHash("sha256");
  for (const sec of sections) {
    h.update(`#${sec.name}\n`);
    for (const e of sec.entries) h.update(`${e.code}|${e.name}|${e.extra.join("|")}\n`);
  }
  const bm = oracle.find((r) => r.id === "incon_member");
  const got = h.digest("hex");
  const names = sections.map((s) => s.name), counts = sections.map((s) => s.entries.length);
  const ok = sections.length === bm.sections &&
    sections.reduce((a, s) => a + s.entries.length, 0) === bm.total_entries &&
    got === bm.sha256 && JSON.stringify(names) === JSON.stringify(bm.names) &&
    JSON.stringify(counts) === JSON.stringify(bm.counts);
  emit("incon_member", ok ? "PASS" : "FAIL",
    `sections=${sections.length} entries=${sections.reduce((a,s)=>a+s.entries.length,0)} sha256=${got}`,
    { sections: bm.sections, total: bm.total_entries, sha256: bm.sha256 },
    { sections: sections.length, total: sections.reduce((a,s)=>a+s.entries.length,0), sha256: got });
}
for (const o of oracle.filter((r) => r.id.startsWith("secmap_"))) {
  const n = o.id.slice(7);
  const m = sectionMap(sections, n);
  const got = m === null ? null : m.length;
  emit(o.id, got === o.entries ? "PASS" : "FAIL", null, o.entries, got);
}
// first-match section_map on the repeated "#####" sections
{
  const m = sectionMap(sections, "#####");
  const first = sections.find((s) => s.name === "#####");
  emit("secmap_first_match", m !== null && m.length === first.entries.length ? "PASS" : "FAIL",
    `"#####" sections: ${sections.filter((s)=>s.name==="#####").length}, first-match entries=${m?.length}`);
}
