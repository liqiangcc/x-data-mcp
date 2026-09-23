// NM-028 P2: validate catalog source_candidate.mjs (pre-publish part of
// TdxCatalogExecutor::run) against a live Rust oracle. Synthetic fixtures
// + retained gen-2 bytes (hardlinks, read-only). Exit nonzero on FAIL.
// Usage: node catalog_source_probe.mjs <root> [selftest]
import { readFileSync, writeFileSync, mkdirSync, mkdtempSync, rmSync, linkSync, statSync } from "node:fs";
import { createHash } from "node:crypto";
import { join, basename } from "node:path";
import { deflateRawSync } from "node:zlib";
import { assembleCandidate, CatalogSourceError } from "../../src_node/catalog/source_candidate.mjs";

const ROOT = process.argv[2] || ".";
const SELFTEST = process.argv[3] || null;
const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/catalog_source/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

const FIX = mkdtempSync("/dev/shm/nm28-fix-");
const REAL = mkdtempSync("/home/box/nm28-real-");
const cleanup = () => { try { rmSync(FIX, { recursive: true, force: true }); } catch {} try { rmSync(REAL, { recursive: true, force: true }); } catch {} };
process.on("exit", cleanup);

let failures = 0;
const emit = (id, status, detail, expected = null, actual = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ family: "catalog_source", id, status, detail, expected, actual }));
};

// ---------- minimal STORED zip writer (fixtures only) ----------
const crc32 = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; t[n] = c >>> 0; }
  return (b) => { let c = 0xffffffff; for (const x of b) c = t[(c ^ x) & 0xff] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0; };
})();
function mkZip(members /* [{name, data:Buffer}] */) {
  const parts = [], cd = [];
  for (const { name, data } of members) {
    const nb = Buffer.from(name, "latin1");
    const off = parts.reduce((n, p) => n + p.length, 0);
    const lh = Buffer.alloc(30);
    lh.writeUInt32LE(0x04034b50, 0); lh.writeUInt16LE(20, 4); lh.writeUInt16LE(0, 6);
    lh.writeUInt16LE(0, 8); lh.writeUInt16LE(0, 10); lh.writeUInt16LE(0, 12);
    lh.writeUInt32LE(crc32(data), 14); lh.writeUInt32LE(data.length, 18); lh.writeUInt32LE(data.length, 22);
    lh.writeUInt16LE(nb.length, 26); lh.writeUInt16LE(0, 28);
    parts.push(lh, nb, data);
    const ce = Buffer.alloc(46);
    ce.writeUInt32LE(0x02014b50, 0); ce.writeUInt16LE(20, 6); ce.writeUInt16LE(20, 4);
    ce.writeUInt16LE(0, 8); ce.writeUInt16LE(0, 10); ce.writeUInt16LE(0, 12); ce.writeUInt16LE(0, 14);
    ce.writeUInt32LE(crc32(data), 16); ce.writeUInt32LE(data.length, 20); ce.writeUInt32LE(data.length, 24);
    ce.writeUInt16LE(nb.length, 28); ce.writeUInt16LE(0, 30); ce.writeUInt16LE(0, 32);
    ce.writeUInt16LE(0, 34); ce.writeUInt16LE(0, 36); ce.writeUInt32LE(0, 38); ce.writeUInt32LE(off, 42);
    cd.push(Buffer.concat([ce, nb]));
  }
  const cdBuf = Buffer.concat(cd);
  const cdOff = parts.reduce((n, p) => n + p.length, 0);
  const eocd = Buffer.alloc(22);
  eocd.writeUInt32LE(0x06054b50, 0); eocd.writeUInt16LE(members.length, 8); eocd.writeUInt16LE(members.length, 10);
  eocd.writeUInt32LE(cdBuf.length, 12); eocd.writeUInt32LE(cdOff, 16);
  return Buffer.concat([...parts, cdBuf, eocd]);
}
// patch a member's CD-declared uncompressed size (for oversize tests)
function patchCdSize(zip, name, newSize) {
  const nb = Buffer.from(name, "latin1");
  for (let i = 0; i + 46 <= zip.length; i++) {
    if (zip.readUInt32LE(i) === 0x02014b50) {
      const nl = zip.readUInt16LE(i + 28);
      if (zip.subarray(i + 46, i + 46 + nl).equals(nb)) zip.writeUInt32LE(newSize, i + 24);
    }
  }
  return zip;
}
// patch a member's CD-declared CRC32 (for corruption tests)
function patchCdCrc(zip, name, newCrc) {
  const nb = Buffer.from(name, "latin1");
  for (let i = 0; i + 46 <= zip.length; i++) {
    if (zip.readUInt32LE(i) === 0x02014b50) {
      const nl = zip.readUInt16LE(i + 28);
      if (zip.subarray(i + 46, i + 46 + nl).equals(nb)) zip.writeUInt32LE(newCrc, i + 16);
    }
  }
  return zip;
}
const sha = (b) => createHash("sha256").update(b).digest("hex");

const ROLE_URL = {
  base_industry: "https://www.tdx.com.cn/products/data/data/dbf/base.zip",
  gbbq: "https://www.tdx.com.cn/products/data/data/dbf/gbbq.zip",
  day: "https://data.tdx.com.cn/vipdoc/hsjday.zip",
};
function writeSnapshot(dir, tdxhy, ex, incon, { skipTdxhy = false, oversizeTdxhy = false, badCrc = false } = {}) {
  const baseMembers = skipTdxhy ? [["infoharbor_ex.code", ex]] : [["tdxhy.cfg", tdxhy], ["infoharbor_ex.code", ex]];
  let base = mkZip(baseMembers.map(([n, d]) => ({ name: n, data: d })));
  if (oversizeTdxhy) base = patchCdSize(base, "tdxhy.cfg", 64 * 1024 * 1024 + 1);
  if (badCrc) base = patchCdCrc(base, "tdxhy.cfg", 0);
  const zhb = mkZip([{ name: "incon.dat", data: incon }]);
  const gbbq = mkZip([{ name: "zhb.zip", data: zhb }]);
  const day = mkZip([{ name: "placeholder", data: Buffer.from("x") }]);
  writeFileSync(join(dir, "base.zip"), base);
  writeFileSync(join(dir, "gbbq.zip"), gbbq);
  writeFileSync(join(dir, "hsjday.zip"), day);
  const files = [["base_industry", "base.zip"], ["gbbq", "gbbq.zip"], ["day", "hsjday.zip"]].map(([role, n]) => {
    const b = readFileSync(join(dir, n));
    return { role, relative_path: n, source_url: ROLE_URL[role],
      acquisition_started_at: "2026-09-01T00:00:00Z", acquisition_completed_at: "2026-09-01T00:00:01Z",
      last_modified: null, etag: null, bytes: b.length, sha256: sha(b) };
  });
  const mpath = join(dir, "manifest.json");
  writeFileSync(mpath, JSON.stringify({ schema: "x-data-mcp.official-file-input.v1", files }));
  return mpath;
}
const NOW = "2026-09-23T12:00:00Z";

const T = Buffer.from("0|000001|T1001|||X500102\r\n1|600004|T1001|||X5001\r\n", "latin1");
// GB18030 fixtures — bytes verified via iconv (not literal-typed CJK)
const HX = (h) => Buffer.from(h, "hex");
const E_BYTES = HX("3030303030317cc6bdb0b2d2f8d0d07c6b770d0a3630303030347cb0d7d4c6bbfab3a17c6b770d0a"); // 000001|平安银行|kw 600004|白云机场|kw
const I_BYTES = HX("235444584e48590d0a54313030317cbdf0c8da0d0a23544458525348590d0a58353030317cc8edbcfeb7fecef10d0a"); // TDXNHY/T1001|金融 TDXRSHY/X5001|软件服务
const UNNAMED = HX("3939393939397ccedeb9d87c6b770d0a"); // 999999|无关|kw

const CASES = {
  ok_small: () => { const d = join(FIX, "ok"); mkdirSync(d); return [{ scope: "current_sh_sz_a" }, writeSnapshot(d, T, E_BYTES, I_BYTES), d]; },
  unsupported_scope: () => [{ scope: "other" }, "/nonexistent", "/nonexistent"],
  manifest_mismatch: () => { const d = join(FIX, "badmf"); mkdirSync(d);
    const m = writeSnapshot(d, T, E_BYTES, I_BYTES);
    const mj = JSON.parse(readFileSync(m, "utf8")); mj.files[2].sha256 = "0".repeat(64);
    writeFileSync(m, JSON.stringify(mj)); return [{ scope: "current_sh_sz_a" }, m, d]; },
  member_missing: () => { const d = join(FIX, "nmb"); mkdirSync(d);
    return [{ scope: "current_sh_sz_a" }, writeSnapshot(d, T, E_BYTES, I_BYTES, { skipTdxhy: true }), d]; },
  member_corrupt: () => { const d = join(FIX, "corrupt"); mkdirSync(d);
    return [{ scope: "current_sh_sz_a" }, writeSnapshot(d, Buffer.from("not-a-cfg\r\n"), E_BYTES, I_BYTES), d]; },
  member_oversize: () => { const d = join(FIX, "big"); mkdirSync(d);
    return [{ scope: "current_sh_sz_a" }, writeSnapshot(d, T, E_BYTES, I_BYTES, { oversizeTdxhy: true }), d]; },
  member_crc_bad: () => { const d = join(FIX, "crc"); mkdirSync(d);
    return [{ scope: "current_sh_sz_a" }, writeSnapshot(d, T, E_BYTES, I_BYTES, { badCrc: true }), d]; },
  empty_candidate: () => { const d = join(FIX, "empty"); mkdirSync(d);
    return [{ scope: "current_sh_sz_a" }, writeSnapshot(d, Buffer.from("0|000001|T1001|||X500102\r\n"), UNNAMED, I_BYTES), d]; },
  real_gen2: () => {
    for (const [src, dst] of [["official-base.zip", "base.zip"], ["official-hsjday.zip", "hsjday.zip"], ["official-gbbq.zip", "gbbq.zip"]]) {
      linkSync(`/home/box/retained-inputs/${src}`, join(REAL, dst));
    }
    return [{ scope: "current_sh_sz_a" }, join(ROOT, "evidence/issue-3-acceptance-run/snapshot-manifest.json"), REAL];
  },
};

const canon = (v) => v === null || typeof v !== "object" ? JSON.stringify(v)
  : Array.isArray(v) ? "[" + v.map(canon).join(",") + "]"
  : "{" + Object.keys(v).sort().map((k) => JSON.stringify(k) + ":" + canon(v[k])).join(",") + "}";

for (const o of oracle) {
  const [target, manifest, dir] = CASES[o.id]();
  try {
    const got = assembleCandidate(target, manifest, dir, NOW);
    if (o.status === "error") { emit(o.id, "FAIL", `accepted (expected error)`, o.error, "ok"); continue; }
    const want = o.result;
    // full structured compare: candidate + evidence fields. `source` is
    // manifest-bound: synthetic fixture zips differ byte-wise from the
    // oracle's (zip impls differ), so normalize it there; real_gen2
    // shares identical bytes+manifest and verifies source verbatim.
    const norm = (v) => {
      const c = JSON.parse(JSON.stringify(v));
      c.source = "<manifest-bound>";
      if (c.candidate) { c.candidate.source = "<manifest-bound>";
        for (const r of c.candidate.records ?? []) r.source = "<manifest-bound>"; }
      return c;
    };
    const gotOut = {
      source: got.candidate.source, acquired_at: got.candidate.acquired_at,
      published_count: got.evidence.published_count,
      candidate: got.candidate, ...got.evidence,
    };
    const { candidate_sha256: _w, ...wantRest } = want;
    const ok = canon(norm(wantRest)) === canon(norm(gotOut)) &&
      (o.id === "real_gen2" ? got.candidate.source === want.source : /^official-file:[0-9a-f]{16}$/.test(got.candidate.source));
    emit(o.id, ok ? "PASS" : "FAIL",
      ok ? `n=${got.evidence.published_count}` : "candidate/evidence mismatch",
      ok ? null : want.source, ok ? null : `got_n=${got.evidence.published_count}`);
  } catch (e) {
    if (!(e instanceof CatalogSourceError)) { emit(o.id, "FAIL", `non-typed error: ${e.message}`); continue; }
    const wantMsg = o.status === "error" ? (o.error.startsWith("CATALOG_") ? o.error : `CATALOG_SOURCE_FAILURE: ${o.error}`) : null;
    if (SELFTEST === "source" && o.id === "ok_small") {
      emit(o.id, e ? "FAIL" : "PASS"); continue;
    }
    const ok = o.status === "error" && e.message === wantMsg;
    emit(o.id, ok ? "PASS" : "FAIL", ok ? `error verbatim` : `error mismatch`, wantMsg, e.message);
  }
}

// ---------- bounded-read helper: deterministic EOF/bound tests ----------
{
  const { readBoundedFile } = await import("../../src_node/catalog/source_candidate.mjs");
  const tp = join(FIX, "trunc.bin");
  writeFileSync(tp, Buffer.alloc(100));
  try { const b = readBoundedFile(tp, 100); emit("read_bounded_size", b.length === 100 ? "PASS" : "FAIL", "100B ok"); }
  catch (e) { emit("read_bounded_size", "FAIL", e.message); }
  try { const b = readBoundedFile(tp, 200); emit("read_bounded_under", b.length === 100 ? "PASS" : "FAIL", "bound>size ok"); }
  catch (e) { emit("read_bounded_under", "FAIL", e.message); }
  // file exceeds declared bound -> hard error (not hang)
  try { readBoundedFile(tp, 50); emit("read_bounded_over", "FAIL", "accepted oversize"); }
  catch (e) { emit("read_bounded_over", "PASS", e.message.slice(0, 60)); }
  // deterministic premature EOF: sysfs files report st_size=4096 but
  // return a short read then EOF -> the 0-read branch errors, no race.
  const sysf = "/sys/devices/system/cpu/online";
  const sz = statSync(sysf).size;
  if (sz > 0) {
    try { readBoundedFile(sysf, 1 << 20); emit("read_bounded_eof", "FAIL", "accepted short sysfs read"); }
    catch (e) { emit("read_bounded_eof", e.message.includes("unexpected EOF") ? "PASS" : "FAIL", e.message.slice(0, 80)); }
  } else {
    emit("read_bounded_eof", "NOT_RUN", "sysfs size not reported");
  }
}

// selftest: a deliberately wrong expectation must produce FAIL -> exit 1
if (SELFTEST === "source") {
  const d = join(FIX, "selftest");
  mkdirSync(d);
  const manifest = writeSnapshot(d, T, E_BYTES, I_BYTES);
  const got = assembleCandidate({ scope: "current_sh_sz_a" }, manifest, d, NOW);
  const ok = got.evidence.published_count === 999; // intentionally wrong
  emit("selftest_source", ok ? "PASS" : "FAIL", ok ? null : `published_count=${got.evidence.published_count} != 999 — STOP`);
}

process.exitCode = failures === 0 ? 0 : 1;
