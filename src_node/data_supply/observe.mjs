// src/data_supply/observe.rs — Observer: consistent task observations
// and bounded, authorization-bound snapshots over the repository seam
// (ObservationRepository — SqliteSupply via NM-074).
//
// Cursor semantics verbatim: per-instance Saved deque capped at 32; an
// unknown token -> "invalid/expired cursor"; a key/access mismatch ->
// "cursor conditions/authorization changed". Cursors are session-bound:
// they never survive process restart.

import { invalid, StorageError } from "./access.mjs";
import { pageSize, QueryPlan } from "./query.mjs";
import { serValue } from "../mcp/serde_json.mjs";
import { parseDateTime } from "../mcp/serde_value.mjs";
import { visibleStatusOf } from "../storage/task_repository.mjs";
import {
  DATASET_CATALOG, DATASET_DAILY, DATASET_YEARLY,
} from "./wire.mjs";

const byteLen = (v) => Buffer.byteLength(serValue(v), "utf8");
const MAX_BYTES = 16 * 1024 * 1024;

// serde DateTime<Utc> to_rfc3339_opts(AutoSi, use_z=true).
const serTs = (d) => {
  const iso = d.toISOString();
  return iso.endsWith(".000Z") ? iso.slice(0, -5) + "Z" : iso;
};

// Access PartialEq — principal + dataset set + security allowlist.
const accessEq = (a, b) => {
  if (a.principal !== b.principal) return false;
  const setEq = (x, y) => x.size === y.size && [...x].every((v) => y.has(v));
  const sKey = (s) =>
    s === null ? null : [...s].map((e) => `${e.exchange}:${e.code}`).sort();
  const as = sKey(a.securities), bs = sKey(b.securities);
  return setEq(a.datasets, b.datasets) &&
    (as === null ? bs === null
      : bs !== null && as.length === bs.length && as.every((v, i) => v === bs[i]));
};

// observe::entity — "sh.600000" -> Security, first-dot split.
export const entityOf = (symbol) => {
  const i = String(symbol).indexOf(".");
  if (i < 0) return null;
  const ex = symbol.slice(0, i);
  return (ex === "sh" || ex === "sz")
    ? { exchange: ex, code: symbol.slice(i + 1) }
    : null;
};

// observe::authorized — dataset allowlist + per-item security gate.
export const authorized = (s, a) => {
  if (s.task.key.dataset === "task.verification") return a.equalsLocal();
  const datasets = (() => {
    switch (s.task.key.dataset) {
      case "market.daily": return [DATASET_DAILY];
      case "market.yearly": return [DATASET_YEARLY];
      case "securities.catalog": return [DATASET_CATALOG];
      case "legacy_market_compound":
        return s.items.map((i) =>
          i.spec.target?.period === "day" ? DATASET_DAILY : DATASET_YEARLY);
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
        [...a.securities].some((x) => `${x.exchange}:${x.code}` === `${e.exchange}:${e.code}`);
    });
};

// observe::summary — TaskSummary over a TaskFacts; status is
// visible_status(run) — serde TaskStatus serializes as the snake string.
export const summary = (f, run) => {
  const t = f.snapshot.task;
  return {
    task_id: t.task_id,
    request_id: f.request_id,
    dataset: t.key.dataset,
    operation: t.key.operation,
    contract_version: t.key.contract_version,
    status: visibleStatusOf(t.status, t.accepted_run_id, t.owner_run_id, run),
    state_revision: t.state_revision,
    created_at: t.created_at,
    last_progress_at: t.last_progress_at,
  };
};

// confirmed_gap — proven gap -> wire ConfirmedGap; the evidence string is
// serde_json::to_string(EvidenceRef{kind,id,sha256}) — declaration order.
const confirmedGap = (g) => {
  let range;
  if (g.period === "Day") {
    range = { kind: "daily", start_date: g.start_date, end_date: g.end_date };
  } else {
    // Year: natural-year bounds required (chrono NaiveDate year()).
    const sy = Number(String(g.start_date).slice(0, 4));
    const ey = Number(String(g.end_date).slice(0, 4));
    const start = `${String(sy).padStart(4, "0")}-01-01`;
    const end = `${String(ey).padStart(4, "0")}-12-31`;
    if (start !== g.start_date || end !== g.end_date)
      throw IntegrityError(
        "yearly proven gap is not a natural-year range");
    range = { kind: "yearly", start_year: sy, end_year: ey };
  }
  return {
    range,
    evidence: `{"kind":${JSON.stringify(g.evidence_kind)},"id":${JSON.stringify(g.evidence_id)},"sha256":${JSON.stringify(g.evidence_digest)}}`,
  };
};

// market_storage::Error::Integrity — the storage kind, mapped to
// integrity_error by the mcp layer.
const IntegrityError = (m) => new StorageError("integrity", m);

// availability_item — domain classification; storage reports facts.
const availabilityItem = (f, dataset, run, range) => {
  const unavailable = f.integrity_failure !== null && f.integrity_failure !== undefined;
  let availability;
  if (unavailable) availability = "unavailable";
  else if (f.manifest !== null && f.manifest !== undefined &&
           f.manifest.status === "absent") availability = "absent";
  else if (dataset === DATASET_CATALOG) availability = "available";
  else availability = "unknown";
  const coverageEvidence = unavailable
    ? f.integrity_failure
    : f.manifest?.metadata?.coverage_reason ?? "no published version";
  return {
    entity: f.entity,
    version_ref: f.manifest?.version_ref ?? null,
    availability,
    integrity: unavailable ? "failed"
      : availability === "absent" ? "not_applicable" : "verified",
    confirmed_gaps: f.proven_gaps.map(confirmedGap),
    unresolved_ranges: availability === "unknown" ? [range] : [],
    coverage_evidence: coverageEvidence,
    first_record: f.first_record,
    last_record: f.last_record,
    record_count: f.record_count,
    freshness: "unknown",
    related_tasks: f.related_tasks.map((x) => summary(x, run)),
    limitations: [
      "range completeness is not inferred from dates or version coverage alone; no implicit collection or repair",
    ],
  };
};

// budget — observation snapshot bound (10k items / MAX_BYTES).
const budget = (v) => {
  if (v.length > 10000 || byteLen(v) > MAX_BYTES)
    throw invalid("observation snapshot budget exceeded");
};

// frozen_daily_ranges — market.daily range task frozen inputs projected.
const frozenDailyRanges = (task) => {
  if (task.task.key.dataset !== DATASET_DAILY) return [];
  if (!["update_data", "backfill_data", "republish_data"]
      .includes(task.task.key.operation)) return [];
  const explicit = ["explicit-range-task-v1", "explicit-republish-task-v1"]
    .includes(task.task.key.contract_version);
  const isDefault = task.task.key.contract_version === "default-range-task-v1";
  if (!explicit && !isDefault)
    throw IntegrityError("unknown frozen range contract");
  const out = [];
  for (const item of task.items) {
    const frozen = item.spec.frozen_inputs;
    const sym = item.spec.target?.symbol;
    const entity = typeof sym === "string" ? entityOf(sym) : null;
    if (entity === null)
      throw IntegrityError("frozen range entity missing");
    const ranges = [];
    if (explicit) {
      const range = frozen.range;
      // serde maps the explicit frozen range's start/end into
      // start_date/end_date; a missing field fails decode (integrity).
      if (range === null || range === undefined ||
          typeof range.start !== "string" || typeof range.end !== "string")
        throw IntegrityError("frozen range: invalid type: null, expected a string");
      ranges.push({ start_date: range.start, end_date: range.end });
    } else {
      if (frozen.refresh_range !== undefined && frozen.refresh_range !== null)
        ranges.push(frozen.refresh_range);
      const gaps = frozen.proven_gap_ranges;
      if (!Array.isArray(gaps))
        throw IntegrityError("frozen proven gap ranges missing");
      for (const gap of gaps) ranges.push(gap.range);
    }
    for (const range of ranges) {
      if (typeof range?.start_date !== "string" ||
          typeof range?.end_date !== "string")
        throw IntegrityError("frozen range: invalid type: null, expected a string");
      out.push({
        item_id: item.spec.item_id,
        entity,
        base_version_id: frozen.base_version_id,
        base_integrity_sha256: frozen.base_integrity_sha256,
        start_date: range.start_date,
        end_date: range.end_date,
      });
    }
  }
  return out;
};

// frozen_yearly_inputs — market.yearly frozen inputs projected.
const frozenYearlyInputs = (task) => {
  if (task.task.key.dataset !== DATASET_YEARLY ||
      !["update_data", "backfill_data"].includes(task.task.key.operation))
    return [];
  if (task.task.key.contract_version !== "explicit-yearly-task-v1")
    throw IntegrityError("unknown frozen yearly contract");
  return task.items.map((item) => {
    const f = item.spec.frozen_inputs;
    const sym = item.spec.target?.symbol;
    const entity = typeof sym === "string" ? entityOf(sym) : null;
    if (entity === null)
      throw IntegrityError("frozen yearly entity missing");
    const intent = f.intent;
    let publication;
    if (intent === "HistoricalFixedBase") {
      const cid = f.comparison?.version_id;
      publication = { mode: "historical_fixed_base",
        comparison_version_id: typeof cid === "string" ? cid : null };
    } else if (intent !== null && typeof intent === "object" &&
               "CurrentHead" in intent) {
      const h = intent.CurrentHead;
      publication = { mode: "current_head",
        expected_yearly_head: h?.expected_yearly_head ?? null };
    } else {
      throw IntegrityError("daily intent in frozen yearly task");
    }
    return {
      item_id: item.spec.item_id,
      entity,
      base_daily_version_id: f.base_daily_version_id,
      base_integrity_sha256: f.base_integrity_sha256,
      years: f.years,
      publication,
      comparison: f.comparison ?? null,
    };
  });
};

// rust_decimal Decimal::round_dp(2) — MidpointNearestEven over the exact
// rational finished*100/total, emitted with scale 2 (trailing zeros kept).
const executionPercent = (finished, total) => {
  const num = BigInt(finished) * 10000n, den = BigInt(total);
  let q = num / den; const r = num % den;
  const dbl = r * 2n;
  if (dbl > den || (dbl === den && (q & 1n) === 1n)) q += 1n;
  return `${q / 100n}.${String(q % 100n).padStart(2, "0")}`;
};

export class Observer {
  constructor(repository, runId) {
    this.repository = repository;
    this.run_id = runId;
    this.cache = []; // VecDeque<Saved>
    this.sequence = 0;
    this.instance = `obs-${process.pid}-${Date.now() * 1e6 | 0}`;
  }
  id() {
    return `${this.instance}-${++this.sequence}`;
  }
  start(key, access, content) {
    return { token: "", key, access, time: new Date(),
      id: this.id(), offset: 0, content };
  }
  resume(token, key, access) {
    const s = this.cache.find((x) => x.token === token);
    if (!s) throw invalid("invalid/expired cursor");
    if (s.key !== key || !accessEq(s.access, access))
      throw invalid("cursor conditions/authorization changed");
    return s;
  }
  save(s) {
    const token = this.id();
    s.token = token;
    if (this.cache.length === 32) this.cache.shift();
    this.cache.push(s);
    return token;
  }
  // observe.rs:258 — page_size -> inverted range -> dataset require ->
  // resume-or-tasks -> filter/sort -> budget -> page.
  tasks(args, access) {
    pageSize(args.page_size);
    const fromNs = args.created_from === null ? null : args.created_from;
    const toNs = args.created_to === null ? null : args.created_to;
    if (fromNs !== null && toNs !== null && fromNs > toNs)
      throw invalid("inverted task time range");
    if (args.dataset !== null) access.require(args.dataset, null);
    const cursor = args.cursor;
    const key = `tasks:${serValue({ ...args, cursor: null })}`;
    let s;
    if (cursor !== null) {
      s = this.resume(cursor, key, access);
    } else {
      let items = this.repository.tasks(access)
        .map((f) => summary(f, this.run_id))
        .filter((x) =>
          (args.dataset === null || args.dataset === x.dataset) &&
          (args.operation === null || args.operation === x.operation) &&
          (args.status === null || args.status.includes(x.status)) &&
          (fromNs === null ||
            parseDateTime(x.created_at).instant >= fromNs) &&
          (toNs === null ||
            parseDateTime(x.created_at).instant <= toNs));
      items.sort((a, b) =>
        (b.created_at < a.created_at ? -1
          : b.created_at > a.created_at ? 1 : 0) ||
        (a.task_id < b.task_id ? -1 : a.task_id > b.task_id ? 1 : 0));
      budget(items);
      s = this.start(key, access, { kind: "tasks", data: items });
    }
    if (s.content.kind !== "tasks") throw invalid("cursor category");
    const items = s.content.data;
    const end = Math.min(s.offset + args.page_size, items.length);
    const output = items.slice(s.offset, end);
    const more = end < items.length;
    s.offset = end;
    const p = {
      snapshot_id: s.id,
      snapshot_time: serTs(s.time),
      items: output,
      next_cursor: null,
      page_complete: !more,
    };
    if (more) p.next_cursor = this.save(s);
    return p;
  }
  // observe.rs:318 — facts -> progress page -> verify_* -> page window.
  progress(args, access) {
    pageSize(args.page_size);
    const cursor = args.cursor;
    const key = `progress:${serValue({ ...args, cursor: null })}`;
    let s;
    if (cursor !== null) {
      s = this.resume(cursor, key, access);
    } else {
      const facts = this.repository.task(args.task_id, access);
      const unfinished = [];
      let finished = 0, succeeded = 0;
      for (const i of facts.snapshot.items) {
        if (i.outcome !== null && i.outcome !== undefined) {
          finished += 1;
          if (i.outcome.success) succeeded += 1;
        }
        if (!(i.outcome !== null && i.outcome !== undefined &&
              i.outcome.success)) {
          unfinished.push({
            item_id: i.spec.item_id,
            entity: typeof i.spec.target?.symbol === "string"
              ? entityOf(i.spec.target.symbol) : null,
            period: typeof i.spec.target?.period === "string"
              ? i.spec.target.period : null,
            progress_unit: i.spec.progress_unit,
            stage: String(i.stage).toLowerCase(),
            success: i.outcome === null || i.outcome === undefined
              ? null : i.outcome.success,
            error: i.outcome?.error ?? null,
          });
        }
      }
      unfinished.sort((a, b) =>
        a.item_id < b.item_id ? -1 : a.item_id > b.item_id ? 1 : 0);
      budget(unfinished);
      const publicationCheck = facts.snapshot.task.result?.publication_check;
      const stoppedCheck = facts.snapshot.task.result?.stopped_write_check;
      const p = {
        snapshot_id: "",
        snapshot_time: new Date(),
        task: summary(facts, this.run_id),
        total: facts.snapshot.items.length,
        finished,
        succeeded,
        failed: finished - succeeded,
        pending: facts.snapshot.items.filter((i) => i.stage === "pending").length,
        running: facts.snapshot.items.filter((i) => i.stage === "running").length,
        execution_percent: executionPercent(finished, facts.snapshot.items.length),
        unfinished,
        next_cursor: null,
        page_complete: false,
        publication_versions: facts.publications,
        frozen_daily_ranges: frozenDailyRanges(facts.snapshot),
        range_results: facts.range_results,
        frozen_yearly_inputs: frozenYearlyInputs(facts.snapshot),
        yearly_results: facts.yearly_results,
        stopped_write_check: stoppedCheck ?? null,
        publication_check: publicationCheck ?? null,
      };
      if (byteLen(p) > MAX_BYTES)
        throw invalid("progress snapshot byte budget exceeded");
      s = this.start(key, access, { kind: "progress", data: p });
    }
    if (s.content.kind !== "progress") throw invalid("cursor category");
    const p = s.content.data;
    this.repository.verifyVersions(p.publication_versions);
    this.repository.verifyRangeResults(p.range_results);
    this.repository.verifyYearlyResults(p.yearly_results);
    this.repository.verifyStoppedCheck(p.stopped_write_check);
    this.repository.verifyPublicationCheck(p.publication_check);
    const end = Math.min(s.offset + args.page_size, p.unfinished.length);
    const more = end < p.unfinished.length;
    const result = { ...p, unfinished: p.unfinished.slice(s.offset, end),
      snapshot_id: s.id, snapshot_time: serTs(s.time),
      page_complete: !more };
    s.offset = end;
    if (more) result.next_cursor = this.save(s);
    return result;
  }
  // observe.rs:440 — cursor -> plan -> resume-or-availability -> items ->
  // verify_versions -> page.
  availability(args, access) {
    const cursor = args.cursor;
    const plan = new QueryPlan({ ...args, cursor: null }, access);
    const key = `availability:${serValue(plan.args)}`;
    let s;
    if (cursor !== null) {
      s = this.resume(cursor, key, access);
    } else {
      const items = this.repository.availability(plan, access)
        .map((f) => availabilityItem(f, plan.args.dataset, this.run_id,
          plan.args.filter));
      budget(items);
      s = this.start(key, access, { kind: "availability", data: items });
    }
    if (s.content.kind !== "availability") throw invalid("cursor category");
    const items = s.content.data;
    this.repository.verifyVersions(
      items.filter((i) => i.version_ref !== null).map((i) => i.version_ref));
    const end = Math.min(s.offset + plan.args.page_size, items.length);
    const more = end < items.length;
    const result = items.slice(s.offset, end);
    s.offset = end;
    const p = {
      snapshot_id: s.id,
      snapshot_time: serTs(s.time),
      items: result,
      next_cursor: null,
      page_complete: !more,
    };
    if (more) p.next_cursor = this.save(s);
    return p;
  }
}

