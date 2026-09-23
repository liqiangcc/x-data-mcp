// Bounded `infoharbor_ex.code` parser — Node port of
// src/catalog_tdx/ex_code.rs::parse.
//
// Anchored shape: `code|name|keywords` per line, strict GB18030 decoded
// BEFORE splitting (trail bytes can equal `|`), CRLF terminators.
// Fields >= 3 required; only code+name bound, keywords kept uninterpreted
// (may contain `|`). Duplicate code identity rejects the file.
// Error precedence: Decode -> per line EmptyLine -> FieldCount -> BadCode
// -> EmptyName -> DuplicateCode (BTreeMap insertion order = first-seen).
// BadCode value rendered via Rust {:?} Debug escaping.

export class ExCodeError extends Error {
  constructor(message, kind, extra = {}) {
    super(message);
    this.name = "ExCodeError";
    this.kind = kind;
    Object.assign(this, extra);
  }
}

// Rust `{:?}` Debug for &str — escapes " \ \n \r \t \0, other control as
// \u{hex}; printable (incl. ' and non-ASCII graphemes) passes through.
// Verified against live rustc output (same helper semantics as tdxhy).
const dbg = (s) => '"' + [...s].map((ch) => {
  const c = ch.codePointAt(0);
  if (ch === '"') return '\\"';
  if (ch === '\\') return '\\\\';
  if (ch === '\n') return '\\n';
  if (ch === '\r') return '\\r';
  if (ch === '\t') return '\\t';
  if (ch === '\0') return '\\0';
  if (c < 0x20 || c === 0x7f) return `\\u{${c.toString(16)}}`;
  return ch;
}).join('') + '"';

const decoder = new TextDecoder("gb18030", { fatal: true });
const DIGITS6 = /^[0-9]{6}$/;

/**
 * @param {Buffer|Uint8Array} bytes — typed boundary, no coercion
 * @returns {{line:number,code:string,name:string}[]}
 */
export function parse(bytes) {
  if (!(bytes instanceof Uint8Array)) {
    throw new ExCodeError("infoharbor_ex.code input must be a Buffer or Uint8Array", "invalid_input");
  }
  let text;
  try {
    text = decoder.decode(bytes);
  } catch {
    throw new ExCodeError("infoharbor_ex.code is not valid GB18030", "decode");
  }
  const seen = new Map();
  const rows = [];
  const lines = text.split("\r\n");
  for (let index = 0; index < lines.length; index++) {
    const line = lines[index];
    const number = index + 1;
    if (line === "") {
      if (index === lines.length - 1) break;
      throw new ExCodeError(`infoharbor_ex.code line ${number} is empty`, "empty_line", { line: number });
    }
    const fields = line.split("|");
    if (fields.length < 3) {
      throw new ExCodeError(
        `infoharbor_ex.code line ${number} has ${fields.length} fields, expected at least 3`,
        "field_count", { fields: fields.length, line: number });
    }
    const code = fields[0];
    if (!DIGITS6.test(code)) {
      throw new ExCodeError(
        `infoharbor_ex.code line ${number} code ${dbg(code)} is not exactly six digits`,
        "bad_code", { line: number, value: code });
    }
    if (fields[1] === "") {
      throw new ExCodeError(`infoharbor_ex.code line ${number} has an empty name`, "empty_name", { line: number });
    }
    if (seen.has(code)) {
      throw new ExCodeError(
        `infoharbor_ex.code code ${dbg(code)} appears more than once (lines ${seen.get(code)} and ${number})`,
        "duplicate_code", { code, first_line: seen.get(code), line: number });
    }
    seen.set(code, number);
    rows.push({ code, line: number, name: fields[1] });
  }
  return rows;
}

/** Index rows by code — mechanical projection (duplicates already rejected). */
export function nameMap(rows) {
  return new Map(rows.map((r) => [r.code, r.name]));
}
