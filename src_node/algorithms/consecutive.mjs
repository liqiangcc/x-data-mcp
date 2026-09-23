// consecutive_direction — Node port of metrics::consecutive::evaluate
// and ratio_change. Pure exact-decimal comparisons + one final rational
// half-even rounding at scale 12. Serde-compatible typed boundary:
// malformed shapes throw ConsecutiveInputError; domain rejections return
// INVALID_MARKET_COMMAND errors or NotComputable outcomes — identical
// precedence to Rust.
//
// Read-only/pure: no storage verification, service, publish, or
// completion-provenance claims.
//
// Decimal engine reused from qfq_affine (_dec): {m: BigInt mantissa,
// s: scale 0..28} — verified rust_decimal 1.43.0 semantics.

import { _dec } from "./qfq_affine.mjs";

export class ConsecutiveError extends Error {
  constructor(message) {
    super(`INVALID_MARKET_COMMAND: ${message}`);
    this.name = "ConsecutiveError";
  }
}
export class ConsecutiveInputError extends Error {
  constructor(message) { super(message); this.name = "ConsecutiveInputError"; }
}

const invalid = (s) => new ConsecutiveError(s);
const isStr = (v) => typeof v === "string" && v.isWellFormed();

// ---------- typed boundary ----------
// NaiveDate: strict YYYY-MM-DD with proleptic-Gregorian calendar
// validity. Manual check — Date.UTC silently remaps years 0-99 to
// 1900-1999, which would wrongly reject valid years 0001-0099.
const DATE_RE = /^(\d{4})-(\d{2})-(\d{2})$/;
const DIM = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
const isLeap = (y) => y % 4 === 0 && (y % 100 !== 0 || y % 400 === 0);
const isDate = (v) => {
  if (!isStr(v)) return false;
  const m = DATE_RE.exec(v);
  if (!m) return false;
  const y = +m[1], mo = +m[2], d = +m[3];
  return mo >= 1 && mo <= 12 && d >= 1 &&
    d <= DIM[mo - 1] + (mo === 2 && isLeap(y) ? 1 : 0);
};
const COMPLETIONS = new Set(["complete", "incomplete", "unknown"]);
const isCompletion = (v) => COMPLETIONS.has(v);

const isDec = (v) => isStr(v) && _dec.fromStr(v) !== null;

function checkInput(input) {
  if (!Array.isArray(input)) throw new ConsecutiveInputError("input must be an array");
  input.forEach((o, i) => {
    if (o === null || typeof o !== "object" || Array.isArray(o)) {
      throw new ConsecutiveInputError(`input[${i}] must be an object`);
    }
    // year is i32 in Rust — integer check only; range/year-1..9999
    // bounds are domain errors (INVALID_MARKET_COMMAND) inside evaluate.
    if (!Number.isSafeInteger(o.year)) {
      throw new ConsecutiveInputError(`input[${i}].year invalid`);
    }
    if (!isDate(o.first) || !isDate(o.last)) {
      throw new ConsecutiveInputError(`input[${i}].first/last must be YYYY-MM-DD`);
    }
    if (!isDec(o.close)) {
      throw new ConsecutiveInputError(`input[${i}].close is not a valid Decimal`);
    }
    if (typeof o.unfinished !== "boolean") {
      throw new ConsecutiveInputError(`input[${i}].unfinished must be boolean`);
    }
    if (!isCompletion(o.completion)) {
      throw new ConsecutiveInputError(`input[${i}].completion invalid`);
    }
  });
}
function checkRange(r) {
  if (r === null || typeof r !== "object" || Array.isArray(r) ||
      !Number.isSafeInteger(r.start_year) || !Number.isSafeInteger(r.end_year)) {
    throw new ConsecutiveInputError("range must be {start_year,end_year} integers");
  }
}
function checkParams(p) {
  if (p === null || typeof p !== "object" || Array.isArray(p)) {
    throw new ConsecutiveInputError("parameters must be an object");
  }
  if (p.direction !== "up" && p.direction !== "down") {
    throw new ConsecutiveInputError("parameters.direction invalid");
  }
  if (!Number.isSafeInteger(p.min_periods)) {
    throw new ConsecutiveInputError("parameters.min_periods must be an integer");
  }
  // serde(default) applies ONLY to absent keys — explicit null is
  // invalid for bool, not defaulted.
  if (p.completed_only !== undefined && typeof p.completed_only !== "boolean") {
    throw new ConsecutiveInputError("parameters.completed_only must be boolean");
  }
  const completedOnly = p.completed_only === undefined ? true : p.completed_only;
  for (const [k, def] of [["price_field", "close"], ["equal_policy", "break"], ["gap_policy", "reject"]]) {
    if (p[k] !== undefined && p[k] !== def) {
      throw new ConsecutiveInputError(`parameters.${k} invalid`);
    }
  }
  return { direction: p.direction, minPeriods: p.min_periods, completedOnly };
}

// ---------- Dec helpers ----------
const decCmp = (a, b) => {
  const l = a.m * 10n ** BigInt(b.s), r = b.m * 10n ** BigInt(a.s);
  return l < r ? -1 : l > r ? 1 : 0;
};

/**
 * ratio_change(end, anchor) -> String — exact BigInt rational at
 * scale 12, one half-even rounding, Rust verbatim output.
 */
export function ratioChange(endText, anchorText) {
  const end = _dec.fromStr(endText), anchor = _dec.fromStr(anchorText);
  if (end === null || anchor === null) {
    throw new ConsecutiveInputError("ratio inputs must be valid decimals");
  }
  if (anchor.m <= 0n || end.m <= 0n) throw invalid("positive ratio inputs required");
  const ten = 10n;
  const endCoeff = end.m * ten ** BigInt(anchor.s);
  const anchorCoeff = anchor.m * ten ** BigInt(end.s);
  const numerator = (endCoeff - anchorCoeff) * ten ** 12n;
  const denominator = anchorCoeff;
  const bits = (v) => (v < 0n ? -v : v).toString(2).length;
  if (bits(numerator) > 512 || bits(denominator) > 512) {
    throw invalid("rational integer budget exceeded");
  }
  const negative = numerator < 0n;
  const magnitude = negative ? -numerator : numerator;
  let quotient = magnitude / denominator;
  const remainder = magnitude % denominator;
  const double = remainder * 2n;
  const odd = quotient % 2n !== 0n;
  if (double > denominator || (double === denominator && odd)) quotient += 1n;
  const digits = quotient.toString();
  const padded = digits.padStart(13, "0");
  const split = padded.length - 12;
  return `${negative && quotient !== 0n ? "-" : ""}${padded.slice(0, split)}.${padded.slice(split)}`;
}

const pointOf = (o) => ({
  year: o.year, first_trading_date: o.first, last_trading_date: o.last,
  close: _dec.toString(_dec.fromStr(o.close)),
});
const NO_LIMIT = "no inferred coverage, warmup, gap filling or version substitution";
const LB_LIMIT = "at least the observed run; no observations outside the requested left boundary";
const EVIDENCE = "adjacent natural years and explicit completion checked for all used observations";

const no = (reason, excludedTail) => ({
  outcome: "not_computable", reason, excluded_tail: excludedTail, limitations: [NO_LIMIT],
});

/**
 * evaluate(input, range, parameters) — verbatim port.
 * input observations: {year, first:"YYYY-MM-DD", last:"YYYY-MM-DD",
 * close:"<decimal>", unfinished:bool, completion:"complete|incomplete|unknown"}
 * range: {start_year,end_year}; params: {direction,min_periods,...}
 */
export function evaluate(input, range, params) {
  checkInput(input);
  checkRange(range);
  const p = checkParams(params);

  if (range.start_year < 1 || range.end_year > 9999 ||
      range.start_year > range.end_year ||
      range.end_year - range.start_year >= 1000 ||
      input.length > 1000 || !(p.minPeriods >= 1 && p.minPeriods <= 1000)) {
    throw invalid("bounded years/observations/min_periods required");
  }
  for (let i = 1; i < input.length; i++) {
    if (input[i - 1].year >= input[i].year) {
      throw invalid("unordered/duplicate/out-of-range observations");
    }
  }
  for (const o of input) {
    if (o.year < range.start_year || o.year > range.end_year ||
        o.first > o.last || +o.first.slice(0, 4) !== o.year || +o.last.slice(0, 4) !== o.year) {
      throw invalid("unordered/duplicate/out-of-range observations");
    }
  }
  // year-of-date: zero-padded YYYY-MM-DD year field compared numerically.

  const excluded = [];
  if (input.length > 0 && input[input.length - 1].year !== range.end_year) {
    return no("missing_year", excluded);
  }
  let len = input.length;
  if (p.completedOnly) {
    while (len > 0 && input[len - 1].unfinished) {
      excluded.push(input[len - 1].year);
      len -= 1;
    }
  }
  if (len < 2) return no("window_too_short", excluded);

  const data = input.slice(0, len);
  const end = data[len - 1];
  const provisional = !p.completedOnly && end.unfinished;
  const check = (o) => {
    const c = _dec.fromStr(o.close);
    if (c.m <= 0n) return "non_positive_price";
    if (o.completion === "unknown") return "unknown_completion";
    if ((o.completion === "incomplete" && !(provisional && o.year === end.year)) ||
        (o.unfinished && o.year !== end.year)) {
      return "incomplete_period";
    }
    return null;
  };

  let run = 0, anchor = len - 1, earliestChecked = len - 1;
  let kind = "lower_bound";
  let observed = null;
  for (let i = len - 1; i >= 1; i--) {
    earliestChecked = i - 1;
    const a = data[i - 1], b = data[i];
    if (b.year - a.year !== 1) return no("missing_year", excluded);
    const reason = check(b) ?? check(a);
    if (reason !== null) return no(reason, excluded);
    const cmp = decCmp(_dec.fromStr(b.close), _dec.fromStr(a.close));
    const direction = cmp > 0 ? "up" : cmp < 0 ? "down" : "flat";
    if (observed === null) observed = direction;
    const matches = (p.direction === "up" && direction === "up") ||
                    (p.direction === "down" && direction === "down");
    if (!matches) { kind = "exact"; break; }
    run += 1;
    anchor = i - 1;
  }
  if (kind === "lower_bound" && data[0].year !== range.start_year) {
    return no("missing_year", excluded);
  }
  const matches = run >= p.minPeriods ? true : kind === "exact" ? false : null;
  const cumulativeChange = run > 0
    ? ratioChange(end.close, data[anchor].close) : null;

  return {
    outcome: "computed",
    observed_direction: observed,
    run_length: run,
    run_length_kind: kind,
    matches,
    anchor: run > 0 ? pointOf(data[anchor]) : null,
    first_change: run > 0 ? pointOf(data[anchor + 1]) : null,
    end: pointOf(end),
    cumulative_change: cumulativeChange,
    precision: { scale: 12, rounding: "half_even_once", unit: "ratio" },
    observations_used: len - earliestChecked,
    continuity_evidence: EVIDENCE,
    actual_input_range: { start_year: data[earliestChecked].year, end_year: end.year },
    excluded_tail: excluded,
    provisional,
    limitations: kind === "lower_bound" ? [LB_LIMIT] : [],
  };
}
