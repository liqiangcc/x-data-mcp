// S13 proof: flock(LOCK_EX|LOCK_NB) on a regular file via the committed
// binding src_node/native/rename_noreplace.node::flockfd — parity with
// fs2::try_lock in src/local_runtime/paths.rs::DatabaseLease.
// Usage: node scripts/p13_spike/s13_flock.mjs <private-dir>
import fs from "node:fs";
import { spawn, spawnSync } from "node:child_process";
import { join } from "node:path";
import { emit, run, finish, loadNative, dir, ADDON } from "./common.mjs";

const addon = loadNative();
const LOCK_EX = 2, LOCK_NB = 4, LOCK_UN = 8;
const EBADF = 9, EWOULDBLOCK = 11;
const mk = (name) => {
  const p = join(dir, name);
  fs.writeFileSync(p, "", { mode: 0o600 });
  return p;
};

run("flock", "acquire_release_reacquire", () => {
  const fd = fs.openSync(mk("l1.db"), "r+");
  const r1 = addon.flockfd(fd, LOCK_EX | LOCK_NB);
  const r2 = addon.flockfd(fd, LOCK_UN);
  const r3 = addon.flockfd(fd, LOCK_EX | LOCK_NB);
  fs.closeSync(fd); // kernel releases on close
  if (r1 || r2 || r3) throw new Error(`rc=${r1},${r2},${r3}`);
  return { acquire: r1, unlock: r2, reacquire: r3 };
});

run("flock", "contention_second_ofd_rejected", () => {
  const p = mk("l2.db");
  const fd1 = fs.openSync(p, "r+"), fd2 = fs.openSync(p, "r+");
  const held = addon.flockfd(fd1, LOCK_EX | LOCK_NB);
  const contend = addon.flockfd(fd2, LOCK_EX | LOCK_NB); // separate open file description
  fs.closeSync(fd2); fs.closeSync(fd1);
  if (held !== 0 || contend !== EWOULDBLOCK)
    throw new Error(`held=${held} contend=${contend}`);
  return { held, contend_errno: contend, expected: EWOULDBLOCK };
});

run("flock", "external_flock_binary_sees_lease", () => {
  const p = mk("l3.db");
  const fd = fs.openSync(p, "r+");
  const held = addon.flockfd(fd, LOCK_EX | LOCK_NB);
  const ext = spawnSync("flock", ["-n", p, "true"]).status; // must fail
  fs.closeSync(fd);
  if (held !== 0 || ext === 0) throw new Error(`held=${held} ext=${ext}`);
  return { held, external_exit: ext,
           note: "util-linux flock sees the N-API lease -> real kernel flock" };
});

run("flock", "release_on_fd_close", () => {
  const p = mk("l4.db");
  const fd1 = fs.openSync(p, "r+");
  addon.flockfd(fd1, LOCK_EX | LOCK_NB);
  fs.closeSync(fd1);
  const fd2 = fs.openSync(p, "r+");
  const r = addon.flockfd(fd2, LOCK_EX | LOCK_NB);
  fs.closeSync(fd2);
  if (r !== 0) throw new Error(`reacquire errno ${r}`);
  return { reacquire_after_close: r };
});

await new Promise((resolve) => {
  const p = mk("l5.db");
  const fd = fs.openSync(p, "r+");
  const held = addon.flockfd(fd, LOCK_EX | LOCK_NB);
  const child = spawn(process.execPath, [
    "-e",
    `const m=require(${JSON.stringify(ADDON)});` +
    `const fs=require("fs");const fd=fs.openSync(${JSON.stringify(p)},"r+");` +
    `process.stdout.write(String(m.flockfd(fd,6)));fs.closeSync(fd);`,
  ], { encoding: "utf8" });
  let out = "";
  child.stdout.on("data", (d) => (out += d));
  child.on("exit", () => {
    fs.closeSync(fd);
    const errno = Number(out);
    emit("flock", "cross_process_contention_rejected",
      held === 0 && errno === EWOULDBLOCK ? "PASS" : "FAIL",
      `held=${held} child_errno=${errno}`, { expected: EWOULDBLOCK });
    resolve();
  });
});

run("flock", "invalid_fd_rejected", () => {
  const r = addon.flockfd(99999, LOCK_EX | LOCK_NB);
  if (r !== EBADF) throw new Error(`expected EBADF(9), got ${r}`);
  return { errno: r };
});

finish();
