// NM-073 — sqlite_tasks.rs schema lifecycle ports: initialize /
// initialize19 (initialize_target), upgrade_compact, apply_migration,
// check_schema, post_migration, require_business_tables_empty.
// Explicit schema operations only — creation requires a nonexistent
// path (create_new parity); upgrade_compact requires exact schema 18
// with every business table empty (FRESH_REBUILD_REQUIRED — no data
// migration on the compact path).
import { existsSync } from "node:fs";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { openDatabase, StorageError } from "./sqlite_adapter.mjs";
import { MIGRATION_CHAIN, SCHEMA_LEGACY, SCHEMA_COMPACT } from "./schema_gate.mjs";

const sha256 = (b) => createHash("sha256").update(b).digest("hex");
const storage = (m) => new StorageError("unavailable", String(m));
// sqlite_tasks::inconsistent -> Error::NeedsVerification — Display
// carries the ACCEPTANCE_ prefix verbatim (comparator checks it).
const inconsistent = (m) =>
  new StorageError("needs_verification", `ACCEPTANCE_NEEDS_VERIFICATION: ${m}`);

const MIGRATIONS_DIR = new URL("../../migrations/tasks/", import.meta.url).pathname;
const migrationSql = (file) => readFileSync(join(MIGRATIONS_DIR, file), "utf8");
const BUSY_MS = 250; // SqliteTasks::new default busy timeout.
const pragmaVal = (db, name) => Object.values(db.get(`PRAGMA ${name}`))[0];

// configure(c, busy) — pragma set + readback contract check.
const configure = (db) => {
  db.pragma("foreign_keys=ON");
  db.pragma("synchronous=2"); // FULL
  db.pragma("wal_autocheckpoint=1000");
  db.pragma("journal_size_limit=16777216");
  const fk = Number(pragmaVal(db, "foreign_keys"));
  const sync = Number(pragmaVal(db, "synchronous"));
  const mode = String(pragmaVal(db, "journal_mode"));
  const autocp = Number(pragmaVal(db, "wal_autocheckpoint"));
  const jsl = Number(pragmaVal(db, "journal_size_limit"));
  if (fk !== 1 || sync !== 2 || mode !== "wal" || autocp !== 1000 ||
      jsl !== 16777216) {
    throw storage("connection pragmas differ from task contract");
  }
};

// check_schema(c, current) — user_version bounds + checksum chain.
// current=true additionally requires exactly SCHEMA_LEGACY (the
// operational gate); upgrade_compact uses current=false then pins 18.
const checkSchema = (db, current) => {
  const version = Number(pragmaVal(db, "user_version"));
  if (version === 0 || version > SCHEMA_LEGACY ||
      (current && version !== SCHEMA_LEGACY)) {
    throw storage(
      `unsupported/uninitialized schema ${version}; explicit initialization/upgrade required`);
  }
  const rows = db.query(
    "SELECT version,checksum FROM schema_migrations ORDER BY version");
  if (rows.length !== version ||
      rows.some((r, i) => Number(r.version) !== i + 1 ||
        r.checksum !== MIGRATION_CHAIN[i].checksum)) {
    throw storage("schema migration identity mismatch");
  }
  return version;
};

// apply_migration — schema batch + migration row + user_version.
// The user_version pragma is not transactional (header write) — Rust
// issues it inside the tx; same here.
const applyMigration = (db, version, sql) => {
  db.exec(sql);
  db.run("INSERT INTO schema_migrations VALUES(?,?,?)",
    version, sha256(sql), new Date().toISOString());
  db.pragma(`user_version=${version}`);
};

// post_migration data steps — on the initialize path every business
// table is freshly created (empty): migrate_legacy's row loop and the
// manifest evidence backfill are vacuous; migrate_clips' INSERT-SELECT
// on the empty join is vacuous; only the version_segments drop and the
// empty-set checks have observable effect. The populated-DB row bodies
// (envelope::expand / build_balanced / compat::check) are NOT ported —
// they can only run via the populated upgrade path, which is out of
// scope; a row surfacing here would be a contract violation.
const postMigration = (db, version) => {
  if (version === 16) {
    const rows = db.query(
      "SELECT version_id,envelope_json FROM data_versions ORDER BY rowid");
    if (rows.length !== 0) {
      throw storage(
        "compact record migration failed: populated data_versions on initialize");
    }
  } else if (version === 17) {
    const clipsPresent = db.get(
      "SELECT count(*)>0 x FROM sqlite_master WHERE type='table' AND name='version_segments'").x !== 0n;
    if (clipsPresent) {
      const clips = db.query(
        "SELECT vs.version_id,vs.segment_id,vs.clip_start,vs.clip_end " +
        "FROM version_segments vs JOIN data_versions v ON v.version_id=vs.version_id " +
        "ORDER BY v.rowid,vs.seq");
      if (clips.length !== 0) {
        throw storage(
          "manifest migration failed: populated version_segments on initialize");
      }
    }
    const unmanifested = db.get(
      "SELECT EXISTS(SELECT 1 FROM data_versions WHERE json_extract(envelope_json,'$.records.$ref')='segments' AND manifest_root IS NULL) x").x !== 0n;
    if (unmanifested) {
      throw inconsistent("segment-backed version lacks manifest");
    }
    const ids = db.query(
      "SELECT version_id FROM data_versions WHERE evidence_digest IS NULL ORDER BY rowid");
    if (ids.length !== 0) {
      throw storage(
        "manifest migration failed: populated data_versions on initialize");
    }
    db.exec("DROP TABLE IF EXISTS version_segments;");
  }
};

// Immediate transaction with the Rust boundary split: fn errors roll
// back deterministically; commit errors are OutcomeUnknown (the fsync
// boundary makes the durable state unknowable to the caller).
const immediateTx = (db, fn) => {
  db.exec("BEGIN IMMEDIATE");
  let value;
  try {
    value = fn();
  } catch (e) {
    try { db.exec("ROLLBACK"); } catch { /* best-effort rollback */ }
    throw e;
  }
  try {
    db.exec("COMMIT");
  } catch (e) {
    // Error::OutcomeUnknown Display text — verbatim.
    throw new StorageError("outcome_unknown",
      `OUTCOME_UNKNOWN: ${e.message}; query the same request_id before retrying`);
  }
  return value;
};

// initialize_target — create_new parity: refuse any existing file.
const initializeTarget = (path, compact) => {
  if (existsSync(path)) {
    throw storage("File exists (os error 17)");
  }
  const db = openDatabase(path, { mode: "create", busyTimeoutMs: BUSY_MS });
  try {
    const mode = db.pragma("journal_mode=WAL");
    if (String(mode) !== "wal") {
      throw new StorageError("unavailable", "WAL unavailable");
    }
    configure(db);
    immediateTx(db, () => {
      for (const m of MIGRATION_CHAIN) {
        if (m.version > SCHEMA_LEGACY) break;
        applyMigration(db, m.version, migrationSql(m.file));
        postMigration(db, m.version);
      }
      if (compact) {
        applyMigration(db, SCHEMA_COMPACT,
          migrationSql(
            MIGRATION_CHAIN.find((m) => m.version === SCHEMA_COMPACT).file));
      }
    });
  } finally {
    db.close();
  }
};

/** SqliteTasks::initialize — schema-18 fresh create. */
export const initialize = (path) => initializeTarget(path, false);
/** SqliteTasks::initialize19 — schema-19 compact fresh create. */
export const initialize19 = (path) => initializeTarget(path, true);

// require_business_tables_empty — every non-schema, non-sqlite table
// must be empty; else FRESH_REBUILD_REQUIRED.
const requireBusinessTablesEmpty = (db) => {
  const names = db.query(
    "SELECT name FROM sqlite_master WHERE type='table' " +
    "AND name<>'schema_migrations' AND name NOT LIKE 'sqlite_%' ORDER BY name")
    .map((r) => r.name);
  for (const name of names) {
    const populated = db.get(
      `SELECT EXISTS(SELECT 1 FROM "${name.replaceAll('"', '""')}" LIMIT 1) x`).x !== 0n;
    if (populated) {
      throw storage(
        "FRESH_REBUILD_REQUIRED: schema 19 compact market storage requires a new database");
    }
  }
};

/** SqliteTasks::upgrade_compact — explicit 18→19 migration. No data is
 *  moved: every business table must be empty; a populated schema-18
 *  database can only be replaced by a fresh rebuild. */
export const upgradeCompact = (path) => {
  const db = openDatabase(path, { mode: "readwrite", busyTimeoutMs: BUSY_MS });
  let committed = false;
  try {
    // SQLite ignores foreign_keys inside a transaction — set before BEGIN.
    db.pragma("foreign_keys=OFF");
    immediateTx(db, () => {
      const version = checkSchema(db, false);
      if (version !== SCHEMA_LEGACY) {
        throw storage(
          `schema ${version} cannot upgrade to compact ${SCHEMA_COMPACT}; requires exact schema ${SCHEMA_LEGACY}`);
      }
      requireBusinessTablesEmpty(db);
      applyMigration(db, SCHEMA_COMPACT,
        migrationSql(
          MIGRATION_CHAIN.find((m) => m.version === SCHEMA_COMPACT).file));
      if (db.query("PRAGMA foreign_key_check").length > 0) {
        throw inconsistent("schema upgrade found foreign key violations");
      }
    });
    committed = true;
  } finally {
    // Rust re-enables foreign_keys only on the success path; the
    // connection closes either way so the off-state never escapes.
    if (committed) db.pragma("foreign_keys=ON");
    db.close();
  }
};
