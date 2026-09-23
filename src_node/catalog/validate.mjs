// Pure catalog candidate validation — Node port of
// catalog.rs::validate_snapshot + validate_record (pre-publication
// rules). Input is the serde-shaped candidate emitted by
// source_candidate.mjs plus an optional serde CatalogVersion (previous).
// Output: {accepted:true} or {accepted:false, reason:<serde
// RejectReason>}. Deterministic, pure, read-only — no publish/DB.
//
// Typed boundary: malformed JS-only shapes (non-serde enums, unpaired
// surrogates, wrong types) throw ValidationInputError — they are not
// decisions, matching Rust where such values are unrepresentable.

import { parseTs } from "../governance/ts.mjs";

export class ValidationInputError extends Error {
  constructor(message) { super(message); this.name = "ValidationInputError"; }
}

const isStr = (v) => typeof v === "string" && v.isWellFormed();
// Rust str::trim trims chars with the Unicode White_Space property —
// identical set to JS \p{White_Space}; importantly does NOT include
// U+FEFF (JS .trim() would wrongly strip it).
const blank = (s) => /^[\p{White_Space}]*$/u.test(s);
// Rust String Ord is UTF-8 bytewise.
const cmpStr = (a, b) => Buffer.compare(Buffer.from(a, "utf8"), Buffer.from(b, "utf8"));

const EX_ORD = { Shanghai: 0, Shenzhen: 1, Other: 2 };
const isEx = (v) => v === "Shanghai" || v === "Shenzhen" ||
  (v !== null && typeof v === "object" && !Array.isArray(v) &&
   Object.keys(v).length === 1 && isStr(v.Other));
const isSt = (v) => v === "AShare" ||
  (v !== null && typeof v === "object" && !Array.isArray(v) &&
   Object.keys(v).length === 1 && isStr(v.Other));
const SCHEMES = new Set(["tdx_research_industry"]);
const isIndustry = (v) => {
  if (v === null || typeof v !== "object" || Array.isArray(v)) return false;
  const ks = Object.keys(v);
  if (ks.length !== 1) return false;
  if (ks[0] === "Assigned") {
    const a = v.Assigned;
    if (a === null || typeof a !== "object" || Array.isArray(a)) return false;
    // scheme has #[serde(default)]: absent == null; present must be a
    // known scheme label.
    const aks = Object.keys(a).sort().join(",");
    return (aks === "code,name" || aks === "code,name,scheme") &&
      isStr(a.code) && isStr(a.name) && (a.scheme === undefined || a.scheme === null || SCHEMES.has(a.scheme));
  }
  if (ks[0] === "Missing" || ks[0] === "Conflicted") {
    const x = v[ks[0]];
    return x !== null && typeof x === "object" && !Array.isArray(x) &&
      Object.keys(x).length === 1 && isStr(x.reason);
  }
  return false;
};
// Timestamps must be serde-deserializable DateTime<Utc> — the shared
// strict parser covers the chrono/serde set incl. leap + ns.
const isTs = (v) => isStr(v) && parseTs(v) !== null;
const isRecord = (r) => r !== null && typeof r === "object" && !Array.isArray(r) &&
  Object.keys(r).sort().join(",") ===
    "acquired_at,exchange,industry,name,security_type,source,source_security_id,symbol" &&
  isStr(r.symbol) && isStr(r.name) && isEx(r.exchange) && isSt(r.security_type) &&
  isStr(r.source_security_id) && isIndustry(r.industry) && isStr(r.source) && isTs(r.acquired_at);

const isCompleteness = (c) => {
  if (c === null || typeof c !== "object" || Array.isArray(c)) return false;
  const ks = Object.keys(c);
  if (ks.length !== 1) return false;
  if (ks[0] === "Complete") {
    const x = c.Complete;
    return x !== null && typeof x === "object" && !Array.isArray(x) &&
      Object.keys(x).length === 1 && Number.isSafeInteger(x.expected_count) && x.expected_count >= 0;
  }
  if (ks[0] === "Incomplete" || ks[0] === "Unknown") {
    const x = c[ks[0]];
    return x !== null && typeof x === "object" && !Array.isArray(x) &&
      Object.keys(x).length === 1 && isStr(x.reason);
  }
  return false;
};

function checkSnapshotShape(s) {
  if (s === null || typeof s !== "object" || Array.isArray(s)) {
    throw new ValidationInputError("snapshot must be an object");
  }
  if (!isStr(s.source)) throw new ValidationInputError("snapshot.source must be a string");
  if (!isTs(s.acquired_at)) throw new ValidationInputError("snapshot.acquired_at invalid timestamp");
  if (!isCompleteness(s.completeness)) throw new ValidationInputError("snapshot.completeness invalid");
  if (!Array.isArray(s.records)) throw new ValidationInputError("snapshot.records must be an array");
  s.records.forEach((r, i) => {
    if (!isRecord(r)) throw new ValidationInputError(`snapshot.records[${i}] invalid shape`);
  });
}
function checkVersionShape(v) {
  if (v === null || typeof v !== "object" || Array.isArray(v)) {
    throw new ValidationInputError("previous version must be an object");
  }
  if (!isStr(v.version_id)) throw new ValidationInputError("previous.version_id must be a string");
  if (!isStr(v.source)) throw new ValidationInputError("previous.source must be a string");
  if (!isTs(v.published_at)) throw new ValidationInputError("previous.published_at invalid timestamp");
  if (!isTs(v.acquired_at)) throw new ValidationInputError("previous.acquired_at invalid timestamp");
  if (!Array.isArray(v.records)) throw new ValidationInputError("previous.records must be an array");
  v.records.forEach((r, i) => {
    if (!isRecord(r)) throw new ValidationInputError(`previous.records[${i}] invalid shape`);
  });
}

// Rust Debug for String (escape_debug): " \ ' escaped; control/format/
// separator chars -> \u{hex}; printable unicode literal.
const DEBUG_ESC = (ch) => {
  const cp = ch.codePointAt(0);
  return `\\u{${cp.toString(16)}}`;
};
const dbgStr = (s) => {
  let out = '"';
  for (const ch of s) {
    if (ch === '"') out += '\\"';
    else if (ch === "\\") out += "\\\\";
    else if (ch === "'") out += "\\'";
    else if (ch === "\n") out += "\\n";
    else if (ch === "\r") out += "\\r";
    else if (ch === "\t") out += "\\t";
    else if (/[\p{Cc}\p{Cf}\p{Cs}\p{Co}\p{Zl}\p{Zp}\p{Cn}]/u.test(ch)) out += DEBUG_ESC(ch);
    else out += ch;
  }
  return out + '"';
};
const exName = (e) => typeof e === "string" ? e : `Other(${dbgStr(e.Other)})`;
const dbgKey = (k) => `SecurityKey { exchange: ${exName(k.exchange)}, symbol: ${dbgStr(k.symbol)} }`;
const keyOrd = (k) => [EX_ORD[typeof k.exchange === "string" ? k.exchange : "Other"], k.symbol];
// Rust derived Ord on SecurityKey: exchange discriminant, then for
// Other the String payload (UTF-8), then symbol (UTF-8).
const cmpKey = (a, b) => {
  const ea = keyOrd(a)[0], eb = keyOrd(b)[0];
  if (ea !== eb) return ea - eb;
  if (ea === 2) {
    const c = cmpStr(a.exchange.Other, b.exchange.Other);
    if (c !== 0) return c;
  }
  return cmpStr(a.symbol, b.symbol);
};

const rej = (reason) => ({ accepted: false, reason });

/**
 * @param {object} snapshot — serde CatalogSnapshot candidate
 * @param {object|null|undefined} previous — serde CatalogVersion
 * @returns {{accepted:true}|{accepted:false, reason:object|string}}
 */
export function validateSnapshot(snapshot, previous) {
  checkSnapshotShape(snapshot);
  if (previous !== null && previous !== undefined) checkVersionShape(previous);

  if (blank(snapshot.source)) {
    return rej({ InvalidRecord: { message: "catalog source must not be empty" } });
  }
  const comp = snapshot.completeness;
  const cKind = Object.keys(comp)[0];
  if (cKind === "Complete") {
    const n = comp.Complete.expected_count;
    if (n === 0) return rej("EmptySnapshot");
    if (n !== snapshot.records.length) {
      return rej({ Incomplete: { message: `expected ${n} records but received ${snapshot.records.length}` } });
    }
  } else {
    return rej({ Incomplete: { message: comp[cKind].reason } });
  }
  if (snapshot.records.length === 0) return rej("EmptySnapshot");

  const byKey = new Map();   // canonical key string -> source_security_id
  const bySid = new Map();   // source_security_id -> SecurityKey object
  const keyOf = (r) => `${exName(r.exchange)}${r.symbol}`; // unique canonical form
  const keyObj = (r) => ({ exchange: r.exchange, symbol: r.symbol });

  for (const record of snapshot.records) {
    // validate_record — field order symbol, name, source_security_id, source
    for (const [field, value] of [
      ["symbol", record.symbol], ["name", record.name],
      ["source_security_id", record.source_security_id], ["source", record.source]]) {
      if (blank(value)) {
        return rej({ InvalidRecord: { message: `${field} must not be empty` } });
      }
    }
    if (record.source !== snapshot.source) {
      return rej({ IdentityConflict: {
        message: `record source ${record.source} differs from snapshot source ${snapshot.source}` } });
    }
    if (!(record.exchange === "Shanghai" || record.exchange === "Shenzhen")) {
      return rej({ InvalidRecord: { message: "record exchange is outside the Shanghai/Shenzhen scope" } });
    }
    if (!(record.security_type === "AShare")) {
      return rej({ InvalidRecord: { message: "record security type is outside the A-share scope" } });
    }
    const ind = record.industry;
    const ik = Object.keys(ind)[0];
    if (ik === "Assigned") {
      if (blank(ind.Assigned.code) || blank(ind.Assigned.name)) {
        return rej({ InvalidRecord: { message: "assigned industry requires code and name" } });
      }
    } else if (blank(ind[ik].reason)) {
      return rej({ InvalidRecord: { message: "industry missing/conflict requires a reason" } });
    }

    const k = keyOf(record);
    const ko = keyObj(record);
    if (byKey.has(k)) {
      const prevSid = byKey.get(k);
      if (prevSid === record.source_security_id) {
        return rej({ DuplicateSecurity: { key: ko } });
      }
      return rej({ IdentityConflict: {
        message: `security key ${dbgKey(ko)} maps to both source security ids ${prevSid} and ${record.source_security_id}` } });
    }
    byKey.set(k, record.source_security_id);
    if (bySid.has(record.source_security_id)) {
      return rej({ IdentityConflict: {
        message: `source security id ${record.source_security_id} maps to both ${dbgKey(bySid.get(record.source_security_id))} and ${dbgKey(ko)}` } });
    }
    bySid.set(record.source_security_id, ko);
  }

  if (previous != null) {
    const currentKeys = new Set(snapshot.records.map(keyOf));
    const disappeared = previous.records
      .filter((r) => !currentKeys.has(keyOf(r)))
      .map(keyObj)
      .sort(cmpKey);
    if (disappeared.length > 0) {
      return rej({ MemberDisappeared: { members: disappeared.map((k) => ({ exchange: k.exchange, symbol: k.symbol })) } });
    }
  }
  return { accepted: true };
}
