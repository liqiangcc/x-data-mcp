// S13 proof: O_NOFOLLOW / O_DIRECTORY opens via fs.constants + numeric
// flags — parity with OpenOptions::custom_flags in
// src/local_runtime/paths.rs (:42 O_NOFOLLOW|O_DIRECTORY on the parent
// dir, :83 O_NOFOLLOW|O_CLOEXEC on the db, :194 O_NOFOLLOW on owner).
// Usage: node scripts/p13_spike/s13_openflags.mjs <private-dir>
import fs from "node:fs";
import { join } from "node:path";
import { emit, run, expectErr, finish, dir } from "./common.mjs";

const C = fs.constants;
const RD = C.O_RDONLY;

run("openflags", "constants_present", () => {
  // O_CLOEXEC is intentionally absent from fs.constants: libuv sets
  // close-on-exec on every fd it opens, so the Rust custom_flags
  // (O_CLOEXEC) are already the default. O_NOFOLLOW/O_DIRECTORY/O_EXCL
  // are the behavioral flags the port needs.
  for (const k of ["O_NOFOLLOW", "O_DIRECTORY", "O_EXCL"])
    if (typeof C[k] !== "number") throw new Error(`${k} missing`);
  return { O_NOFOLLOW: C.O_NOFOLLOW, O_DIRECTORY: C.O_DIRECTORY,
           O_EXCL: C.O_EXCL, O_CLOEXEC: "implicit (libuv default)" };
});

// fixtures
const realFile = join(dir, "real.db");
fs.writeFileSync(realFile, "x", { mode: 0o600 });
const realDir = join(dir, "realdir");
fs.mkdirSync(realDir, { mode: 0o700 });
const fileLink = join(dir, "file-link");
fs.symlinkSync(realFile, fileLink);
const dirLink = join(dir, "dir-link");
fs.symlinkSync(realDir, dirLink);

run("openflags", "nofollow_regular_file_accepted", () => {
  const fd = fs.openSync(realFile, RD | C.O_NOFOLLOW | C.O_CLOEXEC);
  const st = fs.fstatSync(fd);
  fs.closeSync(fd);
  return { is_file: st.isFile() };
});

expectErr("openflags", "nofollow_symlink_rejected_eloop", /ELOOP/, () => {
  const fd = fs.openSync(fileLink, RD | C.O_NOFOLLOW);
  fs.closeSync(fd);
});

expectErr("openflags", "nofollow_dir_symlink_rejected", /ELOOP|ENOTDIR/, () => {
  // kernel 6.12: O_NOFOLLOW opens the symlink inode itself and the
  // O_DIRECTORY check then fails ENOTDIR (ELOOP for the plain
  // O_NOFOLLOW file case above). Either errno = refused; Rust issues
  // the identical openat() so errno parity holds by construction.
  const fd = fs.openSync(dirLink, RD | C.O_NOFOLLOW | C.O_DIRECTORY);
  fs.closeSync(fd);
});

run("openflags", "directory_open_accepted", () => {
  const fd = fs.openSync(realDir, RD | C.O_DIRECTORY | C.O_NOFOLLOW);
  const st = fs.fstatSync(fd);
  fs.closeSync(fd);
  if (!st.isDirectory()) throw new Error("fd is not a directory");
  return { is_dir: true };
});

expectErr("openflags", "directory_on_file_rejected_enotdir", /ENOTDIR/, () => {
  const fd = fs.openSync(realFile, RD | C.O_DIRECTORY);
  fs.closeSync(fd);
});

expectErr("openflags", "directory_read_eisdir_on_read", /EISDIR|illegal operation/, () => {
  // A dir fd opens fine with O_RDONLY|O_DIRECTORY but read() must fail —
  // matches Rust where File::read on a dir fd errors at use time.
  const fd = fs.openSync(realDir, RD | C.O_DIRECTORY);
  try { fs.readSync(fd, Buffer.alloc(1), 0, 1, 0); }
  finally { fs.closeSync(fd); }
});

run("openflags", "excl_create_and_conflict", () => {
  const p = join(dir, "excl.db");
  const fd = fs.openSync(p, C.O_WRONLY | C.O_CREAT | C.O_EXCL, 0o600);
  fs.closeSync(fd);
  try {
    const fd2 = fs.openSync(p, C.O_WRONLY | C.O_CREAT | C.O_EXCL);
    fs.closeSync(fd2);
    throw new Error("second O_EXCL create not rejected");
  } catch (e) {
    if (e.code === "EEXIST") return { first: "created", second: "EEXIST" };
    throw e;
  }
});

finish();
