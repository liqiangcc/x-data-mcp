// src/sqlite_supply.rs — SqliteSupply: the ReadRepository +
// ObservationRepository that plugs the data-supply seam (S12c).
//
// The struct carries a `market` backend handle exposing `.read(fn)` —
// SqliteCompactMarket / the raw-conn shim both satisfy it. Every method
// runs inside one real read transaction on the Rust side; Node's
// backend read is the same connection-scope boundary.
//
// Error surface (market_storage::Error):
//   storage   -> StorageError-ish MARKET_STORAGE   (Storage)
//   integrity -> MARKET_INTEGRITY                  (Integrity)
//   invalid   -> INVALID_MARKET_COMMAND            (Invalid)
//   Task(t)   -> the task_control error verbatim   (transparent)
//   Base      -> BASE_UNAVAILABLE

import * as MR from "../market/market_read.mjs";
import * as GAPS from "../market/gaps.mjs";
import * as YR from "../market/yearly_results.mjs";
import * as PC from "../storage/publication_checks.mjs";
import * as VS from "../storage/verification_storage.mjs";
import * as CAT from "../catalog/sqlite_catalog.mjs";
import { aggregate } from "../algorithms/yearly.mjs";
import { epochDate } from "../market/compact_market.mjs";
import { MarketError } from "../market/record_codec.mjs";
import { _dec } from "../algorithms/qfq_affine.mjs";
import { parseDateTime } from "../mcp/serde_value.mjs";
import { StorageError } from "../storage/sqlite_adapter.mjs";
import { MAX_ROWS, entities } from "./query.mjs";

const storage = (e) =>
  new MarketError("storage", `MARKET_STORAGE: ${e.message ?? e}`);
const integrity = (m) =>
  new MarketError("integrity", `MARKET_INTEGRITY: ${m}`);
const invalid = (m) =>
  new MarketError("invalid", `INVALID_MARKET_COMMAND: ${m}`);

const datasetPeriod = (d) =>
  d === "market.daily" ? "Day" : d === "market.yearly" ? "Year" : null;

// wire::Security::symbol() — "sh.600000"
const securitySymbol = (e) => `${e.exchange}.${e.code}`;
const securityKey = (e) => `${e.exchange}:${e.code}`;

// observe::entity — "sh.600000" -> Security, first-dot split.
const entityOf = (symbol) => {
  const i = String(symbol).indexOf(".");
  if (i < 0) return null;
  const ex = symbol.slice(0, i);
  return (ex === "sh" || ex === "sz")
    ? { exchange: ex, code: symbol.slice(i + 1) }
    : null;
};

// observe::authorized — dataset allowlist + per-item security gate.
const authorized = (s, a) => {
  if (s.task.key.dataset === "task.verification") return a.equalsLocal();
  const datasets = (() => {
    switch (s.task.key.dataset) {
      case "market.daily": return ["market.daily"];
      case "market.yearly": return ["market.yearly"];
      case "securities.catalog": return ["securities.catalog"];
      case "legacy_market_compound":
        return s.items.map((i) =>
          i.spec.target.period === "day" ? "market.daily" : "market.yearly");
      default: return false;
    }
  })();
  if (!datasets) return false;
  return datasets.every((d) => a.allows(d, null)) &&
    s.items.every((i) => {
      const sym = i.spec.target?.symbol;
      if (typeof sym !== "string") return true;
      const e = entityOf(sym);
      if (e === null) return true;
      return a.securities === null ||
        [...a.securities].some((x) => securityKey(x) === securityKey(e));
    });
};

// market_ref — wire::VersionRef over the record's own envelope digest.
const marketRef = (dataset, entity, r) => ({
  dataset,
  contract_version: "v1",
  entity,
  version_id: r.version.version_id,
  integrity_sha256: r.raw.integrity_sha256,
});

const catalogRef = (v) => ({
  dataset: "securities.catalog",
  contract_version: "v1",
  entity: null,
  version_id: v.version_id,
  integrity_sha256: CAT.digest(v),
});

// read_market — compat record + entity/dataset consistency.
const readMarket = (db, dataset, entity, id) => {
  const r = MR.readRecord(db, id, true, true);
  if (r.version.symbol !== securitySymbol(entity) ||
      r.version.period !== datasetPeriod(dataset)) {
    throw integrity("version entity/dataset mismatch");
  }
  return r;
};

// query::unique_refs — dedup by (dataset, version_id), first-wins,
// BTreeSet ordering (sorted by the tuple).
const uniqueRefs = (refs) => {
  const seen = new Set();
  const out = [];
  for (const r of refs) {
    const k = `${r.dataset}${r.version_id}`;
    if (!seen.has(k)) { seen.add(k); out.push(r); }
  }
  // Rust BTreeSet dedup: the set order is sorted; insertion order after
  // dedup is what verify iterates — same set of pairs either way, and
  // the first mismatch aborts, so ordering doesn't change the result.
  return out;
};

// metadata — unit_currency evidence + coverage from the raw envelope.
const metadata = (r) => {
  const v = r.version, u = v.unit_currency;
  const units = [
    ["price_currency", u.price_currency],
    ["price_unit", u.price_unit],
    ["volume_unit", u.volume_unit],
    ["amount_currency", u.amount_currency],
    ["amount_unit", u.amount_unit],
  ].map(([field, e]) => ({
    field,
    // format!("{:?}", e.status).to_lowercase()
    status: String(e.status).toLowerCase(),
    value: e.value ?? null,
    reason: e.reason,
  }));
  const cov = r.raw?.batch?.coverage ?? {};
  const l = v.yearly_derivation;
  return {
    source: v.source,
    published_at: v.published_at,
    coverage_status: (cov.status ?? "Unknown").toLowerCase(),
    coverage_reason: cov.reason ?? "",
    units,
    lineage: l === null || l === undefined ? null : {
      base_daily_version_id: l.base_daily_version_id,
      algorithm_version: l.algorithm_version,
      first_trading_date: l.first_trading_date?.s ?? l.first_trading_date,
      last_trading_date: l.last_trading_date?.s ?? l.last_trading_date,
    },
  };
};

// yearly_row_meta — v18 reads the stored yearly_bars row; compact
// re-aggregates the verified base daily version and indexes per-year
// lineage by calendar year (same projection stage-time verification
// recomputes).
const yearlyRowMeta = (db, r, bar) => {
  const barDate = bar.date?.s ?? bar.date;
  if (!MR.isCompactSchema(db)) {
    let row;
    try {
      row = db.get(
        "SELECT first_trade_date,last_trade_date,is_finished FROM yearly_bars WHERE version_id=? AND trade_date=?",
        r.version.version_id, barDate);
    } catch (e) { throw storage(e); }
    if (!row) throw storage(new Error("no rows"));
    return [row.first_trade_date, row.last_trade_date, !row.is_finished];
  }
  const lineage = r.version.yearly_derivation;
  if (!lineage) throw integrity("yearly version lacks derivation");
  let asOfDay;
  try {
    asOfDay = db.get(
      "SELECT l.as_of_day FROM yearly_lineage l JOIN data_versions v ON v.version_key=l.version_key WHERE v.version_id=?",
      r.version.version_id)?.as_of_day;
  } catch (e) { throw storage(e); }
  if (asOfDay === undefined || asOfDay === null) {
    throw storage(new Error("no rows"));
  }
  let asOf;
  try { asOf = epochDate(Number(asOfDay)); }
  catch (e) { throw integrity(`stored yearly as_of: ${e.message ?? e}`); }
  const base = MR.readRecord(db, lineage.base_daily_version_id, true, false);
  // aggregate() takes the ported evidence shape {confirmed,value};
  // base.version carries the serde {status,value,reason} projection.
  const ev = (e) => ({ confirmed: e?.status === "Confirmed",
    value: e?.value ?? null });
  let annual;
  try {
    annual = aggregate(
      { ...base.version, period: "day", yearly_derivation: null,
        adjustment_evidence: ev(base.version.adjustment_evidence),
        unit_currency: Object.fromEntries(
          ["price_currency", "price_unit", "volume_unit",
            "amount_currency", "amount_unit"]
            .map((k) => [k, ev(base.version.unit_currency[k])])) },
      null, asOf);
  } catch (e) {
    throw integrity(`yearly re-aggregation: ${e.message ?? e}`);
  }
  const record = annual.records.find(
    (rec) => (rec.bar.date?.s ?? rec.bar.date) === barDate);
  if (!record) {
    throw integrity("yearly record missing from re-aggregation");
  }
  return [record.lineage.first_trading_date,
    record.lineage.last_trading_date, record.year_unfinished];
};

// Dec {m,s} -> Decimal::to_string (scale preserved); text passes
// through verbatim.
const decOut = (v) =>
  (v !== null && typeof v === "object" && typeof v.m === "bigint"
    ? _dec.toString(v) : String(v));

// query — the pub(crate) body behind ReadRepository::query.
export const query = (db, plan, access) => {
  const result = { manifest: [], rows: [] };
  const args = plan.args;
  if (args.dataset === "securities.catalog") {
    const id = args.version.kind === "catalog_version"
      ? args.version.version_id
      : CAT.head(db);
    if (id === null || id === undefined) {
      result.manifest.push({
        entity: null, version_ref: null, status: "absent",
        metadata: null });
      return result;
    }
    const v = CAT.read(db, id);
    const reference = catalogRef(v);
    result.manifest.push({
      entity: null, version_ref: reference, status: "present",
      metadata: {
        source: v.source, published_at: v.published_at,
        coverage_status: "complete",
        coverage_reason: "validated fixture catalog snapshot",
        units: [], lineage: null,
      } });
    const f = args.filter;
    if (f.kind !== "catalog") throw invalid("catalog filter required");
    const members = [...v.records].sort((a, b) => {
      // sort_by_key(key()) — SecurityKey Ord: (exchange, symbol)
      const ka = exchangeSortKey(a.exchange) + "\0" + a.symbol;
      const kb = exchangeSortKey(b.exchange) + "\0" + b.symbol;
      return ka < kb ? -1 : ka > kb ? 1 : 0;
    });
    for (const r of members) {
      const ex = r.exchange === "Shanghai" ? "sh"
        : r.exchange === "Shenzhen" ? "sz" : null;
      if (ex === null) throw integrity("catalog exchange");
      const entity = { exchange: ex, code: r.symbol };
      if (!access.allows("securities.catalog", entity)) continue;
      const ind = r.industry;
      const tag = Object.keys(ind)[0];
      const industry = tag === "Assigned"
        ? { status: "assigned", code: ind.Assigned.code, name: ind.Assigned.name }
        : tag === "Missing"
          ? { status: "missing", reason: ind.Missing.reason }
          : { status: "conflicted", reason: ind.Conflicted.reason };
      if ((f.code !== null && f.code !== undefined && f.code !== entity.code) ||
          (f.name_query !== null && f.name_query !== undefined &&
            !r.name.includes(f.name_query)) ||
          (f.industry_code !== null && f.industry_code !== undefined &&
            !(industry.status === "assigned" &&
              industry.code === f.industry_code))) {
        continue;
      }
      result.rows.push({
        kind: "catalog", entity, version_ref: reference,
        name: r.name, industry });
    }
    return result;
  }
  for (const entity of plan.entities) {
    const id = args.version.kind === "explicit_version_map"
      ? (args.version.versions.find(
          (v) => v.entity.exchange === entity.exchange &&
            v.entity.code === entity.code)?.version_id ?? null)
      : MR.head(db, {
          symbol: securitySymbol(entity),
          period: datasetPeriod(args.dataset) });
    if (id === null || id === undefined) {
      result.manifest.push({
        entity, version_ref: null, status: "absent", metadata: null });
      continue;
    }
    const r = readMarket(db, args.dataset, entity, id);
    const reference = marketRef(args.dataset, entity, r);
    result.manifest.push({
      entity, version_ref: reference, status: "present",
      metadata: metadata(r) });
    const f = args.filter;
    let start, end;
    if (f.kind === "daily") { start = f.start_date; end = f.end_date; }
    else if (f.kind === "yearly") {
      start = `${String(f.start_year).padStart(4, "0")}-01-01`;
      end = `${String(f.end_year).padStart(4, "0")}-12-31`;
    } else {
      throw invalid("market filter");
    }
    const range = { start, end, after: null, limit: MAX_ROWS };
    for (const b of MR.rangeRows(db, id, datasetPeriod(args.dataset), range)) {
      const barDate = b.date?.s ?? b.date;
      let first = null, last = null, unfinished = null;
      if (args.dataset === "market.yearly") {
        [first, last, unfinished] = yearlyRowMeta(db, r, b);
      }
      result.rows.push({
        kind: "market", first_trading_date: first,
        last_trading_date: last, year_unfinished: unfinished,
        entity, version_ref: reference,
        date: barDate,
        // serde Decimal::to_string preserves the stored scale —
        // compact bars carry Dec {m,s}; legacy bars carry text.
        open: decOut(b.open), high: decOut(b.high),
        low: decOut(b.low), close: decOut(b.close),
        volume: b.volume === null || b.volume === undefined
          ? null : decOut(b.volume),
        amount: b.amount === null || b.amount === undefined
          ? null : decOut(b.amount),
      });
      if (result.rows.length > MAX_ROWS) {
        throw invalid("snapshot row budget exceeded");
      }
    }
  }
  return result;
};

// SecurityKey Ord — Exchange derives Ord over the DECLARED variant
// order (Shanghai < Shenzhen < Other), then symbol.
const exchangeSortKey = (e) =>
  e === "Shanghai" ? "0" : e === "Shenzhen" ? "1" : `2${e.Other}`;

// task_facts — observe path read. `readBound` is the validated
// read_bound_task injection (TaskAdmission._readBoundTaskValidated).
const taskFacts = (db, id, access, verifyOutputs, readBound) => {
  // Owner-only check evidence must not leak integrity diagnostics to a
  // reader — early deny, verified authorization still applies below.
  if (!access.equalsLocal()) {
    let priv;
    try {
      priv = db.get(
        "SELECT EXISTS(SELECT 1 FROM tasks WHERE task_id=? AND json_extract(command_key_json,'$.dataset')='task.verification')",
        id);
    } catch (e) { throw storage(e); }
    if (priv && Number(Object.values(priv)[0])) throw invalid("forbidden");
  }
  const raw = readBound(id);
  if (raw === null || raw === undefined) {
    throw new StorageError("task_not_found", "TASK_NOT_FOUND");
  }
  const s = VS.snapshotCanon(raw);
  if (!authorized(s, access)) throw invalid("forbidden");
  let request;
  try {
    request = db.get("SELECT request_id FROM requests WHERE task_id=?", id)
      ?.request_id;
  } catch (e) { throw storage(e); }
  if (request === undefined) throw storage(new Error("no rows"));
  if (!verifyOutputs) {
    return { request_id: request, snapshot: s, publications: [],
      range_results: [], yearly_results: [] };
  }
  const compact = MR.isCompactSchema(db);
  let ids;
  try {
    ids = db.query(compact
      ? "SELECT v.version_id FROM task_publications tp JOIN data_versions v ON v.version_key=tp.version_key WHERE tp.task_id=? ORDER BY v.version_id"
      : "SELECT version_id FROM task_publications WHERE task_id=? ORDER BY version_id",
      id).map((r) => r.version_id);
  } catch (e) { throw storage(e); }
  const publications = ids.map((vid) => {
    const r = MR.readRecord(db, vid, true, true);
    const e = entityOf(r.version.symbol);
    if (e === null) throw integrity("published task symbol");
    return marketRef(
      r.version.period === "Day" ? "market.daily" : "market.yearly",
      e, r);
  });
  const rangeResults = db.query(
    "SELECT result_id FROM market_update_results WHERE task_id=? ORDER BY result_id",
    id).map((r) => rangeResult(db, r.result_id));
  const yearlyIds = db.query(compact
    ? "SELECT publication_id FROM publication_results WHERE task_id=? AND publication_kind=2 ORDER BY publication_id"
    : "SELECT result_id FROM yearly_execution_results WHERE task_id=? ORDER BY result_id",
    id).map((r) => Object.values(r)[0]);
  const yearlyResults = yearlyIds.map((yid) => yearlyResult(db, yid));
  const op = s.task.key.operation;
  if (op === "update_data" || op === "backfill_data" ||
      op === "republish_data") {
    const successful = new Set(s.items
      .filter((i) => i.outcome !== null && i.outcome !== undefined &&
        i.outcome.success)
      .map((i) => i.spec.item_id));
    const evidenced = new Set(
      (s.task.key.dataset === "market.yearly" ? yearlyResults : rangeResults)
        .map((r) => r.item_id));
    if (successful.size !== evidenced.size ||
        [...successful].some((x) => !evidenced.has(x))) {
      throw integrity("successful range task lacks matching result evidence");
    }
  }
  return { request_id: request, snapshot: s, publications,
    range_results: rangeResults, yearly_results: yearlyResults };
};

const taskList = (db, access, readBound) => {
  let rows;
  try {
    rows = db.query(
      "SELECT task_id,command_key_json,frozen_work_json FROM tasks ORDER BY created_at DESC,task_id ASC");
  } catch (e) { throw storage(e); }
  const result = [];
  for (const r of rows) {
    let key;
    try { key = JSON.parse(r.command_key_json); }
    catch (e) { throw storage(e); }
    // serde from_str::<CommandKey> — required fields, declared order.
    for (const f of ["operation", "dataset", "contract_version"]) {
      if (key === null || typeof key !== "object" || Array.isArray(key) ||
          key[f] === undefined) {
        throw storage(new Error(
          `missing field \`${f}\` at line 1 column ${r.command_key_json.length}`));
      }
    }
    const allowed = (() => {
      switch (key.dataset) {
        case "task.verification": return access.equalsLocal();
        case "market.daily": return access.allows("market.daily", null);
        case "market.yearly": return access.allows("market.yearly", null);
        case "securities.catalog":
          return access.allows("securities.catalog", null);
        case "legacy_market_compound":
          return access.allows("market.daily", null) ||
            access.allows("market.yearly", null);
        default: return false;
      }
    })();
    if (!allowed) continue;
    let work;
    try { work = JSON.parse(r.frozen_work_json); }
    catch (e) { throw storage(e); }
    const denied = (work.items ?? []).some((i) => {
      const sym = i.target?.symbol;
      if (typeof sym !== "string") return false;
      const e = entityOf(sym);
      return e !== null && access.securities !== null &&
        ![...access.securities].some(
          (x) => securityKey(x) === securityKey(e));
    });
    if (denied) continue;
    let f;
    try { f = taskFacts(db, r.task_id, access, false, readBound); }
    catch (e) {
      if (e instanceof MarketError && e.code === "invalid" &&
          e.message === "INVALID_MARKET_COMMAND: forbidden") continue;
      throw e;
    }
    result.push(f);
    if (result.length > 10000) {
      throw invalid("task snapshot budget exceeded");
    }
  }
  return result;
};

const rangeResult = (db, id) => {
  const got = MR.readResult(db, id);
  if (!got) throw integrity("range result missing");
  const [audit, receipt] = got;
  const baseId = audit?.evidence?.base_version_id;
  if (typeof baseId !== "string") {
    throw integrity("range base reference missing");
  }
  const base = MR.readRecord(db, baseId, true, true);
  const result = MR.readRecord(db, receipt.version_id, true, true);
  if (base.version.period !== "Day" || result.version.period !== "Day" ||
      base.version.symbol !== result.version.symbol) {
    throw integrity("range result version identity/period mismatch");
  }
  const entity = entityOf(base.version.symbol);
  if (entity === null) throw integrity("range result entity missing");
  // serde_json::json! projection — declaration order via ser().
  return {
    result_id: id,
    item_id: audit?.command?.item_id,
    base_version: marketRef("market.daily", entity, base),
    result_version: marketRef("market.daily", entity, result),
    no_change: audit?.no_change,
    acquired_at: audit?.segment?.provenance?.acquired_at,
  };
};

const yearlyResult = (db, id) => {
  const got = YR.readResult(db, id);
  if (!got) throw integrity("yearly result missing");
  const [audit, receipt] = got;
  const baseId = audit?.base_daily_version_id;
  if (typeof baseId !== "string") throw integrity("yearly base missing");
  const base = MR.readRecord(db, baseId, true, true);
  const result = MR.readRecord(db, receipt.version_id, true, true);
  if (base.version.period !== "Day" || result.version.period !== "Year" ||
      base.version.symbol !== result.version.symbol) {
    throw integrity("yearly result identity/period");
  }
  const entity = entityOf(base.version.symbol);
  if (entity === null) throw integrity("yearly result entity");
  return {
    result_id: id,
    item_id: audit?.command?.item_id,
    base_daily_version: marketRef("market.daily", entity, base),
    result_version: marketRef("market.yearly", entity, result),
    no_change: audit?.no_change,
    derived_at: audit?.candidate?.batch?.provenance?.acquired_at,
  };
};

/**
 * SqliteSupply — {market, readBound}. `market` exposes
 * `.read(fn)` (SqliteCompactMarket or the raw-conn shim); `readBound`
 * is the validated read_bound_task (TaskAdmission._readBoundTaskValidated).
 */
export const sqliteSupply = (market, readBound, readFactsFn) => {
  const read = (fn) => market.read(fn);
  return {
    // ReadRepository::query
    query: (plan, access) => read((c) => query(c, plan, access)),
    // ReadRepository::verify — fixed-version revalidation.
    verify: (refs) => read((c) => {
      for (const r of uniqueRefs(refs)) {
        const actual = r.dataset === "securities.catalog"
          ? catalogRef(CAT.read(c, r.version_id))
          : (() => {
              const entity = r.entity;
              if (!entity) throw invalid("missing version entity");
              return marketRef(r.dataset, entity,
                readMarket(c, r.dataset, entity, r.version_id));
            })();
        if (!refsEq(actual, r)) {
          throw integrity("fixed version changed");
        }
      }
    }),
    // ReadRepository::versions — the entity-scoped version listing.
    versions: (args, access) => {
      const es = entities(args.dataset, args.selection, access);
      return read((c) => {
        const result = [];
        if (args.dataset === "securities.catalog") {
          const ids = c.query(
            "SELECT version_id FROM catalog_versions ORDER BY published_at DESC,version_id ASC")
            .map((r) => r.version_id);
          for (const id of ids) {
            const v = CAT.read(c, id);
            result.push({ version_ref: catalogRef(v),
              published_at: v.published_at });
          }
        } else {
          const compact = MR.isCompactSchema(c);
          const orphan = compact
            ? c.get("SELECT EXISTS(SELECT 1 FROM task_publications tp LEFT JOIN data_versions v ON v.version_key=tp.version_key WHERE v.version_key IS NULL) AS x").x
            : c.get("SELECT EXISTS(SELECT 1 FROM publication_batches b LEFT JOIN data_versions v ON b.version_id=v.version_id WHERE v.version_id IS NULL) AS x").x;
          if (Number(orphan)) throw integrity("orphan market publication");
          const listSql = compact
            ? "SELECT v.version_id FROM data_versions v WHERE v.entity=? AND v.period=? AND (v.state=3 OR EXISTS(SELECT 1 FROM task_publications tp WHERE tp.version_key=v.version_key)) ORDER BY v.published_at DESC,v.version_id ASC"
            : "SELECT version_id FROM data_versions v WHERE symbol=? AND period=? AND (state='published' OR EXISTS(SELECT 1 FROM publication_batches b WHERE b.version_id=v.version_id)) ORDER BY published_at DESC,version_id ASC";
          for (const entity of es) {
            const p = args.dataset === "market.daily"
              ? (compact ? 1 : "day") : (compact ? 2 : "year");
            const ids = c.query(listSql, securitySymbol(entity), p)
              .map((r) => r.version_id);
            for (const id of ids) {
              const r = readMarket(c, args.dataset, entity, id);
              result.push({
                version_ref: marketRef(args.dataset, entity, r),
                published_at: r.version.published_at });
            }
          }
        }
        // published_at DESC, version_id ASC — strings compare
        // lexicographically for RFC3339-fixed-width timestamps.
        result.sort((a, b) =>
          (b.published_at < a.published_at ? -1
            : b.published_at > a.published_at ? 1 : 0) ||
          (a.version_ref.version_id < b.version_ref.version_id ? -1
            : a.version_ref.version_id > b.version_ref.version_id ? 1 : 0));
        return result;
      });
    },
    // ObservationRepository::tasks
    tasks: (access) => read((c) => taskList(c, access, readBound)),
    // ObservationRepository::task — verify_outputs=true
    task: (id, access) =>
      read((c) => taskFacts(c, id, access, true, readBound)),
    // ObservationRepository::verify_versions — ReadRepository::verify
    verifyVersions(refs) { return this.verify(refs); },
    // verify_publication_check — re-read the committed check receipt.
    verifyPublicationCheck(receipt) {
      if (receipt !== null && receipt !== undefined) {
        read((c) => {
          const actual = PC.checkReceipt(c, receipt.verification_task_id,
            readBound, readFactsFn);
          if (!receiptEq(actual, receipt)) {
            throw integrity("publication check changed since snapshot");
          }
        });
      }
    },
    // verify_stopped_check — sqlite_verification::read_receipt compare.
    verifyStoppedCheck(receipt) {
      if (receipt !== null && receipt !== undefined) {
        read((c) => {
          const actual = VS.stopReceipt(c, receipt.verification_task_id,
            readBound);
          if (!receiptEq(actual, receipt)) {
            throw integrity("stopped check changed since snapshot");
          }
        });
      }
    },
    // verify_yearly_results — re-read + typed equality.
    verifyYearlyResults(results) {
      if (!results.length) return;
      read((c) => {
        for (const expected of results) {
          if (!resultEq(yearlyResult(c, expected.result_id), expected)) {
            throw integrity("yearly result changed since snapshot");
          }
        }
      });
    },
    // verify_range_results — re-read + typed equality.
    verifyRangeResults(results) {
      if (!results.length) return;
      read((c) => {
        for (const expected of results) {
          if (!resultEq(rangeResult(c, expected.result_id), expected)) {
            throw integrity("range result changed since snapshot");
          }
        }
      });
    },
    // availability — per-entity query + related tasks + proven gaps;
    // Integrity/Base captured into integrity_failure, others propagate.
    availability(plan, access) {
      return read((c) => {
        const tasks = taskList(c, access, readBound);
        const out = [];
        const entityList = plan.args.dataset === "securities.catalog"
          ? [null] : plan.entities;
        for (const entity of entityList) {
          const single = { args: { ...plan.args }, entities: plan.entities };
          if (entity !== null) single.entities = [entity];
          const relatedTasks = tasks.filter((f) =>
            f.snapshot.items.some((i) => entity !== null
              ? i.spec.target?.symbol === securitySymbol(entity)
              : f.snapshot.task.key.dataset === "securities.catalog"));
          let provenGaps = [];
          const period = entity !== null ? datasetPeriod(plan.args.dataset) : null;
          if (entity !== null && period !== null) {
            provenGaps = GAPS.readOpenGaps(c, securitySymbol(entity), period);
          }
          try {
            const s = query(c, single, access);
            const dates = s.rows
              .filter((r) => r.kind === "market")
              .map((r) => r.date);
            out.push({
              entity,
              manifest: s.manifest[0] ?? null,
              first_record: dates[0] ?? null,
              last_record: dates[dates.length - 1] ?? null,
              record_count: s.rows.length,
              integrity_failure: null,
              proven_gaps: provenGaps,
              related_tasks: relatedTasks,
            });
          } catch (e) {
            if (e instanceof MarketError &&
                (e.kind === "integrity" || e.kind === "base")) {
              out.push({
                entity, manifest: null, first_record: null,
                last_record: null, record_count: 0,
                integrity_failure: e.message.replace(
                  /^MARKET_INTEGRITY: |^BASE_UNAVAILABLE: /, ""),
                proven_gaps: provenGaps,
                related_tasks: relatedTasks,
              });
            } else { throw e; }
          }
        }
        return out;
      });
    },
  };
};

// ——— equality helpers (PartialEq semantics) ———

const canonEq = (a, b) => serSorted(a) === serSorted(b);
const serSorted = (v) => JSON.stringify(sortDeep(v));
const sortDeep = (v) => {
  if (Array.isArray(v)) return v.map(sortDeep);
  if (v !== null && typeof v === "object") {
    return Object.fromEntries(
      Object.keys(v).sort().map((k) => [k, sortDeep(v[k])]));
  }
  return v;
};

// VersionRef PartialEq — dataset/contract/entity/version_id/digest.
const refsEq = (a, b) => canonEq(a, b);
// Receipt PartialEq — the stored receipt bytes vs the supplied serde
// value; canonical compare over the parsed JSON.
const receiptEq = (a, b) => canonEq(a, b);
// RangeExecutionResult/YearlyExecutionResult PartialEq — chrono compares
// instants, not bytes; normalize acquired_at/derived_at to BigInt ns.
const instant = (v) =>
  (typeof v === "bigint" ? v : parseDateTime(v).instant);
const resultEq = (a, b) =>
  a.result_id === b.result_id && a.item_id === b.item_id &&
  a.no_change === b.no_change &&
  canonEq(a.base_version ?? a.base_daily_version,
          b.base_version ?? b.base_daily_version) &&
  canonEq(a.result_version, b.result_version) &&
  instant(a.acquired_at ?? a.derived_at) === instant(b.acquired_at ?? b.derived_at);

export { marketRef, catalogRef, readMarket, metadata, yearlyRowMeta,
  taskFacts, taskList, rangeResult, yearlyResult, entityOf, authorized,
  uniqueRefs };
