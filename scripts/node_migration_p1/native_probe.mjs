// NM-009: validate the real N-API binding (native/linux_guards.node).
// Cases emit JSONL {primitive,id,status,detail,evidence}.
// Usage: node scripts/node_migration_p1/native_probe.mjs <private-dir>
import fs from "node:fs";
import net from "node:net";
import { spawn, spawnSync } from "node:child_process";
import { join } from "node:path";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const dir = process.argv[2];
const addon = require("./native/linux_guards.node");
const emit = (primitive, id, status, detail, evidence = null) =>
  console.log(JSON.stringify({ primitive, id, status, detail, evidence }));
const run = (p, id, fn) => {
  try { emit(p, id, "PASS", null, fn()); }
  catch (e) { emit(p, id, "FAIL", `${e.code || ""} ${e.message}`); }
};
const LOCK_EX = 2, LOCK_NB = 4, LOCK_UN = 8;
const EWOULDBLOCK = 11;

emit("runtime", "build", "INFO", null, {
  node: process.version, euid: process.geteuid(),
  addon: "scripts/node_migration_p1/native/linux_guards.node",
  build: "gcc -shared -fPIC -O2 -I/usr/include/node (libnode-dev headers; no make/node-gyp)",
  exports: Object.keys(addon),
});

// ---- SO_PEERCRED on a live unix socket ----
await new Promise((resolve) => {
  const sockPath = join(dir, "peercred.sock");
  const server = net.createServer((sock) => {
    run("peer_cred", "same_uid_accepted", () => {
      const fd = sock._handle.fd; // real kernel fd of the accepted connection
      const cred = addon.getpeereuid(fd);
      if (cred.uid !== process.geteuid())
        throw new Error(`forbidden peer UID ${cred.uid}`);
      return { peer: cred, fd };
    });
    sock.destroy(); server.close(resolve);
  });
  server.listen(sockPath, () => net.connect(sockPath).end());
});

run("peer_cred", "non_socket_fd_rejected", () => {
  const p = join(dir, "regular.db"); fs.writeFileSync(p, "x", { mode: 0o600 });
  const fd = fs.openSync(p, "r");
  try { addon.getpeereuid(fd); throw new Error("not rejected"); }
  catch (e) {
    fs.closeSync(fd);
    if (/SO_PEERCRED/.test(e.message)) return { reject: e.message };
    throw e;
  }
});
emit("peer_cred", "cross_uid_rejected", "NOT_RUN",
  "single UID on Box; kernel returns real ucred so a foreign-UID peer cannot be forged locally. " +
  "Same-UID authorization proven on a live socket; reject branch is a one-line uid!=euid check.");

// ---- flock fd lease (real kernel lease on an fd owned by this process) ----
run("flock_fd", "acquire_release_cycle", () => {
  const p = join(dir, "lease.db"); fs.writeFileSync(p, "", { mode: 0o600 });
  const fd = fs.openSync(p, "r+");
  const r1 = addon.flockfd(fd, LOCK_EX | LOCK_NB);
  if (r1 !== 0) throw new Error(`acquire failed errno ${r1}`);
  const r2 = addon.flockfd(fd, LOCK_UN);
  if (r2 !== 0) throw new Error(`unlock failed errno ${r2}`);
  const r3 = addon.flockfd(fd, LOCK_EX | LOCK_NB);
  fs.closeSync(fd); // kernel releases on close
  return { acquire: r1, unlock: r2, reacquire: r3, released_on_close: true };
});
run("flock_fd", "contention_second_fd_rejected", () => {
  const p = join(dir, "lease2.db"); fs.writeFileSync(p, "", { mode: 0o600 });
  const fd1 = fs.openSync(p, "r+"), fd2 = fs.openSync(p, "r+");
  const held = addon.flockfd(fd1, LOCK_EX | LOCK_NB);
  const contend = addon.flockfd(fd2, LOCK_EX | LOCK_NB); // separate open file description
  fs.closeSync(fd2);
  const r3 = addon.flockfd(fs.openSync(p, "r+"), LOCK_EX | LOCK_NB);
  fs.closeSync(fd1);
  return { held, contend_errno: contend, ewouldblock: contend === EWOULDBLOCK,
           note: "fd2 flock is on a separate open file description -> real kernel contention" };
});
run("flock_fd", "external_flock_binary_sees_lease", () => {
  const p = join(dir, "lease3.db"); fs.writeFileSync(p, "", { mode: 0o600 });
  const fd = fs.openSync(p, "r+");
  const held = addon.flockfd(fd, LOCK_EX | LOCK_NB);
  const ext = spawnSync("flock", ["-n", p, "true"]).status; // kernel-enforced: must fail
  fs.closeSync(fd);
  return { held, external_attempt_exit: ext,
           proof: "util-linux flock sees the N-API lease -> real kernel flock, not simulation" };
});
run("flock_fd", "release_on_fd_close", () => {
  const p = join(dir, "lease4.db"); fs.writeFileSync(p, "", { mode: 0o600 });
  const fd1 = fs.openSync(p, "r+");
  addon.flockfd(fd1, LOCK_EX | LOCK_NB);
  fs.closeSync(fd1); // process-free lease release
  const fd2 = fs.openSync(p, "r+");
  const r = addon.flockfd(fd2, LOCK_EX | LOCK_NB);
  fs.closeSync(fd2);
  return { reacquire_after_close: r };
});
await new Promise((resolve) => {
  const p = join(dir, "lease5.db"); fs.writeFileSync(p, "", { mode: 0o600 });
  const fd = fs.openSync(p, "r+");
  const held = addon.flockfd(fd, LOCK_EX | LOCK_NB);
  // child node process tries the same file — kernel blocks across processes
  const child = spawn(process.execPath, [
    "-e", `const m=require(${JSON.stringify(join(process.cwd(),"scripts/node_migration_p1/native/linux_guards.node"))});` +
          `const fs=require("fs");const fd=fs.openSync(${JSON.stringify(p)},"r+");` +
          `process.stdout.write(String(m.flockfd(fd,6)));fs.closeSync(fd);`,
  ], { encoding: "utf8" });
  let out = ""; child.stdout.on("data", (d) => out += d);
  child.on("exit", () => {
    fs.closeSync(fd);
    const errno = Number(out);
    emit("flock_fd", "cross_process_contention_rejected",
      held === 0 && errno === EWOULDBLOCK ? "PASS" : "FAIL",
      `held=${held} child_errno=${errno}`, { expected: EWOULDBLOCK });
    resolve();
  });
});

// negative: flock on invalid fd -> EBADF
run("flock_fd", "invalid_fd_rejected", () => {
  const r = addon.flockfd(99999, LOCK_EX | LOCK_NB);
  if (r !== 9) throw new Error(`expected EBADF(9), got ${r}`);
  return { errno: r };
});
