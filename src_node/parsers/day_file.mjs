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

// Strict Gregorian check mirroring NaiveDate::from_ymd_opt.
// Returns epoch day (integer) or null. Year range 1..=9999 as in Rust.
function ymdToEpoch(year, month, day) {
  if (year < 1 || year > 9999 || month < 1 || month > 12 || day < 1 || day > 31)
    return null;
  const d = new Date(0);
  d.setUTCFullYear(year, month - 1, day); // setUTCFullYear: avoids 1900+ mapping for year<100
  d.setUTCHours(0, 0, 0, 0);
  if (d.getUTCFullYear() !== year || d.getUTCMonth() !== month - 1 || d.getUTCDate() !== day)
    return null;
  return d.getTime() / 86400000;
}

function epochToIso(epochDay) {
  const d = new Date(epochDay * 86400000);
  const y = d.getUTCFullYear();
  return `${String(y).padStart(4, "0")}-${String(d.getUTCMonth() + 1).padStart(2, "0")}-${String(d.getUTCDate()).padStart(2, "0")}`;
}

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
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const records = [];
  let prevDay = -Infinity;
  for (let index = 0; index < bytes.length / RECORD_BYTES; index++) {
    const base = index * RECORD_BYTES;
    const raw = view.getUint32(base, true);
    const year = Math.floor(raw / 10000);
    const day = (year >= 1 && year <= 9999)
      ? ymdToEpoch(year, Math.floor(raw / 100) % 100, raw % 100)
      : null;
    if (day === null) {
      throw new DayFileError(
        `invalid calendar date ${raw} at record ${index}`, "invalid_date");
    }
    if (day <= prevDay) {
      throw new DayFileError(`non-increasing date at record ${index}`, "date_order");
    }
    prevDay = day;
    records.push({
      amount_float32_bits: view.getUint32(base + 20, true),
      date: epochToIso(day),
      ohlc_raw: [
        view.getUint32(base + 4, true), view.getUint32(base + 8, true),
        view.getUint32(base + 12, true), view.getUint32(base + 16, true),
      ],
      r7_raw: view.getUint32(base + 28, true),
      volume_raw: view.getUint32(base + 24, true),
    });
  }
  return records;
}
