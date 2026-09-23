// NM-010: post-install smoke — verifies the built binding end-to-end on a
// synthetic tmpfs resource. Exit 0 = usable; nonzero = refuse to run.
import { mkdtempSync, writeFileSync, openSync, closeSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { getpeereuid, flockfd } from "./index.mjs";
import net from "node:net";

const dir = mkdtempSync(join(process.env.NATIVE_TMP || "/dev/shm", "nm010-"));
const checks = [];

// flock acquire/release on a real fd
const p = join(dir, "lease.db");
writeFileSync(p, "", { mode: 0o600 });
const fd = openSync(p, "r+");
checks.push(["flock_acquire", flockfd(fd, 2 | 4) === 0]);
checks.push(["flock_release", flockfd(fd, 8) === 0]);
closeSync(fd);

// SO_PEERCRED on a live accepted unix socket
const sockPath = join(dir, "s.sock");
await new Promise((resolve, reject) => {
  const srv = net.createServer((s) => {
    try {
      const cred = getpeereuid(s._handle.fd);
      checks.push(["peer_uid_is_euid", cred.uid === process.geteuid()]);
      s.destroy(); srv.close(resolve);
    } catch (e) { reject(e); }
  });
  srv.listen(sockPath, () => net.connect(sockPath).end());
});

const failed = checks.filter(([, ok]) => !ok);
console.log(JSON.stringify({
  status: failed.length ? "FAIL" : "PASS",
  checks: Object.fromEntries(checks),
  node: process.version,
}));
process.exit(failed.length ? 1 : 0);
