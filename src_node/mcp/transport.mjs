// Node port of src/mcp.rs:514-632 — the MCP JSON-RPC-2.0 line transport.
//
// ToolHandler-equivalent contract (src/mcp.rs:629-632):
//   handler.tools()            -> the tools array for tools/list
//   handler.call(name, args)   -> result value, or throws ApiError
// Handlers are synchronous, matching the Rust &self method shape (all
// storage drivers are synchronous better-sqlite3 calls).
//
// Handler `args` arrive unwrapped: F64 -> Number, i64/u64 beyond the JS
// safe range -> BigInt (exactness preserved; float-lexeme `1.0` arrives
// as 1 — same loss as JSON.parse). The request `id` is echoed verbatim.

import { Buffer, isUtf8 } from "node:buffer";
import { once } from "node:events";
import { F64, parseJsonBytes, serValue } from "./serde_json.mjs";

export const PROTOCOL_VERSION = "2025-06-18";
export const SERVER_NAME = "x-data-mcp";
// env!("CARGO_PKG_VERSION") at src/mcp.rs:557 — the wire value is the
// Cargo.toml version, pinned so both implementations answer identically.
export const SERVER_VERSION = "0.2.0-preview.24";

// src/mcp.rs:114-124 — ApiError serializes as {code,message,scopes?,
// outcome_certainty?,details?} with absent Option fields skipped.
export class ApiError extends Error {
  constructor(code, message, extra = {}) {
    super(message);
    this.code = code;
    if (extra.scopes !== undefined) this.scopes = extra.scopes;
    if (extra.outcome_certainty !== undefined) this.outcome_certainty = extra.outcome_certainty;
    if (extra.details !== undefined) this.details = extra.details;
  }
  toJSON() {
    const out = { code: this.code, message: this.message };
    if (this.scopes !== undefined) out.scopes = this.scopes;
    if (this.outcome_certainty !== undefined) out.outcome_certainty = this.outcome_certainty;
    if (this.details !== undefined) out.details = this.details;
    return out;
  }
}

const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);

// Deep-convert parsed arguments for the handler: F64 -> Number.
// BigInt stays (safe-range integers already arrive as Number).
const unwrap = (v) => {
  if (v instanceof F64) return v.v;
  if (Array.isArray(v)) return v.map(unwrap);
  if (isObject(v)) {
    const out = {};
    for (const k of Object.keys(v)) out[k] = unwrap(v[k]);
    return out;
  }
  return v;
};

// src/mcp.rs:589-599 — content[0].text is the serialized JSON and
// structuredContent carries the value.
export function toolResult(id, value, isError) {
  return {
    jsonrpc: "2.0",
    id,
    result: {
      content: [{ type: "text", text: serValue(value) }],
      structuredContent: value,
      isError,
    },
  };
}

// src/mcp.rs:601-618
export function rpcError(id, rpcCode, code, message) {
  return {
    jsonrpc: "2.0",
    id,
    error: { code: rpcCode, message, data: { code, message } },
  };
}

// src/mcp.rs:574-587
function handleToolCall(handler, id, params) {
  const name = isObject(params) && typeof params.name === "string" ? params.name : null;
  if (name === null) return rpcError(id, -32602, "invalid_tool_call", "missing tool name");
  const args = isObject(params) && "arguments" in params ? params.arguments : {};
  let value;
  try {
    value = handler.call(name, unwrap(args));
  } catch (error) {
    if (error instanceof ApiError) return toolResult(id, error.toJSON(), true);
    throw error; // Rust call() is typed Result — a non-ApiError throw is a bug
  }
  return toolResult(id, value, false);
}

// src/mcp.rs:527-572. input is the raw line (Buffer or string); returns the
// serialized response line, or null for notifications.
export function handleJson(handler, input) {
  const parsed = parseJsonBytes(input);
  if (!parsed.ok)
    return serValue(rpcError(null, -32700, "parse_error", parsed.message));
  const request = parsed.value;
  const reqObj = isObject(request);
  const id = reqObj && "id" in request ? request.id : null;
  const method = reqObj && typeof request.method === "string" ? request.method : "";
  if (reqObj && method.startsWith("notifications/") && !("id" in request)) return null;
  let response;
  if (method === "initialize") {
    response = {
      jsonrpc: "2.0",
      id,
      result: {
        protocolVersion: PROTOCOL_VERSION,
        capabilities: { tools: {} },
        serverInfo: { name: SERVER_NAME, version: SERVER_VERSION },
      },
    };
  } else if (method === "tools/list") {
    response = { jsonrpc: "2.0", id, result: { tools: handler.tools() } };
  } else if (method === "tools/call") {
    response = handleToolCall(handler, id, reqObj && "params" in request ? request.params : null);
  } else {
    response = rpcError(id, -32601, "method_not_found", `unsupported method: ${method}`);
  }
  return serValue(response);
}

// src/mcp.rs:514-525 — newline-delimited JSON on stdin, one flushed
// response per line. Rust lines() strips a trailing \r and errors on
// invalid UTF-8 (the error propagates and stops the loop); mirrored here.
export async function runStdio(handler, io = {}) {
  const input = io.input ?? process.stdin;
  const output = io.output ?? process.stdout;
  const write = async (line) => {
    if (!output.write(line + "\n")) await once(output, "drain");
  };
  let pending = Buffer.alloc(0);
  const processLine = async (line) => {
    if (line.length > 0 && line[line.length - 1] === 0x0d) line = line.subarray(0, -1);
    if (!isUtf8(line)) throw new Error("stream did not contain valid UTF-8");
    const out = handleJson(handler, line);
    if (out !== null) await write(out);
  };
  for await (const chunk of input) {
    pending = Buffer.concat([pending, chunk]);
    let at;
    while ((at = pending.indexOf(0x0a)) >= 0) {
      const line = pending.subarray(0, at);
      pending = pending.subarray(at + 1);
      await processLine(line);
    }
  }
  if (pending.length > 0) await processLine(pending);
}
