// NM-026 P2: validate src_node/governance/snapshot.mjs against a live
// Rust oracle — synthetic fixture matrix + gen-2 real bytes.
// Usage: node scripts/node_migration_p2/snapshot_probe.mjs <worktree-root>
import { readFileSync, mkdtempSync, writeFileSync, mkdirSync, symlinkSync,
  linkSync, rmSync, readdirSync, unlinkSync } from "node:fs";
import { createHash } from "node:crypto";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { inspect, InspectionError } from "../../src_node/governance/snapshot.mjs";

const ROOT = process.argv[2] || ".";
const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/snapshot_inspect/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

let failures = 0;
const emit = (id, status, detail, expected = null, actual = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ family: "snapshot_inspect", id, status, detail, expected, actual }));
};
const sha256 = (b) => createHash("sha256").update(b).digest("hex");
const NOW = "2026-09-12T00:00:00Z";
const NOW_REAL = "2026-09-23T00:00:00Z";

const BURL = "http://www.tdx.com.cn/products/data/data/dbf/base.zip";
const GURL = "http://www.tdx.com.cn/products/data/data/dbf/gbbq.zip";
const DURL = "https://data.tdx.com.cn/vipdoc/hsjday.zip";
const ent = (role, name, url, data, extra = "") =>
  `{"role":"${role}","relative_path":"${name}","source_url":"${url}","acquisition_started_at":"2026-09-11T01:00:00Z","acquisition_completed_at":"2026-09-11T02:00:00Z","last_modified":null,"etag":null,"bytes":${data.length},"sha256":"${sha256(data)}"${extra}}`;
const man = (files, extra = "") =>
  `{"schema":"x-data-mcp.official-file-input.v1","files":[${files}]${extra}}`;
const B = (s) => Buffer.from(s, "latin1");

const FIX = "/dev/shm/snapfix_node";
rmSync(FIX, { recursive: true, force: true });
mkdirSync(FIX, { recursive: true });
const fx = (id, setup) => {
  const dir = join(FIX, id);
  mkdirSync(dir, { recursive: true });
  if (setup) setup(dir);
  return dir;
};
const std = (dir) => {
  writeFileSync(join(dir, "base.zip"), B("base"));
  writeFileSync(join(dir, "hsjday.zip"), B("day"));
  writeFileSync(join(dir, "gbbq.zip"), B("gbbq"));
};

const good = (x = "") => ent("base_industry", "base.zip", BURL, B("base"), x);
const gd = (x = "") => ent("day", "hsjday.zip", DURL, B("day"), x);
const gg = (x = "") => ent("gbbq", "gbbq.zip", GURL, B("gbbq"), x);

const CASES = {
  valid: () => [fx("valid", std), B(man(`${good()},${gd()},${gg()}`))],
  role_order: () => [fx("role_order", std), B(man(`${gg()},${gd()},${good()}`))],
  nested_path: () => [fx("nested", (d) => {
    std(d); mkdirSync(join(d, "private/archive"), { recursive: true });
    writeFileSync(join(d, "private/archive/base.zip"), B("base"));
    unlinkSync(join(d, "base.zip"));
  }), B(man(`${ent("base_industry", "private/archive/base.zip", BURL, B("base"))},${gd()},${gg()}`))],
  internal_symlink_ok: () => [fx("insym", (d) => {
    std(d); mkdirSync(join(d, "realdir"));
    linkSync(join(d, "base.zip"), join(d, "realdir/base.zip"));
    unlinkSync(join(d, "base.zip"));
    symlinkSync(join(d, "realdir"), join(d, "linkdir"));
  }), B(man(`${ent("base_industry", "linkdir/base.zip", BURL, B("base"))},${gd()},${gg()}`))],
  manifest_too_big: () => [fx("big", std),
    Buffer.concat([B(man(`${good()},${gd()},${gg()}`)), Buffer.alloc(1024 * 1024 + 1, 0x20)])],
  malformed_json: () => [fx("badjson", std), B("{not json")],
  unknown_field_top: () => [fx("unk", std), B(man(`${good()},${gd()},${gg()}`, ',"zzz":1'))],
  unknown_field_file: () => [fx("unk", std), B(man(`${good(',"zzz":1')},${gd()},${gg()}`))],
  wrong_schema: () => [fx("schema", std),
    B(man(`${good()},${gd()},${gg()}`).replace("official-file-input.v1", "other.v2"))],
  wrong_count: () => [fx("schema", std), B(man(`${good()},${gd()}`))],
  empty_files: () => [fx("schema", std), B(man(""))],
  dup_role: () => [fx("dup", std), B(man(`${good()},${gd()},${ent("day", "other_day.zip", DURL, B("day"))}`))],
  dup_path: () => [fx("dup", std), B(man(`${good()},${ent("day", "base.zip", DURL, B("day"))},${gg()}`))],
  path_absolute: () => [fx("path_absolute", std), B(man(`${ent("base_industry", "/tmp/base.zip", BURL, B("base"))},${gd()},${gg()}`))],
  path_dotdot: () => [fx("path_dotdot", std), B(man(`${ent("base_industry", "../base.zip", BURL, B("base"))},${gd()},${gg()}`))],
  path_dot: () => [fx("path_dot", std), B(man(`${ent("base_industry", "./base.zip", BURL, B("base"))},${gd()},${gg()}`))],
  path_empty: () => [fx("path_empty", std), B(man(`${ent("base_industry", "", BURL, B("base"))},${gd()},${gg()}`))],
  wrong_filename: () => [fx("wf", std), B(man(`${ent("base_industry", "notbase.zip", BURL, B("base"))},${gd()},${gg()}`))],
  url_whitespace: () => [fx("url_whitespace", std), B(man(`${ent("base_industry", "base.zip", "http://www.tdx.com.cn /products/data/data/dbf/base.zip", B("base"))},${gd()},${gg()}`))],
  url_authority: () => [fx("url_authority", std), B(man(`${ent("base_industry", "base.zip", "http://evil@www.tdx.com.cn/products/data/data/dbf/base.zip", B("base"))},${gd()},${gg()}`))],
  url_port: () => [fx("url_port", std), B(man(`${ent("base_industry", "base.zip", "http://www.tdx.com.cn:8080/products/data/data/dbf/base.zip", B("base"))},${gd()},${gg()}`))],
  url_query: () => [fx("url_query", std), B(man(`${ent("base_industry", "base.zip", "http://www.tdx.com.cn/products/data/data/dbf/base.zip?x=1", B("base"))},${gd()},${gg()}`))],
  url_wronghost: () => [fx("url_wronghost", std), B(man(`${ent("base_industry", "base.zip", "http://tdx.com.cn.evil/products/data/data/dbf/base.zip", B("base"))},${gd()},${gg()}`))],
  url_wrongpath: () => [fx("url_wrongpath", std), B(man(`${ent("base_industry", "base.zip", "https://data.tdx.com.cn/vipdoc/hsjday.zip", B("base"))},${gd()},${gg()}`))],
  url_noscheme: () => [fx("url_noscheme", std), B(man(`${ent("base_industry", "base.zip", "www.tdx.com.cn/products/data/data/dbf/base.zip", B("base"))},${gd()},${gg()}`))],
  url_https_ok: () => [fx("url_https_ok", std), B(man(`${ent("base_industry", "base.zip", "https://www.tdx.com.cn/products/data/data/dbf/base.zip", B("base"))},${gd()},${gg()}`))],
  url_day_host: () => [fx("url_day_host", std), B(man(`${good()},${ent("day", "hsjday.zip", "https://www.tdx.com.cn/vipdoc/hsjday.zip", B("day"))},${gg()}`))],
  interval_inverted: () => [fx("inv", std), B(man(`${good().replace("2026-09-11T01:00:00Z", "2026-09-11T03:00:00Z")},${gd()},${gg()}`))],
  interval_future: () => [fx("inv", std), B(man(`${good().replace("2026-09-11T02:00:00Z", "2026-09-13T00:00:00Z")},${gd()},${gg()}`))],
  sha_short: () => [fx("hash", std), B(man(`${good().replace(sha256(B("base")), "BAD")},${gd()},${gg()}`))],
  sha_upper: () => [fx("hash", std), B(man(`${good().replace(sha256(B("base")), sha256(B("base")).toUpperCase())},${gd()},${gg()}`))],
  sha_len63: () => [fx("hash", std), B(man(`${good().replace(sha256(B("base")), sha256(B("base")).slice(0, 63))},${gd()},${gg()}`))],
  bytes_zero: () => [fx("hash", std), B(man(`${good().replace('"bytes":4', '"bytes":0')},${gd()},${gg()}`))],
  missing_file: () => [fx("miss", (d) => {
    writeFileSync(join(d, "base.zip"), B("base"));
    writeFileSync(join(d, "gbbq.zip"), B("gbbq"));
  }), B(man(`${good()},${gd()},${gg()}`))],
  nonregular_dir: () => [fx("nonreg", (d) => {
    std(d); unlinkSync(join(d, "hsjday.zip")); mkdirSync(join(d, "hsjday.zip"));
  }), B(man(`${good()},${gd()},${gg()}`))],
  external_symlink: () => [fx("extsym", (d) => {
    std(d); writeFileSync("/dev/shm/outside_target_node", B("base"));
    unlinkSync(join(d, "base.zip"));
    symlinkSync("/dev/shm/outside_target_node", join(d, "base.zip"));
  }), B(man(`${good()},${gd()},${gg()}`))],
  changed_bytes: () => [fx("changed", (d) => {
    std(d); writeFileSync(join(d, "gbbq.zip"), B("CHANGED"));
  }), B(man(`${good()},${gd()},${gg()}`))],
};

for (const o of oracle.filter((r) => r.id !== "real_gen2")) {
  const [dir, mbytes] = CASES[o.id]();
  try {
    const ins = inspect(mbytes, dir, NOW);
    const ok = o.status === "ok" && JSON.stringify(ins) === JSON.stringify(o.inspection);
    emit(o.id, ok ? "PASS" : "FAIL", ok ? null : "inspection mismatch", o.inspection, ins);
  } catch (e) {
    if (o.status !== "error" || !(e instanceof InspectionError)) {
      emit(o.id, "FAIL", `expected ${o.kind ?? o.status} got ${e.name}(${e.kind}): ${e.message}`, o.error, e.message);
      continue;
    }
    // invalid/mismatch: verbatim text; json/io: same category, message text
    // is implementation-specific (serde/Node delta documented).
    const exact = o.kind === "invalid" || o.kind === "mismatch";
    const ok = e.kind === o.kind && (!exact || e.message === o.error);
    emit(o.id, ok ? "PASS" : "FAIL",
      ok ? `${e.kind}${exact ? " verbatim" : ""}` : `kind ${e.kind} msg ${e.message}`,
      o.error, e.message);
  }
}

// ---------- typed boundary ----------
const bnd = (id, fn) => {
  try { fn(); emit(id, "FAIL", "accepted"); }
  catch (e) { emit(id, e instanceof InspectionError && e.kind === "invalid_input" ? "PASS" : "FAIL", `${e.kind}: ${e.message}`); }
};
bnd("reject_string_manifest", () => inspect("{}", "/tmp", NOW));
bnd("reject_bad_root", () => inspect(B("{}"), 5, NOW));
bnd("reject_bad_now", () => inspect(B("{}"), "/tmp", "junk"));

// ---------- real gen-2 (committed manifest + retained bytes, hardlinked
// private root, cleaned after; streaming hash over 549MB) ----------
const REALROOT = mkdtempSync("/home/box/nm26-realroot-");
const manifestBytes = readFileSync(join(ROOT, "evidence/issue-3-acceptance-run/snapshot-manifest.json"));
const manifestSha = sha256(manifestBytes);
{
  const retained = readFileSync("/home/box/retained-inputs/manifest-sha256.txt", "utf8")
    .split("\n").filter(Boolean)
    .map((l) => { const [h, ...p] = l.trim().split(/\s+/); return { h, p: p.join(" ") }; });
  for (const [ret, link] of [["official-base.zip", "base.zip"],
    ["official-gbbq.zip", "gbbq.zip"], ["official-hsjday.zip", "hsjday.zip"]]) {
    const sha = sha256(readFileSync(`/home/box/retained-inputs/${ret}`));
    const entry = retained.find((e) => e.p === ret);
    if (entry === undefined || entry.h !== sha) {
      emit(`real_${ret}`, "FAIL", `retained sha ${sha} manifest ${entry?.h ?? "absent"} — STOP`);
      process.exit(1);
    }
    linkSync(`/home/box/retained-inputs/${ret}`, join(REALROOT, link));
    emit(`real_${ret}`, "PASS", `${ret} sha=${sha} -> ${link} (hardlink, source read-only)`);
  }
}
const realOracle = oracle.find((r) => r.id === "real_gen2");
const rssBefore = process.memoryUsage().rss;
const t0 = performance.now();
const ins = inspect(manifestBytes, REALROOT, NOW_REAL);
const elapsedMs = performance.now() - t0;
const rssAfter = process.memoryUsage().rss;
{
  const ok = JSON.stringify(ins) === JSON.stringify(realOracle.inspection);
  emit("real_gen2", ok ? "PASS" : "FAIL",
    `manifest_sha256=${ins.manifest_sha256} roles=[${ins.files.map((f) => f.role)}] ` +
    `elapsed=${elapsedMs.toFixed(1)}ms rss_delta=${((rssAfter - rssBefore) / 1048576).toFixed(1)}MiB ` +
    `(bounded verification only — no perf claim)`,
    realOracle.inspection.manifest_sha256, ins.manifest_sha256);
}
rmSync(REALROOT, { recursive: true, force: true });
emit("realroot_cleaned", readdirSync("/home/box").includes(REALROOT.split("/").pop()) ? "FAIL" : "PASS",
  `temp root ${REALROOT} removed; sources untouched`);
rmSync(FIX, { recursive: true, force: true });

process.exit(failures === 0 ? 0 : 1);
