// NM-039 P3: validate schema_gate against NM-006 fixtures + mutated
// synthetic DBs. Read-only gate — every path verifies bytes unchanged.
// Exit nonzero on FAIL. Usage: node schema_gate_probe.mjs <root> [selftest]
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdtempSync, readFileSync, rmSync, statSync } from "node:fs";
import { join } from "node:path";

const ROOT = process.argv[2] || ".";
const SELFTEST = process.argv[3] || null;
const { gateDatabase, expectedStructure, MIGRATION_CHAIN, SCHEMA_LEGACY, SCHEMA_COMPACT } =
  await import("../../src_node/storage/schema_gate.mjs");
const { openDatabase, StorageError } = await import("../../src_node/storage/sqlite_adapter.mjs");

let failures = 0;
const emit = (id, status, detail, expected = null, actual = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ family: "schema_gate", id, status, detail, expected, actual }));
};
const run = (id, fn) => {
  try { emit(id, "PASS", null, fn()); }
  catch (e) { emit(id, "FAIL", `${e.code || ""} ${e.message}`); }
};
const expectGateErr = (id, code, msgPart, path) => {
  const before = existsSync(path) ? sha(path) : null;
  try { gateDatabase(path); emit(id, "FAIL", "gate accepted"); }
  catch (e) {
    const ok = e instanceof StorageError && e.code === code &&
      e.message.includes(msgPart) &&
      (before === null ? !existsSync(path) : sha(path) === before);
    emit(id, ok ? "PASS" : "FAIL", `${e.code} ${e.message.slice(0, 100)}`);
  }
};
const sha = (p) => createHash("sha256").update(readFileSync(p)).digest("hex");

const FIX = mkdtempSync("/tmp/nm39-gate-");
process.on("exit", () => rmSync(FIX, { recursive: true, force: true }));
const SRC = "/dev/shm/nm006";
for (const f of ["db18.sqlite", "db19.sqlite", "empty.db", "garbage.bin"]) {
  copyFileSync(join(SRC, f), join(FIX, f));
}
const DB18 = join(FIX, "db18.sqlite"), DB19 = join(FIX, "db19.sqlite");
const EMPTY = join(FIX, "empty.db"), GARBAGE = join(FIX, "garbage.bin");

// mutate a fixture copy via a writable adapter handle
const mutate = (src, name, sql) => {
  const p = join(FIX, name);
  copyFileSync(src, p);
  const d = openDatabase(p, { mode: "readwrite" });
  d.exec(sql);
  d.close();
  return p;
};

emit("migration_chain", "PASS", null,
  MIGRATION_CHAIN.map((m) => ({ v: m.version, file: m.file, sha: m.checksum.slice(0, 12) })));

run("gate_schema18", () => {
  const before = sha(DB18);
  const r = gateDatabase(DB18);
  if (r.schemaVersion !== 18 || r.kind !== "legacy" || r.migrationsVerified !== 18 ||
      !r.bytesUnchanged || sha(DB18) !== before) throw new Error(JSON.stringify(r));
  return { schemaVersion: r.schemaVersion, kind: r.kind, tables: r.tables.length, indexes: r.indexes.length, bytesUnchanged: r.bytesUnchanged };
});
run("gate_schema19", () => {
  const before = sha(DB19);
  const r = gateDatabase(DB19);
  if (r.schemaVersion !== 19 || r.kind !== "compact" || r.migrationsVerified !== 19 ||
      !r.bytesUnchanged || sha(DB19) !== before) throw new Error(JSON.stringify(r));
  return { schemaVersion: r.schemaVersion, kind: r.kind, tables: r.tables.length, indexes: r.indexes.length, bytesUnchanged: r.bytesUnchanged };
});

expectGateErr("gate_empty_zero_version", "unavailable", "unsupported/uninitialized schema 0", EMPTY);
expectGateErr("gate_garbage_corrupt", "corrupt", "not a database", GARBAGE);
expectGateErr("gate_missing_file", "unavailable", "does not exist", join(FIX, "nope.db"));

// unknown version 20 (> supported max)
expectGateErr("gate_version_20", "unavailable", "unsupported/uninitialized schema 20",
  mutate(DB18, "v20.db", "PRAGMA user_version=20"));
// mid-chain version 10 (not a released schema)
expectGateErr("gate_version_10", "unavailable", "unsupported/uninitialized schema 10",
  mutate(DB18, "v10.db", "PRAGMA user_version=10"));
// checksum tamper -> identity mismatch
expectGateErr("gate_checksum_tamper", "unavailable", "schema migration identity mismatch",
  mutate(DB18, "tamper.db", "UPDATE schema_migrations SET checksum='0bad' WHERE version=3"));
// missing row in chain
expectGateErr("gate_row_missing", "unavailable", "schema migration identity mismatch",
  mutate(DB18, "rowmiss.db", "DELETE FROM schema_migrations WHERE version=5"));
// structural drop post-migration: checksum chain intact, table gone
expectGateErr("gate_table_dropped", "unavailable", "missing tables [tasks]",
  mutate(DB18, "notask.db", "DROP TABLE tasks"));
// missing column
expectGateErr("gate_column_dropped", "unavailable", "missing columns",
  mutate(DB18, "nocol.db", "ALTER TABLE tasks DROP COLUMN result_json"));
// missing index
expectGateErr("gate_index_dropped", "unavailable", "indexes [occupancy_task]",
  mutate(DB18, "noidx.db", "DROP INDEX occupancy_task"));
// readonly path never writes: gate then verify byte identity again
run("gate_no_write_side_effects", () => {
  const before = sha(DB19);
  const r = gateDatabase(DB19);
  // readonly open of a WAL-mode DB may create -shm/-wal sidecars — that
  // is read-protocol, not mutation; the DB bytes are the proof.
  const wal = existsSync(DB19 + "-wal"), shm = existsSync(DB19 + "-shm");
  if (sha(DB19) !== before) throw new Error("db bytes mutated");
  return { bytesUnchanged: true, sidecars_created: wal || shm };
});
// gate refuses to provide write handles — readonly attempt via gate path
run("gate_is_readonly_only", () => {
  // the gate's own open is readonly: confirm a write on a readonly-opened
  // handle from the same path is rejected (adapter-level, NM-038)
  const d = openDatabase(DB18);
  try { d.exec("INSERT INTO tasks DEFAULT VALUES"); emit("gate_is_readonly_only", "FAIL", "wrote"); }
  catch (e) { if (e.code !== "readonly") throw e; d.close(); return { write: "readonly-rejected" }; }
});

// selftest
if (SELFTEST === "gate") {
  const r = gateDatabase(DB19);
  emit("selftest_gate", r.schemaVersion === 18 ? "PASS" : "FAIL",
    r.schemaVersion === 18 ? null : `expected wrong version — STOP (got ${r.schemaVersion})`);
}

process.exitCode = failures === 0 ? 0 : 1;
