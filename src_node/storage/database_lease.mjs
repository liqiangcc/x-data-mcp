// NM-056 — local_runtime::paths::DatabaseLease port. The service
// singleton lease: private path + one regular file + local fs + flock +
// dev/ino identity pinning, rechecked on validate().
import { lstatSync, fstatSync, openSync, closeSync, constants } from "node:fs";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
let native = null;
const loadNative = () => {
  if (native) return native;
  native = require("../native/rename_noreplace.node");
  return native;
};

// io::Error::new(InvalidInput, msg) — Display = msg verbatim
const invalid = (m) => Object.assign(new Error(m), { kind: "invalid_input" });

const LOCAL_MAGICS = new Set([0xef53, 0x58465342, 0x9123683e, 0x01021994]);

const uid = () => process.getuid();

// private_metadata — lstat: owned by this uid, mode & 077 == 0, not a
// symlink (and a directory when directory=true)
const privateMetadata = (path, directory = false) => {
  let m;
  try { m = lstatSync(path); } catch (e) { throw e; }
  if (m.uid !== uid() || (m.mode & 0o077) !== 0 || m.isSymbolicLink() ||
      (directory && !m.isDirectory())) {
    throw invalid("runtime path must be owned by this UID, private, and not a symlink");
  }
  return m;
};
const identityOf = (m) => `${m.dev}:${m.ino}`;

const ERRNO = {
  1: "Operation not permitted", 2: "No such file or directory",
  4: "Interrupted system call", 9: "Bad file descriptor",
  11: "Resource temporarily unavailable", 13: "Permission denied",
  22: "Invalid argument", 24: "Too many open files",
  37: "No locks available", 38: "Function not implemented",
  77: "File descriptor in bad state",
};
const strerror = (n) => ERRNO[n] ?? `Unknown error`;
const supportedFile = (fd) => {
  if (process.env.X_DATA_MCP_UNSAFE_NONLOCAL_FS !== undefined) return true;
  return LOCAL_MAGICS.has(loadNative().statfsmagic(fd));
};

export class DatabaseLease {
  /** acquire(path) — private_metadata + regular-nlink-1 + O_NOFOLLOW open
   *  + local-fs + flock(LOCK_EX|LOCK_NB) + identity re-check + validate. */
  static acquire(path) {
    const m = privateMetadata(path, false);
    if (!m.isFile() || m.nlink !== 1) {
      throw invalid("database must be one regular file");
    }
    const fd = openSync(path, constants.O_RDONLY |
      (constants.O_NOFOLLOW ?? 0) | (constants.O_CLOEXEC ?? 0));
    try {
      if (!supportedFile(fd)) {
        throw invalid("service requires a supported local filesystem");
      }
      const e = loadNative().flockfd(fd, 2 | 4); // LOCK_EX|LOCK_NB
      if (e !== 0) {
        // std::fs::File::try_lock: EWOULDBLOCK -> TryLockError::WouldBlock
        // Display "lock acquisition failed because the operation would
        // block"; other errnos -> "<strerror> (os error N)".
        const msg = e === 11
          ? "lock acquisition failed because the operation would block"
          : `${strerror(e)} (os error ${e})`;
        throw invalid(
          `database service already active or lock unavailable: ${msg}`);
      }
      const lease = new DatabaseLease(fd, path, identityOf(fstatSync(fd)));
      if (lease.identity !== identityOf(m)) {
        throw invalid("database identity changed while acquiring service lease");
      }
      lease.validate();
      return lease;
    } catch (e) { try { closeSync(fd); } catch { /* */ } throw e; }
  }
  constructor(fd, path, identity) {
    this._fd = fd; this._path = path; this.identity = identity;
  }
  get path() { return this._path; }
  /** validate() — identity still bound to this live handle. */
  validate() {
    const m = privateMetadata(this._path, false);
    if (!m.isFile() || m.nlink !== 1 ||
        identityOf(m) !== this.identity ||
        identityOf(fstatSync(this._fd)) !== this.identity) {
      throw invalid(
        "database identity changed; stop rather than switch databases");
    }
  }
  close() {
    if (this._fd !== null) { closeSync(this._fd); this._fd = null; }
  }
}
