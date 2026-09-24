#!/usr/bin/env node
// NM-092 fault-injection — Node-side driver, mirrors fault_oracle.rs
// subcommands over the ported surfaces.
import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { openDatabase, StorageError } from "../../src_node/storage/sqlite_adapter.mjs";
import { DatabaseLease } from "../../src_node/storage/database_lease.mjs";
import { Endpoint } from "../../src_node/local_runtime/paths.mjs";
import { AcquireService, Source } from "../../src_node/acquire/service.mjs";
import { coordinatorOpenExisting, schema19Identity } from "../../src_node/market/compact_runtime.mjs";
import { TaskAdmission } from "../../src_node/storage/task_repository.mjs";

// acquisition_tasks/generations row digest — same column set as the
// Rust oracle's db_rows (timestamps excluded: legitimately differ).
const dbRows = (root) => {
  const db = openDatabase(join(root, "acquire.sqlite"), { mode: "readonly" });
  const tasks = db.query(
    "SELECT task_id,kind,state,generation_id,stage,error,detail FROM acquisition_tasks ORDER BY task_id")
    .map((r) => [r.task_id, r.kind, r.state, r.generation_id, r.stage, r.error, r.detail]
      .map((x) => x === null || x === undefined ? "" : String(x)).join("|"));
  const generations = db.query(
    "SELECT generation_id,state,consistency_status FROM generations ORDER BY generation_id")
    .map((r) => [r.generation_id, r.state, r.consistency_status]
      .map((x) => x === null || x === undefined ? "" : String(x)).join("|"));
  db.close();
  return { tasks, generations };
};

const out = (v) => console.log(JSON.stringify(v));
const errJson = (e) =>
  ({ error: e instanceof Error ? `${e.message}` : String(e) });
const [cmd, ...args] = process.argv.slice(2);

try {
  if (cmd === "lease_hold" || cmd === "lease_try") {
    const lease = DatabaseLease.acquire(args[0]);
    out({ acquired: true });
    if (cmd === "lease_hold") {
      process.stdin.resume(); // keep the loop alive until parent EOF
      await new Promise((resolve) => process.stdin.on("end", resolve));
      lease.close?.();
    }
  } else if (cmd === "bind") {
    const lease = DatabaseLease.acquire(join(args[0], args[1]));
    const ep = await Endpoint.bind(join(args[0], "svc.sock"), lease);
    out({ bound: true });
    process.stdin.resume();
    await new Promise((resolve) => process.stdin.on("end", resolve));
    ep.close?.();
  } else if (cmd === "commit_kill") {
    const db = openDatabase(args[0], { mode: "readwrite" });
    db.run("PRAGMA journal_mode=WAL");
    db.run("CREATE TABLE IF NOT EXISTS k(id INTEGER PRIMARY KEY, v TEXT NOT NULL)");
    db.run("BEGIN IMMEDIATE");
    const n = Number(args[1] ?? 8);
    const killAt = Number(args[2] ?? n / 2);
    for (let i = 0; i < n; i++) {
      db.run("INSERT INTO k(v) VALUES(?)", `row-${i}`);
      if (i + 1 === killAt) process.kill(process.pid, "SIGKILL");
    }
    db.run("COMMIT");
    out({ committed: true });
  } else if (cmd === "reopen_state") {
    const db = openDatabase(args[0], { mode: "readwrite" });
    const n = Number(db.get("SELECT count(*) AS c FROM k")?.c ?? -1n);
    const vals = db.query("SELECT v FROM k ORDER BY id").map((r) => r.v);
    const h = createHash("sha256");
    for (const v of vals) h.update(String(v));
    out({
      rows: n, sha256: h.digest("hex"),
      wal_left: existsSync(`${args[0]}-wal`),
      first: vals[0] ?? null, last: vals[vals.length - 1] ?? null,
    });
    db.close();
  } else if (cmd === "acquire_run") {
    const svc = AcquireService.open(args[0], Source.mirror(args[1]));
    svc.acquireGeneration("req-acq-1");
    for (let i = 0; i < 20; i++) if (svc.workerOnce() === null) break;
    svc.close?.();
    out({ opened: true, ...dbRows(args[0]) });
  } else if (cmd === "acquire_open") {
    const svc = AcquireService.open(args[0], Source.mirror(args[1]));
    const status = svc.syncStatus();
    svc.close?.();
    out({ opened: true, status, ...dbRows(args[0]) });
  } else if (cmd === "schema18_init") {
    // Rust SqliteTasks::initialize() — schema-18 migrations. Node has
    // no schema-init path (documented "future task"); the probe seeds
    // the 18 file via the Rust oracle only — Node is exercised on the
    // compact_open refusal side.
    throw new Error("schema18_init is rust-only (no Node init path)");
  } else if (cmd === "compact_open") {
    const lease = DatabaseLease.acquire(args[0]);
    coordinatorOpenExisting(args[0], lease, schema19Identity());
    out({ opened: true });
    lease.close?.();
  } else if (cmd === "batch_kill") {
    const db = openDatabase(args[0], { mode: "readwrite" });
    db.exec("PRAGMA journal_mode=WAL");
    db.run("CREATE TABLE IF NOT EXISTS t1(id INTEGER PRIMARY KEY, v TEXT)");
    db.run("CREATE TABLE IF NOT EXISTS t2(id INTEGER PRIMARY KEY, v TEXT)");
    db.exec("BEGIN IMMEDIATE");
    for (let i = 0; i < 6; i++) {
      db.run("INSERT INTO t1(v) VALUES(?)", `a-${i}`);
      db.run("INSERT INTO t2(v) VALUES(?)", `b-${i}`);
      if (i === 2) process.kill(process.pid, "SIGKILL");
    }
    db.exec("COMMIT");
    out({ committed: true });
  } else if (cmd === "batch_state") {
    const db = openDatabase(args[0], { mode: "readwrite" });
    const n1 = Number(db.query("SELECT count(*) c FROM t1")[0]?.c ?? -1);
    const n2 = Number(db.query("SELECT count(*) c FROM t2")[0]?.c ?? -1);
    const wal_left = existsSync(`${args[0]}-wal`); // measure while open
    db.close();
    out({ t1: n1, t2: n2, wal_left });
  } else if (cmd === "reopen_close") {
    const db = openDatabase(args[0], { mode: "readwrite" });
    const n = Number(db.query("SELECT count(*) c FROM k")[0]?.c ?? -1);
    db.close();
    out({ rows: n, wal_after_close: existsSync(`${args[0]}-wal`) });
  } else if (cmd === "bind_twice") {
    const dir = args[0];
    const lease = DatabaseLease.acquire(join(dir, args[1]));
    const ep = await Endpoint.bind(join(dir, "svc.sock"), lease);
    const ep2 = await Endpoint.bind(join(dir, "svc.sock"), lease);
    out({ double_bound: true });
    ep2.close?.(); ep.close?.(); lease.close?.();
  } else if (cmd === "candidates") {
    const repo = TaskAdmission.open(args[0], { mode: "readwrite" });
    const c = repo.candidates(args[1] ?? "run-x");
    out({ candidates: c.length });
    repo.close();
  } else {
    console.error("usage: fault_helpers.mjs <subcommand> ...");
    process.exit(2);
  }
} catch (e) {
  out(errJson(e));
  process.exit(42);
}
