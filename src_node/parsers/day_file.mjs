// Structural .day decoding — Node port of src/day_file.rs::parse.
// Raw fields are not approved business prices/units; this layer preserves
// them verbatim and fails closed on structural violations only.
//
// Semantics mirrored verbatim:
//   - byte count must be a multiple of RECORD_BYTES (checked before any record)
//   - word0 = YYYYMMDD as u32; year gate 1..=9999, strict proleptic-Gregorian
//   - strictly increasing dates (equal dates also reject)
//   - error text identical to the Rust enum variants

export const RECORD_BYTES = 32;

export class DayFileError extends Error {
  constructor(message, kind) {
    super(message);
    this.name = "DayFileError";
    this.kind = kind; // "truncated" | "invalid_date" | "date_order"
  }
}

// Strict Gregorian check mirroring NaiveDate::from_ymd_opt — the same
// accept/reject domain (year 1..=9999, real calendar day). Ordering needs
// no epoch conversion: for valid dates the YYYYMMDD integer orders
// identically to the epoch day, so `raw <= prev` is the verbatim
// strictly-increasing check.
const DIM = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
function ymdValid(year, month, day) {
  if (year < 1 || year > 9999 || month < 1 || month > 12 || day < 1)
    return false;
  return day <= DIM[month - 1] +
    (month === 2 && (year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0))
      ? 1 : 0);
}

// 2-digit pad table — avoids per-record padStart/template work.
const P2 = Array.from({ length: 100 }, (_, i) => (i < 10 ? `0${i}` : `${i}`));

// Aligned Uint32Array reads: 8 word loads per record instead of 32 byte
// reads. Requires a 4-byte-aligned view and a little-endian platform —
// checked once; the byte-assembly path is the verbatim fallback.
const LE = new Uint32Array(new Uint8Array([1, 0, 0, 0]).buffer)[0] === 1;

/**
 * Parse a .day file buffer into raw records.
 * @param {Uint8Array|Buffer} bytes
 * @returns {{date:string, ohlc_raw:number[], amount_float32_bits:number,
 *           volume_raw:number, r7_raw:number}[]} records with ISO dates
 * @throws {DayFileError}
 */
export function parse(bytes) {
  if (bytes.length % RECORD_BYTES !== 0) {
    throw new DayFileError(
      `day file byte count ${bytes.length} is not divisible by 32`, "truncated");
  }
  const count = bytes.length / RECORD_BYTES;
  const records = new Array(count);
  const words = LE && bytes instanceof Uint8Array &&
      bytes.byteOffset % 4 === 0
    ? new Uint32Array(bytes.buffer, bytes.byteOffset, count * 8) : null;
  // u32 reads: direct word indexing on the aligned fast path; byte
  // assembly (identical little-endian semantics) otherwise.
  const u32 = (i) =>
    (bytes[i] | (bytes[i + 1] << 8) | (bytes[i + 2] << 16) |
      (bytes[i + 3] << 24)) >>> 0;
  let prev = -1;
  // Dates are strictly increasing — the "YYYY-MM-" prefix changes at
  // most once per calendar month; reuse it between records.
  let prevYm = -1, prefix = "";
  if (words) {
    for (let index = 0; index < count; index++) {
      const w = index * 8;
      const raw = words[w];
      const year = Math.floor(raw / 10000);
      const month = Math.floor(raw / 100) % 100;
      const dom = raw % 100;
      if (!ymdValid(year, month, dom)) {
        throw new DayFileError(
          `invalid calendar date ${raw} at record ${index}`, "invalid_date");
      }
      if (raw <= prev) {
        throw new DayFileError(`non-increasing date at record ${index}`, "date_order");
      }
      prev = raw;
      const ym = year * 100 + month;
      if (ym !== prevYm) {
        prevYm = ym;
        prefix = `${String(year).padStart(4, "0")}-${P2[month]}-`;
      }
      records[index] = {
        amount_float32_bits: words[w + 5],
        date: prefix + P2[dom],
        ohlc_raw: [words[w + 1], words[w + 2], words[w + 3], words[w + 4]],
        r7_raw: words[w + 7],
        volume_raw: words[w + 6],
      };
    }
    return records;
  }
  for (let index = 0; index < count; index++) {
    const base = index * RECORD_BYTES;
    const raw = u32(base);
    const year = Math.floor(raw / 10000);
    const month = Math.floor(raw / 100) % 100;
    const dom = raw % 100;
    if (!ymdValid(year, month, dom)) {
      throw new DayFileError(
        `invalid calendar date ${raw} at record ${index}`, "invalid_date");
    }
    if (raw <= prev) {
      throw new DayFileError(`non-increasing date at record ${index}`, "date_order");
    }
    prev = raw;
    const ym = year * 100 + month;
    if (ym !== prevYm) {
      prevYm = ym;
      prefix = `${String(year).padStart(4, "0")}-${P2[month]}-`;
    }
    records[index] = {
      amount_float32_bits: u32(base + 20),
      date: prefix + P2[dom],
      ohlc_raw: [u32(base + 4), u32(base + 8), u32(base + 12), u32(base + 16)],
      r7_raw: u32(base + 28),
      volume_raw: u32(base + 24),
    };
  }
  return records;
}
