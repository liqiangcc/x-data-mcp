// NM-076 probe fixture — mirrors src/sqlite_market/tests.rs setup():
// schema-18 skeleton db (the w1 probe base) -> stage the issue79
// legacy record -> LegacyCollection task accept/claim/startItem ->
// Publisher::prepare equivalent -> commitPublication -> published
// `legacy-daily` + effective head for sh.600006/day/qfq.
// All writes go through already-landed ports (market_write.stage,
// TaskAdmission, market_commit.commitPublication) — the only raw SQL
// is the mark_validated step, replicating Publisher::prepare verbatim.
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

const here = dirname(fileURLToPath(import.meta.url));
const REPO = join(here, "..", "..");
export const NOW = "2026-01-02T00:00:00Z";

// Schema-18 skeleton seeded privately per probe run — the shared
// /dev/shm/nm006 path is mutable across lanes (another worker's
// probe polluted it with committed versions). `task_sqlite init`
// reproduces sqlite_market::tests::setup()'s fresh schema-18 db.
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
// Struct serializers — serde emits fields in DECLARATION order
// (CommandKey/Command/ItemSpec/FrozenWork are structs, not Value).
const serKey = (k) =>
  `{"operation":${JSON.stringify(k.operation)},"dataset":${JSON.stringify(k.dataset)},"contract_version":${JSON.stringify(k.contract_version)}}`;
const serCommand = (c) =>
  `{"key":${serKey(c.key)},"intent":${sortJson(c.intent)}}`;
const serItemSpec = (i) =>
  `{"item_id":${JSON.stringify(i.item_id)},"target":${sortJson(i.target)},"progress_unit":${JSON.stringify(i.progress_unit)},"frozen_inputs":${sortJson(i.frozen_inputs)},"scope_keys":${JSON.stringify([...i.scope_keys].sort())}}`;
const serWork = (w) =>
  `{"items":[${w.items.map(serItemSpec).join(",")}],"scopes":${JSON.stringify([...w.scopes].sort())}${w.shared_evidence != null ? `,"shared_evidence":${sortJson(w.shared_evidence)}` : ""}}`;

// LegacyCollection resolve() for {"symbols":["sh.600006"],
// "periods":["day"]} — verbatim derived frozen work.
const SCOPE = JSON.stringify(["market", "sh.600006", "day"]);
const COLLECTION_KEY = {
  operation: "start_collection",
  dataset: "legacy_market_compound",
  contract_version: "task-acceptance-v1",
};

/** Build a populated market db: published legacy-daily + head.
 *  Returns { db, adm, taskId, itemId, path }. */
export const buildMarketFixture = (path, skeleton) => {
  copyFileSync(skeleton, path);
  const capability = gateDatabase(path);
  const db = openDatabase(path, { mode: "readwrite" });
  const adm = new TaskAdmission(db, capability);

  // db.stage(&samples[0]["record"]) — issue79 legacy record.
  const samples = JSON.parse(readFileSync(
    join(REPO, "tests/fixtures/issue79/legacy-records.json"), "utf8"));
  stage(db, samples[0].record);

  // TaskControl::accept(LegacyCollection, payload) — normalized intent
  // + resolved FrozenWork verbatim.
  const intent = { symbols: ["sh.600006"], periods: ["day"], adjustment: "qfq" };
  const itemSpec = {
    item_id: SCOPE, target: { symbol: "sh.600006", period: "day" },
    progress_unit: "security_period",
    frozen_inputs: {}, scope_keys: new Set([SCOPE]),
  };
  const work = { items: [itemSpec], scopes: new Set([SCOPE]), shared_evidence: null };
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
  const itemId = SCOPE;

  // transition Claim (rev0->1) + StartItem (rev1->2).
  adm.claim(taskId, 0n, "run", NOW);
  adm.startItem(taskId, 1n, "run", itemId, NOW);

  // Publisher::prepare — candidate -> mark_validated -> plan.
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
      task_id: taskId, item_id: itemId, run_id: "run",
      expected_revision: 2,
      intent: { Daily: { expected_head: null } },
    },
    record,
    required_heads: [[scope, null]],
    promote: scope,
    base_digest: null,
  };
  db.transaction(() => commitPublication(db, pubPlan, NOW));

  return { db, adm, taskId, itemId, path };
};

/** Fresh gap task acceptance: GapVerificationContract
 *  normalize -> resolve -> TaskAdmission.admit — the
 *  TaskControl::accept composition (the service wrapper itself is an
 *  S2 non-goal). `repo` is a GapRepository over the same db.
 *  Returns { taskId, itemId, reused }. */
export const acceptGapTask = (repo, contract, requestId, payload, now = NOW) => {
  // TaskControl::accept order — verbatim:
  //   1. empty request_id -> InvalidCommand("empty request_id")
  //   2. read_request existing -> key mismatch or command mismatch ->
  //      REQUEST_CONFLICT; identical -> reused (resolve NOT re-run —
  //      replay never re-freezes the subject).
  //   3. normalize -> resolve -> work.validate -> accept_tx.
  if (typeof requestId !== "string" || requestId.trim() === "") {
    throw new StorageError("invalid_input", "INVALID_COMMAND: empty request_id");
  }
  const command = { key: contract.key(), intent: contract.normalize(payload) };
  const commandJson = serCommand(command);
  const existing = repo.db.get(
    "SELECT command_json,command_digest,task_id FROM requests WHERE request_id=?",
    requestId);
  if (existing) {
    const storedKeyJson = repo.db.get(
      "SELECT command_key_json FROM tasks WHERE task_id=?", existing.task_id)
      ?.command_key_json;
    if (storedKeyJson !== serKey(command.key) ||
        existing.command_json !== commandJson) {
      throw new StorageError("conflict", "REQUEST_CONFLICT");
    }
    return { taskId: existing.task_id, reused: true, command };
  }
  const work = contract.resolve(command.intent);
  // FrozenWork::validate — items/scopes non-empty.
  if (work.items.length === 0 || work.scopes.size === 0) {
    throw new StorageError("invalid_input", "INVALID_COMMAND: empty work");
  }
  const r = repo.adm.admit({
    requestId,
    commandJson,
    executionKeyJson: serKey(command.key),
    workJson: serWork(work),
    items: work.items.map((i) => ({
      item_id: i.item_id,
      target_json: sortJson(i.target),
      progress_unit: i.progress_unit,
      frozen_inputs_json: sortJson(i.frozen_inputs),
      scope_keys_json: JSON.stringify([...i.scope_keys].sort()),
    })),
    scopes: [...work.scopes].sort(),
    runId: "run",
    createdAt: now,
  });
  return { taskId: r.task_id, itemId: work.items[0].item_id, reused: !!r.reused };
};
