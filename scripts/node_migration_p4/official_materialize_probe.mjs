// NM-075 official-materialize differential probe.
//
// Byte-compares the Node port (src_node/official/official_market.mjs)
// against the live Rust implementation (official_materialize_oracle.rs,
// compiled to target/debug/nm075_official_materialize_oracle).
//
//   cargo build --lib && rustc --edition 2021 \
//     scripts/node_migration_p4/official_materialize_oracle.rs \
//     --extern x_data_mcp=target/debug/libx_data_mcp.rlib \
//     --extern chrono=$(ls target/debug/deps/libchrono-*.rlib | head -1) \
//     --extern serde_json=$(ls target/debug/deps/libserde_json-*.rlib | head -1) \
//     --extern serde=$(ls target/debug/deps/libserde-*.rlib | head -1) \
//     -L dependency=target/debug/deps \
//     -o target/debug/nm075_official_materialize_oracle
//
//   node scripts/node_migration_p4/official_materialize_probe.mjs             -> exit 0/1
//   node scripts/node_migration_p4/official_materialize_probe.mjs --selftest  -> exit 1
//   node scripts/node_migration_p4/official_materialize_probe.mjs --out DIR   -> evidence
//
// Cases: "exact" byte-compares serde batch JSON / error Display text.
// "prefix" compares only the stable error prefix — used where the inner
// text is an io::Error whose wording legitimately differs between Rust
// and Node (documented delta, same class).
// "node_unit" rows have no Rust oracle (merge_segment_bars is a private
// fn) — self-asserted against the documented fail-closed semantics.

import { execFileSync, spawnSync } from "node:child_process";
import {
  existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import {
  TdxMarketSource, SourceError, mergeSegmentBars, securityEventsDigest,
} from "../../src_node/official/official_market.mjs";
import { decodeEvents } from "../../src_node/parsers/gbbq_decode.mjs";
import { parse as parseGbbq } from "../../src_node/parsers/gbbq_file.mjs";
import { zipOpen, zipMember } from "../../src_node/lib/zip_member.mjs";

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const GEN3_MANIFEST = join(REPO, "evidence/issue-4-update-gen3-acceptance/snapshot-manifest-gen3.json");
const GEN3_ROOT = "/home/box/retained-inputs/gen3";
const DAY_FIXTURE = join(REPO, "evidence/issue-166-windows-official-behavior/raw/files/sh600004.day");
const NOW = "2026-09-24T00:00:00Z";
const ORACLE = process.env.NM075_ORACLE ?? join(REPO, "target/debug/nm075_official_materialize_oracle");
const args = process.argv.slice(2);
const SELFTEST = args.includes("--selftest");
const OUT = args.includes("--out") ? args[args.indexOf("--out") + 1] : null;

// ------------------------------------------------------------------ helpers
const sha256hex = (b) => createHash("sha256").update(b).digest("hex");
const J = JSON.stringify;
const dateStr = (d) => (typeof d === "string" ? d : d.s);
// serde_json::to_string(&MarketDataBatch) — struct field order, Decimals
// as strings, NaiveDate as ISO string, DateTime<Utc> as RFC3339 AutoSi.
const serOptDec = (v) => (v === null || v === undefined ? "null" : J(v));
const serBar = (b) =>
  `{"date":${J(dateStr(b.date))},"open":${J(b.open)},"high":${J(b.high)},"low":${J(b.low)},` +
  `"close":${J(b.close)},"volume":${serOptDec(b.volume)},"amount":${serOptDec(b.amount)},` +
  `"r7_raw":${b.r7_raw === null || b.r7_raw === undefined ? "null" : String(b.r7_raw)}}`;
const serEvidence = (e) =>
  `{"status":${J(e.status)},"value":${e.value === null || e.value === undefined ? "null" : J(e.value)},"reason":${J(e.reason)}}`;
const serBinding = (b) =>
  `{"role":${J(b.role)},"member":${J(b.member)},"archive_sha256":${J(b.archive_sha256)},"member_sha256":${J(b.member_sha256)}}`;
const serProvenance = (p) =>
  `{"source":${J(p.source)},"source_period":${J(p.source_period)},"source_adjustment":${J(p.source_adjustment)},` +
  `"symbol":${J(p.symbol)},"acquired_at":${J(p.acquired_at)},"adapter_version":${J(p.adapter_version)},` +
  `"yearly_derivation":${p.yearly_derivation === null ? "null" : "<unported>"},` +
  `"snapshot_inputs":${p.snapshot_inputs === null ? "null" : `[${p.snapshot_inputs.map(serBinding).join(",")}]`}}`;
const serBatch = (b) =>
  `{"symbol":${J(b.symbol)},"period":${J(b.period)},"adjustment":${J(b.adjustment)},` +
  `"bars":[${b.bars.map(serBar).join(",")}],"provenance":${serProvenance(b.provenance)},` +
  `"coverage":{"status":${J(b.coverage.status)},"reason":${J(b.coverage.reason)}},` +
  `"unit_currency":{"price_currency":${serEvidence(b.unit_currency.price_currency)},` +
  `"price_unit":${serEvidence(b.unit_currency.price_unit)},"volume_unit":${serEvidence(b.unit_currency.volume_unit)},` +
  `"amount_currency":${serEvidence(b.unit_currency.amount_currency)},` +
  `"amount_unit":${serEvidence(b.unit_currency.amount_unit)}},"adjustment_evidence":${serEvidence(b.adjustment_evidence)}}`;

// minimal stored-method zip writer (fixture construction only)
const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();
const crc32 = (b) => {
  let c = 0xffffffff;
  for (const x of b) c = CRC_TABLE[(c ^ x) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};
function mkzip(members, { badCrc = false, comment = null, prefix = null, zip64 = false, trailer = null } = {}) {
  const parts = [], cds = [];
  const base = prefix ? prefix.length : 0;
  let off = 0;
  for (const m of members) {
    const name = Buffer.from(m.name, "latin1");
    const data = m.data;
    const crc = badCrc ? 0xdeadbeef : crc32(data);
    const lh = Buffer.alloc(30);
    lh.writeUInt32LE(0x04034b50, 0); lh.writeUInt16LE(20, 4);
    lh.writeUInt32LE(crc, 14); lh.writeUInt32LE(data.length, 18);
    lh.writeUInt32LE(data.length, 22); lh.writeUInt16LE(name.length, 26);
    parts.push(lh, name, data);
    const cd = Buffer.alloc(46);
    cd.writeUInt32LE(0x02014b50, 0); cd.writeUInt16LE(20, 4); cd.writeUInt16LE(20, 6);
    cd.writeUInt32LE(crc, 16); cd.writeUInt32LE(data.length, 20);
    cd.writeUInt32LE(data.length, 24); cd.writeUInt16LE(name.length, 28);
    cd.writeUInt32LE(off, 42);
    cds.push(Buffer.concat([cd, name]));
    off += 30 + name.length + data.length;
  }
  const cdBuf = Buffer.concat(cds);
  const eocd = Buffer.alloc(22);
  eocd.writeUInt32LE(0x06054b50, 0);
  if (zip64) {
    // [CD][EOCD64][locator][EOCD32] — EOCD64 at off+cdLen
    const z64off = off + cdBuf.length;
    const z64 = Buffer.alloc(56);
    z64.writeUInt32LE(0x06064b50, 0); z64.writeBigUInt64LE(44n, 4);
    z64.writeUInt16LE(45, 12); z64.writeUInt16LE(45, 14);
    z64.writeBigUInt64LE(BigInt(members.length), 24);
    z64.writeBigUInt64LE(BigInt(members.length), 32);
    z64.writeBigUInt64LE(BigInt(cdBuf.length), 40);
    z64.writeBigUInt64LE(BigInt(off), 48);
    const loc = Buffer.alloc(20);
    loc.writeUInt32LE(0x07064b50, 0);
    loc.writeBigUInt64LE(BigInt(z64off), 8);
    loc.writeUInt32LE(1, 16);
    // zip32 EOCD with zip64 markers
    eocd.writeUInt16LE(0xffff, 8); eocd.writeUInt16LE(0xffff, 10);
    eocd.writeUInt32LE(0xffffffff, 12); eocd.writeUInt32LE(0xffffffff, 16);
    const tail = Buffer.concat([z64, loc, eocd]);
    return Buffer.concat([...(prefix ? [prefix] : []), ...parts, cdBuf, tail, ...(trailer ? [trailer] : [])]);
  }
  eocd.writeUInt16LE(members.length, 8); eocd.writeUInt16LE(members.length, 10);
  eocd.writeUInt32LE(cdBuf.length, 12); eocd.writeUInt32LE(off, 16);
  if (comment) eocd.writeUInt16LE(comment.length, 20);
  return Buffer.concat([...(prefix ? [prefix] : []), ...parts, cdBuf, eocd,
    ...(comment ? [comment] : []), ...(trailer ? [trailer] : [])]);
}
const manifestFor = (dir, files, { badSha = null } = {}) => {
  const manifest = {
    schema: "x-data-mcp.official-file-input.v1",
    files: files.map((f) => {
      const bytes = readFileSync(join(dir, f.name));
      const urlPath = {
        base_industry: "products/data/data/dbf/base.zip",
        gbbq: "products/data/data/dbf/gbbq.zip",
        day: "vipdoc/hsjday.zip",
      }[f.role];
      const host = f.role === "day" ? "https://data.tdx.com.cn/" : "https://tdx.com.cn/";
      return {
        role: f.role,
        relative_path: f.name,
        source_url: host + urlPath,
        acquisition_started_at: "2026-09-20T00:00:00Z",
        acquisition_completed_at: f.completedAt ?? "2026-09-20T01:00:00Z",
        last_modified: null,
        etag: null,
        bytes: bytes.length,
        sha256: badSha === f.role ? "0".repeat(64) : sha256hex(bytes),
      };
    }),
  };
  const p = join(dir, "manifest.json");
  writeFileSync(p, J(manifest, null, 2));
  return p;
};

// ------------------------------------------------------------------ oracle
function runOracle(manifest, root, ops, env = {}) {
  // env overrides apply on top of a scrubbed environment: undefined
  // values delete the var (from_env cases need true absence).
  const oenv = { ...process.env };
  delete oenv.X_DATA_MCP_OFFICIAL_MANIFEST;
  delete oenv.X_DATA_MCP_OFFICIAL_DIR;
  for (const [k, v] of Object.entries(env)) {
    if (v === undefined) delete oenv[k]; else oenv[k] = v;
  }
  const r = spawnSync(ORACLE, [manifest, root, NOW, ...ops], {
    env: oenv, maxBuffer: 1 << 28, encoding: "utf8",
  });
  if (r.status !== 0) throw new Error(`oracle failed (${r.status}): ${r.stderr?.slice(0, 400)}`);
  const out = {};
  for (const line of r.stdout.trim().split("\n")) {
    const row = JSON.parse(line);
    out[row.id] = row.observed;
  }
  return out;
}

// Node-side outcomes mirroring the oracle's observed shape.
function nodeOutcome(fn) {
  try {
    return { status: "ok", batch_json: serBatch(fn()) };
  } catch (e) {
    return { status: "error", error: String(e.message ?? e) };
  }
}
function nodeInspect(root, manifestPath) {
  try {
    const s = TdxMarketSource.inspect(root, readFileSync(manifestPath), new Date(NOW));
    return { status: "ok", manifest_sha256: s.manifestSha256() };
  } catch (e) {
    return { status: "error", error: String(e.message ?? e) };
  }
}
function nodeMat(root, manifestPath, sym) {
  return nodeOutcome(() =>
    TdxMarketSource.inspect(root, readFileSync(manifestPath), new Date(NOW)).materializeDaily(sym));
}
function nodeSess(root, manifestPath, syms) {
  try {
    const s = TdxMarketSource.inspect(root, readFileSync(manifestPath), new Date(NOW));
    const sess = s.materializeSession();
    return { status: "ok", symbols: syms.map((sym) => nodeOutcome(() => sess.materializeDaily(sym))) };
  } catch (e) {
    return { status: "error", error: String(e.message ?? e) };
  }
}

// ------------------------------------------------------------------ fixtures
const SCRATCH = mkdtempSync(join(tmpdir(), "nm075-"));
const gbbqMemberBytes = (() => {
  const b = readFileSync(join(GEN3_ROOT, "gbbq.zip"));
  return zipMember(b, "gbbq", 64 << 20);
})();
// Carve real GBBQ records for a controlled synthetic member: one
// category-1 event for sh600004, one non-category-1 record for
// sh600004 if present, plus one record for another security.
const carved = (() => {
  const file = parseGbbq(gbbqMemberBytes);
  const events = decodeEvents(file);
  const cat1 = events.findIndex((e) => e.market === 1 && e.code === "600004" && e.category === 1);
  const nonCat1 = events.findIndex((e) => e.market === 1 && e.code === "600004" && e.category !== 1);
  const other = events.findIndex((e) => !(e.market === 1 && e.code === "600004"));
  const picked = [cat1, nonCat1, other].filter((i) => i >= 0);
  const recs = picked.map((i) => file.records[i]);
  const hdr = Buffer.alloc(4);
  hdr.writeUInt32LE(recs.length, 0);
  return {
    member: Buffer.concat([hdr, ...recs]),
    picked: picked.map((i) => ({
      market: events[i].market, code: events[i].code,
      category: events[i].category, date_raw: events[i].date_raw,
    })),
  };
})();

const dayReal = readFileSync(DAY_FIXTURE);
const synthOk = join(SCRATCH, "ok");
mkdirSync(synthOk, { recursive: true });
writeFileSync(join(synthOk, "base.zip"), mkzip([{ name: "x", data: Buffer.from("x") }]));
writeFileSync(join(synthOk, "gbbq.zip"), mkzip([{ name: "gbbq", data: carved.member }]));
writeFileSync(join(synthOk, "hsjday.zip"),
  mkzip([{ name: "sh\\lday\\sh600004.day", data: dayReal }]));
const synthOkManifest = manifestFor(synthOk, [
  { role: "base_industry", name: "base.zip" },
  { role: "gbbq", name: "gbbq.zip", completedAt: "2026-09-20T02:00:00Z" },
  { role: "day", name: "hsjday.zip" }]);

const synthVariants = {};
const mkVariant = (id, { gbbq = null, day = null, manifestBadSha = null, omitDay = false }) => {
  const dir = join(SCRATCH, id);
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "base.zip"), mkzip([{ name: "x", data: Buffer.from("x") }]));
  writeFileSync(join(dir, "gbbq.zip"),
    gbbq ?? mkzip([{ name: "gbbq", data: carved.member }]));
  if (!omitDay) {
    writeFileSync(join(dir, "hsjday.zip"),
      day ?? mkzip([{ name: "sh\\lday\\sh600004.day", data: dayReal }]));
  }
  synthVariants[id] = {
    dir,
    // gbbq completes after day in synthetic roots → exercises the
    // acquired_at = max(day, gbbq) gbbq-wins branch (gen3 covers day).
    manifest: manifestFor(dir,
      [{ role: "base_industry", name: "base.zip" },
       { role: "gbbq", name: "gbbq.zip", completedAt: "2026-09-20T02:00:00Z" },
       ...(omitDay ? [] : [{ role: "day", name: "hsjday.zip" }])],
      { badSha: manifestBadSha }),
  };
};
const gbbqBadLen = mkzip([{ name: "gbbq", data: Buffer.alloc(20, 0x55) }]);
const gbbqBadDecode = mkzip([{ name: "gbbq", data: Buffer.concat([Buffer.from([1, 0, 0, 0]), Buffer.alloc(29, 0xaa)]) }]);
const gbbqAbsent = mkzip([{ name: "not-gbbq", data: Buffer.from("x") }]);
const dayDup = mkzip([
  { name: "sh\\lday\\sh600004.day", data: dayReal },
  { name: "sh\\lday\\sh600004.day", data: dayReal },
]);
// Duplicate names collapse in the zip crate's IndexMap (first-insertion
// order, last-wins data) — a distinguishable second member proves which
// entry by_index resolves to.
const dayOne = Buffer.alloc(32);
dayOne.writeUInt32LE(20240102, 0); dayOne.writeUInt32LE(1000, 4);
dayOne.writeUInt32LE(2000, 8); dayOne.writeUInt32LE(1500, 12);
dayOne.writeUInt32LE(1800, 16); dayOne.writeUInt32LE(0x3f800000, 20);
dayOne.writeUInt32LE(99999, 24); dayOne.writeUInt32LE(7, 28);
const dayDupLastWins = mkzip([
  { name: "sh\\lday\\sh600004.day", data: dayReal },
  { name: "sh\\lday\\sh600004.day", data: dayOne },
]);
// Both candidate spellings present → found 2 → ambiguous (verbatim).
const dayBoth = mkzip([
  { name: "sh\\lday\\sh600004.day", data: dayReal },
  { name: "sh/lday/sh600004.day", data: dayReal },
]);
const dayAbsent = mkzip([{ name: "sh\\lday\\sh600005.day", data: dayReal }]);
const dayShort = mkzip([{ name: "sh\\lday\\sh600004.day", data: dayReal.subarray(0, 33) }]);
const dayEmpty = mkzip([{ name: "sh\\lday\\sh600004.day", data: Buffer.alloc(0) }]);
const dayBadCrc = mkzip([{ name: "sh\\lday\\sh600004.day", data: dayReal }], { badCrc: true });
const notZip = Buffer.alloc(4096, 0x77);
// Archive-structure paths: prepended junk (archive_offset detection),
// zip64 EOCD/locator chain, EOCD comment, and a trailing bogus EOCD
// signature that the backward scan must reject then retry below.
const dayZip64 = mkzip([{ name: "sh\\lday\\sh600004.day", data: dayReal }], { zip64: true });
const dayPrefix = mkzip([{ name: "sh\\lday\\sh600004.day", data: dayReal }],
  { prefix: Buffer.alloc(100, 0x58) });
const dayComment = mkzip([{ name: "sh\\lday\\sh600004.day", data: dayReal }],
  { comment: Buffer.from("archive note") });
const bogusEocd = Buffer.alloc(22);
bogusEocd.writeUInt32LE(0x06054b50, 0); // cdOffset=0, filesTotal=0
const dayTrailingEocd = mkzip([{ name: "sh\\lday\\sh600004.day", data: dayReal }],
  { trailer: bogusEocd });
const bogusEocd2 = Buffer.alloc(22);
bogusEocd2.writeUInt32LE(0x06054b50, 0);
bogusEocd2.writeUInt16LE(1, 8); bogusEocd2.writeUInt16LE(1, 10); // filesTotal=1, cdOffset=0
const dayTrailingEocd2 = mkzip([{ name: "sh\\lday\\sh600004.day", data: dayReal }],
  { trailer: bogusEocd2 });

mkVariant("gbbq-bad-len", { gbbq: gbbqBadLen });
mkVariant("gbbq-bad-decode", { gbbq: gbbqBadDecode });
mkVariant("gbbq-absent", { gbbq: gbbqAbsent });
mkVariant("day-dup", { day: dayDup });
mkVariant("day-dup-lastwins", { day: dayDupLastWins });
mkVariant("day-both-candidates", { day: dayBoth });
mkVariant("day-absent", { day: dayAbsent });
mkVariant("day-short", { day: dayShort });
mkVariant("day-empty", { day: dayEmpty });
mkVariant("day-bad-crc", { day: dayBadCrc });
mkVariant("hsjday-notzip", { day: notZip });
mkVariant("day-zip64", { day: dayZip64 });
mkVariant("day-prefix", { day: dayPrefix });
mkVariant("day-comment", { day: dayComment });
mkVariant("day-trailing-eocd", { day: dayTrailingEocd });
mkVariant("day-trailing-eocd2", { day: dayTrailingEocd2 });
mkVariant("sha-mismatch", { manifestBadSha: "gbbq" });

// ------------------------------------------------------------------ cases
const REAL = { manifest: GEN3_MANIFEST, root: GEN3_ROOT };
const CASES = [];
const mat = (id, root, sym, mode = "exact") =>
  CASES.push({ id, mode, op: `mat:${sym}`, run: () => nodeMat(root.dir ?? root.root, root.manifest, sym), oracleKey: `mat:${sym}` });
const roots = { REAL, ...synthVariants };

CASES.push({ id: "real.inspect", mode: "exact", op: "inspect",
  run: () => nodeInspect(REAL.root, REAL.manifest), oracleKey: "inspect" });
for (const sym of ["sh.600004", "sz.000002", "sh.600000", "sz.000001"]) {
  mat(`real.mat.${sym}`, REAL, sym);
}
CASES.push({ id: "real.sess", mode: "exact", op: "sess:sh.600004,sz.000002",
  run: () => nodeSess(REAL.root, REAL.manifest, ["sh.600004", "sz.000002"]),
  oracleKey: "sess:sh.600004,sz.000002" });
mat("real.missing-member", REAL, "sh.699999");
mat("real.missing-member2", REAL, "sz.399999");
for (const sym of ["bj.123456", "sh.12345", "sh.600004x", "sh600004", "sh.600004.5", "SH.600004"]) {
  mat(`real.unsupported.${sym}`, REAL, sym);
}
// synthetic tamper cases
for (const [id, sym] of [
  ["gbbq-bad-len", "sh.600004"], ["gbbq-bad-decode", "sh.600004"],
  ["gbbq-absent", "sh.600004"], ["day-dup", "sh.600004"],
  ["day-dup-lastwins", "sh.600004"], ["day-both-candidates", "sh.600004"],
  ["day-absent", "sh.600004"], ["day-short", "sh.600004"],
  ["day-empty", "sh.600004"], ["day-bad-crc", "sh.600004"],
  ["hsjday-notzip", "sh.600004"], ["day-zip64", "sh.600004"],
  ["day-prefix", "sh.600004"], ["day-comment", "sh.600004"],
  ["day-trailing-eocd", "sh.600004"], ["day-trailing-eocd2", "sh.600004"],
]) {
  mat(`synth.${id}`, synthVariants[id], sym);
}
CASES.push({ id: "synth.sha-mismatch.inspect", mode: "exact", op: "inspect",
  run: () => nodeInspect(synthVariants["sha-mismatch"].dir, synthVariants["sha-mismatch"].manifest),
  oracleKey: "inspect" });
CASES.push({ id: "synth.ok.inspect", mode: "exact", op: "inspect",
  run: () => nodeInspect(synthOk, synthOkManifest),
  oracleKey: "inspect" });
mat("synth.ok.mat", { dir: synthOk, manifest: synthOkManifest }, "sh.600004");
CASES.push({ id: "synth.ok.sess", mode: "exact", op: "sess:sh.600004",
  run: () => nodeSess(synthOk, synthOkManifest, ["sh.600004"]),
  oracleKey: "sess:sh.600004" });

// from_env cases — oracle reads real env; Node gets an env object.
const ENV_CASES = [
  ["none", {}],
  ["manifest-only", { X_DATA_MCP_OFFICIAL_MANIFEST: GEN3_MANIFEST }],
  ["dir-only", { X_DATA_MCP_OFFICIAL_DIR: GEN3_ROOT }],
  ["bad-manifest-path", { X_DATA_MCP_OFFICIAL_MANIFEST: "/nonexistent.json", X_DATA_MCP_OFFICIAL_DIR: GEN3_ROOT }],
  ["bad-dir", { X_DATA_MCP_OFFICIAL_MANIFEST: GEN3_MANIFEST, X_DATA_MCP_OFFICIAL_DIR: "/nonexistent" }],
  ["ok", { X_DATA_MCP_OFFICIAL_MANIFEST: GEN3_MANIFEST, X_DATA_MCP_OFFICIAL_DIR: GEN3_ROOT }],
];

// merge_segment_bars — private fn, no Rust oracle; self-asserted.
const bar = (s, over = {}) => ({
  date: s, open: "10.5", high: "11", low: "9", close: "10.8",
  volume: "100", amount: "1000.5", r7_raw: 7, ...over,
});
const MERGE_CASES = [
  ["fill-unknown-amount", [bar("2020-01-02", { amount: null })], [bar("2020-01-02")], "ok"],
  ["change-known-open", [bar("2020-01-02")], [bar("2020-01-02", { open: "10.6" })],
    'SOURCE_NOT_VERIFIED: source bar 2020-01-02 conflicts with the fixed base'],
  ["lose-known-volume", [bar("2020-01-02")], [bar("2020-01-02", { volume: null })],
    'SOURCE_NOT_VERIFIED: source bar 2020-01-02 conflicts with the fixed base'],
  ["lose-known-r7", [bar("2020-01-02")], [bar("2020-01-02", { r7_raw: null })],
    'SOURCE_NOT_VERIFIED: source bar 2020-01-02 conflicts with the fixed base'],
  ["change-known-r7", [bar("2020-01-02")], [bar("2020-01-02", { r7_raw: 9 })],
    'SOURCE_NOT_VERIFIED: source bar 2020-01-02 conflicts with the fixed base'],
  ["append-sorted", [bar("2020-01-02"), bar("2020-01-03")],
    [bar("2020-01-04"), bar("2020-01-02"), bar("2020-01-01")], "ok"],
  ["empty-segment", [bar("2020-01-02")], [], "ok"],
];
function nodeMerge(records, seg) {
  try {
    const m = mergeSegmentBars({ version: { records } }, seg);
    return { status: "ok", keys: [...m.keys()], size: m.size };
  } catch (e) {
    return { status: "error", error: String(e.message ?? e) };
  }
}

// ------------------------------------------------------------------ run
const rows = [];
const emit = (row) => { rows.push(row); console.log(`${row.status.toUpperCase()} ${row.id}`); };
const pass = (id, detail = "") => emit({ id, status: "pass", detail });
const fail = (id, detail) => emit({ id, status: "fail", detail });

// Canonical comparison form — serde_json::Value objects sort keys while
// Node keeps insertion order, so compare normalized content, with
// batch_json itself byte-compared inside.
const normObserved = (o) => {
  if (o === null || typeof o !== "object") return J(o);
  if (o.status === "ok" && "batch_json" in o) return `ok|${o.batch_json}`;
  if (o.status === "ok" && "symbols" in o) {
    return `ok|${o.symbols.map((s) => normObserved(s)).join("|")}`;
  }
  if (o.status === "ok" && "manifest_sha256" in o) return `ok|${o.manifest_sha256}`;
  if (o.status === "error") return `err|${o.error}`;
  if (o.status === "some") return `some|${o.manifest_sha256}`;
  if (o.status === "none") return "none";
  return J(o);
};
function compare(id, mode, expected, actual) {
  const e = normObserved(expected), a = normObserved(actual);
  if (mode === "prefix") {
    return (a === e || a.startsWith(e) || e.startsWith(a)) ? null : { expected: e, actual: a };
  }
  return a === e ? null : { expected: e, actual: a };
}

if (!SELFTEST) {
  if (!existsSync(ORACLE)) {
    console.error(`oracle binary missing: ${ORACLE}\nbuild it per the header comment`);
    process.exit(2);
  }
  // group ops per (manifest,root) into one oracle invocation
  const byInput = new Map();
  const specOf = (c) => {
    if (c.id.startsWith("real.")) return { manifest: REAL.manifest, root: REAL.root };
    for (const [vid, v] of Object.entries(synthVariants)) {
      if (c.id === `synth.${vid}` || c.id.startsWith(`synth.${vid}.`)) {
        return { manifest: v.manifest, root: v.dir };
      }
    }
    if (c.id.startsWith("synth.ok")) return { manifest: synthOkManifest, root: synthOk };
    return null;
  };
  for (const c of CASES) {
    const spec = specOf(c);
    if (!spec) continue;
    const k = `${spec.manifest}|${spec.root}`;
    if (!byInput.has(k)) byInput.set(k, { spec, ops: [] });
    byInput.get(k).ops.push(c.op);
  }
  const oracleOut = new Map(); // groupKey -> {op -> observed}
  for (const [k, { spec, ops }] of byInput) {
    oracleOut.set(k, runOracle(spec.manifest, spec.root, ops));
  }
  for (const c of CASES) {
    const spec = specOf(c);
    if (!spec) { fail(c.id, "no oracle input mapping"); continue; }
    const expected = oracleOut.get(`${spec.manifest}|${spec.root}`)[c.oracleKey];
    let actual;
    try {
      actual = c.run();
    } catch (e) {
      fail(c.id, `node threw: ${e.message}`);
      continue;
    }
    const diff = compare(c.id, c.mode, expected, actual);
    if (diff === null) pass(c.id, c.mode);
    else fail(c.id, diff);
  }
  // from_env matrix
  for (const [id, env] of ENV_CASES) {
    const oid = `fromenv.${id}`;
    const expected = runOracle(GEN3_MANIFEST, GEN3_ROOT, ["fromenv"], {
      X_DATA_MCP_OFFICIAL_MANIFEST: env.X_DATA_MCP_OFFICIAL_MANIFEST,
      X_DATA_MCP_OFFICIAL_DIR: env.X_DATA_MCP_OFFICIAL_DIR,
    }).fromenv;
    const s = TdxMarketSource.fromEnv({
      X_DATA_MCP_OFFICIAL_MANIFEST: env.X_DATA_MCP_OFFICIAL_MANIFEST,
      X_DATA_MCP_OFFICIAL_DIR: env.X_DATA_MCP_OFFICIAL_DIR,
    });
    const actual = s === null
      ? { status: "none" }
      : { status: "some", manifest_sha256: s.manifestSha256() };
    const diff = compare(oid, "exact", expected, actual);
    if (diff === null) pass(oid); else fail(oid, diff);
  }
  // merge_segment_bars self-asserts
  for (const [id, recs, seg, want] of MERGE_CASES) {
    const got = nodeMerge(recs, seg);
    if (want === "ok") {
      if (got.status === "ok") {
        const sorted = [...got.keys].every((k, i, a) => i === 0 || a[i - 1] < k);
        const uniq = new Set([...recs, ...seg].map((b) => b.date)).size;
        if (sorted && got.size === uniq) pass(`merge.${id}`, `size=${got.size}`);
        else fail(`merge.${id}`, `unsorted or wrong size: ${J(got)}`);
      } else fail(`merge.${id}`, `unexpected error: ${got.error}`);
    } else {
      if (got.status === "error" && got.error === want) pass(`merge.${id}`);
      else fail(`merge.${id}`, `want ${want}, got ${J(got)}`);
    }
  }
} else {
  // negative selftest: deliberately mismatched comparison must fail
  const a = nodeMat(REAL.root, REAL.manifest, "sh.600004");
  const fake = { status: "ok", batch_json: '{"symbol":"tampered"}' };
  if (compare("selftest", "exact", fake, a) === null) {
    console.error("SELFTEST BROKEN: mismatched inputs compared equal");
    process.exit(1);
  }
  console.log("selftest ok (mismatch detected as expected)");
  process.exit(1); // negative selftest exits nonzero by convention
}

const fails = rows.filter((r) => r.status === "fail");
if (OUT) {
  mkdirSync(OUT, { recursive: true });
  writeFileSync(join(OUT, "results.jsonl"), rows.map(J).join("\n") + "\n");
  writeFileSync(join(OUT, "results.json"), J({
    task: "NM-075", generated_at: new Date().toISOString(),
    fixture: { manifest: GEN3_MANIFEST, root: GEN3_ROOT, manifest_sha256: sha256hex(readFileSync(GEN3_MANIFEST)) },
    synthetic_gbbq_records: carved.picked,
    pass: rows.length - fails.length, fail: fails.length, rows,
  }, null, 2) + "\n");
}
// scratch is a fresh mkdtemp dir — remove only what this run created.
rmSync(SCRATCH, { recursive: true, force: true });
console.log(`\n${rows.length - fails.length} pass, ${fails.length} fail`);
process.exit(fails.length === 0 ? 0 : 1);
