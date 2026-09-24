// Node-side MarketRepository+YearlyRepository adapter for the
// yearly_publication pure layer. Reads assemble the ported surfaces:
//   legacy backend -> market_read.readRecord (superset windows OK per
//     the Rust contract)
//   compact backend -> compact_read.fixed/resultFacts (+ in-lane
//     window-years derivation — materialize_windowed/window_years are
//     NOT yet ported; the contract permits a superset read, so compact
//     fixed_window materializes the whole version and filters bars)
// Write arms: `stage` (ported), `mark_validated` (ported in-lane —
// single-row UPDATE + projection re-read), `transition_task_head`/
// `transition_task` (decideHead+persistDecided composition),
// `commit_yearly` -> YearlySeamError (the atomic commit arm lands with
// the runner wiring task).
import * as RC from "../market/record_codec.mjs";
import * as MR from "../market/market_read.mjs";
import * as CR from "../market/compact_read.mjs";
import * as MW from "../market/market_write.mjs";
import { openDatabase } from "../storage/sqlite_adapter.mjs";
import { StorageError } from "../storage/sqlite_adapter.mjs";
import { gateDatabase } from "../storage/schema_gate.mjs";
import { decideHead, persistDecided } from "../storage/task_repository.mjs";
import {
  mConflict, mIntegrity, mNotFound, tcTaskNotFound, YearlySeamError,
} from "./types.mjs";
import { yearlyItemReceipt, yearlyReceipt, readResult } from "../market/yearly_results.mjs";

const mapDriver = (e) =>
  e instanceof RC.MarketError || e instanceof StorageError ? e
    : new StorageError("unavailable", `MARKET_STORAGE: ${e.message}`);

export class YearlyRepositoryAdapter {
  /** @param db open sqlite adapter handle (readwrite) */
  constructor(db) {
    this.db = db;
    this.compact = MR.isCompactSchema(db);
  }
  // ---- reads ----
  candidate(id) { return MR.readRecord(this.db, id, false, true); }
  fixed(id) {
    return this.compact ? CR.fixed(this.db, id)
      : MR.readRecord(this.db, id, true, true);
  }
  fixed_facts(id) {
    if (this.compact) {
      // compact_repo::fixed_facts — version_key + result_facts, must be
      // Published, integrity digest required.
      const key = CR.versionKey(this.db, id);
      const facts = CR.resultFacts(this.db, key);
      if (facts.state !== "Published") throw mNotFound();
      if (facts.integrity_digest === null || facts.integrity_digest === undefined) {
        throw mIntegrity("published version lacks integrity digest");
      }
      return {
        version_id: facts.version_id, symbol: facts.entity,
        period: facts.period,
        integrity_sha256: hexOf(facts.integrity_digest),
        first_day: epochDate(facts.first_day),
        last_day: epochDate(facts.last_day),
      };
    }
    // sqlite_market::fixed_facts — header fields off the full read.
    const record = this.fixed(id);
    const recs = record.version.records;
    if (recs.length === 0) throw mIntegrity("fixed record has no bars");
    const digest = record.raw.integrity_sha256;
    if (typeof digest !== "string") {
      throw mIntegrity("fixed record lacks integrity digest");
    }
    return {
      version_id: record.version.version_id,
      symbol: record.version.symbol, period: record.version.period,
      integrity_sha256: digest,
      first_day: dayStr(recs[0].date),
      last_day: dayStr(recs[recs.length - 1].date),
    };
  }
  fixed_window(id, firstDay, lastDay) {
    // The contract permits returning a superset of the window — the
    // legacy Rust impl returns `fixed` whole; on compact we materialize
    // the full version (windowed decode is not yet ported) and restrict
    // `version.records`/`batch.bars` to the window.
    const record = this.fixed(id);
    if (!this.compact) return record; // legacy verbatim: full record
    const inWin = (b) => {
      const s = typeof b.date === "string" ? b.date : b.date.s;
      return s >= firstDay && s <= lastDay;
    };
    return {
      ...record,
      version: { ...record.version, records: record.version.records.filter(inWin) },
      batch: { ...record.batch, bars: record.batch.bars.filter(inWin) },
    };
  }
  fixed_window_years(id, firstDay, lastDay) {
    const record = this.fixed(id);
    const set = new Set();
    for (const b of record.version.records) {
      const s = typeof b.date === "string" ? b.date : b.date.s;
      if (s >= firstDay && s <= lastDay) set.add(Number(s.slice(0, 4)));
    }
    return [...set].sort((a, b) => a - b);
  }
  task(id) {
    const snap = readBoundTask(this.db, id);
    if (snap === null) throw tcTaskNotFound();
    return snap;
  }
  task_head(id) {
    const h = readTaskHead(this.db, id);
    if (h === null) throw tcTaskNotFound();
    return h;
  }
  task_item_binding(task, item) {
    return MR.__readItemBinding(this.db, task, item);
  }
  // ---- receipts (ported NM-056) ----
  yearly_receipt(id) { return yearlyReceipt(this.db, id); }
  yearly_item_receipt(task, item) { return yearlyItemReceipt(this.db, task, item); }
  read_result(id) { return readResult(this.db, id); }
  // ---- writes ----
  stage(raw) { MW.stage(this.db, raw); }
  mark_validated(record) {
    if (this.compact) {
      // compact_repo::mark_validated needs compact_versions::mark_validated
      // (state write) — not ported; seam per the commit-arm rule. The
      // content compare still runs first so a wrong candidate fails closed.
      const stored = CR.materializeChecked(this.db, record.version.version_id, true);
      if (!RC.__jsonEq(stored.raw, record.raw)) {
        throw mConflict("validated record differs");
      }
      throw new YearlySeamError("mark_validated(compact)",
        "compact version state write not ported");
    }
    // sqlite_market::mark_validated — projection re-read + guarded UPDATE.
    const current = MR.readProjection(this.db, record.version.version_id, false, true);
    if (!RC.__jsonEq(current.raw, record.raw)) {
      throw mIntegrity("candidate changed during validation");
    }
    const n = this.db.run(
      "UPDATE data_versions SET state='validated' WHERE version_id=? AND state IN ('staging','validated')",
      record.version.version_id).changes;
    if (n !== 1) throw mConflict("candidate already published");
  }
  /** transition_task_head — caller-held head+item, verified by the
   *  persisted revision (refreshed in place when stale). */
  transition_task_head(head, item, run, change) {
    const persisted = this.db.get(
      "SELECT state_revision FROM tasks WHERE task_id=?", head.task_id);
    if (!persisted) throw tcTaskNotFound();
    if (BigInt(persisted.state_revision) !== BigInt(head.state_revision)) {
      const fresh = readTaskHead(this.db, head.task_id);
      if (fresh === null) throw tcTaskNotFound();
      Object.assign(head, fresh);
    }
    const old = head.state_revision;
    this.db.run("BEGIN IMMEDIATE");
    try {
      decideHead(head, item ?? null, run, change, nowRfc3339());
      if (BigInt(head.state_revision) !== BigInt(old)) {
        persistDecided(this.db, head,
          (change.kind === "startItem" || change.kind === "finishItem")
            ? item : null,
          old);
        if (change.kind === "startItem") {
          this.db.run(
            "INSERT INTO task_item_execution_starts VALUES(?,?,?,?,?)",
            head.task_id, change.item_id, run, head.state_revision,
            nowRfc3339());
        }
      }
      this.db.run("COMMIT");
    } catch (e) {
      try { this.db.run("ROLLBACK"); } catch { /* rolled back */ }
      throw e;
    }
  }
  transition_task(task, revision, run, change) {
    throw new YearlySeamError("transition_task",
      "full-snapshot transition — used only by task_control paths, not yearly");
  }
  commit_yearly(plan) {
    throw new YearlySeamError("commit_yearly",
      "atomic yearly commit arm lands with the runner wiring task");
  }
}

// read_task_head — task-row projection (same columns as
// market_read's readItemBinding head).
const readTaskHead = (db, id) => {
  const h = db.get(
    "SELECT command_key_json,status,state_revision,accepted_run_id,owner_run_id,last_progress_at,result_json,item_count,items_pending,items_running,items_finished,items_succeeded,evidence_digest FROM tasks WHERE task_id=?",
    id);
  if (!h) return null;
  return {
    task_id: id,
    key: JSON.parse(h.command_key_json),
    status: h.status,
    state_revision: h.state_revision,
    accepted_run_id: h.accepted_run_id,
    owner_run_id: h.owner_run_id,
    last_progress_at: h.last_progress_at,
    result: h.result_json === null ? null : JSON.parse(h.result_json),
    counts: {
      total: h.item_count, pending: h.items_pending,
      running: h.items_running, finished: h.items_finished,
      succeeded: h.items_succeeded,
    },
    evidence_digest: h.evidence_digest,
  };
};
// sqlite_tasks::read_bound_task -> TaskSnapshot {head, items}.
const readBoundTask = (db, id) => {
  const head = readTaskHead(db, id);
  if (!head) return null;
  const rows = db.query(
    "SELECT item_id,target_json,progress_unit,frozen_inputs_json,scope_keys_json,stage,outcome_json FROM task_items WHERE task_id=? ORDER BY item_id",
    id);
  return {
    head,
    items: rows.map((r) => ({
      spec: {
        item_id: r.item_id, target: JSON.parse(r.target_json),
        progress_unit: r.progress_unit,
        frozen_inputs: JSON.parse(r.frozen_inputs_json),
        scope_keys: JSON.parse(r.scope_keys_json),
      },
      stage: r.stage,
      outcome: r.outcome_json === null ? null : JSON.parse(r.outcome_json),
    })),
  };
};
const dayStr = (d) => (typeof d === "string" ? d : d.s);
const hexOf = (v) =>
  typeof v === "string" ? v
    : Buffer.isBuffer(v) ? v.toString("hex")
    : Buffer.from(v).toString("hex");
const epochDate = (days) => {
  // compact_market::epoch_date — days since 1970-01-01 -> "YYYY-MM-DD"
  const d = new Date(Number(days) * 86400_000);
  return d.toISOString().slice(0, 10);
};
const nowRfc3339 = () => new Date().toISOString().replace("Z", "+00:00");

/** Open a yearly repository over an existing market db path — same
 *  fail-closed schema gate as market_read's MarketRepository.open. */
export const openYearlyRepository = (path) => {
  gateDatabase(path);
  const db = openDatabase(path, { mode: "readwrite" });
  return new YearlyRepositoryAdapter(db);
};
