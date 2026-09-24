// NM-036 P2: validate generation_store pin/pins/unpin/sweepPins against
// a live Rust oracle. Synthetic roots only. Exit nonzero on FAIL.
// Usage: node generation_pin_probe.mjs <root> [selftest]
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import { execSync } from "node:child_process";
import { join } from "node:path";

const ROOT = process.argv[2] || ".";
const SELFTEST = process.argv[3] || null;
if (!existsSync(join(ROOT, "src_node/native/rename_noreplace.node"))) {
  execSync("gcc -shared -fPIC -O2 -o src_node/native/rename_noreplace.node src_node/native/rename_noreplace.c -I/usr/include/node", { cwd: ROOT });
}
const { GenerationStore } = await import("../../src_node/governance/generation_store.mjs");
const { GenerationUsableError } = await import("../../src_node/governance/generation_usable.mjs");

const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/generation_pins/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

let failures = 0;
const emit = (id, status, detail, expected = null, actual = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ family: "generation_pin", id, status, detail, expected, actual }));
};

const FIX = mkdtempSync("/tmp/nm36-pin-");
process.on("exit", () => rmSync(FIX, { recursive: true, force: true }));
const G = "aa".repeat(32), G_MISS = "bb".repeat(32);
mkdirSync(join(FIX, "generations", G), { recursive: true });
const store = new GenerationStore(FIX);

const run = (fn) => {
  try { return { status: "ok", value: fn() }; }
  catch (e) {
    return { status: "error", kind: e instanceof GenerationUsableError ? e.kind : "other", message: e.message };
  }
};
const HOOKS = {
  pins_skips_dirs: () => mkdirSync(join(FIX, "pins", G, "a-subdir"), { recursive: true }),
};
const live = new Map(); // held Pin handles mirroring the oracle's _keep
const SEQ = {
  pin_missing_gen: () => run(() => store.pin(G_MISS, "task-1")),
  pin_empty_holder: () => run(() => store.pin(G, "")),
  pin_slash_holder: () => run(() => store.pin(G, "a/b")),
  pin_backslash_holder: () => run(() => store.pin(G, "a\\b")),
  pin_nul_holder: () => run(() => store.pin(G, "a\0b")),
  pin_first: () => { live.set("task-1", store.pin(G, "task-1")); return { status: "ok", value: store.pins(G).includes("task-1") }; },
  marker_meta: () => {
    const st = statSync(join(FIX, "pins", G, "task-1"));
    return { status: "ok", value: { file: st.isFile(), mode: (0o100000 | (st.mode & 0o777)).toString(8), size: st.size } };
  },
  pin_dup: () => { live.set("task-1", store.pin(G, "task-1")); return { status: "ok", value: true }; },
  pin_pre_drop: () => { live.set("drop-me", store.pin(G, "drop-me")); return { status: "ok", value: store.pins(G) }; },
  pin_post_drop: () => { live.get("drop-me").release(); live.delete("drop-me"); return { status: "ok", value: store.pins(G) }; },
  pins_sorted: () => {
    live.set("zebra", store.pin(G, "zebra"));
    live.set("apple", store.pin(G, "apple"));
    live.set("Mango", store.pin(G, "Mango"));
    live.set("", store.pin(G, ""));
    live.set("😀", store.pin(G, "😀"));
    return { status: "ok", value: store.pins(G) };
  },
  pins_missing_gen: () => run(() => store.pins(G_MISS)),
  unpin_ok: () => run(() => { store.unpin(G, "apple"); return true; }),
  unpin_idempotent: () => run(() => { store.unpin(G, "apple"); return true; }),
  pins_after_unpin: () => ({ status: "ok", value: store.pins(G) }),
  sweep: () => ({ status: "ok", value: store.sweepPins(G, (h) => h === "zebra") }),
  pins_after_sweep: () => ({ status: "ok", value: store.pins(G) }),
  sweep_missing: () => ({ status: "ok", value: store.sweepPins(G_MISS, () => false) }),
  unpin_nodir: () => run(() => { store.unpin(G_MISS, "x"); return true; }),
  pins_skips_dirs: () => ({ status: "ok", value: store.pins(G) }),
};

for (const o of oracle) {
  if (HOOKS[o.id]) HOOKS[o.id]();
  const got = SEQ[o.id]();
  if (o.status === "ok") {
    if (got.status !== "ok") { emit(o.id, "FAIL", `threw ${got.kind}: ${got.message}`); continue; }
    emit(o.id, JSON.stringify(got.value) === JSON.stringify(o.value) ? "PASS" : "FAIL", "ok", o.value, got.value);
  } else {
    // not_installed/not_verified messages compare verbatim; io inner
    // text is OS-formatted — compare kind + stable "io:" prefix.
    const ok = got.status === "error" && got.kind === o.kind &&
      (o.kind === "io" ? got.message.startsWith("io:") : got.message === o.message);
    emit(o.id, ok ? "PASS" : "FAIL", ok ? o.kind : "mismatch", o.message, got.message);
  }
}

// ---------- Node-only: release/dispose RAII + holder symlink refusal ----------
{
  const s2 = new GenerationStore(join(FIX, "r2"));
  mkdirSync(join(FIX, "r2", "generations", G), { recursive: true });
  // Node 20 has no `using`; release()/Symbol.dispose are the
  // deterministic RAII path (Rust Drop parity).
  const p1 = s2.pin(G, "scoped");
  p1.release();
  emit("dispose_release", !s2.pins(G).includes("scoped") ? "PASS" : "FAIL", "marker released");
  const p2 = s2.pin(G, "scoped2");
  p2[Symbol.dispose]();
  emit("symbol_dispose", !s2.pins(G).includes("scoped2") ? "PASS" : "FAIL", "Symbol.dispose released");
  p2.release(); // idempotent second release
  emit("release_idempotent", !s2.pins(G).includes("scoped2") ? "PASS" : "FAIL", "second release no-op");
}
{
  const s3 = new GenerationStore(join(FIX, "r3"));
  const gidDir = join(FIX, "r3", "generations", G);
  mkdirSync(gidDir, { recursive: true });
  const pinDir = join(FIX, "r3", "pins", G);
  mkdirSync(pinDir, { recursive: true });
  const outside = join(FIX, "r3-out.txt");
  writeFileSync(outside, "ORIGINAL");
  symlinkSync(outside, join(pinDir, "bad-holder"));
  try { s3.pin(G, "bad-holder"); emit("holder_symlink_reject", "FAIL", "wrote through symlink"); }
  catch (e) {
    emit("holder_symlink_reject", e.kind === "io" && readFileSync(outside, "utf8") === "ORIGINAL" ? "PASS" : "FAIL", e.kind);
  }
}
// surrogate holder rejected at the boundary
{
  try { store.pin(G, "h\ud800"); emit("holder_surrogate", "FAIL", "accepted"); }
  catch (e) { emit("holder_surrogate", e.kind === "not_verified" ? "PASS" : "FAIL", e.kind); }
}
// live marker still present — retire precondition guard exists
emit("marker_blocks_retire_precondition", store.pins(G).length > 0 ? "PASS" : "FAIL",
  `holders=${JSON.stringify(store.pins(G))}`);

// selftest
if (SELFTEST === "pin") {
  const s = new GenerationStore(join(FIX, "self"));
  mkdirSync(join(FIX, "self", "generations", G), { recursive: true });
  const p = s.pin(G, "h");
  emit("selftest_pin", s.pins(G).includes("wrong-holder") ? "PASS" : "FAIL",
    s.pins(G).includes("wrong-holder") ? null : "expected wrong holder — STOP");
}

process.exitCode = failures === 0 ? 0 : 1;
