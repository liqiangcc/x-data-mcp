// securities.catalog candidate assembly — Node port of the
// pre-publication part of catalog_tdx::source::TdxCatalogExecutor::run
// (S0–S6). Read-only: bounded manifest + inspected artifacts -> parsed
// members -> scope/reconciliation/compose -> in-memory candidate +
// pre-publication evidence. No SQLite, no publish, no network.
//
// Failure categories match Rust verbatim prefixes:
//   CATALOG_REJECTED        — unsupported scope / zero publishable members
//   CATALOG_SOURCE_FAILURE  — manifest, inspection, extraction, parse,
//                             reconciliation failures
// Provenance is never inferred: snapshot byte inspection does not
// establish cross-file compatibility, origin authorization, or
// production readiness, and no proof is inserted here.

import { openSync, readSync, closeSync, fstatSync } from "node:fs";
import { join } from "node:path";
import { inspect } from "../governance/snapshot.mjs";
import { parseTs } from "../governance/ts.mjs";
import { zipOpen, zipMember, zipMemberSize } from "../lib/zip_member.mjs";
import { parse as parseTdxhy } from "../parsers/tdxhy.mjs";
import { parse as parseEx, nameMap } from "../parsers/ex_code.mjs";
import { parse as parseIncon } from "../parsers/incon.mjs";
import { apply as scopeApply, reconciliation } from "./scope.mjs";
import { compose } from "./compose.mjs";

const MEMBER_LIMIT = 64 * 1024 * 1024;
const MANIFEST_LIMIT = 1 << 20;
const ROLE_DEBUG = { base_industry: "BaseIndustry", day: "Day", gbbq: "Gbbq" };

export class CatalogSourceError extends Error {
  constructor(message) {
    super(message);
    this.name = "CatalogSourceError";
    this.category = message.startsWith("CATALOG_REJECTED") ? "CATALOG_REJECTED" : "CATALOG_SOURCE_FAILURE";
  }
}
const sf = (m) => new CatalogSourceError(`CATALOG_SOURCE_FAILURE: ${m}`);
const rej = (m) => new CatalogSourceError(`CATALOG_REJECTED: ${m}`);

function readBounded(path, limitPlusOne) {
  const fd = openSync(path, "r");
  try {
    const buf = Buffer.allocUnsafe(limitPlusOne);
    const n = readSync(fd, buf, 0, limitPlusOne, 0);
    return buf.subarray(0, n);
  } finally { closeSync(fd); }
}

// Bounded read with deterministic premature-EOF rejection: reads
// exactly `maxBytes`; a 0-length read before that is a hard error (a
// readSync==0 spin would hang). Only ever used for the inspected
// base.zip/gbbq.zip artifacts — bounded by the manifest-declared size.
export function readBoundedFile(path, maxBytes) {
  const fd = openSync(path, "r");
  try {
    const size = fstatSync(fd).size;
    if (size > maxBytes) throw new Error(`file exceeds declared bound: ${size}`);
    const buf = Buffer.allocUnsafe(size);
    let off = 0;
    while (off < size) {
      const n = readSync(fd, buf, off, size - off, off);
      if (n === 0) throw new Error(`unexpected EOF at ${off} of ${size}`);
      off += n;
    }
    return buf;
  } finally { closeSync(fd); }
}

// Rust member(): by_name -> unavailable; declared size > limit ->
// "exceeds the bounded size limit"; read -> unreadable.
function member(zipBytes, name) {
  let declared;
  try { declared = zipMemberSize(zipBytes, name); }
  catch (e) { throw sf(`member ${name} unavailable: ${e.message}`); }
  if (declared > MEMBER_LIMIT) {
    throw sf(`member ${name} exceeds the bounded size limit`);
  }
  try { return zipMember(zipBytes, name, MEMBER_LIMIT); }
  catch (e) { throw sf(`member ${name} unreadable: ${e.message}`); }
}

/**
 * @param {object} target — item target (e.g. {scope:"current_sh_sz_a"})
 * @param {string} manifestPath — manifest path (bounded read 1MiB+1)
 * @param {string} inputRoot — trusted private root of the three artifacts
 * @param {string|number|Date} now — inspection `now`
 * @returns {{candidate: object|null, evidence: object}}
 */
export function assembleCandidate(target, manifestPath, inputRoot, now) {
  if (target === null || typeof target !== "object" || Array.isArray(target) ||
      target.scope !== "current_sh_sz_a") {
    throw rej(`unsupported catalog scope ${JSON.stringify(target)}`);
  }
  let manifestBytes;
  try { manifestBytes = readBounded(manifestPath, MANIFEST_LIMIT + 1); }
  catch (e) { throw sf(`manifest unreadable: ${e.message}`); }
  let inspection;
  try { inspection = inspect(manifestBytes, inputRoot, now); }
  catch (e) { throw sf(`snapshot inspection failed: ${e.message}`); }
  const artifact = (role) => {
    const f = inspection.files.find((x) => x.role === role);
    if (f === undefined) throw sf(`snapshot lacks role ${ROLE_DEBUG[role]}`);
    return { path: join(inputRoot, f.relative_path), bytes: f.bytes };
  };
  // acquired_at = max file completion timestamp at full ordering
  // precision (leap slots + nanoseconds), not string max.
  let acquiredAt;
  let acquiredKey = null;
  for (const f of inspection.files) {
    const t = parseTs(f.acquisition_completed_at);
    if (t !== null && (acquiredKey === null || t.ns2 > acquiredKey)) {
      acquiredKey = t.ns2;
      acquiredAt = t.out;
    }
  }

  // Rust order: base.zip open+parse members first, then gbbq.zip.
  // Read bound = manifest-declared bytes (already byte-verified by
  // inspect); post-verify truncation surfaces as unreadable, not a hang.
  let baseBytes;
  try { const a = artifact("base_industry"); baseBytes = readBoundedFile(a.path, a.bytes); }
  catch (e) { throw sf(`base.zip unreadable: ${e.message}`); }
  try { zipOpen(baseBytes); } catch (e) { throw sf(`base.zip invalid: ${e.message}`); }
  const tdxhyRows = (() => { try { return parseTdxhy(member(baseBytes, "tdxhy.cfg")); }
    catch (e) { throw e instanceof CatalogSourceError ? e : sf(e.message); } })();
  const exRows = (() => { try { return parseEx(member(baseBytes, "infoharbor_ex.code")); }
    catch (e) { throw e instanceof CatalogSourceError ? e : sf(e.message); } })();

  let gbbqBytes;
  try { const a = artifact("gbbq"); gbbqBytes = readBoundedFile(a.path, a.bytes); }
  catch (e) { throw sf(`gbbq.zip unreadable: ${e.message}`); }
  try { zipOpen(gbbqBytes); } catch (e) { throw sf(`gbbq.zip invalid: ${e.message}`); }
  const sections = (() => {
    const zhb = member(gbbqBytes, "zhb.zip");
    try { zipOpen(zhb); } catch (e) { throw sf(`zhb.zip invalid: ${e.message}`); }
    try { return parseIncon(member(zhb, "incon.dat")); }
    catch (e) { throw e instanceof CatalogSourceError ? e : sf(e.message); }
  })();

  const scoped = scopeApply(tdxhyRows);
  const counts = reconciliation(scoped, tdxhyRows.length);
  if (counts.total !== counts.scope + counts.bj_market + counts.b_share + counts.anomalous) {
    throw sf("scope partition identity violated");
  }

  const source = `official-file:${inspection.manifest_sha256.slice(0, 16)}`;
  let outcome;
  try { outcome = compose(scoped.members, nameMap(exRows), sections, source, acquiredAt); }
  catch (e) { throw sf(e.message); }
  if (outcome.snapshot === null) {
    const names = outcome.nameless.map(([m, c]) => `${m}:${c}`).join(",");
    throw rej(`no publishable members; nameless scope members: ${names}`);
  }

  // Emit the Rust serde shapes — this candidate is what publish_official
  // serializes. Exchange/SecurityType/IndustryInfo/CompletenessEvidence
  // use verbatim enum encodings.
  const EX = { shenzhen: "Shenzhen", shanghai: "Shanghai" };
  const IND = (i) => i.kind === "assigned"
    ? { Assigned: { code: i.code, name: i.name, scheme: i.scheme } }
    : { Missing: { reason: i.reason } };
  const records = outcome.snapshot.records.map((r) => ({
    symbol: r.symbol,
    name: r.name,
    exchange: EX[r.exchange] ?? r.exchange,
    security_type: r.security_type === "a_share" ? "AShare" : r.security_type,
    source_security_id: r.source_security_id,
    industry: IND(r.industry),
    source: r.source,
    acquired_at: r.acquired_at,
  }));
  const candidate = {
    source: outcome.snapshot.source,
    acquired_at: outcome.snapshot.acquired_at,
    completeness: { Complete: { expected_count: outcome.snapshot.completeness.expected_count } },
    records,
  };

  return {
    candidate,
    evidence: {
      published_count: outcome.snapshot.records.length,
      reject_candidate_exclusions: outcome.nameless.map(([m, c]) => ({
        member: `${m}:${c}`,
        reason: "reject_candidate: no name row in infoharbor_ex.code",
      })),
      missing_industry: outcome.missing_industry.map(([m, c]) => `${m}:${c}`),
      t_code_residuals: outcome.t_code_residuals.map((r) => ({
        member: `${r.member[0]}:${r.member[1]}`, t_code: r.t_code,
      })),
      t_dictionary_checked: outcome.t_dictionary_checked,
      scope_partition: {
        total: counts.total, scope: counts.scope, bj_market: counts.bj_market,
        b_share: counts.b_share, anomalous: counts.anomalous,
      },
    },
  };
}
