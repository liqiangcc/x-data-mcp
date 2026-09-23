// QFQ (前复权) affine arithmetic — Node port of src/gbbq.rs pure functions
// (normalize_factor, event_transform, compose_chain, Affine::apply,
// reference_price, displayed_previous_close).
//
// The Decimal semantics mirror rust_decimal 1.43.0 exactly, verified against
// the real crate rlib (evidence: probe-results.jsonl + rust-oracle.jsonl):
//   - values are (mantissa BigInt, scale 0..28); add/sub align scales,
//     mul adds scales — no trailing-zero normalization on ops
//   - div: integer quotient, then remainder-loop producing up to 9 digits
//     per iteration until remainder==0 or scale 28; at cap it rounds
//     HALF-EVEN on the next digit; when remainder processing ran, the
//     result is "unscaled" — trailing zeros stripped in chunks {8 (only
//     when the mantissa is 2^32-divisible), then 4, 2, 1}
//   - mul/add overflow: drop k digits with HALF-EVEN so scale<=28 and
//     mantissa<=2^96-1; still overflowing -> hard error (unreachable in
//     this domain: prices and factors are small)
//   - from_str: scale>28 rounds HALF-UP at 28dp (verified: 5e-29 -> 1e-28);
//     mantissa>2^96-1 -> Err
//   - round_dp_with_strategy(2, MidpointAwayFromZero): business 2dp HALF_UP
//   - f32 factor -> f64 -> Rust Display positional text -> from_str -> 2dp;
//     non-finite -> None -> NonFiniteFactor error (fail-closed)
//
// Scope: pure arithmetic primitives only. No .day bar traversal, no GBBQ
// record decoding, no service wiring.

const MAX96 = 79228162514264337593543950335n; // 2^96 - 1
const MAX_SCALE = 28;
const POW10 = (k) => 10n ** BigInt(k);

export class QfqError extends Error {
  constructor(message, kind, extra = {}) {
    super(message);
    this.name = "QfqError";
    this.kind = kind;
    Object.assign(this, extra);
  }
}

// ---------- Dec engine: {m: signed BigInt mantissa, s: scale} ----------

const ZERO = { m: 0n, s: 0 };
const ONE = { m: 1n, s: 0 };
const TEN = { m: 10n, s: 0 };

// rust_decimal::from_str — returns Dec or null (overflow/malformed).
function decFromStr(text) {
  const m = /^(-?)([0-9]+)(?:\.([0-9]+))?$/.exec(text);
  if (!m) return null;
  const neg = m[1] === "-";
  const digits = m[2] + (m[3] ?? "");
  let scale = (m[3] ?? "").length;
  let mant = BigInt(digits);
  if (scale > MAX_SCALE) {
    const cut = BigInt(scale - MAX_SCALE);
    const dropped = mant % POW10(cut);
    mant /= POW10(cut);
    if (dropped >= 5n * POW10(cut - 1n)) mant += 1n; // HALF_UP (verified)
    scale = MAX_SCALE;
  }
  if (mant > MAX96) return null;
  return { m: neg && mant !== 0n ? -mant : mant, s: scale };
}

// Drop k decimal digits with HALF-EVEN (rust_decimal rescale semantics:
// up iff dropped > half, or dropped == half and kept digit is odd).
function dropDigits(m, k) {
  if (k === 0) return m;
  const p = POW10(k);
  const dropped = m % p;
  let q = m / p;
  const half = p / 2n;
  if (dropped > half || (dropped === half && (q & 1n) === 1n)) q += 1n;
  return q;
}

// Rescale a magnitude to satisfy scale<=28 and mantissa<=MAX96, or throw.
function rescaleFit(mag, s, ctx) {
  const bitsNeeded = (v) => v.toString(2).length;
  let k = Math.max(s - MAX_SCALE, 0);
  while ((mag / POW10(k)) > MAX96) k += 1;
  if (k === 0) return { mag, s };
  if (k > s) {
    throw new QfqError(
      `decimal overflow rescaling (${ctx})`, "decimal_overflow");
  }
  const mag2 = dropDigits(mag, k);
  if (mag2 > MAX96) {
    throw new QfqError(
      `decimal overflow rescaling (${ctx})`, "decimal_overflow");
  }
  return { mag: mag2, s: s - k };
}

// rust_decimal add_impl: a zero operand short-circuits to the OTHER
// operand verbatim (its own scale kept); nonzero operands align scales.
export function decAdd(x, y) {
  if (x.m === 0n) return { ...y };
  if (y.m === 0n) return { ...x };
  const s = Math.max(x.s, y.s);
  const m = x.m * POW10(s - x.s) + y.m * POW10(s - y.s);
  const neg = m < 0n;
  const { mag, s: s2 } = rescaleFit(neg ? -m : m, s, "add");
  return { m: neg ? -mag : mag, s: s2 };
}
export const decSub = (x, y) => decAdd(x, { m: -y.m, s: y.s });

// rust_decimal mul: a zero product becomes Decimal::ZERO (scale 0).
export function decMul(x, y) {
  const m = x.m * y.m;
  if (m === 0n) return { ...ZERO };
  const neg = m < 0n;
  const { mag, s } = rescaleFit(neg ? -m : m, x.s + y.s, "mul");
  return { m: neg ? -mag : mag, s };
}

// find_scale: largest k in 1..9 (capped so s+k<=28) with q*10^k <= MAX96.
function findScale(q, s) {
  let k = Math.min(9, MAX_SCALE - s);
  if (k <= 0) return 0;
  while (k > 0 && q * POW10(k) > MAX96) k -= 1;
  return k;
}

// rust_decimal unscale — chunked trailing-zero strip; the 8-group step
// only runs while the mantissa is divisible by 2^32 (data[0]==0 check).
function unscale(m, s) {
  const U32 = 4294967296n;
  while (s >= 8 && m % U32 === 0n && m % POW10(8) === 0n) { m /= POW10(8); s -= 8; }
  if (s >= 4 && m % POW10(4) === 0n) { m /= POW10(4); s -= 4; }
  if (s >= 2 && m % POW10(2) === 0n) { m /= POW10(2); s -= 2; }
  if (s >= 1 && m % POW10(1) === 0n) { m /= POW10(1); s -= 1; }
  return { m, s };
}

// rust_decimal div_impl — magnitudes; sign applied by caller.
function decDiv(x, y) {
  if (y.m === 0n) throw new QfqError("division by zero", "div_by_zero");
  if (x.m === 0n) return { ...ZERO };
  const neg = (x.m < 0n) !== (y.m < 0n);
  const xm = x.m < 0n ? -x.m : x.m;
  const ym = y.m < 0n ? -y.m : y.m;
  let scale = x.s - y.s;
  let q = xm / ym;
  let rem = xm % ym;
  let requireUnscale = false;
  if (rem !== 0n || scale < 0) {
    for (;;) {
      let ps;
      if (rem === 0n) {
        if (scale >= 0) break;
        ps = Math.min(9, -scale);
      } else {
        requireUnscale = true;
        if (scale === MAX_SCALE || (ps = findScale(q, scale)) === 0 || ps + scale < 0) {
          // cap: HALF-EVEN on the next digit
          const twice = rem * 2n;
          if (twice > ym || (twice === ym && (q & 1n) === 1n)) q += 1n;
          break;
        }
      }
      const power = POW10(ps);
      scale += ps;
      q *= power;
      const rs = rem * power;
      q += rs / ym;
      rem = rs % ym;
    }
    if (requireUnscale) ({ m: q, s: scale } = unscale(q, scale));
    if (q > MAX96) throw new QfqError("decimal overflow in division", "decimal_overflow");
  }
  return { m: neg && q !== 0n ? -q : q, s: scale };
}

// round_dp_with_strategy(dp, MidpointAwayFromZero) — business 2dp HALF_UP.
function decRoundAway(x, dp) {
  const k = x.s - dp;
  if (k <= 0) return { ...x };
  const neg = x.m < 0n;
  const mag = neg ? -x.m : x.m;
  const p = POW10(k);
  let q = mag / p;
  if (mag % p >= p / 2n) q += 1n;
  return { m: neg && q !== 0n ? -q : q, s: dp };
}

export function decToString(x) {
  const neg = x.m < 0n;
  const digits = (neg ? -x.m : x.m).toString().padStart(x.s + 1, "0");
  const body = x.s === 0
    ? digits
    : `${digits.slice(0, digits.length - x.s)}.${digits.slice(digits.length - x.s)}`;
  return (neg ? "-" : "") + body;
}

// ---------- f32 -> positional decimal text (Rust f64 Display) ----------
const dv = new DataView(new ArrayBuffer(4));
function f64Display(v) {
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

// ---------- ported gbbq.rs functions ----------

/// normalize_factor: raw f32 (as a JS number already carrying the f32 value)
/// -> business-decimal 2dp HALF_UP Dec, or null when non-finite/unparseable.
export function normalizeFactor(raw) {
  if (typeof raw !== "number" || !Number.isFinite(raw)) return null;
  const parsed = decFromStr(f64Display(raw));
  if (parsed === null) return null;
  return decRoundAway(parsed, 2);
}

/// Event::factors — c_raw[4] (f32-widened numbers) -> [Dec;4].
/// Fail-closed: non-finite -> QfqError with the Rust error text.
export function factors(cRaw) {
  return cRaw.map((raw, index) => {
    const d = normalizeFactor(raw);
    if (d === null) {
      throw new QfqError(
        `GBBQ factor ${index} is not a finite decimal input`,
        "non_finite_factor", { index });
    }
    return d;
  });
}

/// event_transform: T(x) = a*x + b with
///   a = 10/(10+C3+C4),  b = (C4*C2 - C1)/(10+C3+C4), c = [C1,C2,C3,C4] Dec.
export function eventTransform(c) {
  const denominator = decAdd(decAdd(TEN, c[2]), c[3]);
  return {
    a: decDiv(TEN, denominator),
    b: decDiv(decSub(decMul(c[3], c[1]), c[0]), denominator),
  };
}

/// compose_chain over events ordered oldest->newest, no intermediate rounding.
/// chain(x) = T_n(...T_1(x)); empty -> identity.
export function composeChain(events) {
  let chain = { a: { ...ONE }, b: { ...ZERO } };
  for (const c of events) {
    const t = eventTransform(c);
    chain = { a: decMul(chain.a, t.a), b: decAdd(decMul(t.a, chain.b), t.b) };
  }
  return chain;
}

export const applyAffine = ({ a, b }, x) => decAdd(decMul(a, x), b);
export const referencePrice = (prevClose, c) => applyAffine(eventTransform(c), prevClose);
export const displayedPreviousClose = (prevClose, c) =>
  decRoundAway(referencePrice(prevClose, c), 2);

// Internal helpers exposed for probe/tests (not contract surface).
export const _dec = {
  fromStr: decFromStr, add: decAdd, sub: decSub, mul: decMul, div: decDiv,
  roundAway: decRoundAway, toString: decToString, f64Display,
  ZERO, ONE, TEN,
};
