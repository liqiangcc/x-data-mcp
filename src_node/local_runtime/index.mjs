// src_node/local_runtime — Node port of src/local_runtime/*.
// DatabaseLease is re-exported from storage/database_lease.mjs
// (NM-056) — reconciled, not duplicated (TASK-NM-062 AC).
export { DatabaseLease } from "../storage/database_lease.mjs";
export {
  Endpoint, connect, privateMetadata, identityOf, invalid,
} from "./paths.mjs";
export { readFrame, writeFrame, SocketSource } from "./frame.mjs";
export { WorkerGate, SERVE_STOP_CELL } from "./gate.mjs";
export { LeaseRef } from "./lease_ref.mjs";
export { serve, defaultConfig } from "./serve.mjs";
