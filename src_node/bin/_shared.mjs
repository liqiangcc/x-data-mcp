// Shared helpers for the early CLI bins — serde_json::to_string_pretty
// parity, Rust io::Error Display mapping, and a multi-line-aware
// from_slice-position remap for parseJsonBytes (serde reports real
// line/column for file inputs; mcp transport lines are always line 1).

import { F64, parseJsonBytes, serValue } from "../mcp/serde_json.mjs";

// serde_json::to_string_pretty: 2-space indent, `"key": value`, empty
// containers inline. `sorted` mirrors serde_json::Value (BTreeMap order);
// struct serializations keep JS insertion order (declaration order).
export function serPretty(v, { sorted = false, indent = 0 } = {}) {
  const pad = "  ".repeat(indent);
  if (v === null || v === undefined) return "null";
  if (v instanceof F64) return serValue(v);
  if (typeof v === "bigint" || typeof v === "number" || typeof v === "string" || typeof v === "boolean")
    return serValue(v);
  if (typeof v.toJSON === "function") return serPretty(v.toJSON(), { sorted, indent });
  if (Array.isArray(v)) {
    if (v.length === 0) return "[]";
    const items = v.map((x) => `${"  ".repeat(indent + 1)}${serPretty(x, { sorted, indent: indent + 1 })}`);
    return `[\n${items.join(",\n")}\n${pad}]`;
  }
  let keys = Object.keys(v);
  if (sorted) keys = keys.sort((a, b) => Buffer.compare(Buffer.from(a, "utf8"), Buffer.from(b, "utf8")));
  if (keys.length === 0) return "{}";
  const items = keys.map(
    (k) => `${"  ".repeat(indent + 1)}${JSON.stringify(k)}: ${serPretty(v[k], { sorted, indent: indent + 1 })}`,
  );
  return `{\n${items.join(",\n")}\n${pad}}`;
}

// Rust io::Error Display — `eprintln!("{error}")` on a boxed io error
// prints the OS text + errno; Node's fs errors carry e.code.
export const IO_TEXT = {
  EPERM: "Operation not permitted (os error 1)",
  ENOENT: "No such file or directory (os error 2)",
  EACCES: "Permission denied (os error 13)",
  ENOTDIR: "Not a directory (os error 20)",
  EISDIR: "Is a directory (os error 21)",
  EPIPE: "Broken pipe (os error 32)",
  ENAMETOOLONG: "File name too long (os error 36)",
  ELOOP: "Too many levels of symbolic links (os error 40)",
  ECONNABORTED: "Software caused connection abort (os error 103)",
  ECONNRESET: "Connection reset by peer (os error 104)",
  ETIMEDOUT: "Connection timed out (os error 110)",
  ECONNREFUSED: "Connection refused (os error 111)",
};
export function ioText(e) {
  if (e && typeof e.code === "string" && IO_TEXT[e.code]) return IO_TEXT[e.code];
  return e && e.message ? e.message : String(e);
}

// Rust main(): Err -> eprintln!("{error}") + exit 1.
export function errorText(e) {
  // InspectionError "io" kind embeds the Node fs message — remap the errno
  // prefix to the Rust io::Error Display text.
  if (e && e.kind === "io" && typeof e.message === "string") {
    const m = /^local input unavailable: ([A-Z]+):/.exec(e.message);
    if (m && IO_TEXT[m[1]]) return `local input unavailable: ${IO_TEXT[m[1]]}`;
  }
  if (e && typeof e.code === "string" && e.code.startsWith("E")) return ioText(e);
  return e && e.message ? e.message : String(e);
}
export function die(e) {
  process.stderr.write(errorText(e) + "\n");
  process.exit(1);
}

// parseJsonBytes reports "line 1 column <bytes-consumed>"; remap to the
// real (line, column) for multi-line file inputs: line = 1 + '\n' count
// among the consumed bytes, column = bytes consumed on the final line.
export function parseJsonFile(buf) {
  const r = parseJsonBytes(buf);
  if (r.ok) return r;
  const m = / at line 1 column (\d+)$/.exec(r.message);
  if (!m) return r;
  const consumed = Number(m[1]);
  let lineStart = 0, line = 1;
  for (let i = 0; i < consumed && i < buf.length; i++) {
    if (buf[i] === 0x0a) {
      line++;
      lineStart = i + 1;
    }
  }
  return { ok: false, message: r.message.replace(/ at line 1 column \d+$/, ` at line ${line} column ${consumed - lineStart}`) };
}
