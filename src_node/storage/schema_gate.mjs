// NM-039 P3 — explicit SQLite schema gate (read-only capability reader).
//
// Ports the structural half of sqlite_tasks.rs::check_schema plus a
// required-structure check derived from the committed migration SQL:
//
//   user_version gate -> schema_migrations checksum chain (sha256 of
//   each migrations/tasks/*.sql in apply order, verbatim Rust rule) ->
//   required tables/indexes/columns present in sqlite_master/PRAGMA
//
// Schema 18 = legacy market storage; schema 19 = compact market
// storage (#259) — the gate REPORTS it as compact; it does not treat
// schema-19 as legacy-compatible (Rust's legacy check_schema refuses
// it — this module is the neutral reader, not an adapter).
//
// Read-only only: opens through sqlite_adapter mode "readonly", never
// creates, never writes, never upgrades. Any deviation -> typed
// StorageError, fail closed.

import { createHash } from "node:crypto";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { openDatabase, StorageError } from "./sqlite_adapter.mjs";

const MIGRATIONS_DIR = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "migrations", "tasks");

// --- committed contract: parse CREATE TABLE/INDEX objects + columns ---
const stripComments = (sql) => sql.replace(/--[^\n]*/g, "").replace(/\/\*[\s\S]*?\*\//g, "");
const splitTopLevel = (s) => {
  const out = []; let depth = 0; let cur = "";
  for (const c of s) {
    if (c === "(") depth++;
    if (c === ")") depth--;
    if (c === "," && depth === 0) { out.push(cur); cur = ""; }
    else cur += c;
  }
  if (cur.trim()) out.push(cur);
  return out;
};
const CONSTRAINT_START = /^(PRIMARY|FOREIGN|UNIQUE|CHECK|CONSTRAINT)\b/i;
// Schema DDL isn't append-only: migrations DROP tables, RENAME them,
// and ADD COLUMN. Model the op chain per statement so the required
// structure is the *post-migration* shape, not a union of creates.
function parseMigration(sql) {
  const ops = []; // {op:'create',table,cols}|{op:'drop'}|{op:'rename'}|{op:'addcol'}|{op:'index'}
  for (const stmt of sql.split(";")) {
    const t = stmt.trim();
    let m;
    if ((m = /^CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?["'`]?(\w+)["'`]?\s*\(([\s\S]*)\)\s*(?:STRICT|WITHOUT\s+ROWID)?\s*$/i.exec(t))) {
      ops.push({ op: "create", table: m[1],
        cols: splitTopLevel(m[2]).map((c) => c.trim())
          .filter((c) => c && !CONSTRAINT_START.test(c))
          .map((c) => c.split(/\s+/)[0].replace(/^["'`]|["'`]$/g, "")) });
    } else if ((m = /^CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?["'`]?(\w+)["'`]?\s+AS\b/i.exec(t))) {
      ops.push({ op: "create", table: m[1], cols: [] }); // SELECT-derived; columns verified loosely
    } else if ((m = /^DROP\s+TABLE\s+(?:IF\s+EXISTS\s+)?["'`]?(\w+)["'`]?/i.exec(t))) {
      ops.push({ op: "drop", table: m[1] });
    } else if ((m = /^ALTER\s+TABLE\s+["'`]?(\w+)["'`]?\s+RENAME\s+TO\s+["'`]?(\w+)["'`]?/i.exec(t))) {
      ops.push({ op: "rename", from: m[1], to: m[2] });
    } else if ((m = /^ALTER\s+TABLE\s+["'`]?(\w+)["'`]?\s+ADD\s+COLUMN\s+["'`]?(\w+)["'`]?/i.exec(t))) {
      ops.push({ op: "addcol", table: m[1], col: m[2] });
    } else if ((m = /^CREATE\s+(?:UNIQUE\s+)?INDEX\s+(?:IF\s+NOT\s+EXISTS\s+)?["'`]?(\w+)["'`]?\s+ON\s+["'`]?(\w+)["'`]?/i.exec(t))) {
      ops.push({ op: "index", name: m[1], table: m[2] });
    } else if ((m = /^DROP\s+INDEX\s+(?:IF\s+EXISTS\s+)?["'`]?(\w+)["'`]?/i.exec(t))) {
      ops.push({ op: "dropindex", name: m[1] });
    }
  }
  return { ops };
}

// Migration files apply in filename order 001..018; 019 is the compact
// opt-in step. sha256 is computed over the raw committed bytes — the
// same input Rust hashes (MIGRATIONS[i].as_bytes()).
const FILES = readdirSync(MIGRATIONS_DIR).filter((f) => /^\d+.*\.sql$/.test(f)).sort();
export const MIGRATION_CHAIN = FILES.map((f) => ({
  version: Number(f.slice(0, 3)),
  file: f,
  checksum: createHash("sha256").update(readFileSync(join(MIGRATIONS_DIR, f))).digest("hex"),
  ops: parseMigration(stripComments(readFileSync(join(MIGRATIONS_DIR, f), "utf8"))).ops,
}));
export const SCHEMA_LEGACY = 18;
export const SCHEMA_COMPACT = 19;
export const SUPPORTED_SCHEMAS = Object.freeze([SCHEMA_LEGACY, SCHEMA_COMPACT]);

// Expected structure for a given schema version: union of objects from
// migrations 1..version plus schema_migrations itself.
// Rust post-migration data steps can also change schema —
// sqlite_market::manifest::migrate (migration 17 post step) runs
// `DROP TABLE IF EXISTS version_segments` inside the same transaction.
// These are part of the committed contract, mirrored here.
const POST_MIGRATION_OPS = {
  17: [{ op: "drop", table: "version_segments" }],
};
export function expectedStructure(version) {
  const tables = { schema_migrations: ["version", "checksum", "applied_at"] };
  const indexes = new Set();
  const indexTable = new Map();
  for (const mig of MIGRATION_CHAIN) {
    if (mig.version > version) break;
    const ops = [...mig.ops, ...(POST_MIGRATION_OPS[mig.version] ?? [])];
    for (const op of ops) {

      if (op.op === "create") tables[op.table] = op.cols;
      else if (op.op === "drop") {
        delete tables[op.table];
        for (const [ix, t] of indexTable) if (t === op.table) indexes.delete(ix);
      }
      else if (op.op === "rename") {
        if (op.from in tables) { tables[op.to] = tables[op.from]; delete tables[op.from]; }
        else tables[op.to] = []; // renamed from unparsed source — presence still required
        for (const [ix, t] of indexTable) if (t === op.from) indexTable.set(ix, op.to);
      } else if (op.op === "addcol") {
        if (tables[op.table]) tables[op.table] = [...tables[op.table], op.col];
      } else if (op.op === "index") {
        indexes.add(op.name);
        indexTable.set(op.name, op.table ?? null);
      }
      else if (op.op === "dropindex") indexes.delete(op.name);
    }
  }
  return { tables, indexes: [...indexes] };
}

/** Read-only gate. Returns a capability record:
 *  { schemaVersion, kind: "legacy"|"compact", migrationsVerified,
 *    tables, indexes, bytesUnchanged } — or throws StorageError. */
export function gateDatabase(path) {
  const before = statBytes(path);
  const db = openDatabase(path); // readonly — never creates, never writes
  try {
    const version = Number(db.pragma("user_version"));
    if (version === 0 || !SUPPORTED_SCHEMAS.includes(version)) {
      throw new StorageError("unavailable",
        `unsupported/uninitialized schema ${version}; explicit initialization/upgrade required`,
        "STORAGE_UNAVAILABLE");
    }
    // checksum chain — Rust check_schema verbatim: count + ordered
    // version + sha256(migration bytes)
    const rows = db.query("SELECT version,checksum FROM schema_migrations ORDER BY version");
    const expected = MIGRATION_CHAIN.filter((m) => m.version <= version);
    if (rows.length !== version ||
        rows.some((r, i) => Number(r.version) !== i + 1 || r.checksum !== expected[i].checksum)) {
      throw new StorageError("unavailable", "schema migration identity mismatch", "STORAGE_UNAVAILABLE");
    }
    // required structures — post-migration drops cannot hide behind a
    // valid checksum chain
    const { tables, indexes } = expectedStructure(version);
    const master = db.query(
      "SELECT type,name FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'");
    const present = new Map(master.map((r) => [r.name, r.type]));
    const missingTables = Object.keys(tables).filter((t) => present.get(t) !== "table");
    const missingIndexes = indexes.filter((i) => present.get(i) !== "index");
    if (missingTables.length || missingIndexes.length) {
      throw new StorageError("unavailable",
        `schema structure incomplete: missing tables [${missingTables}] indexes [${missingIndexes}]`,
        "STORAGE_UNAVAILABLE");
    }
    const missingCols = [];
    for (const [t, cols] of Object.entries(tables)) {
      const actual = new Set(
        db.query("SELECT name FROM pragma_table_info(?)", t).map((r) => r.name));
      for (const c of cols) if (!actual.has(c)) missingCols.push(`${t}.${c}`);
    }
    if (missingCols.length) {
      throw new StorageError("unavailable",
        `schema structure incomplete: missing columns [${missingCols.slice(0, 8)}${missingCols.length > 8 ? "…" : ""}]`,
        "STORAGE_UNAVAILABLE");
    }
    return {
      schemaVersion: version,
      kind: version === SCHEMA_COMPACT ? "compact" : "legacy",
      migrationsVerified: rows.length,
      tables: Object.keys(tables).sort(),
      indexes: indexes.sort(),
      bytesUnchanged: statBytes(path) === before,
    };
  } finally { db.close(); }
}

function statBytes(path) {
  try { return statSync(path).size; }
  catch { return -1; }
}
