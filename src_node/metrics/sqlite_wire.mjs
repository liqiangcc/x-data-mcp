// sqlite_metrics.rs wire-side decode shapes — the stored-evidence
// structures the InputRepository::observations path deserializes.
// Decode parity only; unreachable until the SqliteMetrics repository
// slice lands (observations is behind the typed seam).

import {
  tEnumUnit, tI32, tString, tStruct, tVec,
} from "../mcp/serde_value.mjs";

export const Origin = tEnumUnit("Origin", ["fixture"]);
export const Coverage = tEnumUnit("Coverage",
  ["complete", "incomplete", "unknown"]);
export const YearCoverage = tStruct("YearCoverage", [
  { name: "year", dec: tI32 },
  { name: "status", dec: Coverage },
  { name: "evidence_ref", dec: tString },
]);
export const AnalysisCoverage = tStruct("AnalysisCoverage", [
  { name: "origin", dec: Origin },
  { name: "years", dec: tVec(YearCoverage) },
]);

// market_data CoverageStatus/CoverageEvidence (wire shape for the
// embedded local_yearly_derivation_v1 evidence arm).
export const CoverageStatus = tEnumUnit("CoverageStatus",
  ["complete", "incomplete", "unknown"]);
export const CoverageEvidence = tStruct("CoverageEvidence", [
  { name: "status", dec: CoverageStatus },
  { name: "reason", dec: tString },
]);
// struct EmbeddedEvidence { years: BTreeSet<i32>, coverage: CoverageEvidence }
// — BTreeSet decodes from a sequence.
export const EmbeddedEvidence = tStruct("EmbeddedEvidence", [
  { name: "years", dec: tVec(tI32) },
  { name: "coverage", dec: CoverageEvidence },
]);
