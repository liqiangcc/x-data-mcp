// NM-106 — task_verification/wire.rs: closed protocol shapes; they
// carry target identity, never stopping evidence.
import { tStruct, tString } from "../market_update/serde.mjs";
import { ContractVersion } from "../data_supply/wire.mjs";

export const DESCRIPTION =
  "Explicit asynchronous stopping-evidence inspection of a task awaiting verification. " +
  "Admission preserves original claims. The installed verifier can prove an audited " +
  "revision-zero task never started using verified SQLite facts and conditional exclusion " +
  "of a later claim. Started or unsupported executions remain STOP_EVIDENCE_NOT_VERIFIED; " +
  "no caller assertion, PID absence or old run identity can unlock a task. Inspect the " +
  "separate check conclusion via get_task_progress.";

// VerifyStopped — deny_unknown_fields {contract_version, request_id,
// target_task_id} (declaration order).
export const tVerifyStopped = tStruct("VerifyStopped", [
  { name: "contract_version", dec: ContractVersion },
  { name: "request_id", dec: tString },
  { name: "target_task_id", dec: tString },
], true);

// VerificationReply — #[serde(untagged)] Accepted(WriteAcceptance) |
// Existing(RequestView). Both serialize as their inner value verbatim.
export const reply = (v) => v;
