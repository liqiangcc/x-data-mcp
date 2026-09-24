// NM-098 — src/publication_verification/wire.rs port.
// Closed publication-inspection protocol; callers supply identity, not
// evidence. VerifyPublication{contract_version,request_id,target_task_id}
// with deny_unknown_fields.
import { tStruct, tString } from "../mcp/serde_value.mjs";
import { ContractVersion } from "../data_supply/wire.mjs";

export const VerifyPublication = tStruct("VerifyPublication", [
  { name: "contract_version", dec: ContractVersion },
  { name: "request_id", dec: tString },
  { name: "target_task_id", dec: tString },
]);

export const DESCRIPTION =
  "Explicit asynchronous inspection of durable publication evidence for an " +
  "unresolved market task. Reads fixed SQLite publication versions, receipts " +
  "and projections; missing receipt is insufficient evidence, not proof of " +
  "failure. The separate check never publishes, changes heads or releases " +
  "the original task claims. A prior all-recorded conclusion can bind a new " +
  "request without a new inspection. Inspect publication_check via " +
  "get_task_progress. Corrupt evidence and interrupted inspections retain " +
  "protection; no caller approval or force flags. This ready interface does " +
  "not make the official data source production-ready.";
