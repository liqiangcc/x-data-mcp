// NM-048 — port of verification_runtime.rs: Linux runtime exclusion
// guard for verification executors. File lock + descriptor binding +
// local-filesystem gate + boot identity, all fail-closed.
import { createHash } from "node:crypto";
import { closeSync, fstatSync, mkdirSync, openSync, readFileSync, realpathSync, renameSync, lstatSync, existsSync, writeSync, fsyncSync, statSync } from "node:fs";
import { join } from "node:path";
import { createRequire } from "node:module";
import { readSync } from "node:fs";
const require = createRequire(import.meta.url);

let native = null;
const loadNative = () => {
  if (native) return native;
  native = require("../native/rename_noreplace.node");
  return native;
};

export const PROTOCOL = "verification-file-guard-v1";
const MAX_DESCRIPTOR_BYTES = 16 * 1024;
// local filesystem magics (Rust supported_file): ext2/3/4, XFS, Btrfs, tmpfs
const LOCAL_MAGICS = new Set([0xef53, 0x58465342, 0x9123683e, 0x01021994]);
const LOCK_EX_NB = 2 | 4; // LOCK_EX | LOCK_NB

export class RuntimeGuardError extends Error {
  constructor(kind, message) {
    super(message);
    this.name = "RuntimeGuardError";
    this.kind = kind; // 'busy' | 'not_verified' | 'io'
  }
}
const unverified = (m) => new RuntimeGuardError("not_verified", String(m));
const ioErr = (e) => new RuntimeGuardError("io", String(e?.message ?? e));

const sha256 = (b) => createHash("sha256").update(b).digest("hex");

const fileIdentity = (st) => {
  if (!st.isFile() || st.nlink !== 1) {
    throw unverified("requires a regular, non-aliased local file");
  }
  return { device: Number(st.dev), inode: Number(st.ino) };
};
const fileIdentityAt = (path) => fileIdentity(lstatSync(path));

const requireLocalFd = (fd) => {
  if (!LOCAL_MAGICS.has(loadNative().statfsmagic(fd))) {
    throw unverified("filesystem is outside the local runtime-guard contract");
  }
};
const requireLocalPath = (path) => {
  const fd = openSync(path, "r");
  try { requireLocalFd(fd); } finally { closeSync(fd); }
};

const bootId = () => {
  let v;
  try { v = readFileSync("/proc/sys/kernel/random/boot_id", "utf8").trim(); }
  catch (e) { throw ioErr(e); }
  if (v.length !== 36 || ![...v].every((c) => /[0-9a-f-]/i.test(c))) {
    throw unverified("kernel boot identity unavailable");
  }
  return v;
};

// Serde order for GuardDescriptor (deny_unknown_fields, declaration order)
const descriptorJson = (d) => JSON.stringify({
  protocol: d.protocol, task_id: d.task_id, run_id: d.run_id,
  boot_id: d.boot_id, database_path: d.database_path,
  database_identity: { device: d.database_identity.device, inode: d.database_identity.inode },
  lock_path: d.lock_path,
  lock_identity: { device: d.lock_identity.device, inode: d.lock_identity.inode },
});
const parseDescriptor = (bytes) => {
  let d;
  try { d = JSON.parse(bytes.toString("utf8")); } catch (e) {
    throw unverified(`invalid runtime descriptor: ${e.message}`);
  }
  const keys = ["protocol", "task_id", "run_id", "boot_id", "database_path",
    "database_identity", "lock_path", "lock_identity"];
  if (d === null || typeof d !== "object" || Object.keys(d).length !== keys.length ||
      keys.some((k) => !(k in d)) ||
      typeof d.database_identity?.device !== "number" ||
      typeof d.database_identity?.inode !== "number" ||
      typeof d.lock_identity?.device !== "number" ||
      typeof d.lock_identity?.inode !== "number" ||
      Object.keys(d.database_identity ?? {}).length !== 2 ||
      Object.keys(d.lock_identity ?? {}).length !== 2) {
    throw unverified("invalid runtime descriptor shape");
  }
  return d;
};

class Context {
  constructor(database, task, run) {
    if (task.trim() === "" || run.trim() === "") {
      throw unverified("empty task/run identity");
    }
    this.database = realpathSync(database);
    requireLocalPath(this.database);
    this.identity = fileIdentityAt(this.database);
    this.boot = bootId();
    this.directory = `${this.database}.verification-guards`;
    this.lock = join(this.directory, `${sha256(Buffer.from(task, "utf8"))}.lock`);
  }
  checkDirectory() {
    let st;
    try { st = lstatSync(this.directory); } catch (e) { throw ioErr(e); }
    if (!st.isDirectory()) {
      throw unverified("runtime directory is not a direct directory");
    }
    requireLocalPath(this.directory);
  }
  matches(d) {
    return d.protocol === PROTOCOL && d.boot_id === this.boot &&
      d.database_path === this.database &&
      d.database_identity.device === this.identity.device &&
      d.database_identity.inode === this.identity.inode &&
      d.lock_path === this.lock;
  }
}

const flockTry = (fd) => {
  const e = loadNative().flockfd(fd, LOCK_EX_NB);
  if (e === 0) return;
  if (e === 11) throw new RuntimeGuardError("busy", "verification execution guard is held");
  throw ioErr(new Error(`flock errno ${e}`));
};

const readDescriptor = (fd) => {
  const st = fstatSync(fd);
  if (st.size === 0 || st.size > MAX_DESCRIPTOR_BYTES) {
    throw unverified("runtime descriptor byte budget or missing descriptor");
  }
  const buf = Buffer.alloc(st.size);
  let off = 0;
  while (off < st.size) {
    off += readSync(fd, buf, off, st.size - off, off);
  }
  return parseDescriptor(buf);
};

export class ExecutionGuard {
  constructor(fd, descriptor) {
    this._fd = fd;
    this.descriptor = descriptor;
  }
  /** Recheck all bindings while holding the same handle (verbatim
   *  verify_binding ordering). */
  verifyBinding(database) {
    const context = new Context(database, this.descriptor.task_id, this.descriptor.run_id);
    context.checkDirectory();
    requireLocalFd(this._fd);
    if (!context.matches(this.descriptor) ||
        fstatSync(this._fd) && JSON.stringify(fileIdentity(fstatSync(this._fd))) !==
          JSON.stringify(this.descriptor.lock_identity) ||
        JSON.stringify(fileIdentityAt(context.lock)) !==
          JSON.stringify(this.descriptor.lock_identity) ||
        !descEq(readDescriptor(this._fd), this.descriptor)) {
      throw unverified("runtime/database/handle binding changed");
    }
  }
  close() {
    if (this._fd !== null) { closeSync(this._fd); this._fd = null; }
  }
}
const descEq = (a, b) => descriptorJson(a) === descriptorJson(b);

const reopenForExecutor = (database, task, run, context) => {
  fileIdentityAt(context.lock);
  const fd = openSync(context.lock, "r+");
  try {
    flockTry(fd);
    const descriptor = readDescriptor(fd);
    if (descriptor.task_id !== task || descriptor.run_id !== run) {
      throw unverified("runtime descriptor belongs to another task/run");
    }
    const guard = new ExecutionGuard(fd, descriptor);
    guard.verifyBinding(database);
    return guard;
  } catch (e) { closeSync(fd); throw e; }
};

/** Executor-only creation. Never registers or claims a task. */
export const create = (database, task, run) => {
  const context = new Context(database, task, run);
  try {
    mkdirSync(context.directory, { mode: 0o700 });
    fsyncSync(openSync(join(context.database, ".."), "r")); // parent dir sync
  } catch (e) {
    if (e.code !== "EEXIST") {
      if (e instanceof RuntimeGuardError) throw e;
      if (e.code === undefined || e.code === "ENOENT") throw ioErr(e);
      throw ioErr(e);
    }
  }
  context.checkDirectory();
  if (existsSync(context.lock) || lstatExists(context.lock)) {
    return reopenForExecutor(database, task, run, context);
  }
  // temp file in the same directory, locked before publishing
  const tmp = join(context.directory, `.tmp-${process.pid}-${Date.now()}`);
  const fd = openSync(tmp, "w+", 0o600);
  try {
    flockTry(fd);
    const descriptor = {
      protocol: PROTOCOL, task_id: task, run_id: run,
      boot_id: context.boot, database_path: context.database,
      database_identity: context.identity, lock_path: context.lock,
      lock_identity: fileIdentity(fstatSync(fd)),
    };
    const bytes = Buffer.from(descriptorJson(descriptor), "utf8");
    if (bytes.length > MAX_DESCRIPTOR_BYTES) {
      throw unverified("runtime descriptor exceeds byte budget");
    }
    writeSync(fd, bytes);
    fsyncSync(fd);
    try { renameSync(tmp, context.lock); }
    catch (e) {
      if (e.code === "EEXIST" || e.code === "ENOTEMPTY") {
        closeSync(fd);
        return reopenForExecutor(database, task, run, new Context(database, task, run));
      }
      throw ioErr(e);
    }
    fsyncSync(openSync(context.directory, "r"));
    const guard = new ExecutionGuard(fd, descriptor);
    guard.verifyBinding(database);
    return guard;
  } catch (e) {
    try { closeSync(fd); } catch { /* closed */ }
    throw e;
  }
};
const lstatExists = (p) => { try { lstatSync(p); return true; } catch { return false; } };

/** Inspector-only nonblocking acquisition. Never creates files. */
export const tryAcquire = (database, expected) => {
  const context = new Context(database, expected.task_id, expected.run_id);
  if (!context.matches(expected)) {
    throw unverified("registered runtime context differs");
  }
  context.checkDirectory();
  const li = fileIdentityAt(context.lock);
  if (li.device !== expected.lock_identity.device || li.inode !== expected.lock_identity.inode) {
    throw unverified("registered lock file was replaced");
  }
  const fd = openSync(context.lock, "r+");
  try {
    flockTry(fd);
    const guard = new ExecutionGuard(fd, expected);
    guard.verifyBinding(database);
    return guard;
  } catch (e) { closeSync(fd); throw e; }
};
