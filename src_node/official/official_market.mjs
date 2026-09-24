// NM-075 — Node port of src/official_market.rs materialization layer
// (S11a file-format half): TdxMarketSource inspect/from_env/archive_path/
// materialize_session/materialize_daily, security_events_digest,
// ArchiveIndex + session member_bytes, MaterializeSession
// (decoded_gbbq/security_events/day_names/materialize_daily/qfq_bars/
// assemble_daily), MarketDataPipeline impl, merge_segment_bars.
//
// Semantics mirrored verbatim. Known text deltas vs Rust (same class,
// different inner wording — documented per project convention):
//   - io::Error Display inside `archive unavailable:`/`archive open
//     failed:`/`archive is not a valid zip: i/o error:` — Node fs
//     messages differ from Rust os-error text.
//   - Deflate stream errors inside `member bytes failed CRC:` — Node
//     zlib wording differs from flate2.
//   - `{symbol:?}`/`{candidates:?}` use Rust Debug escaping; ASCII
//     inputs are identical, exotic chars may differ from Rust's
//     escape_debug coverage.
//   - Zip64 extra fields inside central-directory entries (member
//     sizes/offsets ≥ 4 GiB) are not interpreted — the official
//     archives are zip32; a crafted zip64 archive fails closed at the
//     CRC/size checks with a possibly different inner error.
//   - Non-ASCII/cp437 member-name decoding is byte-latin1; official
//     members are ASCII.
//   - ArchiveIndex holds an open fd and reads members lazily (Rust:
//     ZipArchive<BufReader<File>>) — central directory parsed once,
//     each member read on demand; no whole-archive buffering.

import { openSync, readSync, closeSync, fstatSync, realpathSync } from "node:fs";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { inflateRawSync } from "node:zlib";
import { inspect as snapshotInspect } from "../governance/snapshot.mjs";
import { parseTs } from "../governance/ts.mjs";
import { parse as parseDay } from "../parsers/day_file.mjs";
import { parse as parseGbbq } from "../parsers/gbbq_file.mjs";
import { decodeEvents } from "../parsers/gbbq_decode.mjs";
import { forwardAdjust } from "../algorithms/qfq_forward.mjs";
import { __cmpDec, __parseDate } from "../market/record_codec.mjs";

export const SOURCE_NAME = "tdx-official-file-snapshot";
export const ADAPTER_VERSION = "tdx-market-source-v1";
export const DECLARED_SCOPE =
  "DECLARED_SCOPE: owner personal research, local-only, no external exposure " +
  "(#71 comment-5749934485); overall production certification pending #225";
const GBBQ_MEMBER = "gbbq";
// Snapshot-binding role for the per-security decoded event set: digests
// the category-1 events actually applied plus the excluded count, so an
// update can prove event continuity for one symbol even when other
// securities' records changed the gbbq member as a whole.
export const SECURITY_EVENTS_ROLE = "gbbq-security-events";
// Largest manifest byte count the installer will read; the pinned snapshot
// schema carries exactly three file entries and stays far below this.
const MANIFEST_LIMIT = 1 << 20;

export const sha256hex = (b) => createHash("sha256").update(b).digest("hex");

// ---------------------------------------------------------------------
// market_data::SourceError — Display text verbatim; `kind` mirrors the
// enum variant.
export class SourceError extends Error {
  constructor(kind, message) {
    super(message);
    this.name = "SourceError";
    this.kind = kind;
  }
}
export const srcNotVerified = (m) => new SourceError("not_verified", `SOURCE_NOT_VERIFIED: ${m}`);
export const srcUnsupported = (m) => new SourceError("unsupported", `unsupported request: ${m}`);
export const srcInvocation = (m) => new SourceError("invocation", `source invocation failed: ${m}`);
export const srcInvalidResponse = (m) => new SourceError("invalid_response", `invalid source response: ${m}`);
// Timeout{timeout_ms} — declared for completeness; this source never times out.
export const srcTimeout = (timeout_ms) =>
  new SourceError("timeout", `source invocation timed out after ${timeout_ms} ms`);

// Rust {:?} Debug formatting for &str: double-quoted, \n \r \t \\ \" \0
// escapes, other control chars as \u{..}. ASCII inputs are identical to
// JSON.stringify.
export const rustDebugStr = (s) => {
  let out = '"';
  for (const ch of s) {
    const cp = ch.codePointAt(0);
    if (ch === '"') out += '\\"';
    else if (ch === "\\") out += "\\\\";
    else if (ch === "\n") out += "\\n";
    else if (ch === "\r") out += "\\r";
    else if (ch === "\t") out += "\\t";
    else if (ch === "\0") out += "\\0";
    else if (cp < 0x20 || cp === 0x7f) out += `\\u{${cp.toString(16)}}`;
    else out += ch;
  }
  return out + '"';
};
const rustDebugStrList = (list) => `[${list.map(rustDebugStr).join(", ")}]`;

// ---------------------------------------------------------------------
// rust_decimal Decimal::from_str (parse_str_radix_10, ROUND=true)
// replicated for the only call site: Decimal::from_str(f64 Display).
// Inputs are `[+-]?digits[.digits]` — f64 Display never emits exponent,
// '_' or NaN/inf text (those fail the digit check → null = Err → .ok()).
const U96_LIMIT = 79228162514264337593543950336n; // 2^96
// maybe_round(data, digit, rest): half-up on the first dropped digit;
// on u96 overflow after +1, +4 then /10 and scale-1 (verbatim). Rest of
// input must be digits only (a second '.' or other char → Err).
function decMaybeRound(mant, digit, rest, scale, neg) {
  if (digit >= 5) {
    mant += 1n;
    if (mant >= U96_LIMIT) {
      if (scale === 0) return null; // "overflow from mantissa after rounding"
      mant = (mant + 4n) / 10n;
      scale -= 1;
    }
  }
  for (const ch of rest) {
    const c = ch.codePointAt(0);
    if (c < 48 || c > 57) return null;
  }
  return { neg, mant, scale };
}
function decimalFromStr(s) {
  let i = 0;
  const neg = s[0] === "-";
  if (neg || s[0] === "+") i = 1;
  let mant = 0n, scale = 0, point = false, has = false;
  for (; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c === 46 && !point) { point = true; continue; }
    if (c < 48 || c > 57) return null; // invalid digit/point → Err
    has = true;
    const d = c - 48;
    if (point && scale >= 28) {
      return decMaybeRound(mant, d, s.slice(i + 1), scale, neg);
    }
    const cand = mant * 10n + BigInt(d);
    if (cand >= U96_LIMIT) {
      if (!point) return null; // "overflow from too many digits"
      return decMaybeRound(mant, d, s.slice(i + 1), scale, neg);
    }
    mant = cand;
    if (point) scale++;
  }
  if (!has) return null; // "no digits found"
  return { neg, mant, scale };
}
// Decimal::to_string / serde serialize_str: mantissa digits with the
// point inserted `scale` from the right, sign preserved (even -0).
function decimalToString(d) {
  let digits = d.mant.toString(10);
  while (digits.length <= d.scale) digits = "0" + digits;
  const out = d.scale === 0 ? digits
    : `${digits.slice(0, digits.length - d.scale)}.${digits.slice(digits.length - d.scale)}`;
  return (d.neg ? "-" : "") + out;
}
// Rust f64 Display: shortest round-trip decimal, never exponential.
// Node's String() emits the same shortest digits but uses e-notation
// outside [1e-6, 1e21) — expand it back to plain digits.
function f64Display(v) {
  if (Number.isNaN(v)) return "NaN";
  if (v === Infinity) return "inf";
  if (v === -Infinity) return "-inf";
  if (Object.is(v, -0)) return "-0";
  const s = String(v);
  const m = /^(\d)(?:\.(\d+))?[eE]([+-]?\d+)$/.exec(s.startsWith("-") ? s.slice(1) : s);
  if (!m) return s;
  const digits = m[1] + (m[2] ?? "");
  const point = 1 + Number(m[3]);
  const sign = s.startsWith("-") ? "-" : "";
  if (point <= 0) return `${sign}0.${"0".repeat(-point)}${digits}`;
  if (point >= digits.length) return `${sign}${digits}${"0".repeat(point - digits.length)}`;
  return `${sign}${digits.slice(0, point)}.${digits.slice(point)}`;
}
const f32v = new DataView(new ArrayBuffer(4));
// Bar.amount: Decimal::from_str(f64::from(f32::from_bits(bits)).to_string()).ok()
const amountFromBits = (bits) => {
  f32v.setUint32(0, bits >>> 0, true);
  const d = decimalFromStr(f64Display(f32v.getFloat32(0, true)));
  return d === null ? null : decimalToString(d);
};

// ---------------------------------------------------------------------
// Canonical digest of one security's interpreted event set: every
// category-1 event's stored (date_raw, c_raw) fields in decode order,
// followed by the excluded non-category-1 record count.
export function securityEventsDigest(events, excluded) {
  const buf = Buffer.allocUnsafe(events.length * 20 + 8);
  let off = 0;
  for (const e of events) {
    buf.writeUInt32LE(e.date_raw >>> 0, off); off += 4;
    for (const bits of e.c_raw_bits) { buf.writeUInt32LE(bits >>> 0, off); off += 4; }
  }
  buf.writeBigUInt64LE(BigInt(excluded), off);
  return sha256hex(buf);
}

// ---------------------------------------------------------------------
// ZipArchive<BufReader<File>> equivalent: EOCD scan -> central directory
// -> per-entry local header, all at open; member bytes read lazily.
// Errors carry zip-crate Display text (ZipError::{InvalidArchive,
// UnsupportedArchive, Io, FileNotFound}).
class ZipTextError extends Error {}
const zipInvalid = (m) => new ZipTextError(`invalid Zip archive: ${m}`);
const zipUnsupported = (m) => new ZipTextError(`unsupported Zip archive: ${m}`);
const zipIo = (m) => new ZipTextError(`i/o error: ${m}`);
const ZIP_NOT_FOUND = () => new ZipTextError("specified file not found in archive");

const SIG_EOCD = 0x06054b50, SIG_CDH = 0x02014b50, SIG_LH = 0x04034b50;
const EOCD_SIG = Buffer.from([0x50, 0x4b, 0x05, 0x06]);
const SIG_EOCD64_BUF = Buffer.from([0x50, 0x4b, 0x06, 0x06]);
const CDFH_SIG_BUF = Buffer.from([0x50, 0x4b, 0x01, 0x02]);

function readExactAt(fd, len, pos) {
  const buf = Buffer.allocUnsafe(len);
  let off = 0;
  while (off < len) {
    const n = readSync(fd, buf, off, len - off, pos + off);
    if (n === 0) throw zipIo("failed to fill whole buffer");
    off += n;
  }
  return buf;
}

const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();
const crc32 = (b) => {
  let c = 0xffffffff;
  for (const x of b) c = CRC_TABLE[(c ^ x) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};

// First sig occurrence in [start, end) — MagicFinder<Forward> over the
// same bounds (the optimistic guess is bounds.start: same outcome since
// it equals the first position). Window reads can't hit EOF inside the
// caller-validated range.
function* sigForward(fd, sig, start, end) {
  let scan = start;
  while (scan + 4 <= end) {
    const wEnd = Math.min(end, scan + (1 << 20));
    const win = readExactAt(fd, wEnd - scan, scan);
    let p = win.indexOf(sig);
    while (p >= 0) { yield scan + p; p = win.indexOf(sig, p + 1); }
    if (wEnd === end) break;
    scan = wEnd - 3; // needle_len-1 overlap
  }
}

// Parse one EOCD block at `off` — Zip32CentralDirectoryEnd::parse:
// block read_exact failure → Io; bad magic → InvalidArchive; comment
// read_exact UnexpectedEof → InvalidArchive("EOCD comment exceeds file
// boundary"). `off` is a position where the EOCD signature matched.
function parseEocd(fd, off, fileLen) {
  if (off + 22 > fileLen) return { error: zipIo("failed to fill whole buffer") };
  const b = readExactAt(fd, 22, off);
  if (b.readUInt32LE(0) !== SIG_EOCD) {
    return { error: zipInvalid("Invalid digital signature header") };
  }
  const commentLen = b.readUInt16LE(20);
  if (off + 22 + commentLen > fileLen) {
    return { error: zipInvalid("EOCD comment exceeds file boundary") };
  }
  return {
    eocd: {
      off,
      diskNumber: b.readUInt16LE(4),
      diskWithCD: b.readUInt16LE(6),
      filesOnDisk: b.readUInt16LE(8),
      filesTotal: b.readUInt16LE(10),
      cdOffset: b.readUInt32LE(16),
      // may_be_zip64: number_of_files == u16::MAX || cd_offset == u32::MAX
      mayBeZip64: b.readUInt16LE(10) === 0xffff || b.readUInt32LE(16) === 0xffffffff,
    },
  };
}

// try_read_eocd64_locator — `.ok()` at the call site swallows every
// error (missing bytes, bad magic, bad fields) into the zip32 branch.
function tryLocator(fd, eocdOff) {
  if (eocdOff < 20) return null;
  const off = eocdOff - 20;
  const b = readExactAt(fd, 20, off); // fits: off+20 = eocdOff < fileLen
  if (b.readUInt32LE(0) !== 0x07064b50) return null;
  return {
    off,
    diskWithCD: b.readUInt32LE(4),
    eocd64Offset: b.readBigUInt64LE(8),
    numDisks: b.readUInt32LE(16),
  };
}

// Zip64CentralDirectoryEnd::parse + the two locator-consistency checks
// in try_read_eocd64. Errors → caller's local_error.
function parseEocd64(fd, off, maxSize) {
  const b = readExactAt(fd, 56, off); // io error propagates verbatim
  if (b.readUInt32LE(0) !== 0x06064b50) {
    throw zipInvalid("Invalid digital signature header");
  }
  const recordSize = b.readBigUInt64LE(4);
  if (recordSize < 44n) throw zipInvalid("Low EOCD64 record size");
  if (recordSize + 12n > BigInt(maxSize)) {
    throw zipInvalid("EOCD64 extends beyond EOCD64 locator");
  }
  const commentLen = Number(recordSize - 44n); // bounded by maxSize ≤ fileLen
  readExactAt(fd, commentLen, off + 56); // comment read_exact → io error
  return {
    recordSize,
    versionMadeBy: b.readUInt16LE(12),
    versionNeeded: b.readUInt16LE(14),
    diskNumber: b.readUInt32LE(16),
    diskWithCD: b.readUInt32LE(20),
    filesOnDisk: b.readBigUInt64LE(24),
    filesTotal: b.readBigUInt64LE(32),
    cdOffset: b.readBigUInt64LE(48),
  };
}

class ArchiveIndex {
  // Rust ArchiveIndex::open — File::open then ZipArchive::new →
  // get_metadata: find_central_directory + CentralDirectoryInfo::try_from
  // + read_central_header, retrying below the EOCD on CD-level failure.
  // `fd` stays open for lazy member reads.
  static open(path) {
    let fd;
    try {
      fd = openSync(path, "r");
    } catch (e) {
      throw srcInvocation(`archive open failed: ${e.message}`);
    }
    try {
      const fileLen = fstatSync(fd).size;
      let endExclusive = fileLen;
      for (;;) {
        // A find_central_directory error aborts (the `?`) — only the
        // try_from/read_central_header failure retries below the EOCD.
        const cde = ArchiveIndex.#findCd(fd, endExclusive, fileLen);
        try {
          return ArchiveIndex.#readCd(fd, cde);
        } catch {
          endExclusive = cde.eocd.off;
        }
      }
    } catch (e) {
      try { closeSync(fd); } catch { /* already closed */ }
      if (e instanceof SourceError) throw e;
      throw srcInvocation(`archive is not a valid zip: ${e.message}`);
    }
  }

  // spec::find_central_directory: backwards EOCD scan over
  // [0, endExclusive). parsing_error rules (verbatim): an EOCD parse
  // failure only fills an empty slot; every later structural failure
  // overwrites. Exhaustion → last recorded error, else
  // "Could not find EOCD".
  static #findCd(fd, endExclusive, fileLen) {
    let parsingError = null;
    const CHUNK = 1 << 20;
    let windowEnd = endExclusive;
    outer: for (;;) {
      const start = Math.max(0, windowEnd - CHUNK);
      const win = readExactAt(fd, windowEnd - start, start);
      let pos = win.lastIndexOf(EOCD_SIG);
      while (pos >= 0) {
        const off = start + pos;
        const t = parseEocd(fd, off, fileLen);
        if (t.error !== undefined) {
          if (parsingError === null) parsingError = t.error;
          pos = win.lastIndexOf(EOCD_SIG, pos - 1);
          continue;
        }
        const eocd = t.eocd;
        if (eocd.mayBeZip64) {
          const loc = tryLocator(fd, off);
          if (loc !== null) {
            if (loc.eocd64Offset >= BigInt(loc.off)) {
              parsingError = zipInvalid("Invalid EOCD64 Locator CD offset");
            } else if (loc.numDisks > 1) {
              parsingError = zipInvalid("Multi-disk ZIP files are not supported");
            } else {
              let localError = null;
              for (const z64off of sigForward(fd, SIG_EOCD64_BUF, Number(loc.eocd64Offset), loc.off)) {
                try {
                  const z64 = parseEocd64(fd, z64off, loc.off - z64off);
                  if (z64.diskWithCD !== loc.diskWithCD) {
                    throw zipInvalid("Invalid EOCD64: inconsistency with Locator data");
                  }
                  if (z64.recordSize + 12n !== BigInt(loc.off - z64off)) {
                    throw zipInvalid("Invalid EOCD64: inconsistent length");
                  }
                  if (BigInt(z64off) < z64.filesTotal * 46n + z64.cdOffset) {
                    localError = zipInvalid("Invalid EOCD64: inconsistent number of files");
                    continue;
                  }
                  return {
                    eocd,
                    eocd64: z64,
                    archiveOffset: BigInt(z64off) - loc.eocd64Offset,
                  };
                } catch (e) {
                  localError = e;
                }
              }
              parsingError = localError ?? zipInvalid("Could not find EOCD64");
            }
            pos = win.lastIndexOf(EOCD_SIG, pos - 1);
            continue;
          }
        }
        // zip32 branch
        const relCd = eocd.cdOffset;
        if (eocd.filesTotal === 0) {
          return { eocd, eocd64: null, archiveOffset: BigInt(Math.max(0, off - relCd)) };
        }
        if (relCd >= off) {
          parsingError = zipInvalid("Invalid CDFH offset in EOCD");
          pos = win.lastIndexOf(EOCD_SIG, pos - 1);
          continue;
        }
        for (const cdOff of sigForward(fd, CDFH_SIG_BUF, relCd, off)) {
          return { eocd, eocd64: null, archiveOffset: BigInt(cdOff - relCd) };
        }
        parsingError = zipInvalid("No CDFH found");
        pos = win.lastIndexOf(EOCD_SIG, pos - 1);
      }
      if (start === 0) break outer;
      windowEnd = start + 4; // needle overlap for straddling signatures
    }
    throw parsingError ?? zipInvalid("Could not find EOCD");
  }

  // CentralDirectoryInfo::try_from + read_central_header: field picks,
  // directory_start = relCd + archive_offset, disk check, CDH walk with
  // eager local-header resolution. Failure → get_metadata retries below
  // this EOCD.
  static #readCd(fd, cde) {
    const { eocd, eocd64, archiveOffset } = cde;
    let relCd, numFiles, diskNumber, diskWithCD;
    if (eocd64 !== null) {
      if (eocd64.filesOnDisk > eocd64.filesTotal) {
        throw zipInvalid(
          "ZIP64 footer indicates more files on this disk than in the whole archive");
      }
      if (eocd64.versionNeeded > eocd64.versionMadeBy) {
        throw zipInvalid(
          "ZIP64 footer indicates a new version is needed to extract this archive than the " +
          "version that wrote it");
      }
      relCd = eocd64.cdOffset; numFiles = eocd64.filesTotal;
      diskNumber = eocd64.diskNumber; diskWithCD = eocd64.diskWithCD;
    } else {
      relCd = BigInt(eocd.cdOffset); numFiles = BigInt(eocd.filesOnDisk);
      diskNumber = eocd.diskNumber; diskWithCD = eocd.diskWithCD;
    }
    const directoryStart = relCd + archiveOffset;
    if (directoryStart > 0xffffffffffffffffn) {
      throw zipInvalid("Invalid central directory size or offset");
    }
    // read_central_header
    const fileCapacity = numFiles > directoryStart ? 0n : numFiles;
    if (diskNumber !== diskWithCD) {
      throw zipUnsupported("Support for multi-disk files is not implemented");
    }
    // numFiles * size_of::<ZipFileData>() > isize::MAX — the exact
    // constant only shifts the unreachable boundary; ~200B.
    if (fileCapacity * 256n > 9223372036854775807n) {
      throw zipUnsupported("Oversized central directory");
    }
    const dirStart = Number(directoryStart); // valid offsets are < fileLen
    const entries = [];
    let off = dirStart;
    for (let i = 0n; i < numFiles; i++) {
      const h = readExactAt(fd, 46, off);
      if (h.readUInt32LE(0) !== SIG_CDH) {
        throw zipInvalid("Invalid Central Directory header");
      }
      const nameLen = h.readUInt16LE(28), extraLen = h.readUInt16LE(30), cmtLen = h.readUInt16LE(32);
      const name = readExactAt(fd, nameLen, off + 46).toString("latin1");
      // extra + comment are skipped over, but Rust's read_exact on them
      // still surfaces io errors — replicate the read.
      readExactAt(fd, extraLen + cmtLen, off + 46 + nameLen);
      const recEnd = off + 46 + nameLen + extraLen + cmtLen;
      const headerStart = Number(BigInt(h.readUInt32LE(42)) + archiveOffset);
      const entry = {
        name,
        flags: h.readUInt16LE(8),
        method: h.readUInt16LE(10),
        crc: h.readUInt32LE(16),
        compSize: h.readUInt32LE(20),
        uncompSize: h.readUInt32LE(24),
        headerStart,
      };
      if (entry.headerStart >= dirStart) {
        throw zipInvalid("A local file entry can't start after the central directory");
      }
      // find_data_start (eager in the crate): parse the local header.
      const lh = readExactAt(fd, 30, entry.headerStart);
      if (lh.readUInt32LE(0) !== SIG_LH) {
        throw zipInvalid("Invalid local file header");
      }
      entry.dataStart =
        entry.headerStart + 30 + lh.readUInt16LE(26) + lh.readUInt16LE(28);
      if (entry.dataStart > dirStart) {
        throw zipInvalid("File data can't start after the central directory");
      }
      entries.push(entry);
      off = recEnd;
    }
    return new ArchiveIndex(fd, dirStart, entries);
  }

  // `files` mirrors Shared::files (IndexMap<name, ZipFileData>): one slot
  // per unique name, first-insertion order, last-wins data — duplicate
  // member names collapse, so `member absent or ambiguous` only arises
  // across distinct candidate names.
  constructor(fd, directoryStart, cdEntries) {
    this.fd = fd;
    this.directoryStart = directoryStart;
    const dedup = new Map();
    for (const e of cdEntries) dedup.set(e.name, e);
    this.entries = [...dedup.values()];
    this.members = new Map(); // name -> [indices] in file_names() order
    for (let i = 0; i < this.entries.length; i++) {
      const arr = this.members.get(this.entries[i].name);
      if (arr === undefined) this.members.set(this.entries[i].name, [i]);
      else arr.push(i);
    }
  }

  // ZipFile open phase (by_index_with_optional_password): index lookup,
  // encrypted flag, then compression method (make_crypto_reader order;
  // find_content's seek is infallible since data_start was resolved at
  // open). Read phase (read_to_end over Crc32Reader) is separate so
  // callers can wrap the two phases with the Rust error texts.
  memberOpen(index) {
    const e = this.entries[index];
    if (e === undefined) throw ZIP_NOT_FOUND();
    if (e.flags & 0x1) throw zipUnsupported("Password required to decrypt file");
    if (e.method !== 0 && e.method !== 8) {
      throw zipUnsupported("Compression method not supported");
    }
    return e;
  }

  // ZipFile::read_to_end: Take(compressed_size) over the resolved data
  // start — a truncated file yields fewer bytes, never an io error —
  // then decompress (stored: passthrough) + CRC check at EOF. A wrong
  // byte count almost surely CRC-fails; uncompressed_size is only a
  // capacity hint in the crate, not an enforced bound.
  memberRead(e) {
    const avail = Math.max(0, fstatSync(this.fd).size - e.dataStart);
    const want = Math.min(e.compSize, avail);
    const data = Buffer.allocUnsafe(want);
    let off = 0;
    while (off < want) {
      const n = readSync(this.fd, data, off, want - off, e.dataStart + off);
      if (n === 0) break;
      off += n;
    }
    const comp = data.subarray(0, off);
    const out = e.method === 0 ? comp : inflateRawSync(comp);
    if (crc32(out) !== e.crc) throw new Error("Invalid checksum");
    return out;
  }
}

// Rust Path::components: RootDir for '/', collapsed separators, interior
// '.' dropped, a leading '.' emitted as CurDir, '..' kept as ParentDir.
function pathComponents(p) {
  const out = [];
  let s = p;
  if (s.startsWith("/")) { out.push("/"); s = s.slice(1); }
  const segs = s.split("/").filter((x) => x !== "");
  let i = 0;
  if (out.length === 0 && segs[0] === ".") { out.push("."); i = 1; }
  for (; i < segs.length; i++) {
    if (segs[i] === ".") continue;
    out.push(segs[i]);
  }
  return out;
}
// Path::starts_with — component-wise prefix.
function pathStartsWith(path, base) {
  const a = pathComponents(path), b = pathComponents(base);
  if (b.length > a.length) return false;
  return b.every((c, i) => a[i] === c);
}

// ---------------------------------------------------------------------
export class TdxMarketSource {
  // Construction is the gate: only an inspect()-passed manifest may form
  // a source. Any inspection failure is NotVerified, never a fallback.
  constructor(root, manifestSha256, day, gbbq) {
    this.root = root;
    this.manifest_sha256 = manifestSha256;
    this.day = day;
    this.gbbq = gbbq;
  }

  static inspect(root, manifest, now) {
    let inspection;
    try {
      inspection = snapshotInspect(manifest, root, now);
    } catch (e) {
      throw srcNotVerified(`snapshot inspection failed: ${e.message ?? e}`);
    }
    if (inspection.production_ready) {
      throw srcNotVerified("inspection must not approve production readiness");
    }
    const take = (role) => {
      const f = inspection.files.find((x) => x.role === role);
      if (f === undefined) throw srcNotVerified("inspected role missing");
      return { ...f };
    };
    return new TdxMarketSource(root, inspection.manifest_sha256, take("day"), take("gbbq"));
  }

  // Service install hook: only when X_DATA_MCP_OFFICIAL_MANIFEST
  // (manifest file) and X_DATA_MCP_OFFICIAL_DIR (snapshot root) are both
  // configured AND the manifest passes inspect() does a source exist.
  static fromEnv(env = process.env) {
    const manifest = env.X_DATA_MCP_OFFICIAL_MANIFEST;
    const root = env.X_DATA_MCP_OFFICIAL_DIR;
    if (manifest === undefined || root === undefined) return null;
    let bytes;
    try {
      const fd = openSync(manifest, "r");
      try {
        const buf = Buffer.allocUnsafe(MANIFEST_LIMIT + 1);
        let off = 0;
        for (;;) {
          const n = readSync(fd, buf, off, MANIFEST_LIMIT + 1 - off, null);
          if (n === 0) break;
          off += n;
        }
        bytes = buf.subarray(0, off);
      } finally {
        closeSync(fd);
      }
    } catch {
      return null;
    }
    if (bytes.length > MANIFEST_LIMIT) return null;
    try {
      return TdxMarketSource.inspect(root, bytes, new Date());
    } catch {
      return null;
    }
  }

  // Byte-verified snapshot binding carried into version provenance.
  manifestSha256() {
    return this.manifest_sha256;
  }

  // Resolve an inspected input to its canonical archive path under the
  // snapshot root. Containment check vs the raw root, verbatim.
  archivePath(input) {
    let archive;
    try {
      archive = realpathSync(join(this.root, input.relative_path));
    } catch (e) {
      throw srcInvocation(`archive unavailable: ${e.message}`);
    }
    if (!pathStartsWith(archive, this.root)) {
      throw srcInvocation("archive resolves outside inspected root");
    }
    return archive;
  }

  // Shared-decode session over this source: each archive's central
  // directory is parsed at most once and the gbbq member is decoded at
  // most once per session.
  materializeSession() {
    return new MaterializeSession(this);
  }

  // Single-symbol convenience — identical to a fresh session's
  // materialize_daily.
  materializeDaily(symbol) {
    return this.materializeSession().materializeDaily(symbol);
  }

  // MarketDataPipeline::materialize_daily — Adjustment has exactly one
  // variant; the Rust match is exhaustive so any other value is a typed
  // boundary violation, not a source error.
  pipelineMaterializeDaily(symbol, adjustment) {
    if (adjustment === "Forward") return this.materializeDaily(symbol);
    throw new TypeError(`expected Adjustment "Forward", got ${String(adjustment)}`);
  }

  // MarketDataPipeline::derive_yearly — verbatim refusal.
  deriveYearly(_daily) {
    throw srcUnsupported(
      "yearly is derived from published daily by the yearly path, not a source fetch");
  }
}

export class MaterializeSession {
  constructor(source) {
    this.source = source;
    this._gbbq = null; // OnceCell<DecodedGbbq>
    this.archives = new Map(); // canonical archive path -> ArchiveIndex
  }

  // Read exactly one archive member through the session's shared index.
  memberBytes(input, candidates) {
    const path = this.source.archivePath(input);
    let index = this.archives.get(path);
    if (index === undefined) {
      index = ArchiveIndex.open(path);
      this.archives.set(path, index);
    }
    const found = [];
    for (const name of candidates) {
      const indices = index.members.get(name);
      if (indices !== undefined) {
        for (const i of indices) found.push([name, i]);
      }
    }
    if (found.length !== 1) {
      throw srcInvocation(
        `member absent or ambiguous: expected exactly one of ${rustDebugStrList(candidates)}, found ${found.length}`);
    }
    const [member, memberIndex] = found[0];
    let entry;
    try {
      entry = index.memberOpen(memberIndex);
    } catch (e) {
      throw srcInvocation(`member open failed: ${e.message}`);
    }
    let bytes;
    try {
      bytes = index.memberRead(entry);
    } catch (e) {
      throw srcInvocation(`member bytes failed CRC: ${e.message}`);
    }
    return [member, bytes];
  }

  // Ensure the gbbq member is decoded and return the shared state.
  decodedGbbq() {
    if (this._gbbq === null) {
      const [member, bytes] = this.memberBytes(this.source.gbbq, [GBBQ_MEMBER]);
      let raw;
      try {
        raw = parseGbbq(bytes);
      } catch (e) {
        throw srcInvalidResponse(`gbbq member malformed: ${e.message}`);
      }
      let events;
      try {
        events = decodeEvents(raw);
      } catch (e) {
        throw srcInvalidResponse(`gbbq record decode failed: ${e.message}`);
      }
      const bySecurity = new Map(); // `${market}|${code}` -> {events, excluded}
      for (const event of events) {
        // Only category 1 has verified QFQ semantics; other categories
        // are never interpreted or applied to prices.
        const key = `${event.market}|${event.code}`;
        let entry = bySecurity.get(key);
        if (entry === undefined) {
          entry = { events: [], excluded: 0 };
          bySecurity.set(key, entry);
        }
        if (event.category === 1) entry.events.push(event);
        else entry.excluded += 1;
      }
      this._gbbq = { member, sha256: sha256hex(bytes), bySecurity };
    }
    return this._gbbq;
  }

  // The security's interpreted event set: category-1 events in decode
  // order plus the count of excluded non-category-1 records.
  securityEvents(market, code) {
    const gbbq = this.decodedGbbq();
    const marketByte = market === "sh" ? 1 : 0;
    const entry = gbbq.bySecurity.get(`${marketByte}|${code}`);
    return entry === undefined ? [[], 0] : [entry.events, entry.excluded];
  }

  // Candidate member names for a security's .day file.
  static dayNames(market, code) {
    return [
      `${market}\\lday\\${market}${code}.day`,
      `${market}/lday/${market}${code}.day`,
    ];
  }

  // Same output as TdxMarketSource::materialize_daily: the .day member
  // is re-read per symbol through the shared archive index while the
  // verified gbbq member bytes are decoded once and shared.
  materializeDaily(symbol) {
    const source = this.source;
    const dot = symbol.indexOf(".");
    const mc = dot < 0 ? null : [symbol.slice(0, dot), symbol.slice(dot + 1)];
    if (mc === null || !(mc[0] === "sh" || mc[0] === "sz") ||
        mc[1].length !== 6 || !/^[0-9]+$/.test(mc[1])) {
      throw srcUnsupported(
        `unsupported symbol ${rustDebugStr(symbol)}; expected sh|sz + six digits`);
    }
    const [market, code] = mc;
    const [dayMember, dayBytes] =
      this.memberBytes(source.day, MaterializeSession.dayNames(market, code));
    let rawBars;
    try {
      rawBars = parseDay(dayBytes);
    } catch (e) {
      throw srcInvalidResponse(`.day member malformed: ${e.message}`);
    }
    if (rawBars.length === 0) {
      throw srcInvalidResponse(".day member carries no records");
    }
    const [events, excluded] = this.securityEvents(market, code);
    const bars = MaterializeSession.qfqBars(rawBars, events);
    return this.assembleDaily(
      symbol,
      { member: dayMember, sha256: sha256hex(dayBytes) },
      bars,
      events,
      excluded,
    );
  }

  // Map verified-raw .day records through the QFQ event chain into
  // publishable bars.
  static qfqBars(rawBars, events) {
    let adjusted;
    try {
      adjusted = forwardAdjust(rawBars, events);
    } catch (e) {
      throw srcInvalidResponse(`qfq chain failed: ${e.message}`);
    }
    return adjusted.map((b) => ({
      date: __parseDate(b.date),
      open: b.open,
      high: b.high,
      low: b.low,
      close: b.close,
      volume: String(b.volume_raw), // Decimal::from(u32)
      amount: amountFromBits(b.amount_float32_bits),
      r7_raw: b.r7_raw,
    }));
  }

  // Build the snapshot-bound batch: the same bars plus bindings covering
  // the .day member, the gbbq member, and this security's decoded event
  // set.
  assembleDaily(symbol, day, bars, events, excluded) {
    const source = this.source;
    const gbbq = this.decodedGbbq();
    const dayNs = parseTs(source.day.acquisition_completed_at).ns2;
    const gbbqNs = parseTs(source.gbbq.acquisition_completed_at).ns2;
    const acquiredAt = dayNs >= gbbqNs
      ? parseTs(source.day.acquisition_completed_at).out
      : parseTs(source.gbbq.acquisition_completed_at).out;
    const dayBinding = {
      role: source.day.role, // serde snake_case — to_value(FileRole).as_str()
      member: day.member,
      archive_sha256: source.day.sha256,
      member_sha256: day.sha256,
    };
    const gbbqBinding = {
      role: source.gbbq.role,
      member: gbbq.member,
      archive_sha256: source.gbbq.sha256,
      member_sha256: gbbq.sha256,
    };
    const eventsBinding = {
      role: SECURITY_EVENTS_ROLE,
      member: `gbbq#${symbol}`,
      archive_sha256: source.gbbq.sha256,
      member_sha256: securityEventsDigest(events, excluded),
    };
    return {
      symbol,
      period: "Day",
      adjustment: "Forward",
      bars,
      provenance: {
        source: SOURCE_NAME,
        source_period: "day",
        source_adjustment: "qfq",
        symbol,
        acquired_at: acquiredAt,
        adapter_version: ADAPTER_VERSION,
        yearly_derivation: null,
        snapshot_inputs: [dayBinding, gbbqBinding, eventsBinding],
      },
      coverage: {
        status: "Complete",
        reason:
          `complete .day member history from inspected snapshot manifest ${source.manifest_sha256}; ${DECLARED_SCOPE}`,
      },
      unit_currency: {
        price_currency: {
          status: "Confirmed", value: "CNY",
          reason: ".day raw OHLC are fen integers of yuan-denominated prices (#166/#167 anchored)",
        },
        price_unit: {
          status: "Confirmed", value: "yuan",
          reason: ".day raw OHLC are fen integers of yuan-denominated prices (#166/#167 anchored)",
        },
        volume_unit: {
          status: "Confirmed", value: "share",
          reason: ".day volume is u32 shares; anchored by #239 TdxQuant oracle (37,345-bar full-history match, PR #242); not adjustment-adjusted",
        },
        amount_currency: {
          status: "Confirmed", value: "CNY",
          reason: ".day amount is f32 yuan; anchored by #239 TdxQuant oracle (37,288/37,345 within tolerance, PR #242); not adjustment-adjusted",
        },
        amount_unit: {
          status: "Confirmed", value: "yuan",
          reason: ".day amount is f32 yuan; anchored by #239 TdxQuant oracle (37,288/37,345 within tolerance, PR #242); not adjustment-adjusted",
        },
      },
      adjustment_evidence: {
        status: "Confirmed", value: "qfq",
        reason:
          `qfq::forward_adjust over category==1 GBBQ affine chain (${events.length} events, ${excluded} non-category-1 records excluded uninterpreted); ${DECLARED_SCOPE}`,
      },
    };
  }
}

// Overlay in-range segment bars onto the fixed base's records. Same rule
// as market_update::prepare (#4 AC-71): filling an unknown optional
// field is enrichment, not a correction — a source bar contradicting a
// known base value is a basis conflict and fails closed.
// `base` is the compat::check() shape {version:{records:[Bar]}}; returns
// a Map keyed by ISO date in ascending order (BTreeMap iteration order).
export function mergeSegmentBars(base, segmentBars) {
  const dateKey = (d) => (typeof d === "string" ? d : d.s);
  const optVal = (v) => (v === undefined ? null : v);
  const merged = new Map();
  for (const b of base.version.records) merged.set(dateKey(b.date), b);
  for (const bar of segmentBars) {
    const key = dateKey(bar.date);
    const existing = merged.get(key);
    if (existing !== undefined) {
      const optChanged = (a, b) => a !== null && b !== null && __cmpDec(a, b) !== 0;
      const optLost = (a, b) => a !== null && b === null;
      const ev = existing, sg = bar;
      const changedKnown =
        __cmpDec(ev.open, sg.open) !== 0 ||
        __cmpDec(ev.high, sg.high) !== 0 ||
        __cmpDec(ev.low, sg.low) !== 0 ||
        __cmpDec(ev.close, sg.close) !== 0 ||
        optChanged(optVal(ev.volume), optVal(sg.volume)) ||
        optChanged(optVal(ev.amount), optVal(sg.amount)) ||
        (optVal(ev.r7_raw) !== null && optVal(sg.r7_raw) !== null && ev.r7_raw !== sg.r7_raw);
      const lostKnown =
        optLost(optVal(ev.volume), optVal(sg.volume)) ||
        optLost(optVal(ev.amount), optVal(sg.amount)) ||
        (optVal(ev.r7_raw) !== null && optVal(sg.r7_raw) === null);
      if (changedKnown || lostKnown) {
        throw srcNotVerified(`source bar ${key} conflicts with the fixed base`);
      }
    }
    merged.set(key, bar);
  }
  // BTreeMap<NaiveDate, Bar> iterates in date order; rebuild sorted.
  return new Map([...merged].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)));
}
