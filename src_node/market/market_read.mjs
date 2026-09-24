// NM-051 — legacy (schema-18) market read path: head / read_projection /
// envelope::expand / base_record / validate_base / range / versions /
// candidate / fixed. Verbatim port of src/sqlite_market.rs read helpers.
import { createHash } from "node:crypto";
import { gateDatabase } from "../storage/schema_gate.mjs";
import { openDatabase, StorageError } from "../storage/sqlite_adapter.mjs";
import * as RC from "./record_codec.mjs";
import * as C from "./compact_read.mjs";
import * as CW from "./compact_write.mjs";
import { aggregate as yearlyAggregate } from "../algorithms/yearly.mjs";
import { _dec } from "../algorithms/qfq_affine.mjs";

const sha = (s) => createHash("sha256").update(s).digest("hex");
const integrity = RC.__integrity ?? ((m) => new RC.MarketError("integrity", `MARKET_INTEGRITY: ${m}`));
const invalid = (m) => new RC.MarketError("invalid", `INVALID_MARKET_COMMAND: ${m}`);
const baseErr = (m) => new RC.MarketError("base", `BASE_UNAVAILABLE: ${m}`);
const notFound = () => new RC.MarketError("not_found", "MARKET_NOT_FOUND");
const storageErr = (m) => new RC.MarketError("storage", `MARKET_STORAGE: ${m}`);
const unsupportedCompact = () =>
  new RC.MarketError("invalid", "compact market schema dispatch is not ported (NM-051 boundary)");

export const isCompactSchema = (db) =>
  db.get("SELECT NOT EXISTS(SELECT 1 FROM sqlite_master WHERE name='publication_batches') AS x").x === 1n;

const periodName = (p) => p === "Day" ? "day" : "year";

// Decimal::normalize().to_string() — accepts decimal text or a
// compact Dec {m:bigint,s:int} (compact read path emits typed Decs).
const decStr = (v) => {
  if (v !== null && typeof v === "object" && typeof v.m === "bigint") {
    let { m, s } = v;
    while (s > 0 && m % 10n === 0n) { m /= 10n; s -= 1; }
    if (m === 0n) return "0";
    return _dec.toString({ m, s });
  }
  const n = RC.__decNorm(v);
  return (n.neg ? "-" : "") + n.int + (n.frac ? `.${n.frac}` : "");
};

// chrono to_rfc3339(): UTC form "+00:00" (AutoSi fractional)
const toRfc3339 = (s) => {
  const m = /^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(\.\d+)?(Z|[+-]\d{2}:\d{2})$/.exec(s);
  if (!m) throw integrity(`invalid timestamp ${JSON.stringify(s)}`);
  const [, base, fracRaw, off] = m;
  let frac = fracRaw ? fracRaw.slice(1) : "";
  if (off !== "Z" && off !== "+00:00") {
    // shift to UTC
    const sign = off[0] === "+" ? 1 : -1;
    const [oh, om] = off.slice(1).split(":").map(Number);
    const ms = Date.parse(`${base}Z`) - sign * (oh * 3600 + om * 60) * 1000;
    const d = new Date(ms);
    return `${d.toISOString().slice(0, 19)}${frac ? `.${frac.slice(0, 9).padEnd(3, "0")}` : ""}+00:00`
      .replace(/\.\d+?0+$/, (x) => x); // keep as-is; AutoSi handled below
  }
  // AutoSi: 0 -> none, %1e6==0 -> 3 digits, %1e3==0 -> 6, else 9
  if (frac) {
    let f = frac.slice(0, 9).padEnd(9, "0");
    const n = Number(f);
    frac = n === 0 ? "" : n % 1000000 === 0 ? `.${f.slice(0, 3)}`
      : n % 1000 === 0 ? `.${f.slice(0, 6)}` : `.${f}`;
  }
  return `${base}${frac}+00:00`;
};
// published_at (+08:00) calendar date — as_of
const asOfDate = (publishedAt) => {
  const ms = Date.parse(publishedAt) + 8 * 3600 * 1000;
  return new Date(ms).toISOString().slice(0, 10);
};

export function head(db, scope) {
  if (isCompactSchema(db)) return C.headId(db, scope);
  return db.get(
    "SELECT version_id FROM effective_heads WHERE symbol=? AND period=? AND adjustment='qfq'",
    scope.symbol, periodName(scope.period))?.version_id ?? null;
}

// ---------------- envelope::expand ----------------
const fetchPayload = (db, id) => {
  const row = db.get("SELECT payload_json FROM payloads WHERE payload_id=?", id);
  if (!row) throw integrity("referenced payload missing");
  const json = row.payload_json;
  if (sha(json) !== id) throw integrity("payload content digest mismatch");
  return JSON.parse(json);
};
const resolve = (db, versionId, v, records) => {
  const r = v !== null && typeof v === "object" && !Array.isArray(v) ? v.$ref : undefined;
  if (r === undefined) return v;
  if (r === "records") {
    if (records === undefined) throw integrity("envelope records reference out of context");
    return records;
  }
  if (r === "segments") {
    if (versionId === null) throw integrity("envelope segments reference out of context");
    const root = rootOf(db, versionId);
    if (root === null) throw integrity("segment-backed version lacks manifest");
    return manifestBars(db, root);
  }
  if (r.startsWith("payload:")) return fetchPayload(db, r.slice(8));
  if (r.startsWith("annual_bars:")) {
    const entries = fetchPayload(db, r.slice(12));
    if (!Array.isArray(entries)) throw integrity("annual payload is not an array");
    return entries.map((e) => e.bar);
  }
  throw integrity("unknown envelope reference");
};
const inflateRef = (db, v) => {
  const r = v !== null && typeof v === "object" && !Array.isArray(v) ? v.$ref : undefined;
  if (r === undefined) return v;
  if (!r.startsWith("payload:")) throw integrity("unexpected shared-evidence reference");
  return fetchPayload(db, r.slice(8));
};
export const expand = (db, versionId, env) => {
  const raw = JSON.parse(JSON.stringify(env));
  const records = resolve(db, versionId, raw.records, undefined);
  raw.records = records;
  if (raw.batch && "bars" in raw.batch) {
    raw.batch.bars = resolve(db, versionId, raw.batch.bars, records);
  }
  for (const key of ["unit_currency", "coverage", "adjustment_evidence"]) {
    if (raw.batch && key in raw.batch) raw.batch[key] = inflateRef(db, raw.batch[key]);
  }
  if (raw.local_yearly_derivation_v1?.annual?.records !== undefined) {
    raw.local_yearly_derivation_v1.annual.records =
      resolve(db, versionId, raw.local_yearly_derivation_v1.annual.records, records);
  }
  return raw;
};

// ---------------- manifest read side ----------------
const nodeGet = (db, id) => {
  const n = db.get(
    "SELECT left_id,right_id,segment_id,clip_start,clip_end,first_date,last_date,row_count,height FROM manifest_nodes WHERE node_id=?",
    id);
  if (!n) throw integrity("manifest node missing");
  const leaf = n.segment_id !== null;
  if (leaf === (n.left_id !== null || n.right_id !== null) ||
      (!leaf && (n.left_id === null || n.right_id === null)) ||
      (leaf && (n.clip_start === null || n.clip_end === null))) {
    throw integrity("manifest node malformed");
  }
  return n;
};
export const rootOf = (db, versionId) =>
  db.get("SELECT manifest_root FROM data_versions WHERE version_id=?", versionId)?.manifest_root ?? null;
export const manifestBars = (db, root) => {
  const out = [];
  const stack = [root];
  const order = [];
  while (stack.length) {
    const n = nodeGet(db, stack.pop());
    if (n.segment_id !== null) order.push(n);
    else {
      stack.push(n.right_id ?? (() => { throw integrity("manifest node malformed"); })());
      stack.push(n.left_id ?? (() => { throw integrity("manifest node malformed"); })());
    }
  }
  for (const n of order) {
    const rows = db.query(
      "SELECT bar_json FROM segment_bars WHERE segment_id=? AND trade_date>=? AND trade_date<=? ORDER BY trade_date",
      n.segment_id, n.clip_start, n.clip_end);
    if (BigInt(rows.length) !== n.row_count) throw integrity("manifest clip length mismatch");
    for (const r of rows) out.push(JSON.parse(r.bar_json));
  }
  return out;
};
const clipsInRange = (db, root, start, end) => {
  const out = [];
  const stack = [root];
  while (stack.length) {
    const n = nodeGet(db, stack.pop());
    if (n.last_date < start || n.first_date > end) continue;
    if (n.segment_id !== null) {
      if (n.clip_start === null || n.clip_end === null) throw integrity("manifest leaf malformed");
      out.push([n.segment_id, n.clip_start > start ? n.clip_start : start,
        n.clip_end < end ? n.clip_end : end]);
    } else {
      stack.push(n.right_id ?? (() => { throw integrity("manifest node malformed"); })());
      stack.push(n.left_id ?? (() => { throw integrity("manifest node malformed"); })());
    }
  }
  return out;
};

// ---------------- projections ----------------
const metadata = (r) => [
  r.version.symbol, periodName(r.version.period), "qfq", r.version.source,
  toRfc3339(r.version.published_at),
  String(r.version.validation.record_count),
  String(r.version.validation.unique_record_count),
  String(r.version.validation.duplicate_count),
  r.batch.coverage.status, r.batch.coverage.reason,
  r.version.records[0].date.s,
  r.version.records[r.version.records.length - 1].date.s,
];
const evidenceRows = (r) => {
  const u = r.version.unit_currency;
  const rows = [];
  for (const [field, e] of [
    ["price_currency", u.price_currency], ["price_unit", u.price_unit],
    ["volume_unit", u.volume_unit], ["amount_currency", u.amount_currency],
    ["amount_unit", u.amount_unit],
  ]) rows.push([field, JSON.stringify(e.status), e.value, e.reason]);
  const e = r.version.adjustment_evidence;
  rows.push(["adjustment", JSON.stringify(e.status), e.value, e.reason]);
  rows.sort((a, b) => {
    for (let i = 0; i < 4; i++) {
      if (a[i] === null) { if (b[i] !== null) return -1; continue; }
      if (b[i] === null) return 1;
      if (a[i] !== b[i]) return a[i] < b[i] ? -1 : 1;
    }
    return 0;
  });
  return rows;
};
const barValues = (b) => [
  b.date.s, decStr(b.open), decStr(b.high), decStr(b.low), decStr(b.close),
  b.volume === null ? null : decStr(b.volume),
  b.amount === null ? null : decStr(b.amount),
];
const annualValues = (r, base) =>
  r.version.records.map((bar) => {
    const dates = base.version.records.filter((b) => b.date.y === bar.date.y).map((b) => b.date);
    if (!dates.length) throw baseErr("year absent from fixed base");
    const first = dates[0], last = dates[dates.length - 1];
    return [String(bar.date.y), ...barValues(bar), first.s, last.s,
      String(bar.date.y < Number(asOfDate(r.version.published_at).slice(0, 4)) ? 1 : 0)];
  });
const lineageValues = (r) => {
  const l = r.version.yearly_derivation;
  if (!l) return null;
  return [l.base_daily_version_id, l.algorithm_version,
    l.first_trading_date.s, l.last_trading_date.s, asOfDate(r.version.published_at)];
};

const baseRecord = (db, r) =>
  r.version.yearly_derivation === null ? null
    : (() => {
      try { return readProjection(db, r.version.yearly_derivation.base_daily_version_id, true, false); }
      catch (e) { throw baseErr(e.message); }
    })();

const validateBaseIdentity = (record, base) => {
  const v = record.version;
  if (v.period === "Day") return;
  const lineage = v.yearly_derivation ?? (() => { throw baseErr("yearly lineage missing"); })();
  const daily = (base ?? (() => { throw baseErr("fixed base missing"); })()).version;
  const mismatch = () => { throw baseErr("fixed base identity/period/adjustment/source mismatch"); };
  if (daily.version_id !== lineage.base_daily_version_id || daily.symbol !== v.symbol ||
      daily.period !== "Day" || daily.adjustment !== v.adjustment || daily.source !== v.source ||
      !RC.__ucEq(daily.unit_currency, v.unit_currency) ||
      !(daily.adjustment_evidence.status === "Confirmed" && daily.adjustment_evidence.value === "qfq")) {
    mismatch();
  }
};

// read_projection's recursive base check uses validate_base_identity only;
// the outer read_record then runs the full validate_base aggregation.
const checkPublicationLink = (db, version) => {
  const row = db.get(
    "SELECT b.publication_id,b.command_json,b.receipt_json,b.task_id,b.item_id FROM publication_batches b JOIN task_publications p ON p.publication_id=b.publication_id AND p.task_id=b.task_id AND p.version_id=b.version_id WHERE b.version_id=?",
    version);
  if (!row) throw integrity("published version lacks atomic publication evidence");
  const command = parsePublication(JSON.parse(row.command_json));
  const receipt = parseReceipt(JSON.parse(row.receipt_json));
  if (command.publication_id !== row.publication_id ||
      command.version_id !== version || command.task_id !== row.task_id ||
      command.item_id !== row.item_id ||
      receipt.publication_id !== row.publication_id ||
      receipt.version_id !== version || receipt.task_id !== row.task_id) {
    throw integrity("publication association mismatch");
  }
};

export function readProjection(db, id, published, allowYearly) {
  if (isCompactSchema(db)) throw unsupportedCompact();
  const row = db.get(
    "SELECT envelope_json,content_digest,digest_format,state,evidence_digest FROM data_versions WHERE version_id=?",
    id);
  const publicationExists = db.get(
    "SELECT EXISTS(SELECT 1 FROM publication_batches WHERE version_id=?) AS x", id).x === 1n;
  if (!row) {
    throw publicationExists ? integrity("published version record missing") : notFound();
  }
  if (published && row.state !== "published") {
    throw publicationExists ? integrity("published version state changed") : notFound();
  }
  let env;
  try { env = JSON.parse(row.envelope_json); } catch (e) { throw integrity(e.message); }
  const raw = expand(db, id, env);
  const record = RC.check(raw); // throws integrity verbatim
  if (record.version.version_id !== id ||
      row.content_digest !== (raw.integrity_sha256 ?? "") ||
      row.digest_format !== RC.DIGEST_FORMAT) {
    throw integrity("compat/version identity");
  }
  if (!allowYearly && record.version.period !== "Day") {
    throw baseErr("base period is not daily");
  }
  if (row.state === "published") checkPublicationLink(db, id);
  const base = baseRecord(db, record);
  validateBaseIdentity(record, base);
  const actual = db.query(
    "SELECT symbol,period,adjustment,source,published_at,CAST(record_count AS TEXT),CAST(unique_count AS TEXT),CAST(duplicate_count AS TEXT),coverage_status,coverage_reason,first_date,last_date FROM data_versions WHERE version_id=?",
    id).map((r) => Object.values(r).map((x) => x === null ? null : String(x)));
  if (JSON.stringify(actual) !== JSON.stringify([metadata(record)])) {
    throw integrity("version metadata projection");
  }
  const evDigest = sha(JSON.stringify(evidenceRows(record)));
  if (row.evidence_digest !== evDigest) throw integrity("unit/adjustment projection");
  const yearly = db.query(
    "SELECT CAST(year AS TEXT) AS y,trade_date,open,high,low,close,volume,amount,first_trade_date,last_trade_date,CAST(is_finished AS TEXT) AS f FROM yearly_bars WHERE version_id=? ORDER BY year",
    id).map((r) => [r.y, r.trade_date, r.open, r.high, r.low, r.close, r.volume, r.amount, r.first_trade_date, r.last_trade_date, r.f]);
  const lineage = db.query(
    "SELECT base_daily_version_id,algorithm_version,first_trade_date,last_trade_date,as_of FROM yearly_lineage WHERE version_id=?",
    id).map((r) => [r.base_daily_version_id, r.algorithm_version, r.first_trade_date, r.last_trade_date, r.as_of]);
  if (record.version.period === "Day") {
    if (yearly.length || lineage.length) throw integrity("daily projection");
  } else if (
    JSON.stringify(yearly) !== JSON.stringify(annualValues(record, base)) ||
    JSON.stringify(lineage) !== JSON.stringify([lineageValues(record)])
  ) {
    throw integrity("yearly/lineage projection");
  }
  return record;
}

const validateBase = (record, base) => {
  validateBaseIdentity(record, base);
  const v = record.version;
  if (v.period === "Day") return;
  const lineage = v.yearly_derivation;
  const daily = base.version;
  const years = [...new Set(v.records.map((b) => b.date.y))].sort((a, b) => a - b);
  const asOf = asOfDate(v.published_at);
  // project into the aggregate port's expected shape (Dec + lowercase period)
  const toDec = (x) => _dec.fromStr(String(x));
  const dailyView = {
    period: "day", yearly_derivation: null, version_id: daily.version_id,
    symbol: daily.symbol, source: daily.source,
    adjustment_evidence: {
      confirmed: daily.adjustment_evidence.status === "Confirmed",
      value: daily.adjustment_evidence.value,
    },
    unit_currency: Object.fromEntries(
      ["price_currency", "price_unit", "volume_unit", "amount_currency", "amount_unit"]
        .map((k) => [k, {
          confirmed: daily.unit_currency[k].status === "Confirmed",
          value: daily.unit_currency[k].value,
        }])),
    records: daily.records.map((b) => ({
      date: b.date.s, open: toDec(b.open), high: toDec(b.high),
      low: toDec(b.low), close: toDec(b.close),
      volume: b.volume === null ? null : toDec(b.volume),
      amount: b.amount === null ? null : toDec(b.amount),
    })),
  };
  let annual;
  try { annual = yearlyAggregate(dailyView, years, asOf); }
  catch (e) { throw baseErr(e.message); }
  const decStrOf = (v) =>
    v !== null && typeof v === "object" && typeof v.m === "bigint" ? _dec.toString(v) : v;
  const decEq = (a, b) => RC.__cmpDec(decStrOf(a), decStrOf(b)) === 0;
  const barEqAgg = (x, y) =>
    x.date.s === y.date.s && decEq(x.open, y.open) && decEq(x.high, y.high) &&
    decEq(x.low, y.low) && decEq(x.close, y.close) &&
    (x.volume === null ? y.volume === null : y.volume !== null && decEq(x.volume, y.volume)) &&
    (x.amount === null ? y.amount === null : y.amount !== null && decEq(x.amount, y.amount)) &&
    x.r7_raw === (y.r7_raw ?? null);
  const aggBars = annual.records.map((r) => r.bar);
  if (aggBars.length !== v.records.length ||
      !aggBars.every((ab, i) => barEqAgg({
        date: { s: ab.date }, open: ab.open, high: ab.high, low: ab.low, close: ab.close,
        volume: ab.volume ?? null, amount: ab.amount ?? null, r7_raw: null,
      }, v.records[i])) ||
      lineage.algorithm_version !== "local-natural-year-v1" ||
      annual.records[0]?.lineage.first_trading_date !== lineage.first_trading_date.s ||
      annual.records[annual.records.length - 1]?.lineage.last_trading_date !== lineage.last_trading_date.s) {
    throw baseErr("yearly content/lineage does not match fixed daily");
  }
};

export function readRecord(db, id, published, allowYearly) {
  if (isCompactSchema(db)) {
    const record = C.materializeChecked(db, id, !published);
    if (!allowYearly && record.version.period !== "Day") {
      throw integrity("yearly record in daily context");
    }
    return record;
  }
  const record = readProjection(db, id, published, allowYearly);
  const base = baseRecord(db, record);
  validateBase(record, base);
  return record;
}

// ---------------- range ----------------
export const validateRange = (range) => {
  const s = RC.__parseDate(range.start), e = RC.__parseDate(range.end);
  if (s.y <= 0 || e.y > 9999 || s.s > e.s || range.limit === 0 || range.limit > 1000) {
    throw invalid("date range/page size (1..1000)");
  }
};

const parseBarJson = (j) => {
  const b = JSON.parse(j);
  delete b.r7_raw; // internal evidence, not part of the range wire value
  return b;
};

export function rangeRows(db, id, p, range) {
  if (isCompactSchema(db)) return C.compactRangeRows(db, id, p, range);
  if (p === "Day") {
    const segBacked = db.get(
      "SELECT json_extract(envelope_json,'$.records.$ref')='segments' AS x FROM data_versions WHERE version_id=?",
      id);
    if (!segBacked) throw notFound();
    let jsons;
    if (segBacked.x === 1n) {
      const root = rootOf(db, id);
      if (root === null) throw integrity("segment-backed version lacks manifest");
      const clips = clipsInRange(db, root, range.start, range.end);
      const out = [];
      let remaining = range.limit + 1;
      for (const [seg, lo, hi] of clips) {
        if (remaining === 0) break;
        const rows = db.query(
          "SELECT bar_json FROM segment_bars WHERE segment_id=? AND trade_date>=? AND trade_date<=? AND (? IS NULL OR trade_date>?) ORDER BY trade_date LIMIT ?",
          seg, lo, hi, range.after ?? null, range.after ?? null, remaining);
        remaining -= rows.length;
        for (const r of rows) out.push(r.bar_json);
      }
      jsons = out;
    } else {
      const envRow = db.get("SELECT envelope_json FROM data_versions WHERE version_id=?", id);
      const env = JSON.parse(envRow.envelope_json);
      const raw = expand(db, id, env);
      if (!Array.isArray(raw.records)) throw integrity("record lacks records array");
      jsons = raw.records
        .filter((b) => {
          const d = b.date ?? "";
          return d >= range.start && d <= range.end &&
            (range.after === null || range.after === undefined || d > range.after);
        })
        .slice(0, range.limit + 1)
        .map((b) => JSON.stringify(b));
    }
    return jsons.map(parseBarJson);
  }
  const values = db.query(
    "SELECT trade_date,open,high,low,close,volume,amount FROM yearly_bars WHERE version_id=? AND year>=CAST(? AS INTEGER) AND year<=CAST(? AS INTEGER) AND (? IS NULL OR trade_date>?) ORDER BY year LIMIT ?",
    id, String(RC.__parseDate(range.start).y), String(RC.__parseDate(range.end).y),
    range.after ?? null, range.after ?? null, range.limit + 1);
  return values.map((v) => {
    const need = (x) => x === null ? (() => { throw integrity("missing numeric/date field"); })() : x;
    return {
      date: need(v.trade_date), open: need(v.open), high: need(v.high),
      low: need(v.low), close: need(v.close),
      volume: v.volume, amount: v.amount,
    };
  });
}

export function range(db, scope, id, rangeObj) {
  validateRange(rangeObj);
  const versionMissingFromHead = id === null || id === undefined;
  let vid = id;
  if (vid === null || vid === undefined) {
    vid = head(db, scope);
    if (vid === null) throw notFound();
  }
  let record;
  try { record = readRecord(db, vid, true, true); }
  catch (e) {
    if (versionMissingFromHead && e instanceof RC.MarketError && e.kind === "not_found") {
      throw integrity("effective head target unavailable");
    }
    throw e;
  }
  if (record.version.symbol !== scope.symbol || periodName(record.version.period) !== periodName(scope.period)) {
    throw integrity("version/scope mismatch");
  }
  const rows = rangeRows(db, vid, record.version.period, rangeObj);
  const more = rows.length > rangeObj.limit;
  const out = rows.slice(0, rangeObj.limit);
  return {
    integrity_sha256: record.raw.integrity_sha256,
    version: record.version,
    rows: out,
    next_after: more ? out[out.length - 1].date : null,
  };
}

export function versions(db, scope) {
  if (isCompactSchema(db)) return C.compactVersions(db, scope);
  const orphan = db.get(
    "SELECT EXISTS(SELECT 1 FROM publication_batches b LEFT JOIN data_versions v ON v.version_id=b.version_id WHERE v.version_id IS NULL) AS x").x === 1n;
  if (orphan) throw integrity("publication references missing version");
  const ids = db.query(
    "SELECT version_id FROM data_versions v WHERE symbol=? AND period=? AND adjustment='qfq' AND (state='published' OR EXISTS(SELECT 1 FROM publication_batches b WHERE b.version_id=v.version_id)) ORDER BY published_at DESC,version_id ASC",
    scope.symbol, periodName(scope.period)).map((r) => r.version_id);
  return ids.map((id) => readRecord(db, id, true, true).version);
}

/** MarketRepository facade — gated readonly open, verbatim read path. */
export const MarketRepository = {
  open(path) {
    gateDatabase(path); // fail-closed schema identity before any read
    const db = openDatabase(path, { mode: "readonly" });
    return {
      db,
      candidate: (id) => readRecord(db, id, false, true),
      fixed: (id) => readRecord(db, id, true, true),
      range: (scope, id, r) => range(db, scope, id, r),
      versions: (scope) => versions(db, scope),
      head: (scope) => head(db, scope),
      close: () => db.close(),
    };
  },
};

export const __decStr = decStr;
export const __toRfc3339 = toRfc3339;
export const __nodeGet = nodeGet;
export const __metadata = metadata;
export const __evidenceRows = evidenceRows;
export const __annualValues = annualValues;
export const __lineageValues = lineageValues;
export const __baseRecord = baseRecord;
export const __validateBaseIdentity = validateBaseIdentity;

// ---------------- NM-052: update.rs result/receipt reads ----------------
const resultDigest = (audit, receipt) => sha(`[${JSON.stringify(audit)},${JSON.stringify(receipt)}]`);

// envelope::require_refs — fail closed on stored audit referencing absent payloads
export const requireRefs = (db, v) => {
  if (v !== null && typeof v === "object") {
    if (!Array.isArray(v)) {
      const r = v.$ref;
      if (typeof r === "string") {
        if (r.startsWith("payload:") || r.startsWith("annual_bars:")) {
          fetchPayload(db, r.split(":")[1]);
        } else if (r !== "records" && r !== "segments") {
          throw integrity("unknown audit envelope reference");
        }
      }
      for (const x of Object.values(v)) requireRefs(db, x);
    } else {
      for (const x of v) requireRefs(db, x);
    }
  }
};

// sqlite_tasks::read_item_binding (task head + item + marker + occupancy)
const readItemBinding = (db, taskId, itemId) => {
  const h = db.get(
    "SELECT command_key_json,status,state_revision,accepted_run_id,owner_run_id,last_progress_at,result_json,item_count,items_pending,items_running,items_finished,items_succeeded,evidence_digest FROM tasks WHERE task_id=?",
    taskId);
  const i = db.get(
    "SELECT item_id,target_json,progress_unit,frozen_inputs_json,scope_keys_json,stage,outcome_json FROM task_items WHERE task_id=? AND item_id=?",
    taskId, itemId);
  if (!h || !i) return null;
  const item = {
    spec: {
      item_id: i.item_id, target: JSON.parse(i.target_json),
      progress_unit: i.progress_unit, frozen_inputs: JSON.parse(i.frozen_inputs_json),
      scope_keys: JSON.parse(i.scope_keys_json),
    },
    stage: i.stage,
    outcome: i.outcome_json === null ? null : JSON.parse(i.outcome_json),
  };
  const marker = item.spec.frozen_inputs?.selection_evidence?.evidence_digest;
  if (typeof marker === "string" && marker !== h.evidence_digest) {
    // task_control inconsistent() -> Error::Task(NeedsVerification) upstream
    throw new StorageError("needs_verification",
      "ACCEPTANCE_NEEDS_VERIFICATION: item evidence marker mismatches task shared evidence");
  }
  const bound = db.get(
    "SELECT count(*) AS c FROM scope_occupancy WHERE task_id=? AND scope_key IN (SELECT value FROM json_each(?))",
    taskId, i.scope_keys_json).c;
  return {
    head: {
      task_id: taskId,
      key: JSON.parse(h.command_key_json),
      status: h.status,
      state_revision: h.state_revision,
      accepted_run_id: h.accepted_run_id,
      owner_run_id: h.owner_run_id,
      last_progress_at: h.last_progress_at,
      result: h.result_json === null ? null : JSON.parse(h.result_json),
      counts: {
        total: h.item_count, pending: h.items_pending, running: h.items_running,
        finished: h.items_finished, succeeded: h.items_succeeded,
      },
      evidence_digest: h.evidence_digest,
    },
    item,
    scopes_bound: bound > 0n && bound === BigInt(item.spec.scope_keys.length),
  };
};

// serde Receipt shape — 4 required fields; missing -> verbatim serde error
const parseReceipt = (c) => {
  if (c === null || typeof c !== "object" || Array.isArray(c)) {
    throw integrity("missing field `publication_id`");
  }
  for (const k of ["publication_id", "version_id", "task_id"]) {
    if (typeof c[k] !== "string") throw integrity(`missing field \`${k}\``);
  }
  if (typeof c.task_revision !== "number" || !Number.isInteger(c.task_revision)) {
    throw integrity("missing field `task_revision`");
  }
  return c;
};

// serde Publication shape — all 7 fields required; intent one of
// Daily{expected_head}|HistoricalFixedBase|CurrentHead{daily,yearly}
const PUB_FIELDS = ["publication_id", "version_id", "task_id", "item_id", "run_id"];
const parsePublication = (c) => {
  if (c === null || typeof c !== "object" || Array.isArray(c)) {
    throw integrity("missing field `publication_id`");
  }
  for (const k of PUB_FIELDS) {
    if (typeof c[k] !== "string") throw integrity(`missing field \`${k}\``);
  }
  if (typeof c.expected_revision !== "number" || !Number.isInteger(c.expected_revision)) {
    throw integrity("missing field `expected_revision`");
  }
  const i = c.intent;
  if (i === undefined) throw integrity("missing field `intent`");
  const ok = i === "HistoricalFixedBase" ||
    (i !== null && typeof i === "object" &&
      (("Daily" in i && "expected_head" in (i.Daily ?? {})) ||
       ("CurrentHead" in i && typeof i.CurrentHead?.expected_daily_head === "string" &&
         "expected_yearly_head" in (i.CurrentHead ?? {}))));
  if (!ok) throw integrity("range command malformed");
  return c;
};

export const readResult = (db, id) => {
  if (isCompactSchema(db)) {
    const r = CW.readUpdateAudit(db, id);
    return r === null ? null : [r.audit, r.receipt];
  }
  const row = db.get(
    "SELECT task_id,item_id,base_version_id,result_version_id,no_change,audit_json,result_sha256,receipt_json FROM market_update_results WHERE result_id=?",
    id);
  if (!row) return null;
  const [taskId, itemId, baseId, versionId, noChange] =
    [row.task_id, row.item_id, row.base_version_id, row.result_version_id, row.no_change === 1n];
  if (resultDigest(row.audit_json, row.receipt_json) !== row.result_sha256) {
    throw integrity("range audit digest mismatch");
  }
  const audit = JSON.parse(row.audit_json);
  requireRefs(db, audit.segment);
  const command = parsePublication(audit.command);
  const receipt = parseReceipt(JSON.parse(row.receipt_json));
  const intentMatch = command.intent &&
    Object.keys(command.intent).length === 1 &&
    command.intent.Daily !== undefined &&
    command.intent.Daily.expected_head === baseId;
  if (command.publication_id !== id || command.task_id !== taskId ||
      command.item_id !== itemId || command.version_id !== versionId ||
      receipt.publication_id !== id || receipt.task_id !== taskId ||
      receipt.version_id !== versionId ||
      audit.evidence?.base_version_id !== baseId ||
      audit.no_change !== noChange || !intentMatch ||
      noChange !== (baseId === versionId) ||
      audit.algorithm_version !== "local-range-update-v1") {
    throw integrity("range result association mismatch");
  }
  const base = readRecord(db, baseId, true, true);
  const result = readRecord(db, versionId, true, true);
  if (audit.base_integrity_sha256 !== base.raw.integrity_sha256 ||
      audit.result_integrity_sha256 !== result.raw.integrity_sha256) {
    throw integrity("range result version digest mismatch");
  }
  const binding = readItemBinding(db, taskId, itemId);
  if (!binding) throw integrity("range result task missing");
  if (binding.head.state_revision < BigInt(receipt.task_revision) ||
      BigInt(receipt.task_revision) <= BigInt(command.expected_revision) ||
      binding.item.stage !== "finished" ||
      !(binding.item.outcome && binding.item.outcome.success === true)) {
    throw integrity("range result lacks successful task item");
  }
  if (!noChange) {
    const published = db.get(
      "SELECT receipt_json FROM publication_batches WHERE publication_id=?", id);
    if (!published) throw integrity("range publication missing");
    const pubReceipt = parseReceipt(JSON.parse(published.receipt_json));
    if (pubReceipt.publication_id !== receipt.publication_id ||
        pubReceipt.version_id !== receipt.version_id ||
        pubReceipt.task_id !== receipt.task_id ||
        pubReceipt.task_revision !== receipt.task_revision) {
      throw integrity("range publication receipt mismatch");
    }
  }
  return [audit, receipt];
};

export const itemReceipt = (db, task, item) => {
  const id = db.get(
    "SELECT result_id FROM market_update_results WHERE task_id=? AND item_id=?",
    task, item)?.result_id;
  if (id === undefined) return null;
  const r = readResult(db, id);
  if (r === null) throw integrity("range item receipt missing");
  return r[1];
};
export const __readItemBinding = readItemBinding;
export const rangeReceipt = (db, id) => readResult(db, id)?.[1] ?? null;
