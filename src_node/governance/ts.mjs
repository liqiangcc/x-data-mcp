// Shared chrono-strict RFC3339 parse + total-order key (NM-026/NM-027).
//
// chrono semantics: `[Tt ]` separator, required Z/z or ±HH:MM offset
// (HH<=23, MM<=59), second may be 60 (leap) on any minute, calendar
// strict, frac digits -> nanos (AutoSi output: 3-digit groups).
//
// Ordering key `ns2` = (baseNs*2 + leapBit)*1e9 + fracNs:
// :59.f -> base*2e9+f; :60.f (leap) -> base*2e9+1e9+f — strictly after
// every :59.x, strictly before the next minute's :00.x, ordered by
// fraction inside the leap second. Identical keys compare equal;
// ordering matches chrono DateTime<Utc> exactly.
// `out` is the canonical UTC re-serialization (Z, literal :60, ns groups).
const TS_RE = /^(\d{4})-(\d{2})-(\d{2})[Tt ](\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?([Zz]|([+-])(\d{2}):(\d{2}))$/;
const DIM = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
export function parseTs(s) {
  if (typeof s !== "string") return null;
  const m = TS_RE.exec(s);
  if (!m) return null;
  const [, y, mo, d, hh, mm, ss, frac, , sign, oh, om] = m;
  const year = +y, month = +mo, day = +d, hour = +hh, min = +mm, sec = +ss;
  const leap = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
  if (month < 1 || month > 12 || day < 1 || day > DIM[month - 1] + (month === 2 && leap ? 1 : 0) ||
      hour > 23 || min > 59 || sec > 60) return null;
  // offset: each component bounded (chrono rejects +00:99 / +23:60 / +24:00)
  if (oh !== undefined && (+oh > 23 || +om > 59)) return null;
  const offMin = oh === undefined ? 0 : (+oh * 60 + +om) * (sign === "-" ? -1 : 1);
  // nanos canonical: input digits -> ns (truncate beyond 9), output in
  // 3-digit groups per chrono AutoSi.
  let fracOut = "";
  let fracNs = 0n;
  if (frac !== undefined) {
    const ns9 = (frac.slice(0, 9) + "000000000").slice(0, 9);
    let g = ns9;
    while (g.length > 3 && g.endsWith("000")) g = g.slice(0, -3);
    if (!/^0+$/.test(g)) fracOut = "." + g;
    fracNs = BigInt(ns9);
  }
  const baseSec = sec === 60 ? 59 : sec;
  const wallMs = Date.UTC(year, month - 1, day, hour, min, baseSec, 0);
  const fracMs = Number(fracNs / 1_000_000n);
  const ms = wallMs + fracMs - offMin * 60000;
  const baseNs = BigInt(wallMs - offMin * 60000) * 1_000_000n;
  const ns2 = (baseNs * 2n + (sec === 60 ? 1n : 0n)) * 1_000_000_000n + fracNs;
  // canonical output: UTC date/time + frac + Z; leap second stays in the
  // (offset-adjusted) minute — chrono emits e.g. 22:59:60Z.
  const dt = new Date(ms);
  const pad = (n, w = 2) => String(n).padStart(w, "0");
  const out = `${pad(dt.getUTCFullYear(), 4)}-${pad(dt.getUTCMonth() + 1)}-${pad(dt.getUTCDate())}` +
    `T${pad(dt.getUTCHours())}:${pad(dt.getUTCMinutes())}:${pad(sec === 60 ? 60 : dt.getUTCSeconds())}` +
    fracOut + "Z";
  return { ms, ns2, out };
}

// Same-scale ordering key for a millisecond-precision `now` instant.
export function nowKey(nowMs) {
  const nowSec = Math.floor(nowMs / 1000);
  return BigInt(nowSec) * 2_000_000_000_000_000_000n +
    BigInt(nowMs - nowSec * 1000) * 1_000_000n;
}
