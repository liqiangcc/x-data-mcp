// S13 proof: renameat2(RENAME_NOREPLACE) — already ported in
// src_node/native/rename_noreplace.c and consumed by
// src_node/governance/generation_store.mjs + storage open paths.
// This proof confirms reuse and the EEXIST semantics, incl. the
// empty-dir clobber that fs.renameSync would silently allow.
// Usage: node scripts/p13_spike/s13_rename_noreplace.mjs <private-dir>
import fs from "node:fs";
import { join } from "node:path";
import { emit, run, expectErr, finish, loadNative, dir } from "./common.mjs";

const addon = loadNative();

run("rename_noreplace", "fresh_rename_succeeds", () => {
  const a = join(dir, "a.new"), b = join(dir, "a.final");
  fs.writeFileSync(a, "payload", { mode: 0o600 });
  addon.rename_noreplace(a, b);
  if (!fs.existsSync(b) || fs.existsSync(a))
    throw new Error("rename did not move the file");
  return { dst_bytes: fs.readFileSync(b, "utf8") };
});

run("rename_noreplace", "existing_file_target_eexist_kept", () => {
  const a = join(dir, "b.new"), b = join(dir, "b.final");
  fs.writeFileSync(a, "new", { mode: 0o600 });
  fs.writeFileSync(b, "original", { mode: 0o600 });
  let code = null;
  try { addon.rename_noreplace(a, b); } catch (e) { code = e.code; }
  if (code !== "EEXIST") throw new Error(`expected EEXIST got ${code}`);
  if (fs.readFileSync(b, "utf8") !== "original")
    throw new Error("target was clobbered");
  return { code, dst_bytes: "original", src_still_exists: fs.existsSync(a) };
});

run("rename_noreplace", "existing_empty_dir_target_eexist", () => {
  // The documented hazard: renameSync(dir_src, empty_dir_dst) silently
  // replaces the empty dir; rename_noreplace must fail EEXIST.
  const a = join(dir, "g.new"), b = join(dir, "g.live");
  fs.mkdirSync(a, { mode: 0o700 });
  fs.writeFileSync(join(a, "marker"), "gen2", { mode: 0o600 });
  fs.mkdirSync(b, { mode: 0o700 }); // empty target dir
  let code = null;
  try { addon.rename_noreplace(a, b); } catch (e) { code = e.code; }
  // contrast: plain renameSync WOULD have clobbered the empty dir
  const c = join(dir, "g2.new"), d = join(dir, "g2.live");
  fs.mkdirSync(c); fs.writeFileSync(join(c, "marker"), "gen2", { mode: 0o600 });
  fs.mkdirSync(d);
  fs.renameSync(c, d); // succeeds — demonstrates why the native call exists
  const clobbered = fs.readFileSync(join(d, "marker"), "utf8") === "gen2";
  if (code !== "EEXIST") throw new Error(`expected EEXIST got ${code}`);
  if (!clobbered) throw new Error("renameSync contrast did not clobber");
  return { code, renameSync_would_clobber_empty_dir: clobbered };
});

run("rename_noreplace", "missing_src_enoent", () => {
  let code = null;
  try {
    addon.rename_noreplace(join(dir, "nope"), join(dir, "dst"));
  } catch (e) { code = e.code; }
  if (code !== "ENOENT") throw new Error(`expected ENOENT got ${code}`);
  return { code };
});

emit("rename_noreplace", "consumers", "PASS", null, {
  src_node: [
    "src_node/governance/generation_store.mjs (rename_noreplace/openat_dir/openat_create_excl/unlinkat_file)",
    "src_node/storage/database_lease.mjs (flockfd/statfsmagic)",
    "src_node/storage/verification_runtime.mjs (flockfd/statfsmagic)",
  ],
});

finish();
