// NM-056 — sqlite_market/verification.rs verbatim port: read_facts
// (legacy + compact arms) producing publication_verification::Facts.
// One verified snapshot; fail-closed on any association divergence.
import { createHash } from "node:crypto";
import { MarketError } from "./record_codec.mjs";
import { readResult as readUpdateResult, readRecord, isCompactSchema } from "./market_read.mjs";
import { canonEq } from "../storage/task_repository.mjs";
import { readResult as readYearlyResult } from "./yearly_results.mjs";

const sha256 = (s) => createHash("sha256").update(s).digest("hex");
const integrity = (m) => new MarketError("integrity", `MARKET_INTEGRITY: ${m}`);
const taskErr = (code) => new MarketError(code, code.toUpperCase());
const storage = (m) => new MarketError("storage", `MARKET_STORAGE: ${m}`);

const LEGACY_KEY = { operation: "start_collection", dataset: "legacy_market_compound", contract_version: "task-acceptance-v1" };
const RANGE_CONTRACT = "explicit-range-task-v1";
const YEARLY_CONTRACT = "explicit-yearly-task-v1";

// publication_verification::contract_kind
const keyEq = (a, b) =>
  a && b && a.operation === b.operation && a.dataset === b.dataset &&
  a.contract_version === b.contract_version;
export const contractKind = (key) => {
  if (keyEq(key, LEGACY_KEY) ||
      (key.operation === "start_collection" && key.contract_version === "v1" &&
       (key.dataset === "market.daily" || key.dataset === "market.yearly"))) {
    return "legacy";
  }
  if (key.operation !== "update_data" && key.operation !== "backfill_data") {
    return null;
  }
  if (key.dataset === "market.daily" && key.contract_version === RANGE_CONTRACT) {
    return "daily";
  }
  if (key.dataset === "market.yearly" && key.contract_version === YEARLY_CONTRACT) {
    return "yearly";
  }
  return null;
};

const PUB_FIELDS = ["publication_id", "version_id", "task_id", "item_id", "run_id"];
const parsePublication = (c) => {
  if (c === null || typeof c !== "object" || Array.isArray(c)) {
    throw integrity("missing field `publication_id`");
  }
  for (const k of PUB_FIELDS) {
    if (typeof c[k] !== "string") throw integrity(`missing field \`${k}\``);
  }
  if (typeof c.expected_revision !== "number" ||
      !Number.isInteger(c.expected_revision)) {
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
  // expected_head/expected_yearly_head are Option<String> — absent or
  // null both deserialize. Non-string values are serde-invalid.
  if (variant === "Daily" &&
      i.Daily?.expected_head !== undefined &&
      i.Daily?.expected_head !== null &&
      typeof i.Daily.expected_head !== "string") {
    throw integrity("invalid type: expected Option<String>");
  }
  if (variant === "CurrentHead" &&
      (typeof i.CurrentHead?.expected_daily_head !== "string" ||
       (i.CurrentHead?.expected_yearly_head !== undefined &&
        i.CurrentHead?.expected_yearly_head !== null &&
        typeof i.CurrentHead.expected_yearly_head !== "string"))) {
    throw integrity("missing field `expected_daily_head`");
  }
  return c;
};

// Receipt serde-shape parse — required fields + integer task_revision.
const parseReceiptShape = (r) => {
  if (r === null || typeof r !== "object" || Array.isArray(r)) {
    throw integrity("missing field `publication_id`");
  }
  for (const k of ["publication_id", "version_id", "task_id"]) {
    if (typeof r[k] !== "string") throw integrity(`missing field \`${k}\``);
  }
  if (typeof r.task_revision !== "number" || !Number.isInteger(r.task_revision)) {
    throw integrity("missing field `task_revision`");
  }
  return r;
};

const occupancyKey = (version) =>
  JSON.stringify(["market", version.symbol,
    version.period === "Day" ? "day" : "year"]);
const parseJson = (s) => { try { return JSON.parse(s); } catch (e) { throw integrity(e.message); } };

// publication_verification::decision helpers live with the audit port in
// publication_checks.mjs; readFacts only builds the Facts value.

/** read_facts — task = bound snapshot (read_bound_task inside the same
 *  read snapshot). readBound(id) must provide the validated bound read
 *  ({task, items, scopes, claims}) for `id`. */
export const readFacts = (db, taskId, readBound) => {
  const task = readBound(taskId);
  if (!task) throw taskErr("task_not_found");
  const key = typeof task.task.command_key_json === "string"
    ? JSON.parse(task.task.command_key_json)
    : task.task.key;
  const kind = contractKind(key);
  if (kind === null) throw taskErr("unsupported_contract");
  const daily = kind === "daily";
  const legacy = kind === "legacy";
  const compact = isCompactSchema(db);
  if (compact) return readFactsCompact(db, task, daily, legacy);

  const rows = db.query(
    "SELECT 0 AS kind,result_id,item_id FROM market_update_results WHERE task_id=? " +
    "UNION ALL SELECT 1,result_id,item_id FROM yearly_execution_results WHERE task_id=?",
    taskId, taskId);
  const found = new Map();
  let published = 0;
  for (const row of rows) {
    const [k, id, item] = [Number(row.kind), row.result_id, row.item_id];
    if (legacy || (k === 0) !== daily ||
        !task.items.some((i) => i.item_id === item) ||
        found.has(item)) {
      throw integrity("publication result contract/item association differs");
    }
    const r = daily ? readUpdateResult(db, id) : readYearlyResult(db, id);
    if (r === null) {
      throw integrity("publication result disappeared inside snapshot");
    }
    const [audit, receipt] = r;
    const reused = audit.no_change;
    if (typeof reused !== "boolean") throw integrity("reuse fact missing");
    if (!reused) published += 1;
    found.set(item, {
      Committed: { receipt, reused_version: reused,
        evidence_sha256: sha256(JSON.stringify([audit, receipt])) },
    });
  }
  if (legacy) {
    const rows2 = db.query(
      "SELECT publication_id,version_id,item_id,command_json,receipt_json FROM publication_batches WHERE task_id=?",
      taskId);
    for (const row of rows2) {
      const [id, version, item, rawCommand, rawReceipt] =
        [row.publication_id, row.version_id, row.item_id,
         row.command_json, row.receipt_json];
      const commandValue = parseJson(rawCommand);
      const command = parsePublication(commandValue);
      const receipt = parseReceiptShape(parseJson(rawReceipt));
      const record = readRecord(db, version, true, true);
      const iv = command.intent;
      const intentOk =
        (iv === "HistoricalFixedBase" && record.version.period === "Year") ||
        (iv !== null && typeof iv === "object" && Object.keys(iv).length === 1 &&
         (("Daily" in iv && record.version.period === "Day") ||
          ("CurrentHead" in iv && record.version.period === "Year")));
      if (receipt.publication_id !== id || receipt.version_id !== version ||
          receipt.task_id !== taskId || command.task_id !== taskId ||
          command.item_id !== item || command.publication_id !== id ||
          command.version_id !== version ||
          task.task.owner_run_id !== command.run_id ||
          !intentOk ||
          !(receipt.task_revision > command.expected_revision) ||
          receipt.task_revision > Number(task.task.state_revision) ||
          found.has(item) ||
          !task.items.some((i) =>
            i.item_id === item && i.stage === "finished" &&
            i.outcome !== null && i.outcome.success === true &&
            canonEq(i.target,
              { symbol: record.version.symbol,
                period: record.version.period === "Day" ? "day" : "year" }) &&
            [...i.scope_keys].includes(occupancyKey(record.version)))) {
        throw integrity("legacy publication/task receipt association differs");
      }
      if (record.version.version_id !== version) {
        throw integrity("legacy fixed version differs");
      }
      published += 1;
      found.set(item, {
        Committed: { receipt, reused_version: false,
          evidence_sha256: sha256(JSON.stringify([record.raw, commandValue, receipt])) },
      });
    }
  }
  for (const table of ["publication_batches", "task_publications"]) {
    const count = Number(db.get(
      `SELECT count(*) AS n FROM ${table} WHERE task_id=?`, taskId).n);
    if (count !== published) {
      throw integrity("task publication associations do not match execution receipts");
    }
  }
  const items = [];
  for (const item of task.items) {
    const evidence = found.has(item.item_id)
      ? found.get(item.item_id)
      : "Missing";
    found.delete(item.item_id);
    if (evidence === "Missing" &&
        item.stage === "finished" && item.outcome !== null &&
        item.outcome.success === true) {
      throw integrity("successful item lacks publication execution evidence");
    }
    items.push({ item_id: item.item_id, evidence });
  }
  return { task, items };
};

const readFactsCompact = (db, task, daily, legacy) => {
  const taskId = task.task.task_id;
  if (legacy) {
    throw integrity("legacy contract facts cannot exist on compact storage");
  }
  const rows = db.query(
    "SELECT 0 AS kind,r.result_id,r.item_id,r.outcome,v.version_id,r.task_revision,r.result_digest " +
    "FROM market_update_results r " +
    "JOIN data_versions v ON v.version_key=r.result_version_key WHERE r.task_id=? " +
    "UNION ALL " +
    "SELECT 1,r.publication_id,r.item_id,r.outcome,v.version_id,r.task_revision,r.result_digest " +
    "FROM publication_results r " +
    "JOIN data_versions v ON v.version_key=r.result_version_key " +
    "WHERE r.task_id=? AND r.publication_kind=2",
    taskId, taskId);
  const found = new Map();
  let published = 0;
  for (const row of rows) {
    const [k, id, item, outcome, versionId, revision, digest] =
      [Number(row.kind), row.result_id, row.item_id, Number(row.outcome),
       row.version_id, row.task_revision, row.result_digest];
    if ((k === 0) !== daily ||
        !task.items.some((i) => i.item_id === item) ||
        found.has(item)) {
      throw integrity("publication result contract/item association differs");
    }
    const reused = outcome === 1;
    if (!reused) published += 1;
    const digestBuf = digest;
    if (!Buffer.isBuffer(digestBuf) || digestBuf.length !== 32) {
      throw integrity("stored result digest is malformed");
    }
    if (typeof revision !== "bigint" || revision < 0n ||
        revision > 18446744073709551615n) {
      throw integrity("stored task revision overflows");
    }
    found.set(item, {
      Committed: {
        receipt: {
          publication_id: id, version_id: versionId, task_id: taskId,
          task_revision: Number(revision),
        },
        reused_version: reused,
        evidence_sha256: Buffer.from(digestBuf).toString("hex"),
      },
    });
  }
  const count = Number(db.get(
    "SELECT count(*) AS n FROM task_publications WHERE task_id=?", taskId).n);
  if (count !== published) {
    throw integrity("task publication associations do not match execution receipts");
  }
  const items = [];
  for (const item of task.items) {
    const evidence = found.has(item.item_id)
      ? found.get(item.item_id)
      : "Missing";
    found.delete(item.item_id);
    if (evidence === "Missing" &&
        item.stage === "finished" && item.outcome !== null &&
        item.outcome.success === true) {
      throw integrity("successful item lacks publication execution evidence");
    }
    items.push({ item_id: item.item_id, evidence });
  }
  return { task, items };
};
