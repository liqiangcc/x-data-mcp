// NM-052 — market staging write path (legacy schema-18): envelope::pack,
// write_payloads, manifest::build (AVL write side), write_day_segments,
// insert_record, stage(). Verbatim port of src/sqlite_market.rs write
// helpers + envelope.rs pack side + manifest.rs build side.
import { createHash } from "node:crypto";
import { serValue, parseJsonValue } from "../mcp/serde_json.mjs";
import { gateDatabase } from "../storage/schema_gate.mjs";
import { openDatabase, StorageError } from "../storage/sqlite_adapter.mjs";
import * as RC from "./record_codec.mjs";
import * as MR from "./market_read.mjs";

const sha = (s) => createHash("sha256").update(s).digest("hex");
const integrity = (m) => new RC.MarketError("integrity", `MARKET_INTEGRITY: ${m}`);
const ser = RC.__ser;

// ---------------- envelope::pack ----------------
const refPayload = (id) => ({ $ref: `payload:${id}` });
const refAnnualBars = (id) => ({ $ref: `annual_bars:${id}` });
const arrayPayload = (v) => {
  if (!Array.isArray(v)) throw integrity("envelope payload is not an array");
  const json = ser(v);
  return { id: sha(json), json };
};
const valuePayload = (v) => {
  const json = ser(v);
  return { id: sha(json), json };
};
const packShared = (batch, out) => {
  for (const key of ["unit_currency", "coverage", "adjustment_evidence"]) {
    const v = batch[key];
    if (v === null || v === undefined || (typeof v === "object" && !Array.isArray(v) && "$ref" in v)) {
      continue;
    }
    const p = valuePayload(v);
    batch[key] = refPayload(p.id);
    out.push(p);
  }
};
const packDaily = (raw, marker) => {
  const env = parseJsonValue(serValue(raw));
  const out = [];
  env.records = marker;
  const bars = raw.batch.bars;
  env.batch.bars = RC.__jsonEq(bars, raw.records)
    ? { $ref: "records" }
    : (() => { const p = arrayPayload(bars); out.push(p); return refPayload(p.id); })();
  packShared(env.batch, out);
  return [env, out];
};
const packYearly = (raw) => {
  const env = parseJsonValue(serValue(raw));
  const out = [];
  let annualBars = null;
  const annual = raw.local_yearly_derivation_v1?.annual?.records;
  if (Array.isArray(annual)) {
    const p = arrayPayload(annual);
    env.local_yearly_derivation_v1.annual.records = refPayload(p.id);
    annualBars = annual.map((e) => e.bar);
    out.push(p);
    env.records = RC.__jsonEq(raw.records, annualBars)
      ? refAnnualBars(p.id)
      : (() => { const p2 = arrayPayload(raw.records); out.push(p2); return refPayload(p2.id); })();
  } else {
    const p = arrayPayload(raw.records);
    out.push(p);
    env.records = refPayload(p.id);
  }
  const bars = raw.batch.bars;
  env.batch.bars = RC.__jsonEq(bars, raw.records)
    ? { $ref: "records" }
    : annualBars !== null && RC.__jsonEq(bars, annualBars)
      ? refAnnualBars(out[0].id)
      : (() => { const p = arrayPayload(bars); out.push(p); return refPayload(p.id); })();
  packShared(env.batch, out);
  return [env, out];
};
export const pack = (raw) =>
  raw.period === "Day" ? packDaily(raw, { $ref: "segments" }) : packYearly(raw);
export const writePayloads = (db, payloads) => {
  for (const p of payloads) {
    db.run("INSERT OR IGNORE INTO payloads VALUES(?,?)", p.id, p.json);
  }
};

// ---------------- manifest build (write side) ----------------
const mPut = (db, n) => {
  const r = db.run(
    "INSERT INTO manifest_nodes VALUES(NULL,?,?,?,?,?,?,?,?,?)",
    n.left, n.right, n.segment, n.clip_start, n.clip_end, n.first, n.last, n.count, n.height);
  return r.lastInsertRowid;
};
const leaf = (db, segment, start, end, count) => {
  if (count <= 0) throw integrity("manifest leaf is empty");
  return mPut(db, { left: null, right: null, segment, clip_start: start, clip_end: end,
    first: start, last: end, count: BigInt(count), height: 1n });
};
const mk = (db, left, right) => {
  const l = nodeGet(db, left), r = nodeGet(db, right);
  return mPut(db, { left, right, segment: null, clip_start: null, clip_end: null,
    first: l.first_date, last: r.last_date,
    count: l.row_count + r.row_count,
    height: (l.height > r.height ? l.height : r.height) + 1n });
};
const rebalance = (db, l, r) => {
  const ln = nodeGet(db, l), rn = nodeGet(db, r);
  if (ln.height > rn.height + 1n) {
    const ll = ln.left_id ?? (() => { throw integrity("manifest node malformed"); })();
    const lr = ln.right_id ?? (() => { throw integrity("manifest node malformed"); })();
    const lrn = nodeGet(db, lr);
    if (lrn.height > nodeGet(db, ll).height) {
      const lrl = lrn.left_id ?? (() => { throw integrity("manifest node malformed"); })();
      const lrr = lrn.right_id ?? (() => { throw integrity("manifest node malformed"); })();
      const a = mk(db, ll, lrl), b = mk(db, lrr, r);
      return mk(db, a, b);
    }
    const b = mk(db, lr, r);
    return mk(db, ll, b);
  }
  if (rn.height > ln.height + 1n) {
    const rl = rn.left_id ?? (() => { throw integrity("manifest node malformed"); })();
    const rr = rn.right_id ?? (() => { throw integrity("manifest node malformed"); })();
    const rln = nodeGet(db, rl);
    if (rln.height > nodeGet(db, rr).height) {
      const rll = rln.left_id ?? (() => { throw integrity("manifest node malformed"); })();
      const rlr = rln.right_id ?? (() => { throw integrity("manifest node malformed"); })();
      const a = mk(db, l, rll), b = mk(db, rlr, rr);
      return mk(db, a, b);
    }
    const a = mk(db, l, rl), b = mk(db, a, rr);
    return mk(db, a, b);
  }
  return mk(db, l, r);
};
const concat = (db, l, r) => {
  const ln = nodeGet(db, l), rn = nodeGet(db, r);
  if (ln.height > rn.height + 1n) {
    const right = concat(db,
      ln.right_id ?? (() => { throw integrity("manifest node malformed"); })(), r);
    return rebalance(db,
      ln.left_id ?? (() => { throw integrity("manifest node malformed"); })(), right);
  }
  if (rn.height > ln.height + 1n) {
    const left = concat(db, l,
      rn.left_id ?? (() => { throw integrity("manifest node malformed"); })());
    return rebalance(db, left,
      rn.right_id ?? (() => { throw integrity("manifest node malformed"); })());
  }
  return mk(db, l, r);
};
const join = (db, l, r) =>
  l !== null && r !== null ? concat(db, l, r) : (l ?? r);
const slice = (db, root, lo, hi) => {
  const n = nodeGet(db, root);
  if (lo > hi || lo < 0n || hi >= n.row_count) throw integrity("manifest slice range out of bounds");
  if (lo === 0n && hi === n.row_count - 1n) return root;
  if (n.segment_id !== null) {
    const seg = n.segment_id;
    const cs = n.clip_start ?? (() => { throw integrity("manifest leaf malformed"); })();
    const ce = n.clip_end ?? (() => { throw integrity("manifest leaf malformed"); })();
    const dateAt = (off) => {
      const row = db.get(
        "SELECT trade_date FROM segment_bars WHERE segment_id=? AND trade_date>=? AND trade_date<=? ORDER BY trade_date LIMIT 1 OFFSET ?",
        seg, cs, ce, off);
      if (!row) throw integrity("manifest clip length mismatch");
      return row.trade_date;
    };
    return leaf(db, seg, dateAt(lo), dateAt(hi), hi - lo + 1n);
  }
  const l = n.left_id ?? (() => { throw integrity("manifest node malformed"); })();
  const r = n.right_id ?? (() => { throw integrity("manifest node malformed"); })();
  const lc = nodeGet(db, l).row_count;
  const a = lo < lc ? slice(db, l, lo, hi < lc - 1n ? hi : lc - 1n) : null;
  const b = hi >= lc ? slice(db, r, lo > lc ? lo - lc : 0n, hi - lc) : null;
  if (a !== null && b === null) return a;
  if (a === null && b !== null) return b;
  if (a !== null && b !== null) return concat(db, a, b);
  throw integrity("manifest slice empty");
};
const buildBalanced = (db, leaves) => {
  if (leaves.length === 0) return null;
  if (leaves.length === 1) return leaves[0];
  const h = leaves.length >> 1;
  return join(db, buildBalanced(db, leaves.slice(0, h)), buildBalanced(db, leaves.slice(h)));
};
// nodeGet lives in market_read (same shape); rebind for write-side use
const nodeGet = (db, id) => MR.__nodeGet(db, id);
export const manifestBuild = (db, baseRoot, pieces) => {
  const trees = [];
  for (const piece of pieces) {
    if (piece.kind === "Base") {
      const root = baseRoot ?? (() => { throw integrity("manifest base absent"); })();
      trees.push(slice(db, root, BigInt(piece.first), BigInt(piece.last)));
    } else {
      trees.push(leaf(db, piece.segment, piece.first_date, piece.last_date, piece.count));
    }
  }
  const root = buildBalanced(db, trees);
  if (root === null) throw integrity("manifest empty");
  return root;
};

// ---------------- insert_record ----------------
const writeDaySegments = (db, r) => {
  const v = r.version;
  const scope = { symbol: v.symbol, period: v.period };
  const headId = MR.head(db, scope);
  let base = [];
  let baseRoot = null;
  if (headId !== null && headId !== v.version_id) {
    baseRoot = MR.rootOf(db, headId);
    if (baseRoot !== null) {
      base = MR.manifestBars(db, baseRoot).map((b) => [b.date ?? "", ser(b)]);
    }
  }
  const baseIndex = new Map(base.map((row, i) => [row[0], i]));
  const shareable = (b) => {
    const idx = baseIndex.get(b[0]);
    return idx !== undefined && base[idx][1] === b[1] ? idx : null;
  };
  const pieces = [];
  const pushNew = (first, last) => {
    const p = pieces[pieces.length - 1];
    if (p && p.kind === "New" && p.last + 1 === first) p.last = last;
    else pieces.push({ kind: "New", first, last });
  };
  if (!Array.isArray(r.raw.records)) throw integrity("record lacks records array");
  const barJsons = r.raw.records.map((b) => [b.date ?? "", ser(b)]);
  if (barJsons.length !== v.records.length) throw integrity("record/records array mismatch");
  let i = 0;
  while (i < barJsons.length) {
    const b = barJsons[i];
    const idx = shareable(b);
    if (idx !== null) {
      let last = idx, j = i + 1;
      while (j < barJsons.length) {
        const nidx = shareable(barJsons[j]);
        if (nidx !== null && nidx === last + 1) { last = nidx; j++; } else break;
      }
      pieces.push({ kind: "Base", first: idx, last });
      i = j;
    } else {
      pushNew(i, i);
      i++;
    }
  }
  let nextSegment = BigInt(db.get("SELECT COALESCE(MAX(segment_id),0)+1 AS x FROM bar_segments").x);
  const ops = [];
  for (const piece of pieces) {
    if (piece.kind === "Base") {
      ops.push({ kind: "Base", first: piece.first, last: piece.last });
    } else {
      const rows = barJsons.slice(piece.first, piece.last + 1);
      const segmentId = nextSegment++;
      db.run("INSERT INTO bar_segments VALUES(?,?,?,?,?)",
        segmentId, v.symbol, rows[0][0], rows[rows.length - 1][0], rows.length);
      for (const [date, json] of rows) {
        db.run("INSERT INTO segment_bars VALUES(?,?,?)", segmentId, date, json);
      }
      ops.push({ kind: "New", segment: segmentId,
        first_date: rows[0][0], last_date: rows[rows.length - 1][0], count: rows.length });
    }
  }
  const root = manifestBuild(db, baseRoot, ops);
  db.run("UPDATE data_versions SET manifest_root=? WHERE version_id=?", root, v.version_id);
};

export const insertRecord = (db, r, base) => {
  const v = r.version;
  const m = MR.__metadata(r);
  const [env, payloads] = pack(r.raw);
  writePayloads(db, payloads);
  const evDigest = sha(serValue(MR.__evidenceRows(r)));
  db.run(
    "INSERT INTO data_versions(version_id,symbol,period,adjustment,state,source,published_at,record_count,unique_count,duplicate_count,coverage_status,coverage_reason,first_date,last_date,envelope_json,content_digest,digest_format,evidence_digest) VALUES(?,?,?,?,'staging',?,?,?,?,?,?,?,?,?,?,?,?,?)",
    v.version_id, m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8], m[9], m[10], m[11],
    ser(env), r.raw.integrity_sha256, RC.DIGEST_FORMAT, evDigest);
  if (v.period === "Day") {
    writeDaySegments(db, r);
  } else {
    if (base === null) throw new RC.MarketError("base", "BASE_UNAVAILABLE: missing base");
    for (const b of MR.__annualValues(r, base)) {
      db.run("INSERT INTO yearly_bars VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
        v.version_id, ...b);
    }
    const l = MR.__lineageValues(r);
    db.run("INSERT INTO yearly_lineage VALUES(?,?,?,?,?,?)", v.version_id, ...l);
  }
};

// Explicit BEGIN IMMEDIATE/COMMIT — Rust `write_transaction` uses
// TransactionBehavior::Immediate; inner MarketError must propagate
// un-wrapped (the adapter's transaction() re-maps every throw).
export function stage(db, raw) {
  const record = RC.check(raw);
  db.run("BEGIN IMMEDIATE");
  try {
    const base = MR.__baseRecord(db, record);
    MR.__validateBaseIdentity(record, base);
    insertRecord(db, record, base);
    db.run("COMMIT");
  } catch (e) {
    try { db.run("ROLLBACK"); } catch { /* already rolled back */ }
    if (e instanceof StorageError) {
      throw new RC.MarketError("storage", `MARKET_STORAGE: ${e.message}`);
    }
    throw e;
  }
}

/** Gated readwrite writer facade (staging only — no publish commit). */
export const MarketWriter = {
  open(path) {
    gateDatabase(path);
    const db = openDatabase(path, { mode: "readwrite" });
    return {
      db,
      stage: (raw) => stage(db, raw),
      close: () => db.close(),
    };
  },
};
