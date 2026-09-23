// Pure catalog composition — Node port of
// src/catalog_tdx/compose.rs::compose.
//
// Joins scoped members to infoharbor_ex.code names (S4), resolves the TDX
// research-industry level-2 assignment through incon.dat #TDXRSHY (S5),
// and assembles the candidate snapshot (S6). Every Assigned record
// carries scheme "tdx_research_industry".
//
// Rulings honored:
// - P1 reject_candidate (comment 5710586446): a nameless scope member is a
//   governed exclusion — never enters the candidate, enumerated verbatim
//   in `nameless`, does not block publication.
// - P2 scheme label: records carry `tdx_research_industry`; never
//   unlabeled or eastmoney.
// - Diagnostics cover PUBLISHED (named) members only — nameless members
//   never appear in missing_industry or t_code_residuals.
//
// Error precedence: MissingDictionary("TDXRSHY") before any member;
// per member BadMarket before the nameless check.
//
// `source` binds the inspected snapshot identity; `acquiredAt` is the
// manifest acquisition time — never an industry effective time. The
// completeness count is published-member count, NOT market-wide
// completeness.

import { researchLevel2, sectionMap } from "../parsers/incon.mjs";

export class ComposeError extends Error {
  constructor(message, kind, extra = {}) {
    super(message);
    this.name = "ComposeError";
    this.kind = kind;
    Object.assign(this, extra);
  }
}

// Rust {:?} Debug for MemberId in BadMarket — (u8, "code").
const dbgId = (m, c) => `(${m}, "${c.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}")`;

const isMember = (m) => m !== null && typeof m === "object" &&
  Number.isInteger(m.market) && m.market >= 0 && m.market <= 255 &&
  typeof m.code === "string" && typeof m.t_code === "string" &&
  typeof m.x_code === "string" && Number.isInteger(m.line) && m.line >= 1;

/**
 * @param {{line,market,code,t_code,x_code}[]} members — scoped members (input order)
 * @param {Map<string,string>} names — ex_code name map (code -> name)
 * @param {{name,entries}[]} sections — incon.dat sections
 * @param {string} source — snapshot identity, e.g. "official-file:<sha16>"
 * @param {string|Date} acquiredAt — acquisition timestamp (ISO string)
 */
export function compose(members, names, sections, source, acquiredAt) {
  if (!Array.isArray(members)) throw new ComposeError("members must be an array", "invalid_input");
  if (!(names instanceof Map)) throw new ComposeError("names must be a Map(code -> name)", "invalid_input");
  if (!Array.isArray(sections)) throw new ComposeError("sections must be an array", "invalid_input");
  if (typeof source !== "string") throw new ComposeError("source must be a string", "invalid_input");
  const acquired = acquiredAt instanceof Date ? acquiredAt.toISOString() : acquiredAt;
  if (typeof acquired !== "string" || Number.isNaN(Date.parse(acquired))) {
    throw new ComposeError("acquired_at must be an ISO timestamp or Date", "invalid_input");
  }

  // MissingDictionary is checked before any member — even empty members.
  const researchEntries = sectionMap(sections, "TDXRSHY");
  if (researchEntries === null) {
    throw new ComposeError("incon.dat has no #TDXRSHY section", "missing_dictionary");
  }
  const research = new Map(researchEntries);
  const tEntries = sectionMap(sections, "TDXNHY");
  const tDictionary = tEntries === null ? null : new Set(tEntries.map(([code]) => code));

  const records = [];
  const nameless = [];
  const missingIndustry = [];
  const tCodeResiduals = [];

  for (let i = 0; i < members.length; i++) {
    const member = members[i];
    if (!isMember(member)) {
      throw new ComposeError(`members[${i}] is not a scoped member shape`, "invalid_input");
    }
    const id = [member.market, member.code];
    // BadMarket checked BEFORE the nameless exclusion (Rust match order).
    const exchange = member.market === 0 ? "shenzhen"
      : member.market === 1 ? "shanghai"
      : null;
    if (exchange === null) {
      throw new ComposeError(
        `scope member ${dbgId(member.market, member.code)} has market digit outside the anchored {0,1} range`,
        "bad_market", { member: id });
    }
    const name = names.get(member.code);
    if (name === undefined) {
      nameless.push(id);
      continue;
    }
    let industry;
    const l2 = researchLevel2(member.x_code);
    if (l2 === null) {
      missingIndustry.push(id);
      industry = { kind: "missing", reason: `tdx_research: no level-2 code derivable from ${member.x_code}` };
    } else {
      const iname = research.get(l2);
      industry = iname === undefined
        ? (missingIndustry.push(id),
           { kind: "missing", reason: `tdx_research: level-2 code ${l2} absent from TDXRSHY` })
        : { code: l2, kind: "assigned", name: iname, scheme: "tdx_research_industry" };
    }
    if (tDictionary !== null && !tDictionary.has(member.t_code)) {
      tCodeResiduals.push({ member: id, t_code: member.t_code });
    }
    records.push({
      acquired_at: acquired,
      exchange,
      industry,
      name,
      security_type: "a_share",
      source,
      source_security_id: `tdxhy:${member.market}:${member.code}`,
      symbol: member.code,
    });
  }

  return {
    missing_industry: missingIndustry,
    nameless,
    snapshot: records.length === 0 ? null : {
      acquired_at: acquired,
      completeness: { expected_count: records.length, kind: "complete" },
      records,
      source,
    },
    t_code_residuals: tCodeResiduals,
    t_dictionary_checked: tDictionary !== null,
  };
}

/** ex_code::name_map equivalent: rows [{code,name}] -> Map(code -> name). */
export function nameMap(rows) {
  return new Map(rows.map((r) => [r.code, r.name]));
}
