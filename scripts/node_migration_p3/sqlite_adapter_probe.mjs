// NM-038 P3: validate the driver-neutral sqlite_adapter. Synthetic DBs
// only (NM-006 fixtures db18/db19/empty/garbage + fresh work/wal DBs).
// Exit nonzero on FAIL. Usage: node sqlite_adapter_probe.mjs <root> [selftest]
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const ROOT = process.argv[2] || ".";
const SELFTEST = process.argv[3] || null;
const { openDatabase, StorageError, ADAPTER_INFO, PINNED_DRIVER } =
  await import("../../src_node/storage/sqlite_adapter.mjs");

let failures = 0;
const norm = (v) => typeof v === "bigint" ? `n:${v}` :
  Array.isArray(v) ? v.map(norm) :
  v && typeof v === "object" ? Object.fromEntries(Object.entries(v).map(([k, x]) => [k, norm(x)])) : v;
const emit = (id, status, detail, expected = null, actual = null) => {
  expected = norm(expected); actual = norm(actual);
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ family: "sqlite_adapter", id, status, detail, expected, actual }));
};
const run = (id, fn) => {
  try { emit(id, "PASS", null, fn()); }
  catch (e) { emit(id, "FAIL", `${e.code || ""} ${e.message}`); }
};
const expectCode = (id, code, fn) => {
  try { fn(); emit(id, "FAIL", "expected rejection but call succeeded"); }
  catch (e) {
    emit(id, e instanceof StorageError && e.code === code ? "PASS" : "FAIL", `${e.code} ${e.message}`, code, e.code);
  }
};
const sha = (p) => createHash("sha256").update(readFileSync(p)).digest("hex");

const FIX = mkdtempSync("/tmp/nm38-sql-");
process.on("exit", () => rmSync(FIX, { recursive: true, force: true }));
// NM-006 fixtures (sha pinned in p1/platform evidence)
const SRC = "/dev/shm/nm006";
for (const f of ["db18.sqlite", "db19.sqlite", "empty.db", "garbage.bin"]) {
  copyFileSync(join(SRC, f), join(FIX, f));
}
const DB19 = join(FIX, "db19.sqlite"), DB18 = join(FIX, "db18.sqlite");
const EMPTY = join(FIX, "empty.db"), GARBAGE = join(FIX, "garbage.bin");

emit("adapter_info", "PASS", null, ADAPTER_INFO);
run("driver_version_pinned", () => {
  const lock = JSON.parse(readFileSync(join(ROOT, "package-lock.json"), "utf8"));
  const v = lock.packages["node_modules/better-sqlite3"].version;
  if (v !== PINNED_DRIVER) throw new Error(`lock=${v} pinned=${PINNED_DRIVER}`);
  return { lock: v, pinned: PINNED_DRIVER };
});

// explicit modes: readonly/readwrite never create
run("readonly_missing_no_create", () => {
  const p = join(FIX, "nonexistent.db");
  try { openDatabase(p); } catch (e) {
    if (e.code !== "unavailable") throw e;
    if (existsSync(p)) throw new Error("readonly open created a file");
    return { code: e.code, created: false };
  }
  throw new Error("readonly open of missing DB succeeded");
});
run("readwrite_missing_no_create", () => {
  const p = join(FIX, "nonexistent2.db");
  try { openDatabase(p, { mode: "readwrite" }); } catch (e) {
    if (e.code !== "unavailable") throw e;
    if (existsSync(p)) throw new Error("readwrite open created a file");
    return { code: e.code, created: false };
  }
  throw new Error("readwrite open of missing DB succeeded");
});

// schema fixtures open read-only; file bytes unchanged
run("open_schema19_readonly", () => {
  const before = sha(DB19);
  const d = openDatabase(DB19);
  const tables = d.query("SELECT name FROM sqlite_master WHERE type='table' ORDER BY 1").map(r => r.name);
  const uv = d.pragma("user_version");
  const jm = d.pragma("journal_mode");
  d.close();
  const need = ["tasks", "requests", "scope_occupancy"];
  const missing = need.filter(t => !tables.includes(t));
  if (missing.length) throw new Error(`missing tables: ${missing}`);
  if (sha(DB19) !== before) throw new Error("readonly open mutated the file");
  return { user_version: uv, journal_mode: jm, table_count: tables.length, bytes_unchanged: true };
});
run("open_schema18_readonly", () => {
  const before = sha(DB18);
  const d = openDatabase(DB18);
  const tables = d.query("SELECT name FROM sqlite_master WHERE type='table' ORDER BY 1").map(r => r.name);
  const uv = d.pragma("user_version");
  d.close();
  if (!tables.includes("tasks")) throw new Error("tasks missing");
  if (sha(DB18) !== before) throw new Error("readonly open mutated the file");
  return { user_version: uv, table_count: tables.length, bytes_unchanged: true };
});

// fail-closed detection
run("empty_db_zero_schema", () => {
  const before = statSync(EMPTY).size;
  const d = openDatabase(EMPTY);
  const tables = d.query("SELECT name FROM sqlite_master WHERE type='table'");
  const uv = d.pragma("user_version");
  d.close();
  if (tables.length !== 0 || Number(uv) !== 0 || statSync(EMPTY).size !== before) {
    throw new Error(`implicit write/schema: tables=${tables.length} uv=${uv}`);
  }
  return { tables: 0, user_version: 0, size_bytes: before };
});
expectCode("garbage_db_corrupt", "corrupt", () => {
  const d = openDatabase(GARBAGE);
  d.query("SELECT 1 FROM sqlite_master LIMIT 1"); // forces header read
});
expectCode("readonly_write_rejected", "readonly", () => {
  const d = openDatabase(DB19);
  try { d.exec("CREATE TABLE nope(x)"); } finally { d.close(); }
});
run("explicit_create_only", () => {
  const p = join(FIX, "created.db");
  const d = openDatabase(p, { mode: "create" });
  d.exec("CREATE TABLE t(x)");
  d.close();
  if (!existsSync(p)) throw new Error("create mode did not create");
  // now readwrite works on it
  const d2 = openDatabase(p, { mode: "readwrite" });
  d2.close();
  return { created: true };
});

// safe-integer precision (default ON) and opt-out boundary
run("bigint_precision_default", () => {
  const p = join(FIX, "ints.db");
  const d = openDatabase(p, { mode: "create" });
  d.exec("CREATE TABLE big(v INTEGER)");
  const vals = [9007199254740993n, -9223372036854775808n, 9223372036854775807n];
  for (const v of vals) d.run("INSERT INTO big(v) VALUES (?)", v);
  const back = d.query("SELECT v FROM big ORDER BY rowid").map(r => r.v);
  d.close();
  if (!back.every((b, i) => b === vals[i])) throw new Error(`precision loss: ${back}`);
  return { roundtrip: back.map(String), all_bigint: true };
});
run("safeintegers_off_boundary", () => {
  const p = join(FIX, "ints.db");
  const d = openDatabase(p, { safeIntegers: false });
  const back = d.query("SELECT v FROM big ORDER BY rowid").map(r => r.v);
  d.close();
  return { note: "safeIntegers:false loses i64>2^53 — deliberate opt-out", raw: back.map(String) };
});

// transactions
run("tx_commit", () => {
  const p = join(FIX, "work.db");
  const d = openDatabase(p, { mode: "create" });
  d.exec("CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT NOT NULL)");
  d.transaction(() => {
    const r = d.run("INSERT INTO t(v) VALUES (?)", "a");
    d.run("INSERT INTO t(v) VALUES (?)", "b");
  });
  const n = d.get("SELECT COUNT(*) c FROM t").c;
  d.close();
  if (n !== 2n) throw new Error(`commit failed: ${n}`);
  return { count: String(n) };
});
run("tx_rollback_atomic", () => {
  const p = join(FIX, "work.db");
  const d = openDatabase(p);
  try {
    d.transaction(() => {
      d.run("INSERT INTO t(v) VALUES ('c')");
      throw new Error("abort");
    });
    throw new Error("tx should have thrown");
  } catch (e) {
    const n = d.get("SELECT COUNT(*) c FROM t").c;
    d.close();
    if (n !== 2n) throw new Error(`partial state: ${n}`);
    return { rolled_back: true, count: String(n) };
  }
});

// WAL + checkpoint
run("wal_checkpoint", () => {
  const p = join(FIX, "wal.db");
  const d = openDatabase(p, { mode: "create" });
  const jm = d.pragma("journal_mode=WAL");
  d.exec("CREATE TABLE w(x)");
  const walDuring = existsSync(p + "-wal") ? statSync(p + "-wal").size : 0;
  d.checkpoint("TRUNCATE");
  const walAfter = existsSync(p + "-wal") ? statSync(p + "-wal").size : 0;
  d.close();
  if (jm !== "wal" || walDuring === 0) throw new Error(`jm=${jm} wal=${walDuring}`);
  return { journal_mode: jm, wal_bytes_during_write: walDuring, wal_bytes_after: walAfter };
});

// busy/locking: writer holds EXCLUSIVE tx; second writer -> busy
run("busy_locking", () => {
  const p = join(FIX, "wal.db");
  const a = openDatabase(p, { mode: "readwrite", busyTimeoutMs: 0 });
  const b = openDatabase(p, { mode: "readwrite", busyTimeoutMs: 0 });
  a.exec("BEGIN EXCLUSIVE");
  let busyErr = null;
  try { b.exec("INSERT INTO w VALUES (1)"); } catch (e) { busyErr = e.code; }
  a.exec("COMMIT");
  a.close(); b.close();
  if (busyErr !== "busy") throw new Error(`second writer not busy-rejected: ${busyErr}`);
  return { second_writer: busyErr };
});
run("busy_timeout_set", () => {
  const p = join(FIX, "wal.db");
  const d = openDatabase(p, { busyTimeoutMs: 250 });
  const v = Number(d.pragma("busy_timeout"));
  d.close();
  if (v !== 250) throw new Error(`busy_timeout=${v}`);
  return { busy_timeout: v };
});

// boundary rejects
expectCode("mode_unsupported", "invalid_input", () => openDatabase(DB19, { mode: "magic" }));
expectCode("path_nul", "invalid_input", () => openDatabase("a\0b"));
expectCode("path_empty", "invalid_input", () => openDatabase(""));
run("closed_handle_rejected", () => {
  const d = openDatabase(DB19);
  d.close();
  try { d.query("SELECT 1"); } catch (e) {
    if (e.code !== "closed") throw e;
    return { code: e.code };
  }
  throw new Error("closed handle still served queries");
});

// selftest
if (SELFTEST === "sql") {
  const p = join(FIX, "self.db");
  const d = openDatabase(p, { mode: "create" });
  d.exec("CREATE TABLE t(x)");
  d.close();
  emit("selftest_sql", existsSync(join(FIX, "ghost.db")) ? "PASS" : "FAIL", "expected missing file — STOP");
}

process.exitCode = failures === 0 ? 0 : 1;
