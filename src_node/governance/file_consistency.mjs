// Cross-file consistency decision — Node port of
// src/file_consistency.rs::validate.
//
// Cross-file compatibility is a separate gate from per-file byte
// integrity. It is accepted ONLY from an explicit immutable proof
// (CompatibilityEvidence) binding all required roles to exact hashes,
// bound source revision/identity, and a revision window. A verified
// file hash, common URL/date/ETag, or caller string never infers
// compatibility; this function does not authenticate proof provenance.
//
// Decision shape mirrors serde output: status SCREAMING_SNAKE_CASE,
// reasons tagged {code,role?} sorted by serialized form, evidence_refs
// sorted, files sorted+deduped by full FileFact order.

import { parseTs } from "./ts.mjs";

export class ConsistencyInputError extends Error {
  constructor(message) {
    super(message);
    this.name = "ConsistencyInputError";
    this.kind = "invalid_input";
  }
}

export const CONTRACT_VERSION = "official-file-consistency-v1";
const REQUIRED_ROLES = ["base_industry", "day", "gbbq"]; // enum order
const ROLE_IDX = Object.assign(Object.create(null), { base_industry: 0, day: 1, gbbq: 2 });
const BI_IDX = Object.assign(Object.create(null), { verified: 0, unverified: 1 });
const CONFLICT_CODES = new Set([
  "conflicting_duplicate", "invalid_file_interval", "invalid_compatibility_window",
  "hash_mismatch", "source_revision_mismatch", "source_identity_mismatch",
  "acquisition_outside_compatibility_window",
]);

const canonicalSha256 = (v) => /^[0-9a-f]{64}$/.test(v);
// Rust String is always valid UTF-8; a JS string containing unpaired
// UTF-16 surrogates has no Rust representation — reject at the boundary
// (Buffer.from would silently lossy-replace).
const isStr = (v) => typeof v === "string" && v.isWellFormed();
const isOptStr = (v) => v === null || typeof v === "string" && v.isWellFormed();

// Rust String Ord is UTF-8 bytewise (e.g. U+E000 < U+1F600); JS `<` is
// UTF-16 code-unit order. Buffer.compare gives the Rust order.
const cmpStr = (a, b) => Buffer.compare(Buffer.from(a, "utf8"), Buffer.from(b, "utf8"));

// Full FileFact order: role, bytes, sha256, source_url, started, completed,
// last_modified, etag, byte_integrity, source_revision, source_identity.
// Option<String>: None < Some; strings compare bytewise (ASCII domain).
const optStr = (v) => (v === null ? 0 : 1);
function cmpFact(a, b) {
  const t = (x, y) => (x < y ? -1 : x > y ? 1 : 0);
  return t(ROLE_IDX[a.role], ROLE_IDX[b.role]) ||
    t(a.bytes, b.bytes) || cmpStr(a.sha256, b.sha256) || cmpStr(a.source_url, b.source_url) ||
    t(a._k1, b._k1) || t(a._k2, b._k2) ||
    t(optStr(a.last_modified), optStr(b.last_modified)) || cmpStr(a.last_modified ?? "", b.last_modified ?? "") ||
    t(optStr(a.etag), optStr(b.etag)) || cmpStr(a.etag ?? "", b.etag ?? "") ||
    t(BI_IDX[a.byte_integrity], BI_IDX[b.byte_integrity]) ||
    t(optStr(a.source_revision), optStr(b.source_revision)) || cmpStr(a.source_revision ?? "", b.source_revision ?? "") ||
    t(optStr(a.source_identity), optStr(b.source_identity)) || cmpStr(a.source_identity ?? "", b.source_identity ?? "");
}

function checkFact(f, i) {
  if (f === null || typeof f !== "object" || Array.isArray(f)) {
    throw new ConsistencyInputError(`files[${i}] must be an object`);
  }
  if (!isStr(f.role) || !Object.hasOwn(ROLE_IDX, f.role)) throw new ConsistencyInputError(`files[${i}].role invalid`);
  if (!Number.isSafeInteger(f.bytes) || f.bytes < 0) throw new ConsistencyInputError(`files[${i}].bytes invalid`);
  if (!isStr(f.sha256)) throw new ConsistencyInputError(`files[${i}].sha256 must be a string`);
  if (!isStr(f.source_url)) throw new ConsistencyInputError(`files[${i}].source_url must be a string`);
  const k1 = parseTs(f.acquisition_started_at), k2 = parseTs(f.acquisition_completed_at);
  if (k1 === null) throw new ConsistencyInputError(`files[${i}].acquisition_started_at invalid RFC3339`);
  if (k2 === null) throw new ConsistencyInputError(`files[${i}].acquisition_completed_at invalid RFC3339`);
  if (!isOptStr(f.last_modified ?? null)) throw new ConsistencyInputError(`files[${i}].last_modified invalid`);
  if (!isOptStr(f.etag ?? null)) throw new ConsistencyInputError(`files[${i}].etag invalid`);
  if (!isStr(f.byte_integrity) || !Object.hasOwn(BI_IDX, f.byte_integrity)) {
    throw new ConsistencyInputError(`files[${i}].byte_integrity invalid`);
  }
  if (!isOptStr(f.source_revision ?? null)) throw new ConsistencyInputError(`files[${i}].source_revision invalid`);
  if (!isOptStr(f.source_identity ?? null)) throw new ConsistencyInputError(`files[${i}].source_identity invalid`);
  return {
    ...f,
    last_modified: f.last_modified ?? null,
    etag: f.etag ?? null,
    source_revision: f.source_revision ?? null,
    source_identity: f.source_identity ?? null,
    _k1: k1.ns2, _k2: k2.ns2,
  };
}

function checkEvidence(e) {
  if (e === null || typeof e !== "object" || Array.isArray(e)) {
    throw new ConsistencyInputError("compatibility must be null or an object");
  }
  for (const k of ["evidence_ref", "rule_version", "compatibility_set_revision", "compatibility_set_identity"]) {
    if (!isStr(e[k])) throw new ConsistencyInputError(`compatibility.${k} must be a string`);
  }
  if (e.member_bindings === null || typeof e.member_bindings !== "object" || Array.isArray(e.member_bindings)) {
    throw new ConsistencyInputError("compatibility.member_bindings must be an object");
  }
  for (const [role, b] of Object.entries(e.member_bindings)) {
    if (!Object.hasOwn(ROLE_IDX, role)) throw new ConsistencyInputError(`member_bindings key ${role} invalid`);
    if (b === null || typeof b !== "object" || !isStr(b.sha256) ||
        !isOptStr(b.source_revision ?? null) || !isOptStr(b.source_identity ?? null)) {
      throw new ConsistencyInputError(`member_bindings.${role} invalid`);
    }
  }
  if (e.valid_window === null || typeof e.valid_window !== "object") {
    throw new ConsistencyInputError("compatibility.valid_window must be an object");
  }
  const wf = parseTs(e.valid_window.valid_from), wu = parseTs(e.valid_window.valid_until);
  if (wf === null || wu === null) throw new ConsistencyInputError("valid_window timestamps invalid RFC3339");
  const bindings = Object.fromEntries(Object.entries(e.member_bindings).map(([k, b]) => [k, {
    sha256: b.sha256,
    source_revision: b.source_revision ?? null,
    source_identity: b.source_identity ?? null,
  }]));
  return { ...e, member_bindings: bindings, _wf: wf.ns2, _wu: wu.ns2 };
}

const r = (code, role) => (role === undefined ? { code } : { code, role });
const rkey = (x) => JSON.stringify(x); // serde reason serialization order {code,role}

/**
 * @param {{files: object[], compatibility: object|null}} input
 * @returns {ConsistencyDecision}
 */
export function validate(input) {
  if (input === null || typeof input !== "object" || !Array.isArray(input.files)) {
    throw new ConsistencyInputError("input.files must be an array");
  }
  const files = input.files.map(checkFact);
  const evidence = input.compatibility === undefined || input.compatibility === null
    ? null : checkEvidence(input.compatibility);

  // sort + exact dedup (full field equality)
  files.sort(cmpFact);
  const dedup = files.filter((f, i) => i === 0 || cmpFact(f, files[i - 1]) !== 0);
  const byRole = new Map();
  for (const f of dedup) {
    if (!byRole.has(f.role)) byRole.set(f.role, []);
    byRole.get(f.role).push(f);
  }

  const reasons = [];
  for (const role of REQUIRED_ROLES) {
    const facts = byRole.get(role);
    if (facts === undefined) reasons.push(r("missing_role", role));
    else if (facts.length > 1) reasons.push(r("conflicting_duplicate", role));
  }
  for (const f of dedup) {
    if (f.byte_integrity !== "verified") reasons.push(r("file_integrity_not_verified", f.role));
    if (f._k1 > f._k2) reasons.push(r("invalid_file_interval", f.role));
    if (!canonicalSha256(f.sha256)) reasons.push(r("invalid_file_hash", f.role));
  }

  const finish = (refs) => {
    const status = reasons.some((x) => CONFLICT_CODES.has(x.code))
      ? "CONFLICT" : reasons.length === 0 ? "VERIFIED_COMPATIBLE" : "NOT_VERIFIED";
    reasons.sort((a, b) => cmpStr(rkey(a), rkey(b)));
    refs.sort(cmpStr);
    return {
      contract_version: CONTRACT_VERSION,
      status,
      reasons,
      evidence_refs: refs,
      files: dedup.map(({ _k1, _k2, ...f }) => f),
    };
  };

  if (evidence === null) {
    reasons.push(r("missing_compatibility_evidence"));
    return finish([]);
  }
  const refs = [evidence.evidence_ref];
  const bindings = evidence.member_bindings;
  const bindingKeys = Object.keys(bindings);
  if (evidence.evidence_ref.trim() === "" ||
      evidence.rule_version !== CONTRACT_VERSION ||
      evidence.compatibility_set_revision.trim() === "" ||
      evidence.compatibility_set_identity.trim() === "" ||
      bindingKeys.length !== REQUIRED_ROLES.length ||
      REQUIRED_ROLES.some((role) => !Object.hasOwn(bindings, role)) ||
      Object.values(bindings).some((b) => !canonicalSha256(b.sha256))) {
    reasons.push(r("incomplete_compatibility_evidence"));
    return finish(refs);
  }
  if (evidence._wf > evidence._wu) {
    reasons.push(r("invalid_compatibility_window"));
    return finish(refs);
  }

  for (const role of REQUIRED_ROLES) {
    const facts = byRole.get(role);
    if (facts === undefined || facts.length !== 1) continue;
    const f = facts[0];
    const b = bindings[role];
    if (b.sha256 !== f.sha256) reasons.push(r("hash_mismatch", role));
    if (b.source_revision !== null) {
      if (f.source_revision !== null) {
        if (f.source_revision !== b.source_revision) reasons.push(r("source_revision_mismatch", role));
      } else {
        reasons.push(r("missing_bound_source_revision", role));
      }
    }
    if (b.source_identity !== null) {
      if (f.source_identity !== null) {
        if (f.source_identity !== b.source_identity) reasons.push(r("source_identity_mismatch", role));
      } else {
        reasons.push(r("missing_bound_source_identity", role));
      }
    }
    if (f._k1 < evidence._wf || f._k2 > evidence._wu) {
      reasons.push(r("acquisition_outside_compatibility_window", role));
    }
  }
  return finish(refs);
}
