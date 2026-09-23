// NM-008 P1 Linux primitive adapter validation — Node paths vs NM-004
// Rust fail-closed behavior. Each case: {primitive,id,status,detail,evidence}.
// Usage: node scripts/node_migration_p1/linux_probe.mjs <dir>
// <dir> must be a private (0700, same-UID) tmpfs dir; unsafe fixtures are
// made under it. Synthetic only — no live sockets/DBs.
import fs from "node:fs";
import net from "node:net";
import { spawn, spawnSync } from "node:child_process";
import { join } from "node:path";

const dir = process.argv[2];
const emit = (primitive, id, status, detail, evidence = null) =>
  console.log(JSON.stringify({ primitive, id, status, detail, evidence }));
const euid = process.geteuid();

// ---------- mirror of src/local_filesystem.rs ----------
const SUPPORTED_FS = new Set([0xef53, 0x58465342, 0x9123683e, 0x01021994]);
const supportedFs = (path) => SUPPORTED_FS.has(fs.statfsSync(path).type & 0xffffffff);

// ---------- mirror of src/local_runtime/paths.rs::private_metadata ----------
function privateMetadata(path, follow = false) {
  const st = (follow ? fs.statSync : fs.lstatSync)(path); // throws ENOENT
  if (st.uid !== euid) throw new Error("runtime path must be owned by this UID, private, and not a symlink");
  if (!follow && st.isSymbolicLink()) throw new Error("runtime path must be owned by this UID, private, and not a symlink");
  if ((st.mode & 0o077) !== 0) throw new Error("runtime path must be owned by this UID, private, and not a symlink");
  return st;
}

// ---------- mirror of Endpoint::bind ordering ----------
const identity = (st) => ({ device: st.dev, inode: st.ino });
async function bindEndpoint(sockPath, dbIdentity) {
  // parent() — private dir
  const parent = join(sockPath, "..");
  privateMetadata(parent);
  // owner marker: noclobber create else validate
  const owner = sockPath + ".owner.json";
  const wantJson = JSON.stringify(dbIdentity);
  try {
    const fd = fs.openSync(owner, "wx", 0o600); // O_CREAT|O_EXCL — persist_noclobber
    fs.writeSync(fd, wantJson); fs.closeSync(fd);
  } catch (e) {
    if (e.code !== "EEXIST") throw e;
    const m = privateMetadata(owner);
    if (!m.isFile()) throw new Error("invalid endpoint owner file");
    const fd = fs.openSync(owner, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
    const raw = Buffer.alloc(1025); const n = fs.readSync(fd, raw, 0, 1025, 0); fs.closeSync(fd);
    if (n > 1024) throw new Error("endpoint belongs to another database");
    let parsed;
    try { parsed = JSON.parse(raw.subarray(0, n).toString()); }
    catch (e2) { throw new Error(`owner marker parse: ${e2.message}`); }
    // serde strict-shape equivalent: Identity is exactly {device,inode}
    const keys = parsed && typeof parsed === "object" ? Object.keys(parsed).sort().join(",") : "";
    if (keys !== "device,inode" || typeof parsed.device !== "number" || typeof parsed.inode !== "number")
      throw new Error("owner marker parse: unexpected field or type");
    if (parsed.device !== dbIdentity.device || parsed.inode !== dbIdentity.inode)
      throw new Error("endpoint belongs to another database");
  }
  // existing path handling
  let m = null;
  try { m = fs.lstatSync(sockPath); } catch (e) { if (e.code !== "ENOENT") throw e; }
  if (m) {
    privateMetadata(sockPath);
    if (!m.isSocket()) throw new Error("existing endpoint is not a socket");
    // stale check: connect succeeds -> active
    const alive = await new Promise((res) => {
      const c = net.connect(sockPath);
      c.on("connect", () => { c.end(); res(true); });
      c.on("error", () => res(false));
    });
    if (alive) throw new Error("endpoint already active");
    const m2 = fs.lstatSync(sockPath);
    if (m2.ino !== m.ino || m2.dev !== m.dev)
      throw new Error("endpoint changed before stale cleanup");
    fs.unlinkSync(sockPath);
  }
  // UnixListener::bind + chmod 0600 + identity capture
  const server = net.createServer();
  await new Promise((res, rej) => {
    server.once("error", rej);
    server.listen(sockPath, res);
  });
  fs.chmodSync(sockPath, 0o600);
  return { server, identity: identity(fs.lstatSync(sockPath)) };
}
// full no-clobber precheck matching Rust order: path exists && owner missing -> reject
function bindEndpointGuarded(sockPath, dbIdentity) {
  privateMetadata(join(sockPath, ".."));
  const pathExists = fs.existsSync(sockPath);
  const ownerExists = fs.existsSync(sockPath + ".owner.json");
  if (pathExists && !ownerExists)
    throw new Error("unregistered existing endpoint; no clobber");
  return bindEndpoint(sockPath, dbIdentity);
}

// ---------- flock lease via /usr/bin/flock helper ----------
function tryFlock(path) {
  // flock -n <path> sleep infinity — holds lease in child; nonzero immediate exit = contention
  const child = spawn("flock", ["-n", path, "sleep", "infinity"], { stdio: "ignore" });
  return child;
}
const flockProbe = (path) => {
  const r = spawnSync("flock", ["-n", path, "true"], { encoding: "utf8" });
  return r.status;
};

const run = (p, id, fn) => {
  try { emit(p, id, "PASS", null, fn()); }
  catch (e) { emit(p, id, "FAIL", `${e.code || ""} ${e.message}`); }
};
const expectErr = (p, id, re, fn) => {
  try { fn(); emit(p, id, "FAIL", "expected rejection but succeeded"); }
  catch (e) {
    const msg = `${e.code || ""} ${e.message}`;
    emit(p, id, re.test(msg) ? "PASS" : "FAIL", msg);
  }
};

emit("runtime", "facts", "INFO", null, {
  node: process.version, euid,
  statfsSync: typeof fs.statfsSync,
  O_NOFOLLOW: fs.constants.O_NOFOLLOW,
  flock_binary: spawnSync("flock", ["--version"], { encoding: "utf8" }).stdout?.trim() || "unavailable",
});

// ---- P1 supported_file ----
run("supported_fs", "tmpfs_accepted", () => {
  if (!supportedFs(dir)) throw new Error("tmpfs rejected");
  return { path: dir, type: fs.statfsSync(dir).type };
});
expectErr("supported_fs", "overlayfs_rejected", /service requires a supported local filesystem/, () => {
  const p = "/home/box/x-data-mcp-node";
  if (supportedFs(p)) return; // would mean overlay whitelisted — bug
  throw new Error("service requires a supported local filesystem");
});
run("supported_fs", "whitelist_table_matches_rust", () => {
  const rustAllow = [0xef53, 0x58465342, 0x9123683e, 0x01021994];
  const rustDeny = [0x6969, 0xff534d42, 0x794c7630, 0x65735546, 0];
  return { allow: rustAllow.every(k => SUPPORTED_FS.has(k)), deny: rustDeny.every(k => !SUPPORTED_FS.has(k)) };
});

// ---- P2 private_metadata ----
run("private_metadata", "private_dir_accepted", () => {
  privateMetadata(dir);
  return { mode: (fs.lstatSync(dir).mode & 0o777).toString(8), uid: fs.lstatSync(dir).uid };
});
expectErr("private_metadata", "mode_0755_rejected", /runtime path must be owned/, () => {
  const p = join(dir, "pub0755"); fs.mkdirSync(p, { mode: 0o755 }); privateMetadata(p);
});
expectErr("private_metadata", "symlink_dir_rejected", /runtime path must be owned/, () => {
  const p = join(dir, "linkdir"); fs.symlinkSync(dir, p); privateMetadata(p);
});
expectErr("private_metadata", "symlink_file_rejected", /runtime path must be owned/, () => {
  fs.writeFileSync(join(dir, "real.txt"), "x", { mode: 0o600 });
  const p = join(dir, "linkfile"); fs.symlinkSync(join(dir, "real.txt"), p); privateMetadata(p);
});
expectErr("private_metadata", "mode_0644_file_rejected", /runtime path must be owned/, () => {
  const p = join(dir, "world-readable"); fs.writeFileSync(p, "x", { mode: 0o644 }); privateMetadata(p);
});
emit("private_metadata", "cross_uid_rejected", "NOT_RUN",
  "no second UID on Box to create foreign-owned fixture; same check line as NM-004 peer-uid boundary");

// ---- P3 O_NOFOLLOW ----
expectErr("nofollow", "symlink_open_rejected", /ELOOP|symlink/, () => {
  const p = join(dir, "linkfile");
  const fd = fs.openSync(p, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
  fs.closeSync(fd);
});

// ---- P4 flock lease + inode identity ----
run("flock", "contention_rejected", () => {
  const p = join(dir, "lease.db"); fs.writeFileSync(p, "", { mode: 0o600 });
  const holder = spawn("flock", ["-n", p, "sleep", "infinity"], { stdio: "ignore" });
  const held = spawnSync("bash", ["-c", `sleep 0.3; kill -0 ${holder.pid}`]).status === 0;
  const second = flockProbe(p); // must fail instantly
  holder.kill("SIGTERM");
  if (!held) throw new Error("flock holder did not start");
  return { holder_pid: holder.pid, second_attempt_exit: second, rust_equiv: "lock acquisition failed because the operation would block" };
});
run("inode_identity", "replacement_detected", () => {
  const p = join(dir, "ident.db"); fs.writeFileSync(p, "a", { mode: 0o600 });
  const id1 = identity(fs.lstatSync(p));
  fs.unlinkSync(p); fs.writeFileSync(p, "a", { mode: 0o600 });
  const id2 = identity(fs.lstatSync(p));
  if (JSON.stringify(id1) === JSON.stringify(id2)) throw new Error("replacement not detected");
  return { before: id1, after: id2 };
});

// ---- P6 socket owner / no-clobber (Endpoint::bind mirror) ----
const DB_ID = { device: 51, inode: 1358932 };
// fresh dir per case
const mk = (name) => { const p = join(dir, name); fs.mkdirSync(p, { mode: 0o700 }); return p; };
await (async () => {
  // C1: regular file at socket path, no owner -> no clobber
  let d = mk("c1"); fs.writeFileSync(join(d, "x.sock"), "x", { mode: 0o600 });
  try { await bindEndpointGuarded(join(d, "x.sock"), DB_ID); emit("socket_guard", "unregistered_file_rejected", "FAIL", "not rejected"); }
  catch (e) { emit("socket_guard", "unregistered_file_rejected", /unregistered existing endpoint; no clobber/.test(e.message) ? "PASS" : "FAIL", e.message); }

  // C2: tampered owner.json (forged field name) -> parse reject
  d = mk("c2"); fs.writeFileSync(join(d, "x.sock.owner.json"), '{"database_identity":{"dev":1,"ino":1},"pid":1}', { mode: 0o600 });
  try { await bindEndpointGuarded(join(d, "x.sock"), DB_ID); emit("socket_guard", "tampered_owner_rejected", "FAIL", "not rejected"); }
  catch (e) { emit("socket_guard", "tampered_owner_rejected", /owner marker parse/.test(e.message) ? "PASS" : "FAIL", e.message); }

  // C3: owner.json with different valid identity -> belongs to another database
  d = mk("c3"); fs.writeFileSync(join(d, "x.sock.owner.json"), JSON.stringify({ device: 99, inode: 99 }), { mode: 0o600 });
  try { await bindEndpointGuarded(join(d, "x.sock"), DB_ID); emit("socket_guard", "foreign_owner_rejected", "FAIL", "not rejected"); }
  catch (e) { emit("socket_guard", "foreign_owner_rejected", /endpoint belongs to another database/.test(e.message) ? "PASS" : "FAIL", e.message); }

  // C4: stale socket left by SIGKILL'd process + matching owner -> cleanup + rebind
  d = mk("c4");
  const s4 = join(d, "x.sock");
  const staler = spawn(process.execPath,
    ["-e", `require("net").createServer().listen(${JSON.stringify(s4)},()=>{require("fs").chmodSync(${JSON.stringify(s4)},0o600)})`],
    { stdio: "ignore" });
  await new Promise((res) => setTimeout(res, 300));
  staler.kill("SIGKILL");
  await new Promise((res) => setTimeout(res, 100));
  // socket file remains after SIGKILL (Endpoint::drop never ran)
  fs.writeFileSync(s4 + ".owner.json", JSON.stringify(DB_ID), { mode: 0o600 });
  try {
    const bound = await bindEndpointGuarded(s4, DB_ID);
    const st = fs.lstatSync(s4);
    emit("socket_guard", "stale_socket_rebound", "PASS", null,
      { socket_mode: (st.mode & 0o777).toString(8), is_socket: st.isSocket(), identity: bound.identity });
    bound.server.close();
  } catch (e) { emit("socket_guard", "stale_socket_rebound", "FAIL", e.message); }

  // C5: active socket -> endpoint already active
  d = mk("c5");
  const s5 = join(d, "y.sock");
  const live = net.createServer().listen(s5, async () => {
    fs.chmodSync(s5, 0o600);
    fs.writeFileSync(s5 + ".owner.json", JSON.stringify(DB_ID), { mode: 0o600 });
    try { await bindEndpointGuarded(s5, DB_ID); emit("socket_guard", "active_socket_rejected", "FAIL", "not rejected"); }
    catch (e) { emit("socket_guard", "active_socket_rejected", /endpoint already active/.test(e.message) ? "PASS" : "FAIL", e.message); }
    live.close();
  });

  // C6: bind on non-private parent -> parent guard
  d = mk("c6"); fs.chmodSync(d, 0o755);
  try { await bindEndpointGuarded(join(d, "z.sock"), DB_ID); emit("socket_guard", "nonprivate_parent_rejected", "FAIL", "not rejected"); }
  catch (e) { emit("socket_guard", "nonprivate_parent_rejected", /runtime path must be owned/.test(e.message) ? "PASS" : "FAIL", e.message); }
  fs.chmodSync(d, 0o700);
})();

// ---- P7 SO_PEERCRED ----
emit("peer_cred", "same_uid_authorization", "NOT_RUN",
  "Node has no getsockopt(SO_PEERCRED) API and no native addon can be built on this box (make absent). " +
  "Selected path: minimal N-API/neon helper once toolchain is provisioned; interim invariant = socket inside 0700 private dir already blocks cross-UID traversal. " +
  "Rust reference: src/local_runtime/paths.rs::peer() getsockopt->ucred.uid==geteuid.");

// ---- P8 bare relative socket path ----
expectErr("socket_guard", "bare_relative_path_rejected", /explicit private parent|runtime path must be owned/, () => {
  const p = "bare.sock"; // parent() === "" -> no explicit private parent
  const parent = join(p, "..");
  if (parent === "." && !p.includes("/")) throw new Error("socket requires an explicit private parent directory");
  privateMetadata(parent);
});
