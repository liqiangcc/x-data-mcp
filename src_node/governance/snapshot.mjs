// Read-only snapshot manifest byte inspection — Node port of
// src/snapshot.rs::inspect.
//
// Verifies a bounded manifest plus three local archive bytes and returns
// the limited Inspection facts. It does NOT confer origin authenticity,
// archive-member binding, file-set consistency, business semantics,
// access/use approval, or production readiness.
//
// Error precedence (verbatim static texts preserved):
//   Invalid(&'static str) -> "invalid manifest: {str}"
//   Mismatch(FileRole)    -> "artifact bytes do not match manifest: {Role:?}"
//   Io                    -> "local input unavailable: {os err}"  (Node
//                            message differs from Rust io::Error text —
//                            category preserved, text delta documented)
//   Json                  -> "manifest JSON invalid: {msg}"       (serde
//                            position text differs — same category)
//
// Streaming artifact hashing (64 KiB chunks) — the 549 MB hsjday.zip is
// never loaded into memory. Input root is trusted/private as in Rust.

import { createHash } from "node:crypto";
import { openSync, readSync, realpathSync, statSync, closeSync } from "node:fs";

export class InspectionError extends Error {
  constructor(kind, message, extra = {}) {
    super(message);
    this.name = "InspectionError";
    this.kind = kind; // "invalid" | "mismatch" | "io" | "json"
    Object.assign(this, extra);
  }
}
const invalid = (s) => new InspectionError("invalid", `invalid manifest: ${s}`);
const ioErr = (e) => new InspectionError("io", `local input unavailable: ${e.message}`);
const jsonErr = (m) => new InspectionError("json", `manifest JSON invalid: ${m}`);

const ROLE_ORDER = { base_industry: 0, day: 1, gbbq: 2 };
const ROLE_DEBUG = { base_industry: "BaseIndustry", day: "Day", gbbq: "Gbbq" };
const ROLE_FILE = { base_industry: "base.zip", day: "hsjday.zip", gbbq: "gbbq.zip" };
const ROLE_URL_PATH = {
  base_industry: "products/data/data/dbf/base.zip",
  gbbq: "products/data/data/dbf/gbbq.zip",
  day: "vipdoc/hsjday.zip",
};
const FILE_FIELDS = new Set([
  "role", "relative_path", "source_url", "acquisition_started_at",
  "acquisition_completed_at", "last_modified", "etag", "bytes", "sha256",
]);

// Strict JSON parse mirroring serde's duplicate-field rejection (serde
// struct deserialization fails on duplicate keys anywhere in the manifest;
// JSON.parse silently takes the last value — this closes that gap).
// Bounded recursive-descent parser; depth-capped.
function strictJson(text) {
  let i = 0;
  const ws = () => { while (i < text.length && " \t\n\r".includes(text[i])) i++; };
  const fail = (m) => { throw jsonErr(m); };
  function value(depth) {
    if (depth > 64) fail("nesting too deep");
    ws();
    const ch = text[i];
    if (ch === "{") {
      i++;
      const obj = Object.create(null); // "__proto__" must stay an own key
      const seen = new Set();
      ws();
      if (text[i] === "}") { i++; return obj; }
      for (;;) {
        ws();
        if (text[i] !== '"') fail("key must be a string");
        const k = str();
        if (seen.has(k)) fail(`duplicate field \`${k}\``);
        seen.add(k);
        ws();
        if (text[i] !== ":") fail("expected `:`");
        i++;
        obj[k] = value(depth + 1);
        ws();
        if (text[i] === ",") { i++; continue; }
        if (text[i] === "}") { i++; return obj; }
        fail("expected `,` or `}`");
      }
    }
    if (ch === "[") {
      i++;
      const arr = [];
      ws();
      if (text[i] === "]") { i++; return arr; }
      for (;;) {
        arr.push(value(depth + 1));
        ws();
        if (text[i] === ",") { i++; continue; }
        if (text[i] === "]") { i++; return arr; }
        fail("expected `,` or `]`");
      }
    }
    if (ch === '"') return str();
    const num = /^-?\d+(\.\d+)?([eE][+-]?\d+)?/.exec(text.slice(i, i + 30));
    if (num && /^[0-9-]/.test(ch ?? "")) { i += num[0].length; return Number(num[0]); }
    if (text.startsWith("true", i)) { i += 4; return true; }
    if (text.startsWith("false", i)) { i += 5; return false; }
    if (text.startsWith("null", i)) { i += 4; return null; }
    fail("expected value");
  }
  function str() {
    i++; // opening quote
    let out = "";
    while (i < text.length) {
      const c = text[i];
      if (c === '"') { i++; return out; }
      if (c === "\\") {
        i++;
        const e = text[i++];
        if (e === "u") {
          const h = text.slice(i, i + 4);
          if (!/^[0-9a-fA-F]{4}$/.test(h)) fail("invalid \\u escape");
          i += 4;
          let cp = parseInt(h, 16);
          // serde rejects unpaired JSON \u surrogates
          if (cp >= 0xd800 && cp <= 0xdbff) {
            if (text[i] !== "\\" || text[i + 1] !== "u") fail("lone leading surrogate in hex escape");
            const h2 = text.slice(i + 2, i + 6);
            if (!/^[0-9a-fA-F]{4}$/.test(h2)) fail("invalid \\u escape");
            const lo = parseInt(h2, 16);
            if (lo < 0xdc00 || lo > 0xdfff) fail("lone leading surrogate in hex escape");
            i += 6;
            cp = 0x10000 + ((cp - 0xd800) << 10) + (lo - 0xdc00);
            out += String.fromCodePoint(cp);
          } else if (cp >= 0xdc00 && cp <= 0xdfff) {
            fail("lone trailing surrogate in hex escape");
          } else {
            out += String.fromCharCode(cp);
          }
        } else {
          const map = { '"': '"', "\\": "\\", "/": "/", b: "\b", f: "\f", n: "\n", r: "\r", t: "\t" };
          if (!(e in map)) fail("invalid escape");
          out += map[e];
        }
      } else if (c < " ") {
        fail("control character in string");
      } else {
        out += c;
        i++;
      }
    }
    fail("unterminated string");
  }
  ws();
  const v = value(0);
  ws();
  if (i !== text.length) fail("trailing characters");
  return v;
}

// chrono RFC3339: `[Tt ]` separator, required Z/z or ±HH:MM offset
// (HH<=23, MM<=59), second may be 60 (leap) on any minute, calendar
// strict, frac digits -> nanos (AutoSi output: 3-digit groups).
// Interval ordering key = (baseNs*2 + leapBit)*1e9 + fracNs:
// :59.f -> base*2e9+f; :60.f (leap) -> base*2e9+1e9+f — strictly after
// every :59.x and strictly before the next minute's :00.x, with full
// fractional ordering inside the leap second. No representational skew.
const TS_RE = /^(\d{4})-(\d{2})-(\d{2})[Tt ](\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?([Zz]|([+-])(\d{2}):(\d{2}))$/;
const DIM = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
function parseTs(s) {
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

function declaredOfficialUrl(role, url) {
  if (typeof url !== "string") return false;
  const rest = url.startsWith("https://") ? url.slice(8)
    : url.startsWith("http://") ? url.slice(7) : null;
  if (rest === null) return false;
  const slash = rest.indexOf("/");
  if (slash < 0) return false;
  const host = rest.slice(0, slash), path = rest.slice(slash + 1);
  if (path === "") return false;
  if (/[\t\n\v\f\r ]/.test(url)) return false;
  if (/[@:?#]/.test(host)) return false;
  const hostOk = role === "day" ? host === "data.tdx.com.cn"
    : host === "tdx.com.cn" || host === "www.tdx.com.cn";
  return hostOk && path === ROLE_URL_PATH[role];
}

function validateShape(m) {
  // serde deny_unknown_fields + field types -> "json" category (text delta
  // documented: serde emits expected-list + line/column, Node emits a
  // descriptive message without positions).
  if (m === null || typeof m !== "object" || Array.isArray(m)) {
    throw jsonErr("manifest must be a JSON object");
  }
  for (const k of Object.keys(m)) {
    if (k !== "schema" && k !== "files") throw jsonErr(`unknown field \`${k}\``);
  }
  if (typeof m.schema !== "string") throw jsonErr("missing or invalid field `schema`");
  if (!Array.isArray(m.files)) throw jsonErr("missing or invalid field `files`");
  for (const [i, f] of m.files.entries()) {
    if (f === null || typeof f !== "object" || Array.isArray(f)) {
      throw jsonErr(`files[${i}] must be an object`);
    }
    for (const k of Object.keys(f)) {
      if (!FILE_FIELDS.has(k)) throw jsonErr(`unknown field \`${k}\``);
    }
    if (typeof f.role !== "string" || !(f.role in ROLE_ORDER)) {
      throw jsonErr(`files[${i}].role: unknown variant`);
    }
    if (typeof f.relative_path !== "string") throw jsonErr(`files[${i}].relative_path must be a string`);
    if (typeof f.source_url !== "string") throw jsonErr(`files[${i}].source_url must be a string`);
    if (parseTs(f.acquisition_started_at) === null) throw jsonErr(`files[${i}].acquisition_started_at invalid`);
    if (parseTs(f.acquisition_completed_at) === null) throw jsonErr(`files[${i}].acquisition_completed_at invalid`);
    // serde Option<String>: omitted key -> null (accepted), non-string
    // present value -> reject.
    if (f.last_modified !== null && f.last_modified !== undefined && typeof f.last_modified !== "string") throw jsonErr(`files[${i}].last_modified invalid`);
    if (f.etag !== null && f.etag !== undefined && typeof f.etag !== "string") throw jsonErr(`files[${i}].etag invalid`);
    if (!Number.isSafeInteger(f.bytes) || f.bytes < 0) throw jsonErr(`files[${i}].bytes invalid`);
    if (typeof f.sha256 !== "string") throw jsonErr(`files[${i}].sha256 must be a string`);
  }
}

/**
 * @param {Buffer|Uint8Array} manifestBytes
 * @param {string} root — trusted/private input root
 * @param {Date|string} now
 * @returns {Inspection} — files role-sorted
 */
export function inspect(manifestBytes, root, now) {
  if (!(manifestBytes instanceof Uint8Array)) {
    throw new InspectionError("invalid_input", "manifest_bytes must be a Buffer or Uint8Array");
  }
  if (typeof root !== "string") throw new InspectionError("invalid_input", "root must be a string path");
  const nowMs = now instanceof Date ? now.getTime() : Date.parse(now);
  if (!Number.isFinite(nowMs)) throw new InspectionError("invalid_input", "now must be a Date or timestamp");

  if (manifestBytes.length > 1024 * 1024) throw invalid("manifest exceeds 1 MiB");
  let manifest;
  try {
    // strict UTF-8 first — lossy decoding would accept bytes serde rejects
    const text = new TextDecoder("utf-8", { fatal: true }).decode(manifestBytes);
    manifest = strictJson(text);
  } catch (e) {
    if (e instanceof InspectionError) throw e;
    throw jsonErr(e.message);
  }
  validateShape(manifest);
  if (manifest.schema !== "x-data-mcp.official-file-input.v1" || manifest.files.length !== 3) {
    throw invalid("schema or required role count");
  }
  const roles = new Set(), paths = new Set();
  // Complete contract checked before touching artifact paths.
  for (const input of manifest.files) {
    // Rust Path equality is component-normalized: repeated/trailing
    // separators and interior `.` collapse; a leading `./` is preserved as
    // CurDir and `..` stays ParentDir (both rejected below).
    const rel = input.relative_path;
    const rawSegs = rel.split("/");
    const isAbs = rel.startsWith("/");
    const segs = rawSegs.filter((s, i) => s !== "" && !(s === "." && i > 0));
    const normKey = (isAbs ? "/" : "") + segs.join("/");
    if (roles.has(input.role) || paths.has(normKey)) {
      throw invalid("duplicate role/path");
    }
    roles.add(input.role);
    paths.add(normKey);
    if (rel === "" || isAbs || segs.length === 0 ||
        segs.some((s, i) => s === ".." || (s === "." && i === 0))) {
      throw invalid("artifact path must be relative without parent/current components");
    }
    if (segs[segs.length - 1] !== ROLE_FILE[input.role]) {
      throw invalid("artifact filename does not match its official-file role");
    }
    if (!declaredOfficialUrl(input.role, input.source_url)) {
      throw invalid("expected role-specific declared TDX http(s) URL");
    }
    const started = parseTs(input.acquisition_started_at).ns2;
    const completed = parseTs(input.acquisition_completed_at).ns2;
    const nowNs2 = BigInt(nowMs) * 2_000_000_000_000_000n;
    if (started > completed || completed > nowNs2) {
      throw invalid("inverted/future acquisition interval");
    }
    if (input.bytes === 0 || !/^[0-9a-f]{64}$/.test(input.sha256)) {
      throw invalid("empty file or noncanonical sha256");
    }
  }

  let canonRoot;
  try {
    canonRoot = realpathSync(root);
  } catch (e) {
    throw ioErr(e);
  }
  const rootPrefix = canonRoot.endsWith("/") ? canonRoot : canonRoot + "/";
  const tsOut = (s) => parseTs(s).out;
  for (const input of manifest.files) {
    let resolved;
    try {
      resolved = realpathSync(canonRoot + "/" + input.relative_path);
    } catch (e) {
      throw ioErr(e);
    }
    if (resolved !== canonRoot && !resolved.startsWith(rootPrefix)) {
      throw invalid("artifact resolves outside input root");
    }
    let st;
    try {
      st = statSync(resolved);
    } catch (e) {
      throw ioErr(e);
    }
    if (!st.isFile()) throw invalid("artifact is not a regular file");
    // streaming hash: 64 KiB chunks, never whole-file
    const hash = createHash("sha256");
    let bytes = 0, fd;
    try {
      fd = openSync(resolved, "r");
    } catch (e) {
      throw ioErr(e);
    }
    try {
      const buffer = Buffer.allocUnsafe(65536);
      for (;;) {
        const n = readSync(fd, buffer, 0, 65536, null);
        if (n === 0) break;
        bytes += n;
        hash.update(buffer.subarray(0, n));
      }
    } catch (e) {
      throw ioErr(e);
    } finally {
      closeSync(fd);
    }
    const digest = hash.digest("hex");
    if (bytes !== input.bytes || digest !== input.sha256) {
      throw new InspectionError("mismatch",
        `artifact bytes do not match manifest: ${ROLE_DEBUG[input.role]}`,
        { role: input.role });
    }
  }

  const files = manifest.files
    .map((f) => ({
      role: f.role,
      relative_path: f.relative_path,
      source_url: f.source_url,
      acquisition_started_at: tsOut(f.acquisition_started_at),
      acquisition_completed_at: tsOut(f.acquisition_completed_at),
      last_modified: f.last_modified ?? null,
      etag: f.etag ?? null,
      bytes: f.bytes,
      sha256: f.sha256,
    }))
    .sort((a, b) => ROLE_ORDER[a.role] - ROLE_ORDER[b.role]);
  return {
    schema: "x-data-mcp.official-file-inspection.v1",
    manifest_sha256: createHash("sha256").update(manifestBytes).digest("hex"),
    files,
    byte_integrity: "FILE_BYTES_VERIFIED",
    declared_origin: "NOT_AUTHENTICATED",
    archive_member_binding: "NOT_VERIFIED",
    file_set_consistency: "NOT_VERIFIED",
    business_semantics: "NOT_VERIFIED",
    access_use: "NOT_VERIFIED",
    production_ready: false,
    live_source: "NOT_RUN",
  };
}
