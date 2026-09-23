// NM-029 P2: validate catalog/validate.mjs (validate_snapshot +
// validate_record) against a live Rust oracle (via CatalogService::
// refresh RejectReason serde payloads). Pure-object fixtures; gen-2
// real candidate from NM-028 evidence. Exit nonzero on FAIL.
// Usage: node catalog_validate_probe.mjs <root> [selftest]
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { validateSnapshot, ValidationInputError } from "../../src_node/catalog/validate.mjs";

const ROOT = process.argv[2] || ".";
const SELFTEST = process.argv[3] || null;
const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/catalog_validate/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);
const srcOracle = readFileSync(join(ROOT, "evidence/node-migration/p2/catalog_source/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);
const GEN2 = srcOracle.find((r) => r.id === "real_gen2").result.candidate;

let failures = 0;
const emit = (id, status, detail, expected = null, actual = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ family: "catalog_validate", id, status, detail, expected, actual }));
};

const AT = "2026-09-01T00:00:01Z";
const S = "official-file:0123456789abcdef";
const rec = (symbol, name, exchange, sid, industry, source = S, security_type = "AShare") => ({
  symbol, name, exchange, security_type, source_security_id: sid, industry, source, acquired_at: AT });
const ASSIGNED = { Assigned: { code: "X5001", name: "软件服务", scheme: "tdx_research_industry" } };
const MISSING = { Missing: { reason: "tdx_research: no level-2 code derivable from X00" } };
const good = () => [
  rec("000001", "平安银行", "Shenzhen", "tdxhy:0:000001", ASSIGNED),
  rec("600004", "白云机场", "Shanghai", "tdxhy:1:600004", MISSING)];
const snap = (source, completeness, records) => ({ source, acquired_at: AT, completeness, records });
const prev = (records) => ({ version_id: "v1", published_at: "2026-09-01T00:00:00Z",
  source: "official-file:aaaaaaaaaaaaaaaa", acquired_at: "2026-09-01T00:00:00Z", records });
const COMPLETE = (n) => ({ Complete: { expected_count: n } });

const CASES = {
  empty_source: () => [snap("", COMPLETE(2), good()), null],
  ws_source: () => [snap("  \t\n", COMPLETE(2), good()), null],
  feff_source: () => [snap("﻿", COMPLETE(2), good()), null],
  complete_zero: () => [snap(S, COMPLETE(0), []), null],
  complete_mismatch: () => [snap(S, COMPLETE(5), good()), null],
  complete_ok: () => [snap(S, COMPLETE(2), good()), null],
  incomplete_reason: () => [snap(S, { Incomplete: { reason: "partial" } }, good()), null],
  unknown_reason: () => [snap(S, { Unknown: { reason: "unbounded" } }, good()), null],
  blank_symbol: () => { const rs = good(); rs[0].symbol = "  "; return [snap(S, COMPLETE(2), rs), null]; },
  blank_name: () => { const rs = good(); rs[0].name = " "; return [snap(S, COMPLETE(2), rs), null]; },
  blank_sid: () => { const rs = good(); rs[0].source_security_id = "\t"; return [snap(S, COMPLETE(2), rs), null]; },
  blank_rec_source: () => { const rs = good(); rs[0].source = ""; return [snap(S, COMPLETE(2), rs), null]; },
  feff_symbol: () => { const rs = good(); rs[0].symbol = "﻿"; return [snap(S, COMPLETE(2), rs), null]; },
  source_mismatch: () => { const rs = good(); rs[0].source = "other"; return [snap(S, COMPLETE(2), rs), null]; },
  exchange_other: () => { const rs = good(); rs[0].exchange = { Other: "HK" }; return [snap(S, COMPLETE(2), rs), null]; },
  type_other: () => { const rs = good(); rs[0].security_type = { Other: "Fund" }; return [snap(S, COMPLETE(2), rs), null]; },
  ind_assigned_blank_code: () => { const rs = good(); rs[0].industry = { Assigned: { code: "", name: "x", scheme: "tdx_research_industry" } }; return [snap(S, COMPLETE(2), rs), null]; },
  ind_assigned_blank_name: () => { const rs = good(); rs[0].industry = { Assigned: { code: "X", name: "  ", scheme: null } }; return [snap(S, COMPLETE(2), rs), null]; },
  ind_missing_blank: () => { const rs = good(); rs[0].industry = { Missing: { reason: " " } }; return [snap(S, COMPLETE(2), rs), null]; },
  ind_conflicted_blank: () => { const rs = good(); rs[0].industry = { Conflicted: { reason: "" } }; return [snap(S, COMPLETE(2), rs), null]; },
  ind_conflicted_ok: () => { const rs = good(); rs[0].industry = { Conflicted: { reason: "ok" } }; return [snap(S, COMPLETE(2), rs), null]; },
  dup_key_same_sid: () => { const rs = good(); rs.push(rec("000001", "b", "Shenzhen", "tdxhy:0:000001", { Missing: { reason: "r" } })); return [snap(S, COMPLETE(3), rs), null]; },
  dup_key_diff_sid: () => { const rs = good(); rs.push(rec("000001", "b", "Shenzhen", "tdxhy:0:DIFF", { Missing: { reason: "r" } })); return [snap(S, COMPLETE(3), rs), null]; },
  sid_on_two_keys: () => { const rs = good(); rs.push(rec("000002", "b", "Shenzhen", "tdxhy:0:000001", { Missing: { reason: "r" } })); return [snap(S, COMPLETE(3), rs), null]; },
  sid_conflict_unicode: () => { const rs = good(); rs.push(rec("中﻿x\"", "b", "Shenzhen", "tdxhy:0:000001", { Missing: { reason: "r" } })); return [snap(S, COMPLETE(3), rs), null]; },
  members_disappeared: () => {
    const p = prev([...good(),
      rec("600009", "老成员", "Shanghai", "tdxhy:1:600009", { Missing: { reason: "r" } }),
      rec("000099", "sz老", "Shenzhen", "tdxhy:0:000099", { Missing: { reason: "r" } })]);
    return [snap(S, COMPLETE(2), good()), p]; },
  new_member_ok: () => [snap(S, COMPLETE(2), good()), prev([good()[0]])],
  same_members_ok: () => [snap(S, COMPLETE(2), good()), prev(good())],
  prec_source_first: () => [snap(" ", { Incomplete: { reason: "x" } }, []), null],
  prec_field_before_dup: () => { const rs = good(); rs[0].symbol = ""; rs.push(rec("000001", "b", "Shenzhen", "tdxhy:0:000001", { Missing: { reason: "r" } })); return [snap(S, COMPLETE(3), rs), null]; },
  prec_dup_before_disappear: () => { const rs = good(); rs.push(rec("000001", "b", "Shenzhen", "tdxhy:0:000001", { Missing: { reason: "r" } }));
    return [snap(S, COMPLETE(3), rs), prev([rec("999999", "老", "Shenzhen", "tdxhy:0:999999", { Missing: { reason: "r" } })])]; },
  other_order_disappeared: () => {
    const p = prev([...good(),
      rec("0", "z", { Other: "z" }, "sid-z", { Missing: { reason: "r" } }),
      rec("9", "a", { Other: "a" }, "sid-a", { Missing: { reason: "r" } }),
      rec("5", "sh", "Shanghai", "sid-sh", { Missing: { reason: "r" } })]);
    return [snap(S, COMPLETE(2), good()), p]; },
  legacy_scheme_absent: () => { const rs = good(); rs[0].industry = { Assigned: { code: "X5001", name: "软件服务" } };
    return [snap(S, COMPLETE(2), rs), null]; },
  real_gen2_accept: () => [GEN2, null],
  gen2_members_disappeared: () => {
    const cur = JSON.parse(JSON.stringify(GEN2));
    cur.records = cur.records.filter((r) => !["600009", "000099"].includes(r.symbol));
    cur.completeness.Complete.expected_count = cur.records.length;
    const p = { version_id: "pv", published_at: "2026-09-15T09:00:00Z", ...JSON.parse(JSON.stringify(GEN2)) };
    return [cur, p]; },
  gen2_new_member_ok: () => {
    const cur = JSON.parse(JSON.stringify(GEN2));
    cur.records.push({ symbol: "888888", name: "新成员", exchange: "Shenzhen", security_type: "AShare",
      source_security_id: "tdxhy:0:888888",
      industry: { Assigned: { code: "X5001", name: "软件服务", scheme: "tdx_research_industry" } },
      source: GEN2.source, acquired_at: GEN2.acquired_at });
    cur.completeness.Complete.expected_count = cur.records.length;
    const p = { version_id: "pv", published_at: "2026-09-15T09:00:00Z", ...JSON.parse(JSON.stringify(GEN2)) };
    return [cur, p]; },
};

const canon = (v) => v === null || typeof v !== "object" ? JSON.stringify(v)
  : Array.isArray(v) ? "[" + v.map(canon).join(",") + "]"
  : "{" + Object.keys(v).sort().map((k) => JSON.stringify(k) + ":" + canon(v[k])).join(",") + "}";

for (const o of oracle) {
  const [s, p] = CASES[o.id]();
  try {
    const got = validateSnapshot(s, p ?? null);
    if (o.status === "rejected") {
      const ok = got.accepted === false && canon(got.reason) === canon(o.reason);
      emit(o.id, ok ? "PASS" : "FAIL", ok ? `reject: ${canon(got.reason).slice(0, 80)}` : "reason mismatch", o.reason, got);
    } else {
      emit(o.id, got.accepted === true ? "PASS" : "FAIL",
        got.accepted === true ? "accepted" : `wrong reject: ${canon(got.reason).slice(0, 100)}`, null, got.accepted === true ? null : got.reason);
    }
  } catch (e) {
    emit(o.id, "FAIL", `threw ${e.name}: ${e.message}`);
  }
}

// ---------- typed boundary rejects (Node-only; unrepresentable in Rust) ----------
const bnd = (id, fn) => {
  try { fn(); emit(id, "FAIL", "accepted malformed input"); }
  catch (e) { emit(id, e instanceof ValidationInputError ? "PASS" : "FAIL", `${e.name}: ${e.message.slice(0, 80)}`); }
};
const G = () => snap(S, COMPLETE(2), good());
bnd("reject_snapshot_null", () => validateSnapshot(null, null));
bnd("reject_snapshot_array", () => validateSnapshot([], null));
bnd("reject_source_num", () => validateSnapshot(snap(5, COMPLETE(2), good()), null));
bnd("reject_source_surrogate", () => validateSnapshot(snap("x\ud800", COMPLETE(2), good()), null));
bnd("reject_comp_count_str", () => validateSnapshot(snap(S, { Complete: { expected_count: "2" } }, good()), null));
bnd("reject_comp_count_neg", () => validateSnapshot(snap(S, { Complete: { expected_count: -1 } }, good()), null));
bnd("reject_comp_unknown_variant", () => validateSnapshot(snap(S, { Bogus: {} }, good()), null));
bnd("reject_comp_two_keys", () => validateSnapshot(snap(S, { Complete: { expected_count: 2 }, Incomplete: { reason: "x" } }, good()), null));
bnd("reject_symbol_num", () => { const rs = good(); rs[0].symbol = 5; return validateSnapshot(snap(S, COMPLETE(2), rs), null); });
bnd("reject_symbol_surrogate", () => { const rs = good(); rs[0].symbol = "\udfff"; return validateSnapshot(snap(S, COMPLETE(2), rs), null); });
bnd("reject_exchange_unknown", () => { const rs = good(); rs[0].exchange = "Beijing"; return validateSnapshot(snap(S, COMPLETE(2), rs), null); });
bnd("reject_exchange_obj_extra", () => { const rs = good(); rs[0].exchange = { Other: "HK", extra: 1 }; return validateSnapshot(snap(S, COMPLETE(2), rs), null); });
bnd("reject_type_unknown", () => { const rs = good(); rs[0].security_type = "Bond"; return validateSnapshot(snap(S, COMPLETE(2), rs), null); });
bnd("reject_ind_unknown", () => { const rs = good(); rs[0].industry = { Unknown: { reason: "x" } }; return validateSnapshot(snap(S, COMPLETE(2), rs), null); });
bnd("reject_ind_scheme_bad", () => { const rs = good(); rs[0].industry = { Assigned: { code: "X", name: "n", scheme: "other_scheme" } }; return validateSnapshot(snap(S, COMPLETE(2), rs), null); });
bnd("reject_ind_extra_field", () => { const rs = good(); rs[0].industry = { Assigned: { code: "X", name: "n", scheme: null, extra: 1 } }; return validateSnapshot(snap(S, COMPLETE(2), rs), null); });
bnd("reject_prev_bad_shape", () => validateSnapshot(G(), { version_id: 1 }));
bnd("reject_prev_bad_record", () => { const p = prev(good()); p.records[0].exchange = "Bad"; return validateSnapshot(G(), p); });
bnd("reject_prev_records_obj", () => { const p = prev(good()); p.records = {}; return validateSnapshot(G(), p); });
bnd("reject_rec_extra_field", () => { const rs = good(); rs[0].extra = 1; return validateSnapshot(snap(S, COMPLETE(2), rs), null); });
bnd("reject_rec_missing_field", () => { const rs = good(); delete rs[0].symbol; return validateSnapshot(snap(S, COMPLETE(2), rs), null); });
bnd("reject_record_nonobj", () => validateSnapshot(snap(S, COMPLETE(1), ["x"]), null));
bnd("reject_acquired_num", () => validateSnapshot({ ...G(), acquired_at: 5 }, null));
// R1: serde-incompatible timestamps reject at the boundary
bnd("reject_snap_ts_junk", () => validateSnapshot({ ...G(), acquired_at: "junk" }, null));
bnd("reject_snap_ts_nooffset", () => validateSnapshot({ ...G(), acquired_at: "2026-09-01T00:00:00" }, null));
bnd("reject_rec_ts_junk", () => { const rs = good(); rs[0].acquired_at = "not-a-date"; return validateSnapshot(snap(S, COMPLETE(2), rs), null); });
bnd("reject_prev_ts_junk", () => { const p = prev(good()); p.published_at = "yesterday"; return validateSnapshot(G(), p); });
bnd("reject_prev_rec_ts_junk", () => { const p = prev(good()); p.records[0].acquired_at = "2026-13-40T99:99:99Z"; return validateSnapshot(G(), p); });
// present-but-undefined `scheme` serializes as absent — serde(default) null
{ const rs = good(); const ind = { Assigned: { code: "X", name: "n" } }; ind.Assigned.scheme = undefined;
  rs[0].industry = ind; const d = validateSnapshot(snap(S, COMPLETE(2), rs), null);
  emit("accept_scheme_undefined", d.accepted === true ? "PASS" : "FAIL", d.accepted ? "accepted" : canon(d.reason)); }

// selftest: wrong expectation must FAIL -> exit 1
if (SELFTEST === "validate") {
  const got = validateSnapshot(snap(S, COMPLETE(2), good()), null);
  emit("selftest_validate", got.accepted === false ? "PASS" : "FAIL",
    got.accepted === false ? null : "expected reject, got accept — STOP");
}

process.exitCode = failures === 0 ? 0 : 1;
