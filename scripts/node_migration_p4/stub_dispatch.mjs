// Probe-local dispatch stub — the handleJson(handler, input) contract
// from src_node/mcp/transport.mjs (w2, pending merge): serialized
// response line or null for notifications. Envelope shapes mirrored
// from transport.mjs:59-125; the real module is injected the same way
// once it lands on this branch.
const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
const J = JSON.stringify;

const rpcError = (id, rpcCode, code, message) =>
  J({ jsonrpc: "2.0", id,
      error: { code: rpcCode, message, data: { code, message } } });

const toolResult = (id, value, isError) =>
  J({ jsonrpc: "2.0", id,
      result: { content: [{ type: "text", text: J(value) }],
                structuredContent: value, isError } });

export function handleJson(api, input) {
  let request;
  try {
    request = JSON.parse(input);
  } catch (e) {
    return rpcError(null, -32700, "parse_error", `parse_error: ${e.message}`);
  }
  const reqObj = isObject(request);
  const id = reqObj && "id" in request ? request.id : null;
  const method = reqObj && typeof request.method === "string" ? request.method : "";
  if (reqObj && method.startsWith("notifications/") && !("id" in request))
    return null;
  if (method === "initialize") {
    return J({ jsonrpc: "2.0", id, result: {
      protocolVersion: "2025-06-18", capabilities: { tools: {} },
      serverInfo: { name: "x-data-mcp", version: "0.2.0-preview.24" } } });
  }
  if (method === "tools/list") {
    return J({ jsonrpc: "2.0", id, result: { tools: api.tools() } });
  }
  if (method === "tools/call") {
    const params = reqObj ? request.params : null;
    const name = isObject(params) && typeof params.name === "string"
      ? params.name : null;
    if (name === null)
      return rpcError(id, -32602, "invalid_tool_call", "missing tool name");
    const args = isObject(params) && "arguments" in params
      ? params.arguments : {};
    try {
      return toolResult(id, api.call(name, args), false);
    } catch (e) {
      if (e && e.code) return toolResult(id, { code: e.code, message: e.message }, true);
      throw e; // non-coded throw = bug -> propagates (panic path)
    }
  }
  return rpcError(id, -32601, "method_not_found", `unsupported method: ${method}`);
}
