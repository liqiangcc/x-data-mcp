// Worker-side handler factory for acquire_service — loaded inside each
// handler worker by local_runtime/worker_entry (factory contract:
// createHandler(args) -> api with .call(name,args) synchronous).
//
// Rust's `factory = move || AcquireApi::new(service.clone())` shares one
// Arc'd service across threads. Workers cannot share the instance, so
// each attaches to the same durable store via AcquireService.attach —
// no mkdir/reconcile (the daemon's open() ran them once). ApiError is
// the real transport class so handleToolCall's instanceof routing sees
// adapter errors as typed replies, not worker panics.
import { AcquireApi } from "../acquire/mcp.mjs";
import { AcquireService } from "../acquire/service.mjs";
import { ApiError } from "../mcp/transport.mjs";

export function createHandler({ root, source }) {
  const service = AcquireService.attach(root, source);
  return new AcquireApi(service, { ApiError });
}
