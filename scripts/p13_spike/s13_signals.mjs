// S13 proof: SIGTERM/SIGINT handling — parity with sigaction->AtomicBool
// ->monitor-thread->serve() in src/bin/{data_service,acquire_service}.rs.
// Node mechanism: process.on("SIGTERM"|"SIGINT") sets a stop flag; a
// 10ms interval poll mirrors the Rust monitor thread; graceful exit via
// process.exitCode. Also documents the real semantic difference: the
// Node handler runs on the event loop, so a blocked main thread defers
// it (Rust's handler sets the atomic on the signal frame itself).
// Usage: node scripts/p13_spike/s13_signals.mjs <private-dir>
import { spawn } from "node:child_process";
import { emit, finish } from "./common.mjs";

const childSrc = ({ handlers, blockMs, exitCode }) => [
  "-e",
  `let stopping=null;` +
  (handlers
    ? `process.on("SIGTERM",()=>{stopping="SIGTERM"});` +
      `process.on("SIGINT",()=>{stopping="SIGINT"});`
    : "") +
  `console.log("READY");` +
  (blockMs
    ? `Atomics.wait(new Int32Array(new SharedArrayBuffer(4)),0,0,${blockMs});`
    : "") +
  `const iv=setInterval(()=>{if(!stopping)return;clearInterval(iv);` +
  `console.log("HANDLED:"+stopping+":"+Date.now());` +
  `process.exitCode=${exitCode};},10);`,
];

function runChild({ handlers = true, blockMs = 0, exitCode = 0, signal }) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath,
      childSrc({ handlers, blockMs, exitCode }), { encoding: "utf8" });
    let out = "", tKill = null, sent = false;
    child.stdout.on("data", (d) => {
      out += d;
      if (!sent && out.includes("READY")) {
        sent = true;
        setTimeout(() => { tKill = Date.now(); child.kill(signal); }, 50);
      }
    });
    child.on("exit", (code, sig) => resolve({ code, signal: sig, out, tKill }));
    child.on("error", reject);
    setTimeout(() => reject(new Error("child timeout")), 8000);
  });
}

const handledMs = (r) => {
  const m = r.out.match(/HANDLED:(SIG\w+):(\d+)/);
  return m ? { sig: m[1], delay: Number(m[2]) - r.tKill } : null;
};

{
  const r = await runChild({ signal: "SIGTERM" });
  const h = handledMs(r);
  emit("signals", "sigterm_handler_runs_exit0",
    h?.sig === "SIGTERM" && r.code === 0 && r.signal === null ? "PASS" : "FAIL",
    `code=${r.code} signal=${r.signal} handled_delay_ms=${h?.delay}`);
}
{
  const r = await runChild({ signal: "SIGINT" });
  const h = handledMs(r);
  emit("signals", "sigint_handler_runs_exit0",
    h?.sig === "SIGINT" && r.code === 0 && r.signal === null ? "PASS" : "FAIL",
    `code=${r.code} signal=${r.signal} handled_delay_ms=${h?.delay}`);
}
{
  // Exit code is the service's choice, not the kernel's — proves
  // graceful-shutdown paths can return any code (Rust: Ok->0, Err->1).
  const r = await runChild({ signal: "SIGTERM", exitCode: 7 });
  const h = handledMs(r);
  emit("signals", "exit_code_controllable",
    h?.sig === "SIGTERM" && r.code === 7 ? "PASS" : "FAIL",
    `code=${r.code} expected=7`);
}
{
  // Control: without handlers SIGTERM kills by default disposition.
  const r = await runChild({ handlers: false, signal: "SIGTERM" });
  emit("signals", "no_handler_default_kill",
    r.code === null && r.signal === "SIGTERM" ? "PASS" : "FAIL",
    `code=${r.code} signal=${r.signal}`,
    { note: "control case — handlers genuinely intercept" });
}
{
  // Semantic delta vs Rust: handler waits for a loop turn. SIGTERM sent
  // mid-block is queued, not lost; delay ~= remaining block time.
  const r = await runChild({ signal: "SIGTERM", blockMs: 400 });
  const h = handledMs(r);
  emit("signals", "handler_queued_during_main_block",
    h?.sig === "SIGTERM" && r.code === 0 && h.delay >= 200 ? "PASS" : "FAIL",
    `code=${r.code} handled_delay_ms=${h?.delay} (block=400ms, kill at +50ms)`,
    { implication: "keep blocking work off the responder thread so the stop flag propagates promptly" });
}

finish();
