// NM-006 P1 platform validation probe — driver-neutral minimal adapter +
// behavior matrix against Rust-initialized schema-18/19 SQLite DBs.
// Emits one JSON line per case: {case, status, detail, evidence}.
// Usage: node scripts/node_migration_p1/sqlite_probe.mjs <dir-with-dbs>
// Dbs required in dir: db18.sqlite, db19.sqlite (task_sqlite init/init19),
// empty.db (0 bytes). All synthetic — no production/live DB access.
import Database from "better-sqlite3";
import { statSync, existsSync } from "node:fs";
import { join } from "node:path";

const dir = process.argv[2];
const emit = (c, status, detail, evidence = null) =>
  console.log(JSON.stringify({ case: c, status, detail, evidence }));

// Driver-neutral adapter surface: open/close, exec, query, run,
// transaction(fn) with commit/rollback, pragma. Underlying driver is
// better-sqlite3; callers never see driver types beyond bigint.
class Db {
  constructor(path, opts = {}) {
    this.db = new Database(path, {
      readonly: !!opts.readonly,
      fileMustExist: !!opts.readonly, // writable open may create; init stays explicit
    });
    if (opts.safeIntegers) this.db.defaultSafeIntegers(true);
  }
  exec(sql) { return this.db.exec(sql); }
  query(sql, ...p) { return this.db.prepare(sql).all(...p); }
  get(sql, ...p) { return this.db.prepare(sql).get(...p); }
  run(sql, ...p) { return this.db.prepare(sql).run(...p); }
  pragma(s) { return this.db.pragma(s, { simple: true }); }
  tx(fn) { return this.db.transaction(fn)(); }
  close() { return this.db.close(); }
}

const run = (c, fn) => {
  try { emit(c, "PASS", null, fn()); }
  catch (e) { emit(c, "FAIL", `${e.code || ""} ${e.message}`); }
};
const expectErr = (c, re, fn) => {
  try { fn(); emit(c, "FAIL", "expected rejection but call succeeded"); }
  catch (e) {
    const msg = `${e.code || ""} ${e.message}`;
    emit(c, re.test(msg) ? "PASS" : "FAIL", msg);
  }
};

// environment facts
emit("runtime", "INFO", null, {
  node: process.version, npm_execpath: process.env.npm_execpath || null,
  better_sqlite3: (await import("better-sqlite3/package.json", { with: { type: "json" } })).default.version,
});
try {
  await import("node:sqlite");
  emit("node:sqlite", "AVAILABLE");
} catch (e) {
  emit("node:sqlite", "NOT_RUN", `${e.code}: requires Node >=22.5; system Node is v20.19.2`);
}

// C1: schema-19 DB opens; expected tables + user_version present
run("open_schema19_readonly", () => {
  const d = new Db(join(dir, "db19.sqlite"), { readonly: true });
  const tables = d.query("SELECT name FROM sqlite_master WHERE type='table' ORDER BY 1").map(r => r.name);
  const uv = d.pragma("user_version");
  const jm = d.pragma("journal_mode");
  d.close();
  const need = ["tasks", "requests", "scope_occupancy"];
  const missing = need.filter(t => !tables.includes(t));
  if (missing.length) throw new Error(`missing tables: ${missing}`);
  return { user_version: uv, journal_mode: jm, table_count: tables.length, required_present: need };
});

// C2: schema-18 DB opens likewise
run("open_schema18_readonly", () => {
  const d = new Db(join(dir, "db18.sqlite"), { readonly: true });
  const tables = d.query("SELECT name FROM sqlite_master WHERE type='table' ORDER BY 1").map(r => r.name);
  const uv = d.pragma("user_version");
  d.close();
  if (!tables.includes("tasks")) throw new Error("tasks missing");
  return { user_version: uv, table_count: tables.length };
});

// C3: init-boundary — empty file has no recognized schema (fail-closed detection)
run("empty_db_schema_detection", () => {
  const d = new Db(join(dir, "empty.db"), { readonly: true });
  const tables = d.query("SELECT name FROM sqlite_master WHERE type='table'").map(r => r.name);
  const uv = d.pragma("user_version");
  d.close();
  // Rust-side rejects with STORAGE_UNAVAILABLE; adapter must see zero schema
  if (tables.length !== 0) throw new Error(`unexpected tables: ${tables}`);
  return { tables: 0, user_version: uv, rust_equivalent: "STORAGE_UNAVAILABLE: connection pragmas differ from task contract" };
});

// C4: garbage file rejected at open/read
expectErr("garbage_db_rejected", /not a database|SQLITE_NOTDB/i, () => {
  const p = join(dir, "garbage.bin");
  const d = new Db(p, { readonly: true });
  d.query("SELECT 1 FROM sqlite_master LIMIT 1"); // forces header read
  d.close();
});

// C5: transaction commit — insert+request pair persists
run("tx_commit", () => {
  const p = join(dir, "work.db");
  const d = new Db(p);
  d.exec(`CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT NOT NULL);
          CREATE TABLE r(k TEXT PRIMARY KEY, task_id INTEGER UNIQUE REFERENCES t(id));`);
  d.tx(() => {
    const info = d.run("INSERT INTO t(v) VALUES (?)", "a");
    d.run("INSERT INTO r(k,task_id) VALUES (?,?)", "req-1", info.lastInsertRowid);
  });
  const rows = d.query("SELECT t.v, r.k FROM t JOIN r ON r.task_id=t.id");
  const n = d.get("SELECT COUNT(*) c FROM t").c;
  d.close();
  if (n !== 1 || rows[0].k !== "req-1") throw new Error("commit verification failed");
  return { rows };
});

// C6: transaction rollback — failure inside tx leaves no partial state
run("tx_rollback", () => {
  const p = join(dir, "work.db");
  const d = new Db(p);
  try {
    d.tx(() => {
      d.run("INSERT INTO t(v) VALUES ('b')");
      d.run("INSERT INTO r(k,task_id) VALUES ('req-1', 999)"); // dup k -> abort
    });
    throw new Error("tx should have thrown");
  } catch (e) {
    const n = d.get("SELECT COUNT(*) c FROM t").c;
    d.close();
    if (n !== 1) throw new Error(`partial state leaked: t count=${n}`);
    return { thrown: e.code || e.message, rows_after: n };
  }
});

// C7: BigInt/integer precision — values beyond 2^53 round-trip exactly
run("bigint_precision", () => {
  const p = join(dir, "work.db");
  const d = new Db(p, { safeIntegers: true });
  d.exec("CREATE TABLE IF NOT EXISTS big(v INTEGER)");
  const vals = [9007199254740993n, -9223372036854775808n, 9223372036854775807n, 12345678901234567n];
  for (const v of vals) d.run("INSERT INTO big(v) VALUES (?)", v);
  const back = d.query("SELECT v FROM big ORDER BY rowid").map(r => r.v);
  d.close();
  const ok = back.every((b, i) => b === vals[i]);
  if (!ok) throw new Error(`precision loss: ${back}`);
  return { roundtrip: back.map(String), all_bigint: back.every(v => typeof v === "bigint") };
});

// C8: default (non-safeIntegers) precision loss boundary — document exact behavior
run("default_int64_boundary", () => {
  const p = join(dir, "work.db");
  const d = new Db(p); // safeIntegers OFF
  const back = d.query("SELECT v FROM big ORDER BY rowid").map(r => r.v);
  d.close();
  return { note: "without safeIntegers, i64 beyond 2^53 loses precision — adapter must opt in", raw: back };
});

// C9: WAL mode — journal_mode switch creates wal on write, checkpoint merges
run("wal_checkpoint", () => {
  const p = join(dir, "wal.db");
  const d = new Db(p);
  const jm = d.pragma("journal_mode=WAL");
  d.exec("CREATE TABLE w(x)"); // produces wal
  const walDuring = existsSync(p + "-wal") ? statSync(p + "-wal").size : 0;
  d.pragma("wal_checkpoint(TRUNCATE)");
  const walAfter = existsSync(p + "-wal") ? statSync(p + "-wal").size : 0;
  d.close();
  return { journal_mode: jm, wal_bytes_during_write: walDuring, wal_bytes_after_truncate_checkpoint: walAfter };
});

// C10: busy/locking — writer holds exclusive tx; second conn write -> BUSY
run("busy_locking", () => {
  const p = join(dir, "wal.db");
  const a = new Db(p);
  const b = new Db(p);
  a.pragma("busy_timeout=0");
  b.pragma("busy_timeout=0");
  a.exec("BEGIN EXCLUSIVE");
  let busyErr = null;
  try { b.exec("INSERT INTO w VALUES (1)"); } catch (e) { busyErr = e.code || e.message; }
  // reader behavior while writer holds EXCLUSIVE (WAL: EXCLUSIVE blocks readers too)
  let readOk = null;
  try { readOk = b.get("SELECT COUNT(*) c FROM w").c; } catch (e) { readOk = `rejected: ${e.code} ${e.message}`; }
  a.exec("COMMIT");
  a.close(); b.close();
  if (busyErr === null) throw new Error("concurrent write was not rejected");
  return { writer_holds: "EXCLUSIVE", second_writer: busyErr, concurrent_read: readOk };
});

// C11: read-only enforcement — write attempt on readonly handle rejected
expectErr("readonly_write_rejected", /READONLY|readonly/i, () => {
  const d = new Db(join(dir, "db19.sqlite"), { readonly: true });
  d.exec("CREATE TABLE nope(x)");
});

// C12: explicit upgrade boundary — non-schema ops must not auto-migrate
run("no_implicit_migration", () => {
  const d = new Db(join(dir, "empty.db"), { readonly: true });
  const uv = d.pragma("user_version");
  d.close();
  // opening readonly must not create schema; verify file still 0 bytes
  const sz = statSync(join(dir, "empty.db")).size;
  if (uv !== 0 || sz !== 0) throw new Error(`implicit write detected: uv=${uv} size=${sz}`);
  return { user_version: uv, size_bytes: sz, note: "init/upgrade must be explicit ops, mirroring Rust boundary" };
});
