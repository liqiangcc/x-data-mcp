// .day amount f32 projection — Node port of the Rust expression
// (src/official_market.rs:396-398):
//     Decimal::from_str(&f64::from(f32::from_bits(bits)).to_string()).ok()
//
// Semantics mirrored verbatim:
//   - bits -> f32 reinterpretation -> exact f64 promotion
//   - Rust f64 Display: shortest round-trip decimal digits, ALWAYS positional
//     (JS Number.toString switches to e-notation at |v|>=1e21 or |v|<1e-6 —
//     we expand back to positional form; digit sequence is identical since
//     both are the shortest round-trip decimal of the same f64)
//   - rust_decimal::from_str: scale>28 rounds half-up at 28dp (verified on
//     live Rust: 5e-29 -> 1e-28, 1.5e-28 -> 2e-28); mantissa > 2^96-1 -> Err
//   - NaN/+-inf ("NaN"/"inf"/"-inf" texts) fail parse -> None -> null
//   - "-0" normalizes to Decimal(0) -> "0"
// Returns the Decimal contract string (to_string, scale preserved) or null.

const DECIMAL_MAX = 79228162514264337593543950335n; // 2^96 - 1
const dv = new DataView(new ArrayBuffer(4));

export class AmountF32Error extends Error {
  constructor(msg) { super(msg); this.name = "AmountF32Error"; }
}

// Rust f64 Display -> positional decimal text for a finite f64.
function rustDisplay(v) {
  const s = String(v);
  if (!/[eE]/.test(s)) return s;
  const neg = s.startsWith("-");
  const [mant, expStr] = (neg ? s.slice(1) : s).split(/[eE]/);
  const exp = parseInt(expStr, 10);
  const [i, fr = ""] = mant.split(".");
  const digits = i + fr;
  const point = i.length + exp;
  const body =
    point <= 0 ? "0." + "0".repeat(-point) + digits
    : point >= digits.length ? digits + "0".repeat(point - digits.length)
    : digits.slice(0, point) + "." + digits.slice(point);
  return (neg ? "-" : "") + body;
}

// rust_decimal::from_str on a positional decimal string:
// -> {neg, mantissa, scale} or null (parse failure / overflow).
function parseRustDecimal(text) {
  const m = /^(-?)([0-9]+)(?:\.([0-9]+))?$/.exec(text);
  if (!m) return null;
  const neg = m[1] === "-";
  const intPart = m[2], frac = m[3] ?? "";
  let mantissa = BigInt(intPart + frac);
  let scale = frac.length;
  if (scale > 28) {
    const cut = BigInt(scale - 28);
    const dropped = mantissa % 10n ** cut;
    mantissa /= 10n ** cut;
    // rust_decimal rounds half-up on the first dropped digit
    if (dropped >= 5n * 10n ** (cut - 1n)) mantissa += 1n;
    scale = 28;
  }
  if (mantissa > DECIMAL_MAX) return null;
  return { neg: neg && mantissa !== 0n, mantissa, scale };
}

// Decimal::to_string — positional, scale preserved.
function decimalToString({ neg, mantissa, scale }) {
  const digits = mantissa.toString();
  const body = scale === 0
    ? digits
    : (() => {
        const padded = digits.padStart(scale + 1, "0");
        return `${padded.slice(0, padded.length - scale)}.${padded.slice(padded.length - scale)}`;
      })();
  return (neg ? "-" : "") + body;
}

/**
 * Project a raw IEEE-754 binary32 amount field to the contract decimal
 * representation.
 * @param {number} bits — uint32 bit pattern of the f32 field
 * @returns {string|null} contract decimal string, or null when Rust would
 *          produce None (NaN, +-inf, mantissa overflow)
 * @throws {AmountF32Error} on non-uint32 input (fail-closed; no silent wrap)
 */
export function projectAmountF32(bits) {
  if (typeof bits !== "number" || !Number.isInteger(bits) || bits < 0 || bits > 0xffffffff) {
    throw new AmountF32Error(`amount_f32 bits must be a uint32, got ${bits}`);
  }
  dv.setUint32(0, bits, false);
  const f = dv.getFloat32(0); // f32 reinterpreted, exactly widened to f64
  if (!Number.isFinite(f)) return null; // NaN/inf texts fail Decimal::from_str
  const parsed = parseRustDecimal(rustDisplay(f));
  return parsed === null ? null : decimalToString(parsed);
}
