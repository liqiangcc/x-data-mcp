# NM-058 — S1 MCP transport core (handleJson/runStdio + ToolHandler)

Ports `src/mcp.rs:514-632` to `src_node/mcp/`: newline-delimited JSON-RPC
2.0 over stdio — `handleJson`, `runStdio`, `ApiError`, `toolResult`,
`rpcError`, and the `ToolHandler` contract (`tools()` / `call(name,args)`).

## Differential approach

Oracle = the real Rust `mcp_server` binary (`cargo build --bin mcp_server`,
rustup 1.98.1) fed `scenarios/mcp_transport.lines` over stdin — black-box
stdout comparison; no Rust source changes. Node side runs
`mcp_transport_runner.mjs` (runStdio + stub handler reproducing the
deterministic empty-store responses of the legacy `McpApi`). Byte-for-byte
stdout diff over all 165 response lines: **0 diffs**.

## Coverage

- `mcp_transport.lines` (167 requests): initialize/tools/list/tools/call,
  tool-level error results, unknown method/tool, missing tool name, all
  observed serde_json parse-error classes (incl. recursion limit,
  surrogates, control bytes, BOM), notifications (incl. `id:null` and
  id-bearing `notifications/*` not being dropped), duplicate keys,
  verbatim `id` echo for int/BigInt/float lexemes.
- `mcp_transport_eof.lines`: final line without trailing `\n` still
  answered; `mcp_transport_utf8.lines`: invalid UTF-8 aborts both sides
  (exit 1) with identical stdout prefix.
- Unit vectors: 26 parse-error strings and 19 ryu-style f64 formats
  extracted verbatim from the oracle.

## Byte-parity machinery

`src_node/mcp/serde_json.mjs` is a byte-oriented serde_json::Value clone:
error positions are byte columns; integers keep i64/u64 exactness
(`BigInt` beyond 2^53); float lexemes keep f64-ness (`F64` wrapper) so
`1.0`/`2e2`/`-0` echo as `1.0`/`200.0`/`-0.0`; f64 output uses
ryu::format_finite notation (fixed for exponent −5..=15, else
`d[.ddd]e±x`, `.0` on integral floats); object keys serialize in UTF-8
byte order. Handler `args` arrive unwrapped (F64→Number, BigInt kept).

## Known divergences (documented, not blocking)

- serde arg-decode messages (`missing field`, `invalid type`, `unknown
  field`) live in the *handler* layer; the stub reproduces them only for
  exercised paths. Full `from_value` parity is a later wire slice.
- Handler-produced floats serialize as JS Numbers (integral → int form);
  a handler needing f64 echo emits `F64`.
- Invalid UTF-8 mid-stream aborts both implementations (exit 1); Node's
  stderr text differs from Rust's `io::Error` display (stdout is equal).

## Contents

- `probe-results.jsonl` — 176 verdicts, all PASS (165 line diffs +
  framing + 47 unit vectors).
- `negative-selftest.jsonl` (exit 1), `results.json`, `SHA256SUMS`.

## Reproduce

    node scripts/node_migration_p4/mcp_transport_probe.mjs .        # exit 0
    node scripts/node_migration_p4/mcp_transport_probe.mjs . diff   # selftest, exit 1
