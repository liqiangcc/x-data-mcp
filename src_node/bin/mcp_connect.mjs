// Node port of src/bin/mcp_connect.rs — transport-only stdio proxy.
// EOF/disconnect never invokes task cancellation.
//
// Rust semantics (verbatim):
//   args != 1              -> "usage: mcp_connect <private-socket>"
//   connect(path)?         -> w3 local_runtime::connect port (private
//                             parent + socket + peer-UID + identity)
//   stdin -> socket thread -> io::copy; stdin EOF -> shutdown(Write)
//   main loop              -> socket read -> stdout write_all+flush
//   socket EOF             -> shutdown(Both); exit WITHOUT waiting for
//                             a still-blocked stdin copier; a finished
//                             copier's error is joined (exit 1)
//
// Node streams are evented rather than threaded; the copier runs on the
// event loop and the same happens-before ordering is preserved: a
// socket EOF exits without waiting for stdin.

import { connect } from "../local_runtime/paths.mjs";
import { die } from "./_shared.mjs";

async function run() {
  // cfg(not(linux)) arm — mcp_connect requires Linux owner-only IPC.
  if (process.platform !== "linux")
    throw new Error("mcp_connect requires Linux owner-only IPC");
  const args = process.argv.slice(2);
  if (args.length !== 1)
    throw new Error("usage: mcp_connect <private-socket>");
  const socket = await connect(args[0]);

  // stdin -> socket copier (the Rust input thread): stdin EOF or error
  // half-closes the socket write side.
  let stdinErr = null, stdinFinished = false;
  socket.on("drain", () => process.stdin.resume());
  process.stdin.on("data", (chunk) => {
    if (!socket.write(chunk)) process.stdin.pause();
  });
  process.stdin.on("end", () => {
    stdinFinished = true;
    socket.end(); // shutdown(Shutdown::Write)
  });
  process.stdin.on("error", (e) => {
    stdinErr = e;
    stdinFinished = true;
    socket.end();
  });
  // socket -> stdout: write_all + flush per read.
  socket.on("data", (d) => process.stdout.write(d));
  process.stdout.on("error", (e) => { socket.destroy(); die(e); });
  socket.on("error", (e) => { process.stdin.destroy(); die(e); });
  socket.on("end", () => {
    // response.shutdown(Both): a dead service must produce EOF even
    // when the client keeps stdin open — exit without joining stdin.
    socket.destroy();
    process.stdin.destroy();
    // input.join() only when the copier is already finished.
    if (stdinFinished && stdinErr) die(stdinErr);
    // exit 0 — the event loop drains pending stdout writes first.
  });
}

// Rust main(): Err -> eprintln!("{error}") + exit 1.
run().catch(die);
