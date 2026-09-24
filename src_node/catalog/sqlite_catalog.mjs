// src/sqlite_catalog.rs — the read-side surface NM-074's SqliteSupply
// catalog arms consume: head / read / digest. Write paths
// (publish_fixture / publish_official / selection_facts) stay out of
// scope — no catalog impl (S6) per the task's non-goals.
//
// The rows are the same tables `catalog_versions` (compat_json +
// digest + published_at + source + acquired_at) and `catalog_members`
// (the projection rows). read() revalidates the stored bytes:
// digest match, metadata identity fields, validate_snapshot, and the
// member-projection round-trip — every mismatch is Integrity,
// fail-closed.

import { createHash } from "node:crypto";
import { serValue } from "../mcp/serde_json.mjs";
import { validateSnapshot } from "./validate.mjs";

const sha256 = (s) => createHash("sha256").update(s).digest("hex");
const storage = (e) => new Error(`STORAGE_UNAVAILABLE: ${e.message ?? e}`);
const integrity = (m) => new Error(`MARKET_INTEGRITY: ${m}`);

// catalog.rs Exchange serde: Shanghai|Shenzhen unit, Other(String) tagged.
const exchangeTag = (e) =>
  e === "Shanghai" ? "sh" : e === "Shenzhen" ? "sz" : "invalid";

// Debug-format a String field value (Rust {:?} on String — quoted,
// minimal escapes for the message shapes this surface produces).
const dbgStr = (s) =>
  `"${String(s).replace(/\\/g, "\\\\").replace(/"/g, '\\"')
    .replace(/\n/g, "\\n").replace(/\t/g, "\\t").replace(/\r/g, "\\r")}"`;

const dbgExchange = (e) =>
  typeof e === "string" ? e : `Other(${dbgStr(e.Other)})`;

// SecurityKey { exchange: Exchange, symbol: String }
const dbgKey = (k) =>
  `SecurityKey { exchange: ${dbgExchange(k.exchange)}, symbol: ${dbgStr(k.symbol)} }`;

// RejectReason Debug — validate_snapshot's error is rendered via
// format!("{e:?}") in the Rust read path; reproduce that shape.
const debugReason = (r) => {
  if (typeof r === "string") return r;
  const [tag, v] = Object.entries(r)[0];
  switch (tag) {
    case "SourceFailure": case "Incomplete":
    case "InvalidRecord": case "IdentityConflict":
      return `${tag} { message: ${dbgStr(v.message)} }`;
    case "DuplicateSecurity":
      return `DuplicateSecurity { key: ${dbgKey(v.key)} }`;
    case "MemberDisappeared":
      return `MemberDisappeared { members: [${v.members.map(dbgKey).join(", ")}] }`;
    default:
      return `${tag} ${JSON.stringify(v)}`;
  }
};

// projection — catalog.rs:21 — the canonical member rows a stored
// catalog_versions payload must round-trip to.
const projection = (v) => {
  const rows = v.records.map((r) => {
    const ind = r.industry;
    const tag = Object.keys(ind)[0];
    const [status, code, name, reason] =
      tag === "Assigned"
        ? ["assigned", ind.Assigned.code, ind.Assigned.name, null]
        : tag === "Missing"
          ? ["missing", null, null, ind.Missing.reason]
          : ["conflicted", null, null, ind.Conflicted.reason];
    return [
      exchangeTag(r.exchange), r.symbol, r.name, r.source_security_id,
      status, code ?? null, name ?? null, reason ?? null,
    ];
  });
  rows.sort((a, b) => {
    for (let i = 0; i < 8; i++) {
      const x = a[i], y = b[i];
      if (x === y) continue;
      if (x === null) return -1;
      if (y === null) return 1;
      return x < y ? -1 : 1;
    }
    return 0;
  });
  return rows;
};

// raw(v) = serde_json::to_string(&CatalogVersion) — struct serialization
// emits declaration order, NOT Value-sorted. The stored compat_json is
// already declaration-ordered, so JSON.stringify on the parsed value
// reproduces the exact bytes.
export const digest = (v) => sha256(JSON.stringify(v));

export const head = (db) =>
  db.get("SELECT version_id FROM catalog_head WHERE singleton=1")?.version_id ?? null;

export const read = (db, id) => {
  let row;
  try {
    row = db.get(
      "SELECT compat_json,digest,published_at,source,acquired_at FROM catalog_versions WHERE version_id=?",
      id);
  } catch (e) { throw storage(e); }
  if (!row) throw integrity("catalog version missing");
  const { compat_json: raw, digest: stored, published_at: published,
    source, acquired_at: acquired } = row;
  if (sha256(raw) !== stored) throw integrity("catalog digest mismatch");
  let v;
  try { v = JSON.parse(raw); }
  catch (e) { throw integrity(String(e.message ?? e)); }
  // v.published_at/acquired_at are serde `Z` form; the stored columns
  // are to_rfc3339() `+00:00` — normalize before the identity check.
  const rfc = (s) => String(s).replace(/Z$/, "+00:00");
  if (v.version_id !== id || rfc(v.published_at) !== published ||
      v.source !== source || rfc(v.acquired_at) !== acquired) {
    throw integrity("catalog metadata projection");
  }
  const snapshot = {
    source: v.source,
    acquired_at: v.acquired_at,
    completeness: { Complete: { expected_count: v.records.length } },
    records: v.records,
  };
  const vr = validateSnapshot(snapshot, null);
  if (vr.accepted !== true) throw integrity(debugReason(vr.reason));
  let rows;
  try {
    rows = db.query(
      "SELECT exchange,code,name,source_security_id,industry_status,industry_code,industry_name,industry_reason FROM catalog_members WHERE version_id=? ORDER BY exchange,code",
      id);
  } catch (e) { throw storage(e); }
  const actual = rows.map((r) => [
    r.exchange, r.code, r.name, r.source_security_id,
    r.industry_status, r.industry_code, r.industry_name, r.industry_reason,
  ]);
  const expected = projection(v);
  const same = actual.length === expected.length &&
    actual.every((a, i) => a.every((x, j) => x === expected[i][j]));
  if (!same) throw integrity("catalog member projection");
  return v;
};
