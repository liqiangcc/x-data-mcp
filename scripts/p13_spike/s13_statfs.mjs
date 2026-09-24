// S13 proof: fstatfs/statfs magic-number whitelist — parity with
// src/local_filesystem.rs::supported_file (:3-23). Two mechanisms:
//   a) fs.statfsSync(path).type  (Node builtin, no addon needed)
//   b) native statfsmagic(fd)    (fstatfs on an already-open fd — the
//      exact Rust semantics: the check binds to the opened description)
// Whitelist = ext2/3/4 0xef53, XFS 0x58465342, Btrfs 0x9123683e,
// tmpfs 0x01021994. Box: /dev/shm=tmpfs (accept), /tmp+/home=overlayfs
// (refuse). Also proves the X_DATA_MCP_UNSAFE_NONLOCAL_FS opt-out shape.
// Usage: node scripts/p13_spike/s13_statfs.mjs <private-dir>
import fs from "node:fs";
import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { emit, run, finish, loadNative, dir, ADDON } from "./common.mjs";

const addon = loadNative();
const LOCAL_MAGICS = new Set([0xef53, 0x58465342, 0x9123683e, 0x01021994]);

// Mirror of supported_file: env opt-out first, then fstatfs whitelist.
const supportedFile = (fd) => {
  if (process.env.X_DATA_MCP_UNSAFE_NONLOCAL_FS !== undefined) return true;
  return LOCAL_MAGICS.has(addon.statfsmagic(fd));
};
const supportedPath = (p) => {
  if (process.env.X_DATA_MCP_UNSAFE_NONLOCAL_FS !== undefined) return true;
  return LOCAL_MAGICS.has(fs.statfsSync(p).type & 0xffffffff);
};

run("statfs", "whitelist_table_matches_rust", () => {
  const allow = [0xef53, 0x58465342, 0x9123683e, 0x01021994];
  const deny = [0x6969, 0xff534d42, 0x794c7630, 0x65735546, 0];
  if (!allow.every((k) => LOCAL_MAGICS.has(k)) ||
      !deny.every((k) => !LOCAL_MAGICS.has(k)))
    throw new Error("table drift vs local_filesystem.rs:5");
  return { allow, deny };
});

run("statfs", "tmpfs_devshm_accepted", () => {
  const t = fs.statfsSync("/dev/shm").type & 0xffffffff;
  if (!supportedPath("/dev/shm")) throw new Error(`tmpfs ${t.toString(16)} refused`);
  return { path: "/dev/shm", magic: `0x${t.toString(16)}` };
});

run("statfs", "overlayfs_tmp_refused", () => {
  const t = fs.statfsSync("/tmp").type & 0xffffffff;
  if (supportedPath("/tmp")) throw new Error("overlayfs /tmp accepted");
  return { path: "/tmp", magic: `0x${t.toString(16)}`, refused: true };
});

run("statfs", "overlayfs_home_refused", () => {
  const t = fs.statfsSync("/home").type & 0xffffffff;
  if (supportedPath("/home")) throw new Error("overlayfs /home accepted");
  return { path: "/home", magic: `0x${t.toString(16)}`, refused: true };
});

run("statfs", "fd_bound_tmpfs_accepted", () => {
  // fstatfs on the open description — what DatabaseLease actually checks.
  const p = join(dir, "lease.db");
  fs.writeFileSync(p, "", { mode: 0o600 });
  const fd = fs.openSync(p, "r");
  try {
    const magic = addon.statfsmagic(fd);
    if (!supportedFile(fd)) throw new Error(`fd magic ${magic} refused`);
    return { magic: `0x${magic.toString(16)}`, via: "fstatfs(fd)" };
  } finally { fs.closeSync(fd); }
});

run("statfs", "fd_bound_overlayfs_refused", () => {
  // /tmp is overlayfs on Box — an fd opened there must fail the gate.
  const fd = fs.openSync("/tmp", fs.constants.O_RDONLY | fs.constants.O_DIRECTORY);
  try {
    const magic = addon.statfsmagic(fd);
    if (supportedFile(fd)) throw new Error("overlayfs fd accepted");
    return { magic: `0x${magic.toString(16)}`, refused: true };
  } finally { fs.closeSync(fd); }
});

run("statfs", "unsafe_nonlocal_fs_opt_out", () => {
  // Opt-out parity: env set -> any fs accepted (fail-open only on explicit
  // operator request). Checked in a child so the flag is process-scoped.
  const r = spawnSync(process.execPath, [
    "-e",
    `const m=require(${JSON.stringify(ADDON)});` +
    `const fs=require("fs");` +
    `const ok = process.env.X_DATA_MCP_UNSAFE_NONLOCAL_FS !== undefined || ` +
    `new Set([0xef53,0x58465342,0x9123683e,0x01021994]).has(` +
    `m.statfsmagic(fs.openSync("/tmp", fs.constants.O_RDONLY | fs.constants.O_DIRECTORY)));` +
    `process.exit(ok?0:1);`,
  ], { env: { ...process.env, X_DATA_MCP_UNSAFE_NONLOCAL_FS: "1" } });
  if (r.status !== 0) throw new Error(`child exit ${r.status}`);
  return { child_exit: r.status, env: "X_DATA_MCP_UNSAFE_NONLOCAL_FS=1", on: "/tmp overlayfs" };
});

finish();
