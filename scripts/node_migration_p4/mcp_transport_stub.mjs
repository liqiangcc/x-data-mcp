// Probe-only stub handler for NM-058 transport verification.
//
// Reproduces the deterministic, store-independent responses of the legacy
// `mcp_server` ToolHandler (src/mcp.rs:633-694) for the scenario inputs:
// serde arg-decode ApiErrors, empty-store results, unknown_tool. Anything
// else throws a clearly-marked stub error so scenario drift is loud.
import { ApiError } from "../../src_node/mcp/transport.mjs";

const missing = (field) => new ApiError("invalid_arguments", `missing field \`${field}\``);
const wrongType = (desc, struct) =>
  new ApiError("invalid_arguments", `invalid type: ${desc}, expected struct ${struct}`);
const typeDesc = (v) => {
  if (typeof v === "string") return `string "${v}"`;
  if (typeof v === "number") return Number.isInteger(v) ? `integer \`${v}\`` : `floating point \`${v}\``;
  if (typeof v === "bigint") return `integer \`${v}\``;
  if (typeof v === "boolean") return `boolean \`${v}\``;
  if (v === null) return "null";
  if (Array.isArray(v)) return "a sequence";
  return "a map";
};
const isObj = (v) => v !== null && typeof v === "object" && !Array.isArray(v);

export function stubHandler(tools) {
  return {
    tools() {
      return tools;
    },
    call(name, args) {
      switch (name) {
        case "run_worker_once": {
          // EmptyArgs: deny_unknown_fields, no fields expected
          if (args === null) throw wrongType("null", "EmptyArgs");
          if (!isObj(args)) throw wrongType(typeDesc(args), "EmptyArgs");
          const keys = Object.keys(args);
          if (keys.length > 0)
            throw new ApiError(
              "invalid_arguments",
              `unknown field \`${keys[0]}\`, there are no fields`,
            );
          return { idle: true, task_id: null, status: null };
        }
        case "get_task": {
          if (!isObj(args)) throw wrongType(typeDesc(args), "GetTaskArgs");
          if (!("task_id" in args)) throw missing("task_id");
          // empty JsonFileStore: any task_id -> not found
          throw new ApiError("task_not_found", "task was not found");
        }
        case "get_request": {
          if (!isObj(args)) throw wrongType(typeDesc(args), "GetRequestArgs");
          if (!("request_id" in args)) throw missing("request_id");
          if (String(args.request_id).trim() === "")
            throw new ApiError("invalid_arguments", "empty request_id");
          return { bound: false, request_id: args.request_id, task: null };
        }
        case "cancel_task": {
          if (!isObj(args)) throw wrongType(typeDesc(args), "CancelTaskArgs");
          if (!("task_id" in args)) throw missing("task_id");
          throw new ApiError("task_not_found", `task ${args.task_id} was not found`);
        }
        case "retry_task": {
          if (!isObj(args)) throw wrongType(typeDesc(args), "RetryTaskArgs");
          for (const f of ["original_task_id", "new_request_id", "selected_scopes"])
            if (!(f in args)) throw missing(f);
          throw new ApiError("task_not_found", `task ${args.original_task_id} was not found`);
        }
        case "start_collection": {
          if (!isObj(args)) throw wrongType(typeDesc(args), "StartCollectionArgs");
          for (const f of ["request_id", "symbols", "periods"])
            if (!(f in args)) throw missing(f);
          for (const p of args.periods)
            if (p !== "day" && p !== "year")
              throw new ApiError("invalid_period", `unsupported period: ${p}`);
          throw new ApiError("stub", "start_collection accepts are out of probe scope");
        }
        default:
          throw new ApiError("unknown_tool", `unsupported tool: ${name}`);
      }
    },
  };
}
