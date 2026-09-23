// Conservative dBase III/IV container parser — Node port of src/dbf.rs.
//
// Semantics mirrored verbatim:
//   - header: version gate {0x03,0x83,0x30,0x31,0x32,0x43,0x63}, u16
//     header_length@8 / record_length@10, u32 record_count@4,
//     last_update@1..4, code_page@29
//   - header_length must be >= 33; record_length >= 1; file must reach
//     header_end; descriptor area ends at header_end-1 with 0x0d
//   - 32-byte descriptors: name = bytes[..first NUL or 11] via Rust
//     from_utf8_lossy; type@11; length@16 (zero rejects); decimals@17
//   - sum(field lengths) + 1 must fit record_length (vendor tail kept)
//   - body = record_count * record_length after header; surplus trailing
//     bytes allowed; records keep deleted marker + full raw payload copy
//   - error texts byte-identical to DbfError variants, same precedence
//   - text/code-page/business meaning intentionally uninterpreted

export class DbfError extends Error {
  constructor(message, kind, extra = {}) {
    super(message);
    this.name = "DbfError";
    this.kind = kind;
    Object.assign(this, extra);
  }
}
const SUPPORTED_VERSIONS = new Set([0x03, 0x83, 0x30, 0x31, 0x32, 0x43, 0x63]);
const hex2 = (v) => v.toString(16).padStart(2, "0");

// Rust String::from_utf8_lossy equivalent: per-sequence replacement with
// U+FFFD, resyncing after the consumed valid prefix (Rust error_len).
const CONT = (b) => b >= 0x80 && b <= 0xbf;
function utf8Lossy(bytes) {
  let out = "", i = 0;
  while (i < bytes.length) {
    const b = bytes[i];
    if (b < 0x80) { out += String.fromCharCode(b); i++; continue; }
    let need = 0, lo2 = 0x80, hi2 = 0xbf;
    if (b >= 0xc2 && b <= 0xdf) need = 2;
    else if (b === 0xe0) { need = 3; lo2 = 0xa0; }
    else if (b >= 0xe1 && b <= 0xec) need = 3;
    else if (b === 0xed) { need = 3; hi2 = 0x9f; }
    else if (b === 0xee || b === 0xef) need = 3;
    else if (b === 0xf0) { need = 4; lo2 = 0x90; }
    else if (b >= 0xf1 && b <= 0xf3) need = 4;
    else if (b === 0xf4) { need = 4; hi2 = 0x8f; }
    if (need === 0) { out += "�"; i++; continue; }
    if (i + 1 >= bytes.length) { out += "�"; i = bytes.length; continue; }
    const b2 = bytes[i + 1];
    if (b2 < lo2 || b2 > hi2) { out += "�"; i++; continue; }
    let bad = false;
    for (let k = 2; k < need; k++) {
      if (i + k >= bytes.length || !CONT(bytes[i + k])) { bad = k; break; }
    }
    if (bad !== false) {
      out += "�";
      i = i + bad >= bytes.length ? bytes.length : i + bad;
      continue;
    }
    let cp = b & (0x7f >> need);
    for (let k = 1; k < need; k++) cp = (cp << 6) | (bytes[i + k] & 0x3f);
    out += String.fromCodePoint(cp);
    i += need;
  }
  return out;
}

/**
 * @param {Buffer|Uint8Array} bytes — typed boundary, no coercion
 * @returns {{version:number,last_update:number[],record_count:number,
 *   header_length:number,record_length:number,code_page:number,
 *   fields:{name:string,field_type:number,length:number,decimals:number}[],
 *   records:{deleted:boolean,raw:Uint8Array}[]}}
 */
export function parse(bytes) {
  if (!(bytes instanceof Uint8Array)) {
    throw new DbfError("DBF input must be a Buffer or Uint8Array", "invalid_input");
  }
  if (bytes.length < 32) {
    throw new DbfError(`DBF header is truncated: ${bytes.length} bytes`, "header_truncated");
  }
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const version = bytes[0];
  if (!SUPPORTED_VERSIONS.has(version)) {
    throw new DbfError(`unsupported DBF version 0x${hex2(version)}`, "unsupported_version");
  }
  const headerLength = dv.getUint16(8, true);
  const recordLength = dv.getUint16(10, true);
  const recordCount = dv.getUint32(4, true);
  const minimumHeader = 33;
  if (headerLength < minimumHeader) {
    throw new DbfError(
      `DBF header length ${headerLength} is smaller than field descriptors ${minimumHeader}`,
      "invalid_header_length", { header: headerLength, minimum: minimumHeader });
  }
  if (recordLength < 1) {
    throw new DbfError("DBF record length must include deletion marker", "invalid_record_length");
  }
  const headerEnd = headerLength;
  if (bytes.length < headerEnd) {
    throw new DbfError(`DBF header is truncated: ${bytes.length} bytes`, "header_truncated");
  }
  const descriptorEnd = headerEnd - 1;
  if (bytes[descriptorEnd] !== 0x0d) {
    throw new DbfError("DBF field descriptor area is truncated", "field_descriptor_truncated");
  }
  const fields = [];
  for (let offset = 32; offset < descriptorEnd; offset += 32) {
    if (offset + 32 > descriptorEnd) {
      throw new DbfError("DBF field descriptor area is truncated", "field_descriptor_truncated");
    }
    const descriptor = bytes.subarray(offset, offset + 32);
    let nameEnd = 11;
    for (let k = 0; k < 11; k++) {
      if (descriptor[k] === 0) { nameEnd = k; break; }
    }
    const length = descriptor[16];
    if (length === 0) {
      throw new DbfError(`DBF field ${fields.length} has zero length`, "zero_length_field", { index: fields.length });
    }
    fields.push({
      decimals: descriptor[17],
      field_type: descriptor[11],
      length,
      name: utf8Lossy(descriptor.subarray(0, nameEnd)),
    });
  }
  const fieldBytes = fields.reduce((a, f) => a + f.length, 0);
  if (fieldBytes + 1 > recordLength) {
    throw new DbfError(
      `DBF fields require ${fieldBytes + 1} bytes but record length is ${recordLength}`,
      "fields_exceed_record", { record_length: recordLength, required: fieldBytes + 1 });
  }
  const expected = headerEnd + recordCount * recordLength;
  if (!Number.isSafeInteger(expected)) {
    throw new DbfError("DBF record count does not fit host memory", "record_count_overflow");
  }
  if (bytes.length < expected) {
    throw new DbfError(
      `DBF body is truncated: expected ${expected} bytes, got ${bytes.length}`,
      "body_truncated", { actual: bytes.length, expected });
  }
  const records = new Array(recordCount);
  for (let i = 0; i < recordCount; i++) {
    const start = headerEnd + i * recordLength;
    const record = bytes.subarray(start, start + recordLength);
    records[i] = {
      deleted: record[0] === 0x2a,
      raw: new Uint8Array(record.subarray(1)), // fresh copy — no aliasing
    };
  }
  return {
    code_page: bytes[29],
    fields,
    header_length: headerLength,
    last_update: [bytes[1], bytes[2], bytes[3]],
    record_count: recordCount,
    record_length: recordLength,
    records,
    version,
  };
}

/**
 * DbfRecord::field_bytes — bytes of field `index` inside a record payload,
 * or undefined when out of range.
 */
export function fieldBytes(fields, recordRaw, index) {
  const field = fields[index];
  if (field === undefined) return undefined;
  let offset = 0;
  for (let i = 0; i < index; i++) offset += fields[i].length;
  return recordRaw.subarray(offset, offset + field.length).length === field.length
    ? recordRaw.subarray(offset, offset + field.length)
    : undefined;
}
