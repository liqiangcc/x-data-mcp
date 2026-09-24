// Scripted unix-socket server for the mcp_connect differential probe.
// Both implementations connect to this server — it only needs to own
// the socket file (0600) inside a private dir (0700), which is what
// local_runtime::connect checks on the client side.
//
//   node mcp_connect_server.mjs <sockpath> <mode> [payloadHex]
// Modes:
//   echo  — echo bytes back; on client half-close, close the socket
//   spew  — write payload bytes then close (client stdin stays open)
//   close — close the accepted connection immediately
// Prints "READY" on stdout once listening.

import net from "node:net";
import fs from "node:fs";

const [sockPath, mode, payloadHex] = process.argv.slice(2);
const payload = payloadHex ? Buffer.from(payloadHex, "hex") : Buffer.alloc(0);

try { fs.unlinkSync(sockPath); } catch { /* fresh path */ }
process.on("exit", () => { try { fs.unlinkSync(sockPath); } catch {} });
const server = net.createServer((conn) => {
  if (mode === "close") {
    // Unix sockets have no RST frame — a close with unread inbound data
    // surfaces ECONNRESET on the peer. Pause so the client's byte stays
    // in the kernel buffer, then destroy.
    conn.pause();
    setTimeout(() => conn.destroy(), 300);
    return;
  }
  if (mode === "spew") {
    conn.end(payload); // write payload then FIN
    return;
  }
  // echo: mirror bytes; half-close -> close
  conn.on("data", (d) => conn.write(d));
  conn.on("end", () => conn.end());
});
server.listen(sockPath, () => {
  fs.chmodSync(sockPath, 0o600);
  console.log("READY");
});
