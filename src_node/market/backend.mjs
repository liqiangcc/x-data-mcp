// NM-056 — sqlite_market/backend.rs: runtime-selected market backend.
// open/open_shared dispatch on the durable schema version: absent file
// or schema != 19 -> Legacy; schema 19 -> Compact (lease + coordinator).
import { statSync } from "node:fs";
import { DatabaseLease } from "../storage/database_lease.mjs";
import { openDatabase, StorageError } from "../storage/sqlite_adapter.mjs";
import {
  schema19Identity, coordinatorOpenExisting,
} from "./compact_runtime.mjs";
import { MarketError } from "./record_codec.mjs";

export const COMPACT_SCHEMA_VERSION = 19;

const base = (m) => new MarketError("base", `BASE_UNAVAILABLE: ${m}`);
const storage = (m) => new MarketError("storage", `MARKET_STORAGE: ${m}`);

// SqliteTasks::schema_version — PRAGMA user_version on a fresh read conn.
const schemaVersion = (path) => {
  const db = openDatabase(path, { mode: "readwrite" });
  try {
    return Number(db.get("PRAGMA user_version").user_version);
  } catch (e) { throw storage(e.message); }
  finally { try { db.close(); } catch { /* closing */ } }
};

const isFile = (path) => {
  try { return statSync(path).isFile(); } catch { return false; }
};

// SqliteCompactMarket::open — own lease + coordinator.
export const compactMarketOpen = (path) => {
  let lease;
  try { lease = DatabaseLease.acquire(path); }
  catch (e) { throw base(`database lease: ${e.message}`); }
  try {
    return compactMarketOpenShared(path, lease);
  } catch (e) { try { lease.close(); } catch { /* */ } throw e; }
};

// SqliteCompactMarket::open_shared — shares the caller-held lease.
export const compactMarketOpenShared = (path, lease) => {
  const coordinator = coordinatorOpenExisting(path, lease, schema19Identity());
  return {
    kind: "compact",
    path,
    lease,
    coordinator,
    // The writer connection doubles as the compact market handle for
    // the read/write op surface (single-process coordinator parity).
    db: coordinator.writer,
    read(fn) { return fn(coordinator.writer); },
    write(fn) { return fn(coordinator.writer); },
  };
};

/** MarketBackend::open — schema dispatch; absent file keeps Legacy so a
 *  missing database fails closed per-operation, not at startup. */
export const backendOpen = (path) => {
  if (!isFile(path)) return { kind: "legacy", path };
  const v = schemaVersion(path);
  if (v === COMPACT_SCHEMA_VERSION) return compactMarketOpen(path);
  return { kind: "legacy", path };
};

/** MarketBackend::open_shared — same dispatch on a caller-held lease. */
export const backendOpenShared = (path, lease) => {
  if (!isFile(path)) return { kind: "legacy", path };
  const v = schemaVersion(path);
  if (v === COMPACT_SCHEMA_VERSION) return compactMarketOpenShared(path, lease);
  return { kind: "legacy", path };
};
