// S13 proof: getsockopt(SO_PEERCRED) on a unix socket via the committed
// binding rename_noreplace.node::getpeereuid — parity with peer() in
// src/local_runtime/paths.rs (:123-147). Node core has no API for peer
// credentials; the N-API addon is the mechanism.
// Usage: node scripts/p13_spike/s13_peercred.mjs <private-dir>
import fs from "node:fs";
import net from "node:net";
import { spawn } from "node:child_process";
import { join } from "node:path";
import { emit, run, finish, loadNative, dir } from "./common.mjs";

const addon = loadNative();
let childPid = null;

// Accepted-connection fd carries the CONNECTING peer's credentials.
// Client is a separate process so pid is a real cross-process check.
await new Promise((resolve, reject) => {
  const sockPath = join(dir, "peercred.sock");
  const server = net.createServer((sock) => {
    run("peercred", "peer_pid_uid_gid_returned", () => {
      const fd = sock._handle.fd; // real kernel fd of the accepted connection
      const cred = addon.getpeereuid(fd);
      if (typeof cred.pid !== "number" || typeof cred.uid !== "number" ||
          typeof cred.gid !== "number")
        throw new Error(`bad shape ${JSON.stringify(cred)}`);
      if (cred.uid !== process.geteuid())
        throw new Error(`forbidden peer UID ${cred.uid}`);
      if (cred.gid !== process.getegid())
        throw new Error(`peer GID ${cred.gid} != egid ${process.getegid()}`);
      if (cred.pid !== childPid)
        throw new Error(`peer pid ${cred.pid} != client pid ${childPid}`);
      return { cred, client_pid: childPid, fd };
    });
    sock.destroy();
    server.close(resolve);
  });
  server.once("error", reject);
  server.listen(sockPath, () => {
    // Child connects and holds the socket open briefly so the server can
    // inspect peer credentials of a DIFFERENT process.
    const child = spawn(process.execPath, [
      "-e",
      `const s=require("net").connect(${JSON.stringify(sockPath)});` +
      `s.on("connect",()=>setTimeout(()=>s.end(),400));`,
    ]);
    childPid = child.pid;
  });
});

run("peercred", "non_socket_fd_rejected", () => {
  const p = join(dir, "regular.db");
  fs.writeFileSync(p, "x", { mode: 0o600 });
  const fd = fs.openSync(p, "r");
  try {
    addon.getpeereuid(fd);
    throw new Error("not rejected");
  } catch (e) {
    fs.closeSync(fd);
    if (/SO_PEERCRED/.test(e.message)) return { reject: e.message };
    throw e;
  }
});

run("peercred", "bad_fd_rejected", () => {
  try {
    addon.getpeereuid(99999);
    throw new Error("not rejected");
  } catch (e) {
    if (/SO_PEERCRED/.test(e.message)) return { reject: e.message };
    throw e;
  }
});

emit("peercred", "cross_uid_rejected", "NOT_RUN",
  "single UID on Box; kernel returns the real ucred so a foreign-UID peer " +
  "cannot be forged locally. Same-UID authorization proven on a live " +
  "socket; reject branch is the one-line uid !== euid check, same as " +
  "src/local_runtime/paths.rs:143.");

finish();
