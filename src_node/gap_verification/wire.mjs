// NM-076 — src/gap_verification/wire.rs port.
// Closed protocol shape for `verify_data_gap`: the caller names only
// the target; conclusion, digests, subject version and verifier
// identity are server-derived and can never arrive over the wire
// (deny_unknown_fields rejects any pass-through field at decode).
import { tStruct, tString, tEnumUnit, tDate } from "../mcp/serde_value.mjs";

// ContractVersion — closed literal enum (data_supply::wire).
export const ContractVersion = tEnumUnit("ContractVersion", ["v1"]);

/// Closed period literal; `year` (or anything else) fails decode —
/// yearly stays derived.
export const VerifyGapPeriod = tEnumUnit("VerifyGapPeriod", ["day"]);

/// Which day range of which symbol to verify.
export const VerifyGapTarget = tStruct("VerifyGapTarget", [
  { name: "symbol", dec: tString },
  { name: "period", dec: VerifyGapPeriod },
  { name: "start_date", dec: tDate },
  { name: "end_date", dec: tDate },
]);

/// `verify_data_gap` input: `request_id` + target only.
export const VerifyDataGap = tStruct("VerifyDataGap", [
  { name: "contract_version", dec: ContractVersion },
  { name: "request_id", dec: tString },
  { name: "target", dec: VerifyGapTarget },
]);

/// Deterministic minimal intent for the contract: the wire envelope
/// (request_id/contract_version) never enters the normalized command,
/// so request_id replay matching stays deterministic.
export const gapPayload = (req) => ({ target: req.target });

export const DESCRIPTION =
  "Explicit asynchronous verification of a proven local coverage gap in one published QFQ daily version. The caller names only symbol/day/[start,end]; the subject head is frozen at acceptance and re-checked at decision commit. Decisions come only from the installed trusted source-gap verifier; source limitation or unavailability never produces a proven gap, and a decision never triggers update/backfill by itself. Proving a gap requires the verified active generation (or the installed inspected snapshot in legacy mode); without one runtime readiness is blocked and no gap can be proven.";
