// serde_json::from_slice::<Input>-equivalent decoder for consistency_check.
//
// Schema (src/bin/consistency_check.rs + file_consistency.rs):
//   Input  { files: Vec<InputFile>, compatibility: Option<CompatibilityEvidence> }
//   InputFile { role: FileRole, bytes: u64, sha256, source_url: String,
//     acquisition_started_at/completed_at: DateTime<Utc>,
//     last_modified/etag/source_revision/source_identity: Option<String>,
//     byte_integrity: ByteIntegrity }
//   CompatibilityEvidence { evidence_ref, rule_version,
//     compatibility_set_revision, compatibility_set_identity: String,
//     member_bindings: BTreeMap<FileRole, MemberBinding>,
//     valid_window: CompatibilityWindow{valid_from, valid_until} }
//   MemberBinding { sha256: String, source_revision/identity: Option<String> }
// All structs deny_unknown_fields; serde also binds structs from sequences
// positionally ("invalid length N" / "trailing characters" on arity).
//
// Two passes over the file bytes: parseJsonFile gives serde_json::Value
// syntax errors (real line/column); on success a second byte scan records
// every node's token span so semantic errors report at serde positions —
// invalid type/value/variant/datetime at token end, unknown/duplicate
// field at key end, missing field at `}` end, invalid length at `]` end,
// ExpectedSomeValue at token start+1 (peek).
//
// Covered classes verified against the Rust binary (see scenario corpus);
// exotic enum-as-map values and malformed-year strings are the documented
// boundary.

import { serF64 } from "../mcp/serde_json.mjs";
import { parseJsonFile } from "./_shared.mjs";

// ------------------------------------------------------------ position math
let NL = []; // indices of '\n' bytes in the current input
const posOf = (consumed) => {
  let line = 1, lineStart = 0;
  for (const i of NL) {
    if (i >= consumed) break;
    line++;
    lineStart = i + 1;
  }
  return { line, col: consumed - lineStart };
};
class Decode extends Error {}
const failAt = (consumed, msg) => {
  const p = posOf(consumed);
  throw new Decode(`${msg} at line ${p.line} column ${p.col}`);
};

// --------------------------------------------------------------- annotation
// Runs only on input that already passed parseJsonFile — records node spans.
function annotate(buf) {
  let pos = 0;
  const WS = (b) => b === 0x20 || b === 0x09 || b === 0x0a || b === 0x0d;
  const skipWs = () => { while (pos < buf.length && WS(buf[pos])) pos++; };
  function node() {
    skipWs();
    const start = pos;
    const b = buf[pos];
    if (b === 0x22) return { kind: "str", v: str(), start, end: pos };
    if (b === 0x7b) {
      pos++;
      const entries = [];
      skipWs();
      if (buf[pos] === 0x7d) { pos++; return { kind: "obj", entries, start, end: pos }; }
      for (;;) {
        skipWs();
        const key = str();
        const keyEnd = pos;
        skipWs(); pos++; // ':'
        const v = node();
        entries.push({ key, keyEnd, node: v });
        skipWs();
        if (buf[pos] === 0x2c) { pos++; continue; }
        pos++; // '}'
        return { kind: "obj", entries, start, end: pos };
      }
    }
    if (b === 0x5b) {
      pos++;
      const items = [];
      skipWs();
      if (buf[pos] === 0x5d) { pos++; return { kind: "arr", items, start, end: pos }; }
      for (;;) {
        items.push(node());
        skipWs();
        if (buf[pos] === 0x2c) { pos++; continue; }
        pos++; // ']'
        return { kind: "arr", items, start, end: pos };
      }
    }
    if (b === 0x74) { pos += 4; return { kind: "bool", v: true, start, end: pos }; }
    if (b === 0x66) { pos += 5; return { kind: "bool", v: false, start, end: pos }; }
    if (b === 0x6e) { pos += 4; return { kind: "null", v: null, start, end: pos }; }
    while (pos < buf.length && !WS(buf[pos]) && buf[pos] !== 0x2c && buf[pos] !== 0x7d && buf[pos] !== 0x5d)
      pos++;
    const lex = buf.toString("ascii", start, pos);
    if (/^-?(0|[1-9]\d*)$/.test(lex)) {
      const v = BigInt(lex);
      if (lex === "-0") return { kind: "f64", v: -0, start, end: pos };
      return { kind: "int", v, start, end: pos };
    }
    return { kind: "f64", v: Number(lex), start, end: pos };
  }
  function str() {
    const start = pos;
    pos++; // opening quote
    while (buf[pos] !== 0x22) {
      if (buf[pos] === 0x5c) pos += buf[pos + 1] === 0x75 ? 6 : 2;
      else if (buf[pos] >= 0xf0) pos += 4;
      else if (buf[pos] >= 0xe0) pos += 3;
      else if (buf[pos] >= 0xc0) pos += 2;
      else pos++;
    }
    pos++;
    return JSON.parse(buf.toString("utf8", start, pos));
  }
  skipWs();
  return node();
}

// ----------------------------------------------------------------- desc text
const rustStr = (s) =>
  '"' + s.replace(/[\\"\n\r\t]|[\x00-\x1f\x7f]/g, (c) => {
    if (c === '"') return '\\"';
    if (c === "\\") return "\\\\";
    if (c === "\n") return "\\n";
    if (c === "\r") return "\\r";
    if (c === "\t") return "\\t";
    return `\\u{${c.codePointAt(0).toString(16)}}`;
  }) + '"';
const desc = (n) =>
  n.kind === "null" ? "null" :
  n.kind === "bool" ? `boolean \`${n.v}\`` :
  n.kind === "int" ? `integer \`${n.v}\`` :
  n.kind === "f64" ? `floating point \`${serF64(n.v)}\`` :
  n.kind === "str" ? `string ${rustStr(n.v)}` :
  n.kind === "arr" ? "sequence" : "map";
// serde_json position for invalid_type: scalars report at token end;
// mismatched containers report at the open-bracket index (peek position).
const invalidType = (n, expected) =>
  failAt(n.kind === "obj" || n.kind === "arr" ? n.start : n.end,
    `invalid type: ${desc(n)}, expected ${expected}`);
const expectedFields = (xs) =>
  xs.length === 1 ? `\`${xs[0]}\`` :
  xs.length === 2 ? `\`${xs[0]}\` or \`${xs[1]}\`` :
  `one of ${xs.map((x) => `\`${x}\``).join(", ")}`;

// ------------------------------------------------------------------- scalars
const I64_MIN = -(1n << 63n);
const U64_MAX = (1n << 64n) - 1n;
const isObj = (n) => n.kind === "obj";
const isArr = (n) => n.kind === "arr";

function decString(n) {
  if (n.kind !== "str") invalidType(n, "a string");
  return n.v;
}
function decU64(n) {
  if (n.kind === "int") {
    if (n.v >= 0n && n.v <= U64_MAX) return Number(n.v);
    if (n.v >= I64_MIN && n.v < 0n) // in-range negative i64
      failAt(n.end, `invalid value: integer \`${n.v}\`, expected u64`);
    // outside i64/u64 -> serde parsed the lexeme as f64
    failAt(n.end, `invalid type: floating point \`${serF64(Number(n.v))}\`, expected u64`);
  }
  invalidType(n, "u64");
}
// chrono DateTime<Utc> RFC3339 classes: premature end / invalid chars /
// out of range / trailing input (+ leap :60 and signed years accepted).
function decDateTime(n) {
  if (n.kind !== "str")
    invalidType(n, "an RFC 3339 formatted date and time string");
  const res = chronoClassify(n.v);
  if (res !== null) failAt(n.end, res);
  // chrono accepts signed 4+-digit years; normalize "+2026"/"-2026" so the
  // downstream canonical re-serialization matches chrono's DateTime output
  return n.v.replace(/^([+-])(\d{4})-/, "$2-");
}
const TS_FULL = /^([+-]?\d{4,})-(\d{2})-(\d{2})[Tt ](\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?([Zz]|([+-])(\d{2}):(\d{2}))$/;
function chronoClassify(s) {
  const m = TS_FULL.exec(s);
  if (m) {
    const [, y, mo, d, hh, mm, ss, , , , oh, om] = m;
    const yy = Math.abs(parseInt(y, 10));
    const leap = yy % 4 === 0 && (yy % 100 !== 0 || yy % 400 === 0);
    const dim = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if (+mo < 1 || +mo > 12 || +d < 1 || +d > dim[+mo - 1] + (mo === "02" && leap ? 1 : 0) ||
        +hh > 23 || +mm > 59 || +ss > 60 || (oh !== undefined && (+oh > 23 || +om > 59)))
      return "input is out of range";
    return null; // valid
  }
  // complete datetime shape + junk -> trailing input
  const prefix = /^([+-]?\d{4,}-\d{2}-\d{2}[Tt ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:[Zz]|[+-]\d{2}:\d{2}))/.exec(s);
  if (prefix && s.slice(prefix[0].length).trim() !== "") return "trailing input";
  // a valid-but-incomplete datetime prefix -> premature end
  if (/^[+-]?\d{4,}-\d{2}-\d{2}([Tt ]\d{2}(:\d{2}(:\d{2}(\.\d*)?)?)?)?$/.test(s) ||
      /^\d{4}(-\d{0,2}){0,2}$/.test(s))
    return "premature end of input";
  return "input contains invalid characters";
}
function decEnum(n, variants) {
  if (n.kind === "str") {
    if (variants.includes(n.v)) return n.v;
    failAt(n.end, `unknown variant \`${n.v}\`, expected ${expectedFields(variants)}`);
  }
  // externally-tagged unit form {"variant":null} is accepted by serde_json
  if (n.kind === "obj" && n.entries.length === 1) {
    const k = n.entries[0].key;
    if (variants.includes(k) && n.entries[0].node.kind === "null") return k;
    if (!variants.includes(k))
      failAt(n.entries[0].keyEnd, `unknown variant \`${k}\`, expected ${expectedFields(variants)}`);
  }
  // ExpectedSomeValue at the offending byte (peek position = start + 1)
  failAt(n.start + 1, "expected value");
}
const opt = (n, inner) => (n.kind === "null" ? null : inner(n));

// ------------------------------------------------------------- struct decode
function decStruct(n, fields, structName) {
  if (isObj(n)) {
    const names = fields.map((f) => f.name);
    const out = {};
    const seen = new Set();
    for (const { key, keyEnd, node } of n.entries) {
      const f = fields.find((x) => x.name === key);
      if (!f) failAt(keyEnd, `unknown field \`${key}\`, expected ${expectedFields(names)}`);
      if (seen.has(key)) failAt(keyEnd, `duplicate field \`${key}\``);
      seen.add(key);
      out[f.name] = f.dec(node);
    }
    for (const f of fields) {
      if (f.name in out) continue;
      if (f.opt) out[f.name] = null;
      else failAt(n.end, `missing field \`${f.name}\``);
    }
    return out;
  }
  if (isArr(n)) {
    if (n.items.length < fields.length)
      failAt(n.end, `invalid length ${n.items.length}, expected struct ${structName} with ${fields.length} elements`);
    const out = {};
    fields.forEach((f, i) => { out[f.name] = f.dec(n.items[i]); });
    if (n.items.length > fields.length)
      // serde consumes the comma then errors at the extra element's first byte
      failAt(n.items[fields.length].start + 1, "trailing characters");
    return out;
  }
  invalidType(n, `struct ${structName}`);
}
const decVec = (n, itemDec) => {
  if (!isArr(n)) invalidType(n, "a sequence");
  return n.items.map(itemDec);
};

// ------------------------------------------------------------------- schema
const ROLES = ["base_industry", "day", "gbbq"];
const BYTE_INTEGRITY = ["verified", "unverified"];
const optStr = (n) => opt(n, decString);

const memberBinding = (n) => decStruct(n, [
  { name: "sha256", dec: decString },
  { name: "source_revision", dec: optStr, opt: true },
  { name: "source_identity", dec: optStr, opt: true },
], "MemberBinding");

const compatEvidence = (n) => decStruct(n, [
  { name: "evidence_ref", dec: decString },
  { name: "rule_version", dec: decString },
  { name: "compatibility_set_revision", dec: decString },
  { name: "compatibility_set_identity", dec: decString },
  {
    name: "member_bindings",
    dec: (x) => {
      if (!isObj(x)) invalidType(x, "a map");
      const out = {};
      for (const { key, keyEnd, node } of x.entries) {
        if (!ROLES.includes(key))
          failAt(keyEnd, `unknown variant \`${key}\`, expected ${expectedFields(ROLES)}`);
        out[key] = memberBinding(node); // BTreeMap: duplicate keys keep last
      }
      return out;
    },
  },
  {
    name: "valid_window",
    dec: (x) => decStruct(x, [
      { name: "valid_from", dec: decDateTime },
      { name: "valid_until", dec: decDateTime },
    ], "CompatibilityWindow"),
  },
], "CompatibilityEvidence");

const inputFile = (n) => decStruct(n, [
  { name: "role", dec: (x) => decEnum(x, ROLES) },
  { name: "bytes", dec: decU64 },
  { name: "sha256", dec: decString },
  { name: "source_url", dec: decString },
  { name: "acquisition_started_at", dec: decDateTime },
  { name: "acquisition_completed_at", dec: decDateTime },
  { name: "last_modified", dec: optStr, opt: true },
  { name: "etag", dec: optStr, opt: true },
  { name: "byte_integrity", dec: (x) => decEnum(x, BYTE_INTEGRITY) },
  { name: "source_revision", dec: optStr, opt: true },
  { name: "source_identity", dec: optStr, opt: true },
], "InputFile");

const INPUT_FIELDS = [
  { name: "files", dec: (x) => decVec(x, inputFile) },
  { name: "compatibility", dec: (x) => opt(x, compatEvidence), opt: true },
];

// Returns {files: [...], compatibility: {...}|null} mirroring the decoded
// Rust Input; throws Decode with the serde error text on failure.
export function decodeInput(buf) {
  const syntax = parseJsonFile(buf);
  if (!syntax.ok) throw new Decode(syntax.message);
  NL = [];
  for (let i = 0; i < buf.length; i++) if (buf[i] === 0x0a) NL.push(i);
  const root = annotate(buf);
  const out = decStruct(root, INPUT_FIELDS, "Input");
  return { files: out.files, compatibility: out.compatibility ?? null };
}
