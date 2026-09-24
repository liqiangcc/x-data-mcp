// NM-064 probe — src_node/acquire store + wire types, asserting
// src/acquire/{store,wire}.rs semantics verbatim. JSONL results.
// Usage: node scripts/node_migration_p4/acquire_store_probe.mjs <dir>
// (any writable dir — the acquire store has no fs-whitelist guard;
//  /dev/shm or a private tmpdir both work)
import fs from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { Db, AcquireStoreError } from "../../src_node/acquire/store.mjs";
import * as wire from "../../src_node/acquire/wire.mjs";
import { openDatabase } from "../../src_node/storage/sqlite_adapter.mjs";

const dir = process.argv[2];
let failures = 0;
const emit = (id, status, detail, evidence = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ id, status, detail, evidence }));
};
const acheck = async (id, fn) => {
  try { emit(id, "PASS", null, await fn()); }
  catch (e) { emit(id, "FAIL", `${e.code ?? e.name ?? ""} ${e.message}`); }
};
const T = (y, m, d, hh, mm, ss, ms = 0) =>
  new Date(Date.UTC(y, m, d, hh, mm, ss, ms));
const T0 = T(2026, 0, 1, 0, 0, 0), T1 = T(2026, 0, 1, 0, 0, 1),
      T2 = T(2026, 0, 1, 0, 0, 2), T3 = T(2026, 0, 1, 0, 0, 3, 123);

const dbPath = join(dir, "acquire.db");

// ---------- lifecycle ----------
await acheck("exists_readable_absent", async () => {
  const db = new Db(dbPath);
  const v = db.existsReadable();
  if (v !== false) throw new Error("expected false on missing file");
  return { v };
});

await acheck("open_creates_schema", async () => {
  const db = new Db(dbPath);
  db.open();
  if (!fs.existsSync(dbPath)) throw new Error("db not created");
  if (db.existsReadable() !== true) throw new Error("not readable");
  db.open(); // idempotent re-open
  const raw = openDatabase(dbPath);
  const tables = raw.query(
    "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
    .map((r) => r.name);
  raw.close();
  const want = ["acquisition_tasks", "generations", "request_outcomes",
                "requests", "seen"];
  if (JSON.stringify(tables) !== JSON.stringify(want))
    throw new Error(`tables ${JSON.stringify(tables)}`);
  return { tables };
});

await acheck("exists_readable_corrupt_is_error", async () => {
  const p = join(dir, "garbage.db");
  fs.writeFileSync(p, "not a database file at all", { mode: 0o600 });
  try {
    new Db(p).existsReadable();
    throw new Error("expected storage error");
  } catch (e) {
    if (!(e instanceof AcquireStoreError) || e.kind !== "storage" ||
        !/acquire store unavailable/.test(e.message)) throw e;
    return { err: e.message };
  }
});

// ---------- request idempotency ----------
const db = new Db(dbPath);

await acheck("accept_request_new", async () => {
  const { request, reused } = db.acceptRequest("r1", "acquire", "{}", T1);
  if (reused !== false) throw new Error("expected reused=false");
  if (request.request_id !== "r1" || request.kind !== "acquire")
    throw new Error("bad row");
  if (request.created_at.getTime() !== T1.getTime())
    throw new Error("created_at not round-tripped");
  return { reused, created_at: request.created_at.toISOString() };
});

await acheck("accept_request_replay_same", async () => {
  const { request, reused } = db.acceptRequest("r1", "acquire", "{}", T2);
  if (reused !== true) throw new Error("expected reused=true");
  if (request.created_at.getTime() !== T1.getTime())
    throw new Error("replay must return ORIGINAL row (created_at T1)");
  return { reused, original_ts_kept: true };
});

await acheck("accept_request_conflict_payload", async () => {
  try {
    db.acceptRequest("r1", "acquire", "{\"different\":true}", T3);
    throw new Error("not rejected");
  } catch (e) {
    if (!(e instanceof AcquireStoreError) || e.kind !== "request_conflict" ||
        e.message !== "request_id already bound to a different command")
      throw e;
    return { err: e.message };
  }
});

await acheck("accept_request_conflict_kind", async () => {
  try {
    db.acceptRequest("r1", "verify", "{}", T3);
    throw new Error("not rejected");
  } catch (e) {
    if (e.kind !== "request_conflict") throw e;
    return { err: e.message };
  }
});

await acheck("request_lookup", async () => {
  const r = db.request("r1");
  if (!r || r.kind !== "acquire" || r.payload !== "{}")
    throw new Error("lookup failed");
  if (db.request("missing") !== null) throw new Error("phantom row");
  return { found: true };
});

// ---------- outcomes ----------
await acheck("outcome_bind_first_wins", async () => {
  db.bindOutcome("r1", "{\"first\":1}", T2);
  db.bindOutcome("r1", "{\"second\":2}", T3); // INSERT OR IGNORE — no-op
  const v = db.outcome("r1");
  if (v?.first !== 1) throw new Error(`expected first outcome, got ${JSON.stringify(v)}`);
  if (db.outcome("none") !== null) throw new Error("phantom outcome");
  return { first_wins: true };
});

await acheck("record_outcome_json_roundtrip", async () => {
  const reply = wire.activateReply({
    request_id: "r2", generation_id: "g1", state: "active",
    reused: false, activated_at: "2026-01-01T00:00:00+00:00",
    previous_generation_id: null,
  });
  db.recordOutcome("r2", JSON.parse(wire.serJson(reply)), T3);
  const v = db.outcome("r2");
  if (v?.state !== "active" || v?.request_id !== "r2")
    throw new Error("roundtrip failed");
  return { outcome: v };
});

// ---------- task lifecycle ----------
const task1 = {
  task_id: "t1", request_id: "r1", kind: "acquire", state: "open",
  generation_id: null, stage: "fetch", bytes_done: 0n,
  error: null, detail: null, created_at: T1, updated_at: T1,
};
await acheck("task_create_lookup", async () => {
  db.createTask(task1);
  const got = db.task("t1");
  if (!got || got.state !== "open" || got.stage !== "fetch")
    throw new Error("task() failed");
  const byReq = db.taskByRequest("r1");
  if (!byReq || byReq.task_id !== "t1") throw new Error("taskByRequest failed");
  if (got.bytes_done !== 0n) throw new Error(`bytes_done ${got.bytes_done}`);
  return { task_id: got.task_id };
});

await acheck("task_update_whole_row", async () => {
  const t = { ...task1, state: "staged", stage: "verify",
              bytes_done: 4096n, updated_at: T2,
              detail: "{\"note\":\"halfway\"}" };
  db.updateTask(t);
  const got = db.task("t1");
  if (got.state !== "staged" || got.bytes_done !== 4096n ||
      got.detail !== "{\"note\":\"halfway\"}" ||
      got.updated_at.getTime() !== T2.getTime())
    throw new Error(`update not whole-row: ${JSON.stringify(got, (k,v)=>typeof v==="bigint"?v.toString():v)}`);
  return { state: got.state, bytes_done: got.bytes_done.toString() };
});

await acheck("tasks_in_state_order", async () => {
  // insert out of order; expect ORDER BY created_at
  db.createTask({ ...task1, task_id: "t2", request_id: "r2",
                  state: "open", created_at: T3, updated_at: T3 });
  db.createTask({ ...task1, task_id: "t0", request_id: "r3",
                  state: "open", created_at: T0, updated_at: T0 });
  const rows = db.tasksIn(["open", "missing-state"]);
  const ids = rows.map((r) => r.task_id);
  if (JSON.stringify(ids) !== JSON.stringify(["t0", "t2"]))
    throw new Error(`order ${JSON.stringify(ids)}`);
  return { order: ids };
});

await acheck("task_u64_bigint_exact", async () => {
  const big = 2n ** 53n + 5n; // beyond f64 safe range
  db.updateTask({ ...task1, bytes_done: big, updated_at: T3 });
  const got = db.task("t1");
  if (got.bytes_done !== big)
    throw new Error(`lost precision: ${got.bytes_done}`);
  return { bytes_done: got.bytes_done.toString() };
});

await acheck("task_bytes_negative_clamped", async () => {
  // Rust reads .max(0): negative stored bytes clamp to 0 on read.
  const raw = openDatabase(dbPath, { mode: "readwrite" });
  raw.run("UPDATE acquisition_tasks SET bytes_done=-9 WHERE task_id='t1'");
  raw.close();
  const got = db.task("t1");
  if (got.bytes_done !== 0n)
    throw new Error(`expected clamp 0n, got ${got.bytes_done}`);
  return { clamped: true };
});

// ---------- generations ----------
await acheck("generation_upsert_and_order", async () => {
  const g = (id, state, installed) => ({
    generation_id: id, state, manifest_sha256: "m-" + id,
    consistency_status: null, consistency_reasons: null,
    decision_sha256: null, evidence_json: null,
    acquisition_request_id: null, installed_at: installed,
    updated_at: installed,
  });
  db.upsertGeneration(g("g2", "installed", T2));
  db.upsertGeneration(g("g0", "installed", T0));
  db.upsertGeneration(g("g1", "retired", T1));
  // upsert path: same id overwrites
  db.upsertGeneration(g("g1", "installed", T1));
  const all = db.generations();
  if (JSON.stringify(all.map((x) => x.generation_id)) !==
      JSON.stringify(["g0", "g1", "g2"]))
    throw new Error("generations() not ordered by installed_at");
  const inst = db.generationsIn(["installed"]);
  if (inst.length !== 3 ||
      JSON.stringify(inst.map((x) => x.generation_id)) !==
      JSON.stringify(["g0", "g1", "g2"]))
    throw new Error("generations_in order/filter failed");
  const g1 = db.generation("g1");
  if (g1.state !== "installed") throw new Error("upsert conflict path failed");
  if (db.generation("nope") !== null) throw new Error("phantom generation");
  return { order: all.map((x) => x.generation_id), g1_state: g1.state };
});

// ---------- seen ----------
await acheck("seen_upsert_roundtrip", async () => {
  db.recordSeen({ role: "bars", etag: "e1", last_modified: "m1",
                  content_length: 100n, seen_at: T1 });
  let s = db.seen("bars");
  if (!s || s.etag !== "e1" || s.content_length !== 100n)
    throw new Error("first read failed");
  db.recordSeen({ role: "bars", etag: "e2", last_modified: null,
                  content_length: null, seen_at: T2 });
  s = db.seen("bars");
  if (s.etag !== "e2" || s.last_modified !== null ||
      s.content_length !== null || s.seen_at.getTime() !== T2.getTime())
    throw new Error("upsert did not overwrite");
  if (db.seen("other") !== null) throw new Error("phantom seen");
  return { etag: s.etag };
});

// ---------- timestamp fidelity ----------
await acheck("timestamp_rfc3339_shape", async () => {
  const raw = openDatabase(dbPath);
  const r = raw.get("SELECT created_at FROM requests WHERE request_id='r1'");
  raw.close();
  if (!/\+00:00$/.test(r.created_at))
    throw new Error(`not chrono rfc3339: ${r.created_at}`);
  // T1 has 0 ms -> no fraction (chrono AutoSi drops it)
  if (r.created_at !== "2026-01-01T00:00:01+00:00")
    throw new Error(`unexpected shape ${r.created_at}`);
  return { stored: r.created_at };
});

await acheck("timestamp_ms_fraction", async () => {
  db.acceptRequest("r-ms", "k", "p", T3); // T3 has .123 ms
  const raw = openDatabase(dbPath);
  const r = raw.get("SELECT created_at FROM requests WHERE request_id='r-ms'");
  raw.close();
  if (r.created_at !== "2026-01-01T00:00:03.123+00:00")
    throw new Error(`fraction wrong: ${r.created_at}`);
  return { stored: r.created_at };
});

// ---------- wire types ----------
await acheck("wire_parse_ok", async () => {
  const p = wire.parse.AcquireGeneration({
    contract_version: "v1", request_id: "r-9" });
  if (p.request_id !== "r-9" || p.contract_version !== "v1")
    throw new Error("parse failed");
  return { parsed: p };
});

await acheck("wire_deny_unknown_fields", async () => {
  try {
    wire.parse.AcquireGeneration({
      contract_version: "v1", request_id: "r", extra: 1 });
    throw new Error("not rejected");
  } catch (e) {
    if (!/unknown field `extra`/.test(e.message)) throw e;
    return { err: e.message };
  }
});

await acheck("wire_missing_required", async () => {
  try {
    wire.parse.AcquireGeneration({ contract_version: "v1" });
    throw new Error("not rejected");
  } catch (e) {
    if (!/missing field `request_id`/.test(e.message)) throw e;
    return { err: e.message };
  }
});

await acheck("wire_option_absent_or_null", async () => {
  const a = wire.parse.ActivateGeneration({
    contract_version: "v1", request_id: "r", generation_id: "g" });
  if (a.expected_previous !== null) throw new Error("absent !-> null");
  const b = wire.parse.ActivateGeneration({
    contract_version: "v1", request_id: "r", generation_id: "g",
    expected_previous: null });
  if (b.expected_previous !== null) throw new Error("null !-> null");
  const c = wire.parse.ActivateGeneration({
    contract_version: "v1", request_id: "r", generation_id: "g",
    expected_previous: "g-prev" });
  if (c.expected_previous !== "g-prev") throw new Error("value lost");
  return { absent: a.expected_previous, explicit_null: b.expected_previous };
});

await acheck("wire_option_wrong_type", async () => {
  try {
    wire.parse.ActivateGeneration({
      contract_version: "v1", request_id: "r", generation_id: "g",
      expected_previous: 5 });
    throw new Error("not rejected");
  } catch (e) {
    if (!/invalid type: integer `5`, expected option/.test(e.message)) throw e;
    return { err: e.message };
  }
});

await acheck("wire_version_variant", async () => {
  try {
    wire.parse.SyncStatus({ contract_version: "v2" });
    throw new Error("not rejected");
  } catch (e) {
    if (!/unknown variant `"v2"`, expected `v1`/.test(e.message)) throw e;
    return { err: e.message };
  }
});

await acheck("wire_workeronce_empty_only", async () => {
  const ok = wire.parse.WorkerOnce({});
  try {
    wire.parse.WorkerOnce({ stray: 1 });
    throw new Error("not rejected");
  } catch (e) {
    if (!/unknown field `stray`, there are no fields/.test(e.message)) throw e;
    return { parsed: ok, err: e.message };
  }
});

await acheck("wire_invalid_type_string", async () => {
  try {
    wire.parse.GetAcquisitionTask({ contract_version: "v1", task_id: 5 });
    throw new Error("not rejected");
  } catch (e) {
    if (!/invalid type: integer `5`, expected a string/.test(e.message))
      throw e;
    return { err: e.message };
  }
});

await acheck("wire_reply_parse_roundtrip", async () => {
  const reply = wire.activateReply({
    request_id: "r", generation_id: "g", state: "active",
    reason_code: null, detail: null, manifest_sha256: "abc",
    activated_at: "2026-01-01T00:00:00+00:00",
    previous_generation_id: "g0", reused: false });
  const text = wire.serJson(reply);
  const back = wire.parse.ActivateReply(JSON.parse(text));
  if (back.previous_generation_id !== "g0" || back.reused !== false)
    throw new Error("roundtrip mismatch");
  return { text };
});

await acheck("wire_serialize_field_order", async () => {
  const v = wire.acquisitionTaskView({
    task_id: "t", request_id: "r", kind: "k", state: "s",
    bytes_done: 5n, created_at: "c", updated_at: "u" });
  const keys = Object.keys(v);
  const want = ["task_id", "request_id", "kind", "state",
    "generation_id", "stage", "bytes_done", "error", "detail",
    "created_at", "updated_at"];
  if (JSON.stringify(keys) !== JSON.stringify(want))
    throw new Error(`order ${JSON.stringify(keys)}`);
  const text = wire.serJson(v);
  if (!/"bytes_done":5/.test(text)) throw new Error(`u64 emit: ${text}`);
  return { keys, text };
});

await acheck("wire_u16_range", async () => {
  for (const bad of [65536, -1, 1.5, "x"]) {
    try {
      wire.remoteFile({ role: "r", url: "u", status: bad,
        change: wire.Change.Changed });
      throw new Error(`status ${bad} accepted`);
    } catch (e) {
      if (!/invalid type/.test(e.message)) throw e;
    }
  }
  const ok = wire.remoteFile({ role: "r", url: "u", status: 200,
    change: wire.Change.Changed });
  return { rejected: [65536, -1, 1.5, "x"], ok_status: ok.status };
});

await acheck("wire_change_enum_variants", async () => {
  try {
    wire.remoteFile({ role: "r", url: "u", status: 200, change: "moved" });
    throw new Error("not rejected");
  } catch (e) {
    if (!/unknown variant `"moved"`/.test(e.message)) throw e;
    return { err: e.message };
  }
});

emit("summary", failures === 0 ? "PASS" : "FAIL", `${failures} failures`);
process.exit(failures ? 1 : 0);
