// LeaseRef — worker-side equivalent of DatabaseLease::validate
// (src/local_runtime/paths.rs:109-121).
//
// The main thread holds the authoritative DatabaseLease
// (src_node/storage/database_lease.mjs — flock + fd identity). A
// worker cannot share that fd, so it opens its OWN
// O_RDONLY|O_NOFOLLOW fd on the same path at attach() time — the same
// fail-closed check: if the file is swapped, lstat sees the new inode
// while fstat(this fd) still sees the old one.
//
// NO flock here: flock is per open file description — a second
// LOCK_EX|NB on the same file would contend with the lease the main
// thread already holds (EWOULDBLOCK), so the ref only observes
// identity, never locks.
import { lstatSync, fstatSync, openSync, closeSync, constants } from "node:fs";
import { invalid, privateMetadata, identityOf } from "./paths.mjs";

export class LeaseRef {
  constructor(path, identity) {
    this._path = path;
    this._identity = identity; // "dev:ino" — same shape as DatabaseLease
    this._fd = null;
  }
  static of(lease) {
    return new LeaseRef(lease.path, lease.identity);
  }
  // Serializable form for workerData.
  describe() {
    return { path: this._path, identity: this._identity };
  }
  attach() {
    this._fd = openSync(
      this._path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
    this.validate();
    return this;
  }
  // paths.rs:109-121 verbatim — private + regular + nlink==1 +
  // identity stable across path lookup and the held fd.
  validate() {
    const m = privateMetadata(this._path, false);
    if (!m.isFile() || m.nlink !== 1 ||
        `${identityOf(m).device}:${identityOf(m).inode}` !== this._identity ||
        `${fstatSync(this._fd).dev}:${fstatSync(this._fd).ino}` !==
          this._identity) {
      throw invalid(
        "database identity changed; stop rather than switch databases");
    }
  }
  close() {
    if (this._fd !== null) { closeSync(this._fd); this._fd = null; }
  }
}
