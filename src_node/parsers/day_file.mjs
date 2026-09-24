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
// Pure-integer civil math (Howard Hinnant days_from_civil) — the same
// algorithm chrono's parser uses; identical accept/reject domain.
const DIM = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
function ymdToEpoch(year, month, day) {
  if (year < 1 || year > 9999 || month < 1 || month > 12 || day < 1)
    return null;
  const dim = DIM[month - 1] +
    (month === 2 && (year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0))
      ? 1 : 0);
  if (day > dim) return null;
  const y = month <= 2 ? year - 1 : year;
  const era = Math.floor(y / 400); // y >= 0 always (year >= 1)
  const yoe = y - era * 400;
  const doy = Math.floor((153 * (month + (month > 2 ? -3 : 9)) + 2) / 5) + day - 1;
  const doe = yoe * 365 + Math.floor(yoe / 4) - Math.floor(yoe / 100) + doy;
  return era * 146097 + doe - 719468;
}

const P2 = (n) => (n < 10 ? `0${n}` : `${n}`);

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
  // u32le via raw byte reads — faster than DataView.getUint32 in the
  // hot loop (identical little-endian semantics, same bounds domain).
  const u32 = (i) =>
    (bytes[i] | (bytes[i + 1] << 8) | (bytes[i + 2] << 16) |
      (bytes[i + 3] << 24)) >>> 0;
  const count = bytes.length / RECORD_BYTES;
  const records = new Array(count);
  let prevDay = -Infinity;
  for (let index = 0; index < count; index++) {
    const base = index * RECORD_BYTES;
    const raw = u32(base);
    const year = Math.floor(raw / 10000);
    const month = Math.floor(raw / 100) % 100;
    const dom = raw % 100;
    const day = ymdToEpoch(year, month, dom);
    if (day === null) {
      throw new DayFileError(
        `invalid calendar date ${raw} at record ${index}`, "invalid_date");
    }
    if (day <= prevDay) {
      throw new DayFileError(`non-increasing date at record ${index}`, "date_order");
    }
    prevDay = day;
    records[index] = {
      amount_float32_bits: u32(base + 20),
      // ISO date formatted directly from the parsed ints — no Date.
      date: `${String(year).padStart(4, "0")}-${P2(month)}-${P2(dom)}`,
      ohlc_raw: [u32(base + 4), u32(base + 8), u32(base + 12), u32(base + 16)],
      r7_raw: u32(base + 28),
      volume_raw: u32(base + 24),
    };
  }
  return records;
}
