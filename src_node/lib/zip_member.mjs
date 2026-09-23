// Bounded, read-only ZIP member access — production location (hardened
// NM-024 R3 semantics). Reads from an in-memory archive buffer; no
// writes, no persistent state.
import { inflateRawSync } from "node:zlib";

const MAX_MEMBER = 64 * 1024 * 1024; // 64 MiB inflated bound
const SIG_CD = 0x02014b50, SIG_LH = 0x04034b50, SIG_EOCD = 0x06054b50;

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
    const lho = buf.readUInt32LE(off + 42);
    if (off + 46 + nameLen > buf.length) throw new Error("invalid ZIP: name out of bounds");
    const name = buf.subarray(off + 46, off + 46 + nameLen).toString("latin1");
    entries.push({ name, method, compSize, uncompSize, lho });
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
  return out;
}
