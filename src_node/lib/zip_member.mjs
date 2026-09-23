// Bounded, read-only ZIP member access — production location (hardened
// NM-024 R3 semantics). Reads from an in-memory archive buffer; no
// writes, no persistent state.
import { openSync, readSync, closeSync, fstatSync } from "node:fs";
import { inflateRawSync } from "node:zlib";

const MAX_MEMBER = 64 * 1024 * 1024; // 64 MiB inflated bound
const SIG_CD = 0x02014b50, SIG_LH = 0x04034b50, SIG_EOCD = 0x06054b50;

// CRC32 (IEEE) — the zip crate wraps extraction in Crc32Reader and
// rejects a mismatch with the central-directory CRC. We must do the
// same or a manifest-pinned but internally corrupt archive would parse.
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

// Parse EOCD + central directory, validating signatures/bounds/methods.
// Throws Error("invalid ZIP: ...") on structural failure.
export function zipOpen(buf) {
  if (!(buf instanceof Uint8Array)) throw new Error("zip input must be bytes");
  const eocd = buf.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06]));
  if (eocd < 0 || eocd + 22 > buf.length || buf.readUInt32LE(eocd) !== SIG_EOCD) {
    throw new Error("invalid ZIP: EOCD signature not found");
  }
  const cdCount = buf.readUInt16LE(eocd + 10);
  let off = buf.readUInt32LE(eocd + 16);
  if (off + 46 * cdCount > eocd) throw new Error("invalid ZIP: central directory out of bounds");
  const entries = [];
  for (let i = 0; i < cdCount; i++) {
    if (off + 46 > buf.length || buf.readUInt32LE(off) !== SIG_CD) {
      throw new Error(`invalid ZIP: central entry ${i} signature`);
    }
    const method = buf.readUInt16LE(off + 10);
    const compSize = buf.readUInt32LE(off + 20);
    const uncompSize = buf.readUInt32LE(off + 24);
    const nameLen = buf.readUInt16LE(off + 28), extraLen = buf.readUInt16LE(off + 30), cmtLen = buf.readUInt16LE(off + 32);
    const crc = buf.readUInt32LE(off + 16);
    const lho = buf.readUInt32LE(off + 42);
    if (off + 46 + nameLen > buf.length) throw new Error("invalid ZIP: name out of bounds");
    const name = buf.subarray(off + 46, off + 46 + nameLen).toString("latin1");
    entries.push({ name, method, compSize, uncompSize, crc, lho });
    off += 46 + nameLen + extraLen + cmtLen;
  }
  return entries;
}

// Declared uncompressed size for a member; Error("specified file not
// found in archive") when absent (zip crate FileNotFound text).
export function zipMemberSize(buf, name) {
  const e = zipOpen(buf).find((x) => x.name === name);
  if (e === undefined) throw new Error("specified file not found in archive");
  return e.uncompSize;
}

export function zipMember(buf, name, maxOut = MAX_MEMBER) {
  const entries = zipOpen(buf);
  const e = entries.find((x) => x.name === name);
  if (e === undefined) throw new Error("specified file not found in archive");
  if (e.method !== 0 && e.method !== 8) throw new Error(`unsupported ZIP method ${e.method}`);
  if (e.uncompSize > maxOut) throw new Error(`member exceeds bound: ${e.uncompSize}`);
  if (e.lho + 30 > buf.length || buf.readUInt32LE(e.lho) !== SIG_LH) {
    throw new Error("invalid ZIP: local header signature");
  }
  const lNameLen = buf.readUInt16LE(e.lho + 26), lExtraLen = buf.readUInt16LE(e.lho + 28);
  const dataOff = e.lho + 30 + lNameLen + lExtraLen;
  if (dataOff + e.compSize > buf.length) throw new Error("invalid ZIP: data out of bounds");
  const data = buf.subarray(dataOff, dataOff + e.compSize);
  const out = e.method === 0 ? Buffer.from(data) : inflateRawSync(data, { maxOutputLength: maxOut });
  if (out.length !== e.uncompSize) {
    throw new Error(`size mismatch: declared ${e.uncompSize} got ${out.length}`);
  }
  // zip crate Crc32Reader failure text is "Invalid checksum" — kept
  // verbatim so member() wraps it as `unreadable: Invalid checksum`.
  if (crc32(out) !== e.crc) throw new Error("Invalid checksum");
  return out;
}

// Bounded fd read with deterministic premature-EOF rejection: reads
// exactly `maxBytes` (bound from fstat after open); a 0-length read
// before that is a hard error (a readSync==0 spin would hang). A file
// that grew after fstat contributes only its declared prefix — callers
// digest-check or parse-fail downstream, staying fail-closed.
export function readBoundedFile(path, maxBytes) {
  const fd = openSync(path, "r");
  try {
    const size = fstatSync(fd).size;
    if (size > maxBytes) throw new Error(`file exceeds declared bound: ${size}`);
    const buf = Buffer.allocUnsafe(size);
    let off = 0;
    while (off < size) {
      const n = readSync(fd, buf, off, size - off, off);
      if (n === 0) throw new Error(`unexpected EOF at ${off} of ${size}`);
      off += n;
    }
    return buf;
  } finally { closeSync(fd); }
}
