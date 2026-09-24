// NM-056 — sqlite_market/yearly_results.rs verbatim port:
// read_result / yearly_receipt / yearly_item_receipt. The compact
// (schema-19) arm dispatches to compact_results::read_yearly_audit.
import { createHash } from "node:crypto";
import * as RC from "./record_codec.mjs";
import * as MR from "./market_read.mjs";
import * as CW from "./compact_write.mjs";
import { canonEq } from "../storage/task_repository.mjs";

const sha256 = (s) => createHash("sha256").update(s).digest("hex");
const integrity = (m) => new RC.MarketError("integrity", `MARKET_INTEGRITY: ${m}`);

const CONTRACT_VERSION = "explicit-yearly-task-v1";

// sha256 of the serde_json::json!([audit, receipt]) byte form —
// two-element JSON array of the RAW strings (strings are serialized
// with escapes, not embedded).
const resultDigest = (audit, receiptJson) =>
  sha256(JSON.stringify([audit, receiptJson]));

// yearly_publication::tasks::frozen — json! shape, verbatim key order
const frozen = (base, digest, years, intent) => ({
  base_daily_version_id: base, base_integrity_sha256: digest,
  years, intent,
});

// yearly_publication::results::equivalent
const equivalent = (prior, next) => {
  const facts = (r) => {
    const annual = r.raw?.local_yearly_derivation_v1?.annual;
    if (!annual || annual.records === undefined) return null;
    return {
      symbol: r.version.symbol, source: r.version.source,
      adjustment: r.version.adjustment,
      adjustment_evidence: r.version.adjustment_evidence,
      units: r.version.unit_currency, bars: r.version.records,
      lineage: r.version.yearly_derivation,
      coverage: r.batch.coverage,
      annual_records: annual.records,
      analysis_coverage_v1: r.raw.analysis_coverage_v1,
    };
  };
  const a = facts(prior), b = facts(next);
  return a !== null && b !== null && canonEq(a, b);
};

// serde parse helpers — verbatim missing-field wording
const parsePublication = (c) => {
  if (c === null || typeof c !== "object" || Array.isArray(c)) {
    throw integrity("missing field `publication_id`");
  }
  for (const k of ["publication_id", "version_id", "task_id", "item_id", "run_id"]) {
    if (typeof c[k] !== "string") throw integrity(`missing field \`${k}\``);
  }
  if (typeof c.expected_revision !== "number" || !Number.isInteger(c.expected_revision)) {
    throw integrity("missing field `expected_revision`");
  }
  const i = c.intent;
  if (i === undefined) throw integrity("missing field `intent`");
  const variant = typeof i === "string" ? i
    : i !== null && typeof i === "object" ? Object.keys(i)[0] : null;
  if (!["Daily", "HistoricalFixedBase", "CurrentHead"].includes(variant)) {
    throw integrity(
      `unknown variant \`${variant}\`, expected \`Daily\` or ` +
      `\`HistoricalFixedBase\` or \`CurrentHead\``);
  }
  if (variant === "Daily" && typeof i.Daily?.expected_head !== "string") {
    throw integrity("missing field `expected_head`");
  }
  if (variant === "CurrentHead" &&
      (typeof i.CurrentHead?.expected_daily_head !== "string" ||
       typeof i.CurrentHead?.expected_yearly_head !== "string")) {
    throw integrity("missing field `expected_daily_head`");
  }
  return c;
};
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
const parseJson = (s) => {
  try { return JSON.parse(s); }
  catch (e) { throw integrity(e.message); }
};

export const readResult = (db, id) => {
  if (MR.isCompactSchema(db)) {
    const r = CW.readYearlyAudit(db, id);
    return r === null ? null : [r.audit, r.receipt];
  }
  const row = db.get(
    "SELECT task_id,item_id,base_daily_version_id,result_version_id,no_change,audit_json,result_sha256,receipt_json FROM yearly_execution_results WHERE result_id=?",
    id);
  if (!row) return null;
  const [taskId, itemId, baseId, versionId, noChange] =
    [row.task_id, row.item_id, row.base_daily_version_id,
     row.result_version_id, row.no_change === 1n];
  const raw = row.audit_json, storedDigest = row.result_sha256,
    receiptRaw = row.receipt_json;
  if (resultDigest(raw, receiptRaw) !== storedDigest) {
    throw integrity("yearly result digest mismatch");
  }
  const audit = parseJson(raw);
  const command = parsePublication(audit.command);
  const receipt = parseReceipt(parseJson(receiptRaw));
  if (audit.format !== "local-yearly-result-v1" ||
      audit.no_change !== noChange ||
      audit.base_daily_version_id !== baseId ||
      command.publication_id !== id || command.task_id !== taskId ||
      command.item_id !== itemId || command.version_id !== versionId ||
      receipt.publication_id !== id || receipt.version_id !== versionId ||
      receipt.task_id !== taskId) {
    throw integrity("yearly result association mismatch");
  }
  const base = MR.readRecord(db, baseId, true, true);
  const result = MR.readRecord(db, versionId, true, true);
  const candidateRaw =
    audit.candidate !== null && typeof audit.candidate === "object" &&
    audit.candidate.records !== null && typeof audit.candidate.records === "object" &&
    audit.candidate.records.$ref !== undefined
      ? MR.expand(db, null, audit.candidate)
      : audit.candidate;
  const candidate = RC.check(candidateRaw);
  MR.__validateBase(candidate, base);
  if (candidate.version.period !== "Year" ||
      audit.base_integrity_sha256 !== base.raw.integrity_sha256 ||
      audit.result_integrity_sha256 !== result.raw.integrity_sha256) {
    throw integrity("yearly result fixed version digest/period mismatch");
  }
  if (typeof audit.prior_version_id === "string") {
    const prior = MR.readRecord(db, audit.prior_version_id, true, true);
    if (audit.prior_integrity_sha256 !== prior.raw.integrity_sha256) {
      throw integrity("yearly prior digest mismatch");
    }
  }
  if (noChange) {
    if (audit.prior_version_id !== versionId || !equivalent(result, candidate)) {
      throw integrity("yearly reuse lacks equal fixed facts");
    }
  } else if (!canonEq(result.raw, candidate.raw)) {
    throw integrity("yearly published candidate mismatch");
  }
  const binding = MR.__readItemBinding(db, taskId, itemId);
  if (!binding) throw integrity("yearly result task missing");
  if (binding.head.state_revision < BigInt(receipt.task_revision) ||
      BigInt(receipt.task_revision) <= BigInt(command.expected_revision) ||
      binding.item.stage !== "finished" ||
      !(binding.item.outcome && binding.item.outcome.success === true)) {
    throw integrity("yearly result lacks successful task item");
  }
  if (binding.head.key.operation === "update_data" ||
      binding.head.key.operation === "backfill_data") {
    const years = [...new Set(candidate.version.records.map((r) =>
      Number(r.date.slice(0, 4))))].sort((a, b) => a - b);
    const expected = frozen(
      baseId, base.raw.integrity_sha256, years, command.intent);
    expected.comparison = audit.prior_version_id == null
      ? null
      : { version_id: audit.prior_version_id,
          integrity_sha256: audit.prior_integrity_sha256 };
    // Generation mode stamps source_generation_id at accept; legacy
    // tasks leave the key absent — carry it through only when present.
    const stamped = binding.item.spec.frozen_inputs?.source_generation_id;
    if (stamped !== undefined) expected.source_generation_id = stamped;
    if (binding.head.key.contract_version !== CONTRACT_VERSION ||
        binding.head.key.dataset !== "market.yearly" ||
        !canonEq(binding.item.spec.frozen_inputs, expected)) {
      throw integrity("yearly result differs from frozen task");
    }
  }
  if (!noChange) {
    const published = db.get(
      "SELECT receipt_json FROM publication_batches WHERE publication_id=?",
      id);
    if (!published) throw integrity("yearly publication missing");
    const pubReceipt = parseReceipt(parseJson(published.receipt_json));
    if (pubReceipt.publication_id !== receipt.publication_id ||
        pubReceipt.version_id !== receipt.version_id ||
        pubReceipt.task_id !== receipt.task_id ||
        pubReceipt.task_revision !== receipt.task_revision) {
      throw integrity("yearly publication receipt mismatch");
    }
  }
  return [audit, receipt];
};

export const yearlyReceipt = (db, id) => readResult(db, id)?.[1] ?? null;
export const yearlyItemReceipt = (db, task, item) => {
  const id = db.get(
    "SELECT result_id FROM yearly_execution_results WHERE task_id=? AND item_id=?",
    task, item)?.result_id;
  if (id === undefined) return null;
  const r = readResult(db, id);
  if (r === null) throw integrity("yearly item result missing");
  return r[1];
};
