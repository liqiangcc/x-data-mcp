// GBBQ container structural parser — Node port of src/gbbq.rs::parse.
//
// Semantics mirrored verbatim:
//   - input must be byte data (Buffer/Uint8Array); other types rejected
//     without coercion (the Rust signature &[u8] makes this a typed
//     boundary — mirrors the REVIEW-NM-014 typed-boundary condition)
//   - <4 bytes -> HeaderTruncated(len)
//   - count = u32 LE at [0..4]; expected = count*29 + 4 (checked
//     arithmetic — count<=2^32-1 so expected <= 124,554,051,559, always
//     representable; RecordCountOverflow path kept for parity)
//   - bytes.length !== expected -> LengthMismatch (checked BEFORE any
//     record allocation — a huge declared count allocates nothing)
//   - records preserved verbatim as fresh copies (Rust Vec<[u8;29]>
//     ownership semantics — no shared-buffer aliasing)
//   - error text byte-identical to the Rust enum variants

export const RECORD_BYTES = 29;

export class GbbqFileError extends Error {
  constructor(message, kind) {
    super(message);
    this.name = "GbbqFileError";
    this.kind = kind; // "header_truncated" | "length_mismatch" | "record_count_overflow" | "invalid_input"
  }
}

/**
 * Validate the observed container shape while preserving every encrypted
 * record. No field or category is interpreted here.
 * @param {Buffer|Uint8Array} bytes
 * @returns {{record_count:number, records:Uint8Array[]}}
 * @throws {GbbqFileError}
 */
export function parse(bytes) {
  if (!(bytes instanceof Uint8Array)) {
    throw new GbbqFileError(
      `GBBQ parse expects Buffer/Uint8Array input, got ${bytes === null ? "null" : typeof bytes}`,
      "invalid_input");
  }
  if (bytes.length < 4) {
    throw new GbbqFileError(
      `GBBQ file is shorter than its count header: ${bytes.length} bytes`,
      "header_truncated");
  }
  const recordCount = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
    .getUint32(0, true);
  // Rust: usize::try_from(u32) + checked_mul/add — cannot overflow on
  // 64-bit; kept for parity with the declared invariant.
  const expected = recordCount * RECORD_BYTES + 4;
  if (!Number.isSafeInteger(expected)) {
    throw new GbbqFileError(
      "GBBQ record count does not fit host memory", "record_count_overflow");
  }
  if (bytes.length !== expected) {
    throw new GbbqFileError(
      `GBBQ length ${bytes.length} does not equal 4 + count(${recordCount}) * 29 = ${expected}`,
      "length_mismatch");
  }
  const records = [];
  for (let i = 0; i < recordCount; i++) {
    // fresh copy per record — Buffer.slice would alias the shared buffer
    records.push(new Uint8Array(
      bytes.subarray(4 + i * RECORD_BYTES, 4 + (i + 1) * RECORD_BYTES)));
  }
  return { record_count: recordCount, records };
}
