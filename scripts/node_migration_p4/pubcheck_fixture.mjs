// NM-098 probe fixture — mirrors src/sqlite_market/tests.rs setup()
// (same shape as the NM-076 gap fixture) plus the publication_checks
// test arms: an unresolved target task owned by a foreign run, a
// verify_publication_result check task accepted through the real
// contract path, and (optionally) a committed publication on the
// target so readFacts yields Committed evidence.
import { copyFileSync, existsSync, readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { openDatabase, StorageError } from "../../src_node/storage/sqlite_adapter.mjs";
import { gateDatabase } from "../../src_node/storage/schema_gate.mjs";
import { TaskAdmission } from "../../src_node/storage/task_repository.mjs";
import { stage } from "../../src_node/market/market_write.mjs";
import { commitPublication } from "../../src_node/market/market_commit.mjs";
import * as MR from "../../src_node/market/market_read.mjs";
import { snapshotCanon } from "../../src_node/storage/verification_storage.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const REPO = join(here, "..", "..");
export const NOW = "2026-01-02T00:00:00Z";

export const ensureSkeleton = (dir) => {
  const p = join(dir, "db18-skeleton.sqlite");
  if (!existsSync(p)) {
    execFileSync(join(REPO, "target/debug/task_sqlite"), ["init", p]);
  }
  return p;
};

// serde_json::to_string of a Value: map keys sort (BTreeMap).
export const sortJson = (v) => {
  if (v === null || typeof v !== "object") return JSON.stringify(v);
  if (Array.isArray(v)) return `[${v.map(sortJson).join(",")}]`;
  return `{${Object.keys(v).sort()
    .map((k) => `${JSON.stringify(k)}:${sortJson(v[k])}`).join(",")}}`;
};
// Struct serializers — serde emits fields in DECLARATION order.
export const serKey = (k) =>
  `{"operation":${JSON.stringify(k.operation)},"dataset":${JSON.stringify(k.dataset)},"contract_version":${JSON.stringify(k.contract_version)}}`;
export const serCommand = (c) =>
  `{"key":${serKey(c.key)},"intent":${sortJson(c.intent)}}`;
export const serItemSpec = (i) =>
  `{"item_id":${JSON.stringify(i.item_id)},"target":${sortJson(i.target)},"progress_unit":${JSON.stringify(i.progress_unit)},"frozen_inputs":${sortJson(i.frozen_inputs)},"scope_keys":${JSON.stringify([...i.scope_keys].sort())}}`;
export const serWork = (w) =>
  `{"items":[${w.items.map(serItemSpec).join(",")}],"scopes":${JSON.stringify([...w.scopes].sort())}${w.shared_evidence != null ? `,"shared_evidence":${sortJson(w.shared_evidence)}` : ""}}`;

const SCOPE = JSON.stringify(["market", "sh.600006", "day"]);
const COLLECTION_KEY = {
  operation: "start_collection",
  dataset: "legacy_market_compound",
  contract_version: "task-acceptance-v1",
};

/** TaskControl::accept composition (verbatim order): empty id ->
 *  InvalidCommand; read_request existing -> key mismatch /
 *  command mismatch -> REQUEST_CONFLICT, identical -> reused;
 *  normalize -> resolve -> work.validate -> admit. `contract` is a
 *  {key(), normalize(payload), resolve(intent)} instance; `repo` any
 *  {db, adm} seam with readRequest. */
export const controlAccept = (repo, contract, requestId, payload, now = NOW) => {
  if (typeof requestId !== "string" || requestId.trim() === "") {
    throw new StorageError("invalid_input", "INVALID_COMMAND: empty request_id");
  }
  const key = contract.key();
  const command = { key, intent: contract.normalize(payload) };
  const commandJson = serCommand(command);
  const existing = repo.db.get(
    "SELECT command_json,task_id FROM requests WHERE request_id=?",
    requestId);
  if (existing) {
    const storedKey = repo.db.get(
      "SELECT command_key_json FROM tasks WHERE task_id=?", existing.task_id)
      ?.command_key_json;
    if (storedKey !== serKey(key) || existing.command_json !== commandJson) {
      throw new StorageError("conflict", "REQUEST_CONFLICT");
    }
    return { taskId: existing.task_id, reused: true, command };
  }
  const work = contract.resolve(command.intent);
  if (work.items.length === 0 || work.scopes.length === 0) {
    throw new StorageError("invalid_input", "INVALID_COMMAND: empty work");
  }
  const r = repo.adm.admit({
    requestId,
    commandJson,
    executionKeyJson: serKey(key),
    workJson: serWork(work),
    items: work.items.map((i) => ({
      item_id: i.item_id,
      target_json: sortJson(i.target),
      progress_unit: i.progress_unit,
      frozen_inputs_json: sortJson(i.frozen_inputs),
      scope_keys_json: JSON.stringify([...i.scope_keys].sort()),
    })),
    scopes: [...work.scopes].sort(),
    runId: contract.runId,
    createdAt: now,
  });
  return { taskId: r.task_id, itemId: work.items[0].item_id, reused: !!r.reused };
};

/** tests::setup() — stage legacy record, accept+claim+startItem the
 *  LegacyCollection task; when `publish` is set, Publisher::prepare +
 *  commitPublication make the target terminal-succeeded. */
export const buildTargetFixture = (path, skeleton, { publish = false } = {}) => {
  copyFileSync(skeleton, path);
  const capability = gateDatabase(path);
  const db = openDatabase(path, { mode: "readwrite" });
  const adm = new TaskAdmission(db, capability);

  const samples = JSON.parse(readFileSync(
    join(REPO, "tests/fixtures/issue79/legacy-records.json"), "utf8"));
  stage(db, samples[0].record);

  const intent = { symbols: ["sh.600006"], periods: ["day"], adjustment: "qfq" };
  const itemSpec = {
    item_id: SCOPE, target: { symbol: "sh.600006", period: "day" },
    progress_unit: "security_period",
    frozen_inputs: {}, scope_keys: [SCOPE],
  };
  const work = { items: [itemSpec], scopes: [SCOPE], shared_evidence: null };
  adm.admit({
    requestId: "r",
    commandJson: serCommand({ key: COLLECTION_KEY, intent }),
    executionKeyJson: serKey(COLLECTION_KEY),
    workJson: serWork(work),
    items: [{
      item_id: SCOPE,
      target_json: sortJson(itemSpec.target),
      progress_unit: "security_period",
      frozen_inputs_json: "{}",
      scope_keys_json: JSON.stringify([SCOPE]),
    }],
    scopes: [SCOPE],
    runId: "run",
    createdAt: NOW,
  });
  const taskId = adm._db
    .get("SELECT task_id FROM requests WHERE request_id='r'").task_id;
  adm.claim(taskId, 0n, "run", NOW);
  adm.startItem(taskId, 1n, "run", SCOPE, NOW);

  if (publish) {
    const record = MR.readRecord(db, "legacy-daily", false, true);
    const n = db.run(
      "UPDATE data_versions SET state='validated' WHERE version_id=? AND state IN ('staging','validated')",
      "legacy-daily").changes;
    if (n !== 1) {
      throw new StorageError("conflict", "PUBLISH_PRECONDITION: candidate already published");
    }
    const scope = { symbol: "sh.600006", period: "Day" };
    const pubPlan = {
      command: {
        publication_id: "p", version_id: "legacy-daily",
        task_id: taskId, item_id: SCOPE, run_id: "run",
        expected_revision: 2,
        intent: { Daily: { expected_head: null } },
      },
      record,
      required_heads: [[scope, null]],
      promote: scope,
      base_digest: null,
    };
    db.exec("BEGIN IMMEDIATE");
    try { commitPublication(db, pubPlan, NOW); db.exec("COMMIT"); }
    catch (e) { try { db.exec("ROLLBACK"); } catch {} throw e; }
  }
  return { db, adm, taskId, itemId: SCOPE, path };
};

/** The check task built through the real admission contract
 *  (PublicationVerification.normalize/resolve) + controlAccept —
 *  mirrors TaskControl::accept(verify_publication_result). */
export const acceptCheckTask = (repo, contract, requestId, targetId, now = NOW) => {
  const key = contract.key();
  const command = { key, intent: contract.normalize({ target_task_id: targetId }) };
  const work = contract.resolve(command.intent);
  const r = repo.adm.admit({
    requestId,
    commandJson: serCommand(command),
    executionKeyJson: serKey(key),
    workJson: serWork(work),
    items: work.items.map((i) => ({
      item_id: i.item_id,
      target_json: sortJson(i.target),
      progress_unit: i.progress_unit,
      frozen_inputs_json: sortJson(i.frozen_inputs),
      scope_keys_json: JSON.stringify([...i.scope_keys].sort()),
    })),
    scopes: [...work.scopes].sort(),
    runId: contract.runId,
    createdAt: now,
  });
  return { taskId: r.task_id, itemId: work.items[0].item_id };
};
