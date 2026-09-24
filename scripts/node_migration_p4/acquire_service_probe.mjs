// NM-065 probe — src_node/acquire service orchestration over a Mirror
// fixture: open/reconcile/check_updates/acquire→advance→install,
// idempotent replay, capacity refusal, resume pins. JSONL results.
// Usage: node acquire_service_probe.mjs <dir>  (any writable dir)
import fs from "node:fs";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { AcquireService, Source } from "../../src_node/acquire/service.mjs";
import { openDatabase } from "../../src_node/storage/sqlite_adapter.mjs";

const dir = process.argv[2];
let failures = 0;
const emit = (id, status, detail, evidence = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ id, status, detail, evidence }));
};
const acheck = async (id, fn) => {
  try { emit(id, "PASS", null, await fn()); }
  catch (e) { emit(id, "FAIL", `${e.kind ?? e.code ?? e.name ?? ""} ${e.message}`); }
};

const sha = (b) => createHash("sha256").update(b).digest("hex");

// ---------- fixtures ----------
const mirror = join(dir, "mirror");
fs.mkdirSync(mirror, { recursive: true });
fs.writeFileSync(join(mirror, "base.zip"), Buffer.from("BASE-BYTES-1"));
fs.writeFileSync(join(mirror, "gbbq.zip"), Buffer.from("GBBQ-BYTES-1"));
fs.writeFileSync(join(mirror, "hsjday.zip"), Buffer.from("DAY-BYTES-1"));

const root = join(dir, "svc");
let svc;
let svc3; // fetch_resume fixture service — reused by reconcile tests

// ---------- open ----------
await acheck("open_initializes_dirs_db_mode", async () => {
  svc = AcquireService.open(root, Source.mirror(mirror));
  for (const d of ["staging", "generations", "pins"]) {
    if (!fs.statSync(join(root, d)).isDirectory())
      throw new Error(`missing ${d}`);
  }
  const st = fs.statSync(svc.dbPath());
  if ((st.mode & 0o777) !== 0o600)
    throw new Error(`db mode ${(st.mode & 0o777).toString(8)}`);
  return { db: svc.dbPath().endsWith("acquire.sqlite"), mode: "0600" };
});

// ---------- check_updates ----------
await acheck("check_updates_first_run_unknown", async () => {
  const r = svc.checkUpdates();
  if (r.files.length !== 3) throw new Error("expected 3 files");
  if (r.changed.length !== 0 || r.unknown.length !== 3)
    throw new Error(`changed=${r.changed} unknown=${r.unknown}`);
  for (const f of r.files) {
    if (f.status !== 200 || f.change !== "unknown" || !f.etag)
      throw new Error(`bad file ${JSON.stringify(f)}`);
  }
  return { unknown: r.unknown };
});

await acheck("check_updates_unchanged_on_etag", async () => {
  const r = svc.checkUpdates();
  if (r.unknown.length !== 0 || r.changed.length !== 0)
    throw new Error("expected all unchanged");
  for (const f of r.files)
    if (f.change !== "unchanged") throw new Error(`${f.role}: ${f.change}`);
  return { unchanged: r.files.map((f) => f.role) };
});

await acheck("check_updates_changed_on_etag", async () => {
  // Mirror etag = "<size:x>-<mtime:x>" — change size to flip it.
  fs.writeFileSync(join(mirror, "gbbq.zip"),
                   Buffer.from("GBBQ-BYTES-2-LONGER"));
  const r = svc.checkUpdates();
  if (r.changed.length !== 1 || r.changed[0] !== "gbbq")
    throw new Error(`changed=${JSON.stringify(r.changed)}`);
  const gbbq = r.files.find((f) => f.role === "gbbq");
  if (gbbq.change !== "changed") throw new Error("gbbq not changed");
  return { changed: r.changed };
});

await acheck("check_updates_fetch_failure_unknown", async () => {
  const failing = {
    head() { throw new Error("boom"); },
    fetch() { throw new Error("boom"); },
  };
  const svc2 = AcquireService.open(join(dir, "svc-fail"),
                                   Source.fetcher(failing));
  const r = svc2.checkUpdates();
  if (r.unknown.length !== 3) throw new Error("expected all unknown");
  for (const f of r.files)
    if (f.status !== 0 || f.etag !== null || f.change !== "unknown")
      throw new Error(`bad file ${JSON.stringify(f)}`);
  return { unknown: r.unknown };
});

// ---------- acquire + advance lifecycle ----------
await acheck("acquire_generation_accept", async () => {
  const a = svc.acquireGeneration("req-1");
  if (a.reused !== false || a.state !== "accepted" ||
      !a.task_id.startsWith("acq-") || a.request_id !== "req-1")
    throw new Error(JSON.stringify(a));
  const want = `acq-${sha("req-1").slice(0, 24)}`;
  if (a.task_id !== want) throw new Error(`task_id ${a.task_id}`);
  const t = svc.task(a.task_id);
  if (t.stage !== "fetch:base_industry" || t.kind !== "download_install")
    throw new Error(JSON.stringify(t));
  return { task_id: a.task_id };
});

await acheck("acquire_generation_replay", async () => {
  const first = svc.acquireGeneration("req-1");
  if (first.reused !== true) throw new Error("expected reused=true");
  // replay returns the ORIGINAL task state, whatever it is now
  return { task_id: first.task_id, reused: true };
});

await acheck("acquire_binding_lost", async () => {
  // request row exists, no task bound — Refused("request_binding_lost")
  const db = openDatabase(svc.dbPath(), { mode: "readwrite" });
  db.run("INSERT INTO requests(request_id,kind,payload,created_at) VALUES('orphan-req','acquire_generation','{}','2026-01-01T00:00:00+00:00')");
  db.close();
  try {
    svc.acquireGeneration("orphan-req");
    throw new Error("not refused");
  } catch (e) {
    if (e.kind !== "refused" || e.message !== "request_binding_lost" ||
        e.code() !== "request_binding_lost") throw e;
    return { err: e.message };
  }
});

await acheck("worker_once_fetch_chain", async () => {
  const seq = [];
  for (let i = 0; i < 3; i++) {
    const t = svc.workerOnce();
    seq.push({ stage: t.stage, state: t.state });
  }
  const stages = seq.map((s) => s.stage);
  if (JSON.stringify(stages) !==
      JSON.stringify(["fetch:gbbq", "fetch:day", "manifest"]))
    throw new Error(JSON.stringify(stages));
  const stage = join(root, "staging", "req-1");
  for (const f of ["base.zip", "gbbq.zip", "hsjday.zip"]) {
    if (!fs.statSync(join(stage, f)).isFile())
      throw new Error(`missing staged ${f}`);
  }
  const t = svc.task("acq-" + sha("req-1").slice(0, 24));
  const detail = JSON.parse(t.detail);
  const bytes = Object.values(detail.files).reduce((a, f) => a + f.bytes, 0);
  if (BigInt(t.bytes_done) !== BigInt(bytes))
    throw new Error(`bytes_done ${t.bytes_done} vs ${bytes}`);
  return { stages, bytes_done: t.bytes_done.toString() };
});

await acheck("worker_once_manifest_written", async () => {
  svc.workerOnce(); // manifest stage -> install
  const t = svc.task("acq-" + sha("req-1").slice(0, 24));
  if (t.stage !== "install") throw new Error(`stage ${t.stage}`);
  const mp = join(root, "staging", "req-1", "snapshot-manifest.json");
  const text = fs.readFileSync(mp, "utf8");
  const m = JSON.parse(text);
  if (m.schema !== "x-data-mcp.official-file-input.v1" ||
      m.files.length !== 3)
    throw new Error("bad manifest");
  const roles = m.files.map((f) => f.role);
  if (JSON.stringify(roles) !==
      JSON.stringify(["base_industry", "gbbq", "day"]))
    throw new Error(`roles ${JSON.stringify(roles)}`);
  // serde pretty shape: 2-space indent, declared field order, Z ts
  if (!text.startsWith('{\n  "schema": ')) throw new Error("not pretty");
  const f0 = m.files[0];
  const keys = Object.keys(f0);
  if (JSON.stringify(keys) !== JSON.stringify(
    ["role", "relative_path", "source_url", "acquisition_started_at",
     "acquisition_completed_at", "last_modified", "etag", "bytes",
     "sha256"])) throw new Error(`field order ${keys}`);
  if (!/Z"$/.test(JSON.stringify(f0.acquisition_started_at)))
    throw new Error("timestamp not chrono-Z");
  return { roles, manifest: true };
});

await acheck("worker_once_install_succeeds", async () => {
  const t = svc.workerOnce(); // install stage
  if (t.state !== "succeeded" || t.stage !== "done")
    throw new Error(JSON.stringify({ state: t.state, stage: t.stage }));
  if (!t.generation_id) throw new Error("no generation_id");
  const gen = svc.generations(); // the GenerationStore
  const gdir = join(root, "generations", t.generation_id);
  if (!fs.statSync(gdir).isDirectory())
    throw new Error("generation dir missing");
  if (fs.existsSync(join(root, "staging", "req-1")))
    throw new Error("staging residue");
  const outcome = svc.request("req-1").terminal_result;
  if (outcome?.state !== "succeeded" ||
      outcome?.generation_id !== t.generation_id)
    throw new Error(`outcome ${JSON.stringify(outcome)}`);
  const grow = svc.syncStatus().generations
    .find((g) => g.generation_id === t.generation_id);
  if (grow?.state !== "byte_verified")
    throw new Error(`gen state ${grow?.state}`);
  return { generation_id: t.generation_id, state: grow.state };
});

await acheck("worker_once_empty", async () => {
  const t = svc.workerOnce();
  if (t !== null) throw new Error("expected null");
  return { idle: true };
});

// ---------- replay returns recorded acceptance ----------
await acheck("acquire_replay_after_done", async () => {
  const a = svc.acquireGeneration("req-1");
  if (a.reused !== true || a.state !== "succeeded")
    throw new Error(JSON.stringify(a));
  return { state: a.state };
});

// ---------- request/task views ----------
await acheck("task_not_found_refused", async () => {
  try {
    svc.task("nope");
    throw new Error("not refused");
  } catch (e) {
    if (e.kind !== "refused" || e.message !== "task_not_found") throw e;
    return { err: e.message };
  }
});

await acheck("request_unbound", async () => {
  const v = svc.request("never-seen");
  if (v.bound !== false || v.task !== null || v.terminal_result !== null)
    throw new Error(JSON.stringify(v));
  return { bound: false };
});

// ---------- fetch resume (pin-match keeps staged bytes) ----------
await acheck("fetch_resume_keeps_pinned_bytes", async () => {
  // Fresh service with a counting fetcher over the mirror.
  const calls = { base: 0, gbbq: 0, day: 0 };
  const inner = new (await import("../../src_node/acquire/fetch.mjs"))
    .Mirror(mirror);
  const counting = {
    head: (u) => inner.head(u),
    fetch: (u, d) => {
      if (u.endsWith("base.zip")) calls.base++;
      if (u.endsWith("gbbq.zip")) calls.gbbq++;
      if (u.endsWith("hsjday.zip")) calls.day++;
      return inner.fetch(u, d);
    },
  };
  svc3 = AcquireService.open(join(dir, "svc-resume"),
                             Source.fetcher(counting));
  svc3.acquireGeneration("req-r");
  svc3.workerOnce(); // fetch:base_industry -> fetch:gbbq
  if (calls.base !== 1) throw new Error(`base fetches ${calls.base}`);
  // Rewind the stage with the SAME detail — resume must not refetch.
  const db = openDatabase(svc3.dbPath(), { mode: "readwrite" });
  db.run("UPDATE acquisition_tasks SET stage='fetch:base_industry' WHERE request_id='req-r'");
  db.close();
  svc3.workerOnce(); // re-advances fetch:base_industry via pin match
  if (calls.base !== 1)
    throw new Error(`base re-fetched: ${calls.base}`);
  const t = svc3.task("acq-" + sha("req-r").slice(0, 24));
  if (t.stage !== "fetch:gbbq") throw new Error(`stage ${t.stage}`);
  return { base_fetches: calls.base, stage: t.stage };
});

// ---------- reconcile ----------
await acheck("reconcile_running_returns_accepted", async () => {
  // svc3 has a task sitting in "running" (worker claimed it).
  const db = openDatabase(svc3.dbPath(), { mode: "readwrite" });
  db.run("UPDATE acquisition_tasks SET state='running' WHERE request_id='req-r'");
  db.close();
  const reopened = AcquireService.open(join(dir, "svc-resume"),
                                       Source.mirror(mirror));
  const t = reopened.task("acq-" + sha("req-r").slice(0, 24));
  if (t.state !== "accepted")
    throw new Error(`state ${t.state} — running not reset`);
  return { state: t.state };
});

await acheck("reconcile_adopts_orphan_install", async () => {
  // Drop the DB row for the installed generation -> reopen adopts it.
  const gid = svc.task("acq-" + sha("req-1").slice(0, 24)).generation_id;
  const db = openDatabase(svc.dbPath(), { mode: "readwrite" });
  db.run("DELETE FROM generations WHERE generation_id=?", gid);
  db.close();
  const reopened = AcquireService.open(root, Source.mirror(mirror));
  const g = reopened.syncStatus().generations
    .find((x) => x.generation_id === gid);
  if (!g || g.state !== "byte_verified")
    throw new Error(`orphan adoption failed: ${JSON.stringify(g)}`);
  return { generation_id: gid, state: g.state };
});

await acheck("reconcile_corrupt_dir_recorded", async () => {
  // Garbage generation dir -> recorded corrupt, never deleted.
  const bad = join(root, "generations", "deadbeef".repeat(8));
  fs.mkdirSync(bad);
  fs.writeFileSync(join(bad, "snapshot-manifest.json"), "{}");
  const reopened = AcquireService.open(root, Source.mirror(mirror));
  const g = reopened.syncStatus().generations
    .find((x) => x.generation_id === "deadbeef".repeat(8));
  if (!g || g.state !== "corrupt" ||
      !g.manifest_sha256.startsWith("unverified:"))
    throw new Error(`corrupt adoption ${JSON.stringify(g)}`);
  if (!fs.existsSync(bad)) throw new Error("corrupt dir deleted");
  return { state: g.state, kept: true };
});

await acheck("reconcile_pointer_realigns_state", async () => {
  // Row says consistency_verified; on-disk pointer names it active.
  const gid = svc.task("acq-" + sha("req-1").slice(0, 24)).generation_id;
  const db = openDatabase(svc.dbPath(), { mode: "readwrite" });
  db.run("UPDATE generations SET state='consistency_verified' WHERE generation_id=?", gid);
  db.close();
  fs.writeFileSync(join(root, "active-generation.json"), JSON.stringify({
    schema: "x-data-mcp.active-generation.v1", generation_id: gid,
    manifest_sha256: "m", activated_at: "2026-01-01T00:00:00Z",
    previous_generation_id: null, activation_request_id: "req-x",
  }));
  const reopened = AcquireService.open(root, Source.mirror(mirror));
  const g = reopened.syncStatus().generations
    .find((x) => x.generation_id === gid);
  if (g.state !== "active")
    throw new Error(`pointer realign failed: ${g.state}`);
  return { state: g.state };
});

await acheck("reconcile_cleans_terminal_staging", async () => {
  // Residue under a terminal request id is removed at open.
  const dir2 = join(root, "staging", "req-1");
  fs.mkdirSync(dir2, { recursive: true });
  fs.writeFileSync(join(dir2, "residue.bin"), "x");
  AcquireService.open(root, Source.mirror(mirror));
  if (fs.existsSync(dir2)) throw new Error("residue kept");
  return { removed: true };
});

// ---------- capacity ----------
await acheck("capacity_refused_at_six_hot", async () => {
  const db = openDatabase(svc.dbPath(), { mode: "readwrite" });
  for (let i = 0; i < 6; i++) {
    db.run("INSERT OR REPLACE INTO generations(generation_id,state,manifest_sha256,installed_at,updated_at) VALUES(?,?,?,?,?)",
      `cap-${i}`, "installed", "m", "2026-01-01T00:00:00+00:00",
      "2026-01-01T00:00:00+00:00");
  }
  db.close();
  try {
    svc.acquireGeneration("req-cap");
    throw new Error("not refused");
  } catch (e) {
    if (e.kind !== "refused" || e.message !== "generation_capacity" ||
        e.code() !== "generation_capacity") throw e;
  }
  const outcome = svc.request("req-cap").terminal_result;
  if (outcome?.state !== "refused" ||
      outcome?.reason_code !== "generation_capacity")
    throw new Error(`outcome ${JSON.stringify(outcome)}`);
  // cleanup so later runs stay sane
  const db2 = openDatabase(svc.dbPath(), { mode: "readwrite" });
  db2.run("DELETE FROM generations WHERE generation_id LIKE 'cap-%'");
  db2.close();
  return { refused: "generation_capacity", outcome_durable: true };
});

// ---------- unknown stage ----------
await acheck("advance_unknown_stage_fails_task", async () => {
  const svc4 = AcquireService.open(join(dir, "svc-unknown"),
                                   Source.mirror(mirror));
  svc4.acquireGeneration("req-u");
  const db = openDatabase(svc4.dbPath(), { mode: "readwrite" });
  db.run("UPDATE acquisition_tasks SET stage='bogus' WHERE request_id='req-u'");
  db.close();
  const t = svc4.workerOnce();
  if (t.state !== "failed" || !/unknown_stage/.test(t.error))
    throw new Error(`state=${t.state} err=${t.error}`);
  const outcome = svc4.request("req-u").terminal_result;
  if (outcome?.state !== "failed" || !/unknown_stage/.test(outcome.error))
    throw new Error(`outcome ${JSON.stringify(outcome)}`);
  return { state: t.state, error: t.error };
});

emit("summary", failures === 0 ? "PASS" : "FAIL", `${failures} failures`);
process.exit(failures ? 1 : 0);
