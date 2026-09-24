// NM-056 — sqlite_market/compact_runtime.rs write-path open surface:
// open_writer (WAL + pragma readback + autocommit), inspect_schema_identity,
// schema19_identity, WriteCoordinator::open_existing parity.
import { createHash } from "node:crypto";
import { openDatabase } from "../storage/sqlite_adapter.mjs";
import { MIGRATION_CHAIN } from "../storage/schema_gate.mjs";
import { MarketError } from "./record_codec.mjs";

const base = (m) => new MarketError("base", `BASE_UNAVAILABLE: ${m}`);
const integrity = (m) => new MarketError("integrity", `MARKET_INTEGRITY: ${m}`);
const storage = (m) => new MarketError("storage", `MARKET_STORAGE: ${m}`);

const DOMAIN = Buffer.from("x-data-schema-identity-v1\0");
const JOURNAL_SIZE_LIMIT = 16777216;
const BUSY_MS = 5000;

const u32be = (n) => { const b = Buffer.alloc(4); b.writeUInt32BE(n >>> 0); return b; };
const pushText = (out, s) => { const b = Buffer.from(s, "utf8"); out.push(u32be(b.length), b); };
const sha256hex = (b) => createHash("sha256").update(b).digest("hex");

// schema19_identity — recomputes the committed migration checksums.
// MIGRATION_CHAIN already holds sha256-of-file-bytes hex strings — the
// exact bytes push_text embeds.
export const schema19Identity = () => {
  const payload = [DOMAIN, u32be(19), u32be(MIGRATION_CHAIN.length)];
  MIGRATION_CHAIN.forEach((m, i) => {
    payload.push(u32be(i + 1));
    pushText(payload, m.checksum);
  });
  return { user_version: 19, digest: sha256hex(Buffer.concat(payload)) };
};

// inspect_schema_identity — recompute from live schema_migrations rows.
export const inspectSchemaIdentity = (db) => {
  const raw = Number(db.get("PRAGMA user_version").user_version);
  if (!(raw > 0) || raw > 4294967295) {
    throw integrity("schema user_version out of range");
  }
  let rows;
  try {
    rows = db.query("SELECT version,checksum FROM schema_migrations ORDER BY version");
  } catch (e) { throw storage(e.message); }
  if (rows.length !== raw) {
    throw integrity("schema migration count differs from user_version");
  }
  const payload = [DOMAIN, u32be(raw), u32be(rows.length)];
  for (const [i, r] of rows.entries()) {
    const version = Number(r.version);
    if (version !== i + 1 || r.checksum === "") {
      throw integrity("schema migration sequence invalid");
    }
    payload.push(u32be(version));
    pushText(payload, r.checksum);
  }
  return { user_version: raw, digest: sha256hex(Buffer.concat(payload)) };
};

const verifyWal = (db) => {
  const mode = db.get("PRAGMA journal_mode").journal_mode;
  if (String(mode).toLowerCase() !== "wal") {
    throw base("journal_mode is not WAL");
  }
};
const setPragma = (db, name, value) => {
  db.exec(`PRAGMA ${name}=${value}`);
  const back = Number(Object.values(db.get(`PRAGMA ${name}`))[0]);
  if (back !== value) {
    throw base(`pragma ${name} readback mismatch`);
  }
};

// open_writer — READ_WRITE|NO_MUTEX + busy + WAL + pragmas + autocommit
export const openWriter = (path, busyMs = BUSY_MS) => {
  const db = openDatabase(path, { mode: "readwrite", busyTimeoutMs: busyMs });
  try {
    verifyWal(db);
    setPragma(db, "foreign_keys", 1);
    setPragma(db, "synchronous", 2);
    setPragma(db, "wal_autocheckpoint", 1000);
    setPragma(db, "journal_size_limit", JOURNAL_SIZE_LIMIT);
    if (db._db.inTransaction !== false) {
      throw integrity("writer connection is not in autocommit");
    }
    return db;
  } catch (e) { try { db.close(); } catch { /* closing */ } throw e; }
};

// WriteCoordinator::open_existing — lease path match + validate +
// writer + schema identity == expected.
export const coordinatorOpenExisting = (path, lease, expected, busyMs = BUSY_MS) => {
  if (path !== lease.path) {
    throw base("coordinator path does not match lease");
  }
  try { lease.validate(); }
  catch (e) { throw base(e.message); }
  const writer = openWriter(path, busyMs);
  try {
    const identity = inspectSchemaIdentity(writer);
    if (identity.user_version !== expected.user_version ||
        identity.digest !== expected.digest) {
      throw integrity("schema identity does not match expected");
    }
  } catch (e) { try { writer.close(); } catch { /* closing */ } throw e; }
  return { path, lease, expected, busy: busyMs, writer };
};
