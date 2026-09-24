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
// writer + schema identity == expected. Returns a WriteCoordinator.
export const coordinatorOpenExisting = (path, lease, expected, busyMs = BUSY_MS) =>
  new WriteCoordinator(path, lease, expected, busyMs);

// ---------------------------------------------------------------------
// NM-073 — compact_runtime.rs WriteCoordinator runtime surface.
//
// Threading model (documented per task): Rust serializes the single
// writer Connection behind Mutex + lifecycle RwLock + a bounded reader
// pool (ReaderSlot/MAX_READERS=16) because tokio-style concurrency can
// reach the coordinator from many threads. Node is single-threaded:
// every coordinator method is a synchronous call on one event loop, so
// mutual exclusion is inherent and the reader-slot bound has no
// runnable equivalent (a second reader cannot interleave). The
// observable semantics are preserved verbatim: shut-down rejection,
// WAL-drain requirements, lease + schema-identity boundary checks, and
// the exact checkpoint metrics tuple.
import { statSync } from "node:fs";

const base2 = base; // alias for the unavailable() mapping used below
const walBytes = (path) => {
  try { return BigInt(statSync(`${path}-wal`).size); }
  catch { return 0n; }
};

// recover_leak — a write/read closure that leaks a transaction is a
// contract violation; rollback deterministically, then fail if the
// connection still isn't in autocommit.
const recoverLeak = (db) => {
  if (db._db.inTransaction === false) return; // is_autocommit
  db.exec("ROLLBACK");
  if (db._db.inTransaction !== false) {
    throw integrity("connection remains transactional after rollback");
  }
};

export class WriteCoordinator {
  /** WriteCoordinator::open_existing — lease path match + validate +
   *  writer + schema identity == expected. */
  constructor(path, lease, expected, busyMs = BUSY_MS) {
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
    this.path = path;
    this.lease = lease;
    this.expected = expected;
    this.busy = busyMs;
    this.writer = writer; // Handle or null once shut down
  }

  // verify_boundary — lease + schema identity unchanged.
  _verifyBoundary(db) {
    try { this.lease.validate(); }
    catch (e) { throw base2(e.message); }
    const identity = inspectSchemaIdentity(db);
    if (identity.user_version !== this.expected.user_version ||
        identity.digest !== this.expected.digest) {
      throw integrity("schema identity changed");
    }
  }

  /** write — lifecycle read + writer + boundary + autocommit + fn +
   *  leak recovery + post-verify. fn receives the raw Handle (non-tx
   *  writes allowed). */
  write(fn) {
    const conn = this.writer;
    if (!conn) throw base("coordinator is shut down");
    this._verifyBoundary(conn);
    if (conn._db.inTransaction !== false) {
      throw integrity("writer connection is not in autocommit");
    }
    try {
      const value = fn(conn);
      if (conn._db.inTransaction !== false) {
        recoverLeak(conn);
        throw integrity("write closure leaked a transaction");
      }
      this._verifyBoundary(conn);
      return value;
    } catch (e) {
      // fn/leak errors -> recover the writer before propagating.
      try { recoverLeak(conn); } catch (fatal) { throw fatal; }
      throw e;
    }
  }

  /** transaction — immediate tx inside write; commit failure ->
   *  OutcomeUnknown. */
  transaction(fn) {
    return this.write((conn) => {
      conn.exec("BEGIN IMMEDIATE");
      let value;
      try { value = fn(conn); }
      catch (e) {
        try { conn.exec("ROLLBACK"); } catch { /* best-effort */ }
        throw e;
      }
      try { conn.exec("COMMIT"); }
      catch (e) {
        // market_storage OutcomeUnknown Display — verbatim.
        throw new MarketError("outcome_unknown",
          `PUBLISH_OUTCOME_UNKNOWN: ${e.message}; inspect the same publication_id`);
      }
      return value;
    });
  }

  /** read — writer-alive check → lease → fresh read-only conn with
   *  reader pragmas → deferred tx → identity check → fn → boundary
   *  re-verify → commit. */
  read(fn) {
    if (!this.writer) throw base("coordinator is shut down");
    try { this.lease.validate(); }
    catch (e) { throw base2(e.message); }
    const conn = openDatabase(this.path, { mode: "readonly",
      busyTimeoutMs: this.busy });
    try {
      setPragma(conn, "foreign_keys", 1);
      setPragma(conn, "query_only", 1);
      verifyWal(conn);
      conn.exec("BEGIN");
      const before = inspectSchemaIdentity(conn);
      if (before.user_version !== this.expected.user_version ||
          before.digest !== this.expected.digest) {
        throw integrity("schema identity changed");
      }
      const value = fn(conn);
      try { this.lease.validate(); }
      catch (e) { throw base2(e.message); }
      const after = inspectSchemaIdentity(conn);
      if (after.user_version !== this.expected.user_version ||
          after.digest !== this.expected.digest) {
        throw integrity("schema identity changed");
      }
      conn.exec("COMMIT");
      return value;
    } finally {
      try { conn.exec("ROLLBACK"); } catch { /* already committed */ }
      conn.close();
    }
  }

  // run_checkpoint — management connection + pragmas + boundary
  // checks around wal_checkpoint(mode).
  _runCheckpoint(mode) {
    const writer = this.writer;
    this._verifyBoundary(writer);
    const mgmt = openDatabase(this.path, { mode: "readwrite",
      busyTimeoutMs: this.busy });
    try {
      verifyWal(mgmt);
      setPragma(mgmt, "foreign_keys", 1);
      setPragma(mgmt, "synchronous", 2);
      setPragma(mgmt, "wal_autocheckpoint", 1000);
      setPragma(mgmt, "journal_size_limit", JOURNAL_SIZE_LIMIT);
      try { this.lease.validate(); }
      catch (e) { throw base2(e.message); }
      const idb = inspectSchemaIdentity(mgmt);
      if (idb.user_version !== this.expected.user_version ||
          idb.digest !== this.expected.digest) {
        throw integrity("schema identity changed");
      }
      const walBefore = walBytes(this.path);
      const row = mgmt.get(
        `PRAGMA wal_checkpoint(${mode === "PASSIVE" ? "PASSIVE" : "TRUNCATE"})`);
      const vals = Object.values(row);
      const busy = BigInt(vals[0]), logFrames = BigInt(vals[1]),
        checkpointed = BigInt(vals[2]);
      try { this.lease.validate(); }
      catch (e) { throw base2(e.message); }
      const ida = inspectSchemaIdentity(mgmt);
      if (ida.user_version !== this.expected.user_version ||
          ida.digest !== this.expected.digest) {
        throw integrity("schema identity changed");
      }
      const walAfter = walBytes(this.path);
      return { mode, busy, log_frames: logFrames,
        checkpointed_frames: checkpointed,
        wal_bytes_before: walBefore, wal_bytes_after: walAfter };
    } finally {
      mgmt.close();
    }
  }

  /** checkpoint_passive — writer present → PASSIVE checkpoint. */
  checkpointPassive() {
    if (!this.writer) throw base("coordinator is shut down");
    return this._runCheckpoint("PASSIVE");
  }

  /** checkpoint_idle — TRUNCATE + drain requirement. */
  checkpointIdle() {
    if (!this.writer) throw base("coordinator is shut down");
    const metrics = this._runCheckpoint("TRUNCATE");
    if (metrics.busy !== 0n || metrics.wal_bytes_after !== 0n) {
      throw base("WAL did not drain");
    }
    return metrics;
  }

  /** shutdown — TRUNCATE drain, then close the writer. A failed close
   *  keeps the writer usable (Rust restores it). */
  shutdown() {
    if (!this.writer) throw base("coordinator is shut down");
    const metrics = this._runCheckpoint("TRUNCATE");
    if (metrics.busy !== 0n || metrics.wal_bytes_after !== 0n) {
      throw base("WAL did not drain; coordinator remains usable");
    }
    const conn = this.writer;
    this.writer = null;
    try { conn.close(); }
    catch (e) {
      this.writer = conn;
      throw base(`writer close failed: ${e.message}`);
    }
    return metrics;
  }
}
