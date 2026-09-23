// NM-025 P2: validate src_node/catalog/compose.mjs against a live Rust
// oracle — synthetic compose matrix + real gen-2 retained members.
// Usage: node scripts/node_migration_p2/catalog_compose_probe.mjs <worktree-root> [selftest]
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { join } from "node:path";
import { compose, nameMap, ComposeError } from "../../src_node/catalog/compose.mjs";
import { apply as scopeApply } from "../../src_node/catalog/scope.mjs";
import { parse as tdxhyParse } from "../../src_node/parsers/tdxhy.mjs";
import { parse as exParse } from "../../src_node/parsers/ex_code.mjs";
import { parse as inconParse } from "../../src_node/parsers/incon.mjs";
import { zipMember } from "./zip_member.mjs";

const ROOT = process.argv[2] || ".";
const SELFTEST = process.argv[3] || null;
const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/catalog_compose/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

let failures = 0;
const emit = (id, status, detail, expected = null, actual = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ family: "compose", id, status, detail, expected, actual }));
};
const abort = (id, detail) => { emit(id, "FAIL", detail); process.exit(1); };
const sha256 = (buf) => createHash("sha256").update(buf).digest("hex");
const hx = (h) => Buffer.from(h.replace(/\s/g, ""), "hex");

const SOURCE = "official-file:deadbeefcafe0001";
const TS = "2023-11-14T22:13:20+00:00"; // ts 1700000000 (labeled test value)

// iconv-verified GB18030 bytes:
// 金融=bdf0c8da 信息技术=d0c5cfa2bcbccaf5 软件服务=c8edbcfeb7fecef1
// 细分领域=cfb8b7d6c1ecd3f2 测试科创=b2e2cad4bfc6b4b4 重名=d6d8c3fb
// 万科=cdf2bfc6 测试=b2e2cad4 平安银行=c6bdb0b2d2f8d0d0
// 白云机场=b0d7d4c6bbfab3a1
const SEC = hx("235444584e48590d0a54313030317cbdf0c8da0d0a23544458525348590d0a5835307cd0c5cfa2bcbccaf50d0a58353030317cc8edbcfeb7fecef10d0a583530303130327ccfb8b7d6c1ecd3f20d0a");
const SEC_NO_TDXNHY = hx("23544458525348590d0a5835307cd0c5cfa2bcbccaf50d0a58353030317cc8edbcfeb7fecef10d0a");
const SEC_NO_TDXRSHY = hx("235444584e48590d0a54313030317cbdf0c8da0d0a");

const CASES = {
  assigned_both_markets: {
    tdxhy: "0|000001|T1001|||X500102\r\n1|600004|T1001|||X5001\r\n",
    ex: hx("3030303030317cc6bdb0b2d2f8d0d07c6b770d0a3630303030347cb0d7d4c6bbfab3a17c6b770d0a"),
    incon: SEC },
  nameless_excluded: {
    tdxhy: "0|000001|T1001|||X500102\r\n0|000099|T1001|||X500102\r\n0|000100|T1001|||X500102\r\n",
    ex: hx("3030303030317cc6bdb0b2d2f8d0d07c6b770d0a"),
    incon: SEC },
  all_nameless_null_snapshot: {
    tdxhy: "0|000099|T1001|||X500102\r\n",
    ex: hx("3030303030317cc6bdb0b2d2f8d0d07c6b770d0a"),
    incon: SEC },
  unresolvable_and_absent_l2: {
    tdxhy: "0|000002|T1001|||X42\r\n0|000003|T1001|||X9901\r\n",
    ex: hx("3030303030327ccdf2bfc67c6b770d0a3030303030337cb2e2cad47c6b770d0a"),
    incon: SEC },
  t_residual: {
    tdxhy: "1|688688|T00|||X500102\r\n",
    ex: hx("3638383638387cb2e2cad4bfc6b4b47c6b770d0a"),
    incon: SEC },
  missing_tdxrshy: {
    tdxhy: "0|000001|T1001|||X5001\r\n",
    ex: hx("3030303030317cc6bdb0b2d2f8d0d07c6b770d0a"),
    incon: SEC_NO_TDXRSHY },
  missing_tdxrshy_empty_members: { tdxhy: "", ex: "", incon: SEC_NO_TDXRSHY },
  tdxnhy_absent_no_check: {
    tdxhy: "1|688688|T00|||X500102\r\n",
    ex: hx("3638383638387cb2e2cad4bfc6b4b47c6b770d0a"),
    incon: SEC_NO_TDXNHY },
  same_name_two_members: {
    tdxhy: "0|000001|T1001|||X5001\r\n1|600004|T1001|||X5001\r\n",
    ex: hx("3030303030317cd6d8c3fb7c6b770d0a3630303030347cd6d8c3fb7c6b770d0a"),
    incon: SEC },
  ordering_preserved: {
    tdxhy: "1|600004|T1001|||X5001\r\n0|000001|T1001|||X5001\r\n0|000002|T1001|||X42\r\n",
    ex: hx("3630303030347cb0d7d4c6bbfab3a17c6b770d0a3030303030317cc6bdb0b2d2f8d0d07c6b770d0a3030303030327ccdf2bfc67c6b770d0a"),
    incon: SEC },
};

function canonOutcome(o) {
  // canonicalize into the oracle's JSON shape (BTreeMap key order)
  const id = ([m, c]) => [m, c];
  return {
    missing_industry: o.missing_industry.map(id),
    nameless: o.nameless.map(id),
    snapshot: o.snapshot === null ? null : {
      acquired_at: o.snapshot.acquired_at,
      completeness: o.snapshot.completeness,
      records: o.snapshot.records.map((r) => ({
        exchange: r.exchange, industry: r.industry, name: r.name,
        security_type: r.security_type, source: r.source,
        source_security_id: r.source_security_id, symbol: r.symbol })),
      source: o.snapshot.source },
    t_code_residuals: o.t_code_residuals.map((t) => ({ member: id(t.member), t_code: t.t_code })),
    t_dictionary_checked: o.t_dictionary_checked,
  };
}

for (const o of oracle.filter((r) => r.id !== "real_digest" && r.id !== "real_gen2")) {
  const c = CASES[o.id];
  try {
    const rows = tdxhyParse(Buffer.from(c.tdxhy, "latin1"));
    const scoped = scopeApply(rows);
    const names = nameMap(exParse(c.ex instanceof Buffer ? c.ex : Buffer.from(c.ex, "latin1")));
    const sections = inconParse(Buffer.from(c.incon, "latin1"));
    const outcome = compose(scoped.members, names, sections, SOURCE, TS);
    const ok = o.status === "ok" && JSON.stringify(canonOutcome(outcome)) === JSON.stringify(o.outcome);
    emit(o.id, ok ? "PASS" : "FAIL", ok ? null : "outcome mismatch", o.outcome, canonOutcome(outcome));
  } catch (e) {
    const ok = o.status === "error" && e instanceof ComposeError && e.message === o.error;
    emit(o.id, ok ? "PASS" : "FAIL", ok ? null : `got ${e.name}(${e.kind}): ${e.message}`, o.error, e.message);
  }
}

// ---------- typed boundary / shape failures (fail closed) ----------
const bnd = (id, fn, kind) => {
  try { fn(); emit(id, "FAIL", "accepted"); }
  catch (e) { emit(id, e instanceof ComposeError && e.kind === kind ? "PASS" : "FAIL", `${e.kind}: ${e.message}`); }
};
const sec = inconParse(SEC);
const m1 = { code: "000001", line: 1, market: 0, t_code: "T1001", x_code: "X5001" };
bnd("reject_bad_market_before_nameless", () =>
  compose([{ code: "000099", line: 1, market: 9, t_code: "T1", x_code: "X1" }],
    new Map(), sec, SOURCE, TS), "bad_market");
bnd("reject_members_not_array", () => compose("x", new Map(), sec, SOURCE, TS), "invalid_input");
bnd("reject_names_not_map", () => compose([], {}, sec, SOURCE, TS), "invalid_input");
bnd("reject_sections_not_array", () => compose([], new Map(), {}, SOURCE, TS), "invalid_input");
bnd("reject_source_not_string", () => compose([], new Map(), sec, 5, TS), "invalid_input");
bnd("reject_acquired_bad", () => compose([], new Map(), sec, SOURCE, "not-a-date"), "invalid_input");
bnd("reject_member_shape", () => compose([{ code: "01" }], new Map(), sec, SOURCE, TS), "invalid_input");
bnd("reject_null_member", () => compose([null], new Map(), sec, SOURCE, TS), "invalid_input");
{ // Date input accepted
  try { const o = compose([m1], new Map([["000001", "n"]]), sec, SOURCE, new Date(1_700_000_000_000));
    const ok = Date.parse(o.snapshot.records[0].acquired_at) === 1_700_000_000_000;
    emit("accept_date_ts", ok ? "PASS" : "FAIL", o.snapshot.records[0].acquired_at); }
  catch (e) { emit("accept_date_ts", "FAIL", e.message); }
}

// ---------- real gen-2 members (source.rs paths; manifest-verified) ----------
const BASE = "/home/box/retained-inputs/official-base.zip";
const GBBQ = "/home/box/retained-inputs/official-gbbq.zip";
const MANIFEST = "/home/box/retained-inputs/manifest-sha256.txt";
const manifest = readFileSync(MANIFEST, "utf8").split("\n").filter(Boolean)
  .map((l) => { const [h, ...p] = l.trim().split(/\s+/); return { h, p: p.join(" ") }; });
for (const [path, file] of [[BASE, "official-base.zip"], [GBBQ, "official-gbbq.zip"]]) {
  const sha = sha256(readFileSync(path));
  const entry = manifest.find((e) => e.p === file);
  if (entry === undefined || entry.h !== sha) {
    abort(`manifest_${file}`, `ABORT before extraction: sha=${sha} manifest=${entry?.h ?? "absent"}`);
  }
  emit(`manifest_${file}`, "PASS", `sha256=${sha}`);
}
const tdxhyBytes = zipMember(readFileSync(BASE), "tdxhy.cfg");
const exBytes = zipMember(readFileSync(BASE), "infoharbor_ex.code");
const zhb = zipMember(readFileSync(GBBQ), "zhb.zip");
const inconBytes = zipMember(zhb, "incon.dat");
emit("member_tdxhy", sha256(tdxhyBytes) === "df7cb7051d0f73a0d1381e9b36eafa33c8f5081d28ddef87cad406dd28ed8ea6" ? "PASS" : "FAIL", `${tdxhyBytes.length}B`);
emit("member_ex", sha256(exBytes) === "79eed0f6857ffde3d1d3cc8966ceff6e400f45e6e2387224446e453850fb5281" ? "PASS" : "FAIL", `${exBytes.length}B`);
emit("member_incon", sha256(inconBytes) === "86f758b29033632fae23b85a7f002b4a6906fb88a3d2904495865c3b2cfb6f51" ? "PASS" : "FAIL", `${inconBytes.length}B`);

const rows = tdxhyParse(tdxhyBytes);
const scoped = scopeApply(rows);
const names = nameMap(exParse(exBytes));
const sections = inconParse(inconBytes);
const outcome = compose(scoped.members, names, sections, SOURCE, TS);
const real = oracle.find((r) => r.id === "real_gen2");
const realDigest = oracle.find((r) => r.id === "real_digest");
const canon = canonOutcome(outcome);
{
  const got = sha256(Buffer.from(JSON.stringify(canon), "utf8"));
  emit("real_gen2_digest", got === realDigest.sha256 ? "PASS" : "FAIL",
    `records=${canon.snapshot.records.length} nameless=${canon.nameless.length} missing=${canon.missing_industry.length} residuals=${canon.t_code_residuals.length} sha256=${got}`,
    realDigest.sha256, got);
}
{
  const got = JSON.stringify(canon);
  emit("real_gen2_outcome", got === JSON.stringify(real.outcome) ? "PASS" : "FAIL",
    `full normalized outcome vs oracle (${got.length}B)`);
}

// fail closed
process.exit(failures === 0 ? 0 : 1);
