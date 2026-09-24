// NM-038 P3 — driver-neutral SQLite adapter over the pinned
// better-sqlite3@11.10.0 (NM-006 decision). Callers see only this
// surface; driver types never escape except bigint row values.
//
// Fail-closed rules:
// - modes are explicit: "readonly" / "readwrite" (fileMustExist) /
//   "create" (the ONLY mode that may create a file)
// - safeIntegers defaults ON — without it i64 > 2^53 silently loses
//   precision (NM-006 C8); opt out deliberately with safeIntegers:false
// - no implicit schema upgrade, no fallback DB creation
// - driver version + runtime gated at load

import Database from "better-sqlite3";
import { existsSync, statSync } from "node:fs";
import { createRequire } from "node:module";

const req = createRequire(import.meta.url);
const DRIVER_VERSION = req("better-sqlite3/package.json").version;
export const PINNED_DRIVER = "11.10.0";
if (DRIVER_VERSION !== PINNED_DRIVER) {
  throw new Error(
    `sqlite_adapter: unsupported driver better-sqlite3@${DRIVER_VERSION}; pinned is ${PINNED_DRIVER} (NM-006)`);
}
const NODE_MAJOR = Number(process.versions.node.split(".")[0]);
if (!(NODE_MAJOR >= 20)) {
  throw new Error(`sqlite_adapter: unsupported runtime node ${process.versions.node}; requires >=20`);
}

/** Driver-neutral typed error. code: busy|readonly|corrupt|unavailable|
 *  constraint|closed|invalid_input|error (driver code preserved). */
export class StorageError extends Error {
  constructor(code, message, driverCode = null) {
    super(message);
    this.name = "StorageError";
    this.code = code;
    this.driverCode = driverCode;
  }
}
const mapErr = (e) => {
  if (e instanceof StorageError) return e;
  const dc = e.code || "SQLITE_ERROR";
  const kind =
    /BUSY|LOCKED/.test(dc) ? "busy" :
    /READONLY/.test(dc) ? "readonly" :
    /NOTADB|NOTDB|CORRUPT|HEADER/.test(dc) ? "corrupt" :
    /CANTOPEN|IOERR|PERM/.test(dc) ? "unavailable" :
    /CONSTRAINT/.test(dc) ? "constraint" : "error";
  return new StorageError(kind, e.message, dc);
};

export const OPEN_MODES = Object.freeze(["readonly", "readwrite", "create"]);

/** Open a database. path must be a wellformed non-empty string with no
 *  NUL. mode defaults to "readonly" (never creates, never writes).
 *  Returns a handle; throws StorageError, never driver errors. */
export function openDatabase(path, opts = {}) {
  if (typeof path !== "string" || path === "" || !path.isWellFormed() || path.includes("\0")) {
    throw new StorageError("invalid_input", "sqlite_adapter: path must be a wellformed non-empty string");
  }
  const mode = opts.mode ?? "readonly";
  if (!OPEN_MODES.includes(mode)) {
    throw new StorageError("invalid_input", `sqlite_adapter: unsupported mode ${JSON.stringify(mode)}`);
  }
  // explicit modes only: readonly/readwrite never create; create may.
  const exists = existsSync(path);
  if (mode !== "create" && !exists) {
    throw new StorageError("unavailable", `sqlite_adapter: database does not exist: ${path}`);
  }
  if (exists && mode === "create" && statSync(path).size === 0 && opts.emptyIsCorrupt) {
    throw new StorageError("corrupt", "sqlite_adapter: empty database file");
  }
  let db;
  try {
    db = new Database(path, {
      readonly: mode === "readonly",
      fileMustExist: mode !== "create",
    });
  } catch (e) { throw mapErr(e); }
  try {
    // safeIntegers default ON — i64 exactness is the contract.
    if (opts.safeIntegers !== false) db.defaultSafeIntegers(true);
    if (opts.busyTimeoutMs !== undefined) {
      const n = opts.busyTimeoutMs;
      if (!Number.isInteger(n) || n < 0) {
        throw new StorageError("invalid_input", "sqlite_adapter: busyTimeoutMs must be a non-negative integer");
      }
      db.pragma(`busy_timeout=${n}`);
    }
    return new Handle(db, mode);
  } catch (e) { try { db.close(); } catch { /* closing */ } throw mapErr(e); }
}

class Handle {
  constructor(db, mode) {
    this._db = db;
    this.mode = mode;
    this._closed = false;
  }
  _live() {
    if (this._closed) throw new StorageError("closed", "sqlite_adapter: handle is closed");
    return this._db;
  }
  /** All rows for sql with positional params. */
  query(sql, ...params) {
    try { return this._live().prepare(sql).all(...params); }
    catch (e) { throw mapErr(e); }
  }
  /** First row or undefined. */
  get(sql, ...params) {
    try { return this._live().prepare(sql).get(...params); }
    catch (e) { throw mapErr(e); }
  }
  /** Write statement; returns {changes, lastInsertRowid}. */
  run(sql, ...params) {
    try {
      const r = this._live().prepare(sql).run(...params);
      return { changes: r.changes, lastInsertRowid: r.lastInsertRowid };
    } catch (e) { throw mapErr(e); }
  }
  /** Multi-statement exec (no params). */
  exec(sql) {
    try { this._live().exec(sql); }
    catch (e) { throw mapErr(e); }
  }
  /** Simple pragma read/set, e.g. pragma("user_version") or
   *  pragma("journal_mode=WAL"). */
  pragma(s) {
    try { return this._live().pragma(s, { simple: true }); }
    catch (e) { throw mapErr(e); }
  }
  /** Explicit transaction: runs fn inside BEGIN/COMMIT; any throw ->
   *  ROLLBACK, no partial state. Nested calls join the outer tx. */
  transaction(fn) {
    const db = this._live();
    try { return db.transaction(fn)(); }
    catch (e) { throw mapErr(e); }
  }
  /** WAL checkpoint; mode PASSIVE|FULL|RESTART|TRUNCATE. */
  checkpoint(mode = "PASSIVE") {
    try { return this._live().pragma(`wal_checkpoint(${mode})`); }
    catch (e) { throw mapErr(e); }
  }
  close() {
    if (this._closed) return;
    this._closed = true;
    try { this._db.close(); }
    catch (e) { throw mapErr(e); }
  }
}

export const ADAPTER_INFO = Object.freeze({
  driver: `better-sqlite3@${DRIVER_VERSION}`,
  node: process.versions.node,
});
