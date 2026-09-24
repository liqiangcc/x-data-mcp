// Node port of src/local_runtime/paths.rs — private path guards,
// peer-UID authorization, client connect, Endpoint bind/close.
// DatabaseLease is NOT re-implemented here: it lives in
// src_node/storage/database_lease.mjs (NM-056) and is re-exported by
// index.mjs — reconcile, don't duplicate (TASK-NM-062).
import fs from "node:fs";
import net from "node:net";
import { basename, dirname, join } from "node:path";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
let native = null;
const loadNative = () => {
  if (!native) native = require("../native/rename_noreplace.node");
  return native;
};

export const invalid = (m) =>
  Object.assign(new Error(m), { kind: "invalid_input" });

// Rust uid() = libc::geteuid() — effective UID.
const uid = () => process.geteuid();

const LOCAL_MAGICS = new Set([0xef53, 0x58465342, 0x9123683e, 0x01021994]);
const supportedFile = (fd) => {
  if (process.env.X_DATA_MCP_UNSAFE_NONLOCAL_FS !== undefined) return true;
  return LOCAL_MAGICS.has(loadNative().statfsmagic(fd));
};

// paths.rs:21-33 — lstat only: owned by euid, mode & 077 == 0, not a
// symlink; directory=true also requires a directory.
export function privateMetadata(path, directory = false) {
  const m = fs.lstatSync(path);
  if (m.uid !== uid() || (m.mode & 0o077) !== 0 || m.isSymbolicLink() ||
      (directory && !m.isDirectory())) {
    throw invalid(
      "runtime path must be owned by this UID, private, and not a symlink");
  }
  return m;
}

export const identityOf = (m) => ({ device: m.dev, inode: m.ino });
const identityEq = (a, b) =>
  a.device === b.device && a.inode === b.inode;

// paths.rs:34-52 — explicit private parent, identity-stable across
// lstat vs open-fd, on a supported local filesystem.
function parent(path) {
  const p = dirname(path);
  if (!p || p === "." || p === path) {
    throw invalid("socket requires an explicit private parent directory");
  }
  const expected = privateMetadata(p, true);
  const dfd = fs.openSync(
    p, fs.constants.O_RDONLY | fs.constants.O_DIRECTORY |
       (fs.constants.O_NOFOLLOW ?? 0));
  try {
    if (!identityEq(identityOf(expected), identityOf(fs.fstatSync(dfd))) ||
        !supportedFile(dfd)) {
      throw invalid(
        "socket directory changed or is not a supported local filesystem");
    }
  } finally {
    fs.closeSync(dfd);
  }
}

// paths.rs:123-147 — getsockopt(SO_PEERCRED); uid must equal euid.
function peerCheck(sock) {
  const fd = sock._handle?.fd; // private accessor — see SPIKE-S13 constraint 3
  if (typeof fd !== "number" || fd < 0)
    throw invalid("peer credential unavailable");
  const cred = loadNative().getpeereuid(fd);
  if (cred.uid !== uid()) throw invalid("forbidden peer UID");
}

// DatabaseLease.identity in storage/database_lease.mjs is "dev:ino";
// the owner marker serializes the Rust Identity struct verbatim.
const leaseIdentity = (db) => {
  const id = db.identity;
  if (typeof id === "string") {
    const [device, inode] = id.split(":").map(Number);
    return { device, inode };
  }
  return { device: id.device, inode: id.inode };
};

// paths.rs:148-160 — client-side connect: parent guard, private socket,
// peer-UID check on the connected fd, identity re-check.
export async function connect(path) {
  parent(path);
  const m = privateMetadata(path, false);
  if (!m.isSocket()) throw invalid("endpoint is not a socket");
  const sock = await new Promise((resolve, reject) => {
    const s = net.connect(path);
    s.once("connect", () => resolve(s));
    s.once("error", reject);
  });
  try {
    peerCheck(sock);
    if (!identityEq(identityOf(privateMetadata(path, false)),
                    identityOf(m))) {
      throw invalid("endpoint changed during connection");
    }
  } catch (e) {
    sock.destroy();
    throw e;
  }
  return sock;
}

export class Endpoint {
  constructor(listener, path, identity) {
    this.listener = listener; // net.Server — the UnixListener equivalent
    this.path = path;
    this.identity = identity;
  }

  // paths.rs:167-235 verbatim ordering: parent guard -> db.validate ->
  // owner marker (atomic publish or verify) -> stale-socket cleanup ->
  // bind -> chmod 0600 -> identity capture.
  static async bind(path, db) {
    parent(path);
    db.validate();
    const dir = dirname(path);
    const owner = join(dir, `${basename(path)}.owner.json`);
    const dbIdentity = leaseIdentity(db);

    let pathExists = true, ownerExists = true;
    try { fs.lstatSync(path); } catch { pathExists = false; }
    try { fs.lstatSync(owner); } catch { ownerExists = false; }
    if (pathExists && !ownerExists)
      throw invalid("unregistered existing endpoint; no clobber");

    // Publish a complete owner marker, never an empty partial file:
    // write+fsync a private temp in the same dir, then rename_noreplace
    // (persist_noclobber equivalent — EEXIST on any existing target).
    // Rust attempts the publish unconditionally and verifies the
    // existing file only on EEXIST; same order here.
    const tmp = join(
      dir, `.${basename(path)}.owner.tmp-${process.pid}-${Date.now()}`);
    const tfd = fs.openSync(
      tmp, fs.constants.O_WRONLY | fs.constants.O_CREAT |
           fs.constants.O_EXCL, 0o600);
    try {
      fs.writeSync(tfd, JSON.stringify(dbIdentity));
      fs.fsyncSync(tfd);
    } finally { fs.closeSync(tfd); }
    try {
      loadNative().rename_noreplace(tmp, owner);
    } catch (e) {
      try { fs.unlinkSync(tmp); } catch { /* temp cleanup */ }
      if (e.code !== "EEXIST") throw e;
      if (!identityEq(readOwner(owner), dbIdentity))
        throw invalid("endpoint belongs to another database");
    }

    // Existing socket: must be a socket, must not answer a connect.
    let m = null;
    try { m = fs.lstatSync(path); }
    catch (e) { if (e.code !== "ENOENT") throw e; }
    if (m) {
      privateMetadata(path, false);
      if (!m.isSocket())
        throw invalid("existing endpoint is not a socket");
      const conn = await tryConnect(path);
      if (conn === "ok") throw invalid("endpoint already active");
      if (conn !== "ECONNREFUSED")
        throw Object.assign(new Error(conn), { code: conn });
      if (!identityEq(identityOf(fs.lstatSync(path)), identityOf(m)))
        throw invalid("endpoint changed before stale cleanup");
      fs.unlinkSync(path);
    }

    const listener = net.createServer();
    await new Promise((resolve, reject) => {
      listener.once("error", reject);
      listener.listen(path, resolve);
    });
    fs.chmodSync(path, 0o600);
    // Rust set_nonblocking(true): Node listeners are evented — no-op.
    const identity = identityOf(privateMetadata(path, false));
    return new Endpoint(listener, path, identity);
  }

  // paths.rs:236-238 — peer-UID authorization of an accepted stream.
  authorize(sock) {
    peerCheck(sock);
  }

  // paths.rs:240-247 Drop: remove the socket file iff it is still ours.
  close() {
    try { this.listener.close(); } catch { /* already closed */ }
    try {
      const m = fs.lstatSync(this.path);
      if (m.isSocket() && identityEq(identityOf(m), this.identity))
        fs.unlinkSync(this.path);
    } catch { /* gone */ }
  }
}

// Read + strict-shape-verify the owner marker (paths.rs:187-200):
// ≤1024 bytes, serde shape {device,inode} only, else reject.
function readOwner(owner) {
  const m = privateMetadata(owner, false);
  if (!m.isFile()) throw invalid("invalid endpoint owner file");
  const fd = fs.openSync(owner, fs.constants.O_RDONLY |
    (fs.constants.O_NOFOLLOW ?? 0));
  let raw;
  try {
    const buf = Buffer.alloc(1025);
    const n = fs.readSync(fd, buf, 0, 1025, 0);
    raw = buf.subarray(0, n);
  } finally { fs.closeSync(fd); }
  if (raw.length > 1024)
    throw invalid("endpoint belongs to another database");
  let parsed;
  try { parsed = JSON.parse(raw.toString()); }
  catch (e) { throw invalid(`owner marker parse: ${e.message}`); }
  const keys = parsed && typeof parsed === "object" && !Array.isArray(parsed)
    ? Object.keys(parsed).sort() : [];
  if (keys.length !== 2 || keys[0] !== "device" || keys[1] !== "inode" ||
      typeof parsed.device !== "number" || typeof parsed.inode !== "number")
    throw invalid("owner marker parse: unexpected field or type");
  return parsed;
}

function tryConnect(path) {
  return new Promise((resolve) => {
    const s = net.connect(path);
    s.once("connect", () => { s.destroy(); resolve("ok"); });
    s.once("error", (e) => resolve(e.code || "EIO"));
  });
}
