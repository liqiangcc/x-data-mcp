// Bounded `incon.dat` sectioned dictionary parser — Node port of
// src/catalog_tdx/incon.rs::parse + code_level/research_level2/section_map.
//
// GB18030 decoded strictly before splitting (trail bytes can equal `|`).
// `#HEADER` lines start sections (name = text after '#', may be empty or
// contain further '#'; the retained trailing `######` parses as an empty
// section named "#####"). Entries `code|name[|extra...]` under each
// header; extra columns retained verbatim, uninterpreted. Duplicate code
// within ONE section rejects the whole file.
// Error precedence: Decode -> per line: (empty->MalformedEntry unless
// last) -> header | EntryBeforeHeader -> fields>=2 -> BadCode ->
// EmptyName -> DuplicateCode. DuplicateCode section/code rendered via
// Rust {:?} Debug escaping.

export class InconError extends Error {
  constructor(message, kind, extra = {}) {
    super(message);
    this.name = "InconError";
    this.kind = kind;
    Object.assign(this, extra);
  }
}

// Rust `{:?}` Debug for &str — escapes " \ \n \r \t \0, other control as
// \u{hex}; printable incl. ' and non-ASCII passes through. Verified
// against live rustc output.
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

/**
 * @param {Buffer|Uint8Array} bytes — typed boundary, no coercion
 * @returns {{name:string,entries:{line,code,name,extra:string[]}[]}[]}
 */
export function parse(bytes) {
  if (!(bytes instanceof Uint8Array)) {
    throw new InconError("incon.dat input must be a Buffer or Uint8Array", "invalid_input");
  }
  let text;
  try {
    text = decoder.decode(bytes);
  } catch {
    throw new InconError("incon.dat is not valid GB18030", "decode");
  }
  const sections = [];
  const lines = text.split("\r\n");
  for (let index = 0; index < lines.length; index++) {
    const line = lines[index];
    const number = index + 1;
    if (line === "") {
      if (index === lines.length - 1) break;
      throw new InconError(
        `incon.dat line ${number} has 0 fields, expected at least 2`,
        "malformed_entry", { fields: 0, line: number });
    }
    if (line.startsWith("#")) {
      sections.push({ entries: [], name: line.slice(1) });
      continue;
    }
    const section = sections[sections.length - 1];
    if (section === undefined) {
      throw new InconError(
        `incon.dat line ${number} has content before the first section header`,
        "entry_before_header", { line: number });
    }
    const fields = line.split("|");
    if (fields.length < 2) {
      throw new InconError(
        `incon.dat line ${number} has ${fields.length} fields, expected at least 2`,
        "malformed_entry", { fields: fields.length, line: number });
    }
    const code = fields[0];
    if (code === "" || ![...code].every((c) => c.charCodeAt(0) < 0x80)) {
      throw new InconError(`incon.dat line ${number} has a non-ASCII or empty code`, "bad_code", { line: number });
    }
    const name = fields[1];
    if (name === "") {
      throw new InconError(`incon.dat line ${number} has an empty name`, "empty_name", { line: number });
    }
    const dup = section.entries.find((e) => e.code === code);
    if (dup !== undefined) {
      throw new InconError(
        `incon.dat section ${dbg(section.name)} repeats code ${dbg(code)} (lines ${dup.line} and ${number})`,
        "duplicate_code", { code, first_line: dup.line, line: number, section: section.name });
    }
    section.entries.push({ code, extra: fields.slice(2), line: number, name });
  }
  return sections;
}

/**
 * Anchored level formula (contract v1): (len-1)/2 for an ASCII letter
 * followed by 2/4/6 digits (total 3/5/7). Anything else -> null.
 */
export function codeLevel(code) {
  if (typeof code !== "string") return null;
  if (!(code.length === 3 || code.length === 5 || code.length === 7)) return null;
  const first = code.charCodeAt(0);
  if (!((first >= 0x41 && first <= 0x5a) || (first >= 0x61 && first <= 0x7a))) return null;
  if (!/^[0-9]+$/.test(code.slice(1))) return null;
  return (code.length - 1) / 2;
}

/**
 * Level-2 prefix of a research-industry code: itself at level 2, first
 * five chars at level 3; level 1 or malformed -> null.
 */
export function researchLevel2(code) {
  const l = codeLevel(code);
  if (l === 2) return code;
  if (l === 3) return code.slice(0, 5);
  return null;
}

/**
 * First section with matching name -> [[code, name], ...], else null.
 */
export function sectionMap(sections, name) {
  const s = sections.find((sec) => sec.name === name);
  return s === undefined ? null : s.entries.map((e) => [e.code, e.name]);
}
