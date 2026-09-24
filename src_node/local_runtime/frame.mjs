// Node port of src/local_runtime/frame.rs read_frame (:7-63) and the
// write side of the session loop (server.rs:144-147 writeln + flush).
//
// Semantics preserved verbatim — bound INCOMPLETE frames, not the
// lifetime of an idle session:
//   * byte cap counts every byte incl. the terminating \n;
//   * the wall-clock deadline starts at the FIRST data byte — retries
//     before any data never trip it;
//   * EOF with empty frame -> null (clean close); EOF mid-frame -> err;
//   * stop flag -> null (checked before and after each fill);
//   * retryable fill errors (timed_out/would_block/interrupted)
//     continue; anything else propagates.
//
// The Rust pull source is BufRead::fill_buf driven by a socket read
// timeout of min(io_timeout, 1s). Here the source is evented:
// SocketSource.fillBuf() resolves with pending bytes, or rejects with
// {kind:"timed_out"} once per tick so the loop re-checks flags —
// the direct equivalent of read(2) returning EAGAIN on the timeout.

const RETRY_KINDS = new Set(["timed_out", "would_block", "interrupted"]);

export async function readFrame(
  input, isStopping, limit, incompleteBudgetMs, now = Date.now) {
  const parts = [];
  let frameLen = 0;
  let started = null;
  for (;;) {
    if (isStopping()) return null;
    if (started !== null && now() - started >= incompleteBudgetMs)
      throw new Error("incomplete IPC frame deadline expired");
    let bytes;
    try {
      bytes = await input.fillBuf();
    } catch (e) {
      if (RETRY_KINDS.has(e.kind)) continue;
      throw e;
    }
    if (isStopping()) return null;
    if (started !== null && now() - started >= incompleteBudgetMs)
      throw new Error("incomplete IPC frame deadline expired");
    if (bytes.length === 0) {
      if (frameLen === 0) return null;
      throw new Error("EOF inside IPC frame");
    }
    const nl = bytes.indexOf(0x0a);
    const count = nl === -1 ? bytes.length : nl + 1;
    if (frameLen + count > limit)
      throw new Error("IPC frame exceeds limit");
    const complete = bytes[count - 1] === 0x0a;
    if (started === null) started = now();
    parts.push(bytes.subarray(0, count));
    frameLen += count;
    input.consume(count);
    if (complete) return Buffer.concat(parts);
  }
}

// fillBuf/consume adapter over a net.Socket. 'data' chunks accumulate;
// consume() drops processed bytes. EOF maps to an empty Buffer.
export class SocketSource {
  constructor(socket, tickMs) {
    this._pending = Buffer.alloc(0);
    this._eof = false;
    this._err = null;
    this._wake = null;
    this._timer = null;
    this._tick = tickMs;
    socket.on("data", (d) => {
      this._pending = this._pending.length
        ? Buffer.concat([this._pending, d]) : d;
      this._notify();
    });
    const eof = () => { this._eof = true; this._notify(); };
    socket.on("end", eof);
    socket.on("close", eof);
    socket.on("error", (e) => { this._err = e; this._notify(); });
  }
  _notify() {
    const w = this._wake;
    if (w) { this._wake = null; clearTimeout(this._timer); w(); }
  }
  fillBuf() {
    return new Promise((resolve, reject) => {
      const attempt = () => {
        this._wake = null;
        if (this._err) return reject(this._err);
        if (this._pending.length) return resolve(this._pending);
        if (this._eof) return resolve(Buffer.alloc(0));
        this._wake = attempt; // re-arm: resolved when data/eof/err lands
        this._timer = setTimeout(() => {
          this._wake = null;
          const e = new Error("read timed out");
          e.kind = "timed_out";
          reject(e);
        }, this._tick);
      };
      attempt();
    });
  }
  consume(n) {
    this._pending = this._pending.subarray(n);
  }
}

// writeln! + flush with the io_timeout write deadline; a stalled peer
// ends the session, matching Rust's set_write_timeout error path.
export function writeFrame(socket, line, timeoutMs) {
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => {
      const e = new Error("IPC write deadline expired");
      e.kind = "timed_out";
      reject(e);
    }, timeoutMs);
    socket.write(line + "\n", (err) => {
      clearTimeout(t);
      if (err) reject(err); else resolve();
    });
  });
}
