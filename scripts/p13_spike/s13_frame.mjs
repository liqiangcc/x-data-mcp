// S13 proof: read_frame ported as a pure function — verbatim parity with
// src/local_runtime/frame.rs (:7-63). Bounds INCOMPLETE frames by byte
// cap + wall-clock deadline from first data byte; an idle session that
// never sends data never trips the deadline.
// Usage: node scripts/p13_spike/s13_frame.mjs <private-dir>
import { emit, finish } from "./common.mjs";

// ---- port: input is a BufRead-equivalent {fillBuf(), consume(n)} ----
// fillBuf() returns Buffer of pending bytes (empty = EOF), or throws
// {kind:"timed_out"|"would_block"|"interrupted"} (retry) or any other
// error (fatal). isStopping() mirrors the AtomicBool; now() is ms.
export function readFrame(input, isStopping, limit, incompleteBudgetMs, now) {
  const parts = [];
  let frameLen = 0;
  let started = null;
  for (;;) {
    if (isStopping()) return null;
    if (started !== null && now() - started >= incompleteBudgetMs)
      throw new Error("incomplete IPC frame deadline expired");
    let bytes;
    try {
      bytes = input.fillBuf();
    } catch (e) {
      if (e.kind === "timed_out" || e.kind === "would_block" ||
          e.kind === "interrupted") {
        continue;
      }
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

// ---- scripted BufRead fake: steps per fillBuf once pending drains ----
//   {d:"..."}  bytes arrive          {r:"timed_out"} retryable error
//   {eof:1}    end of stream         {io:"kind"}     fatal error
//   {do:fn}    side effect, then continue to next step in same call
// Each fillBuf advances the fake clock by tickMs (models a read wait).
function fakeBuf(script, clock, tickMs) {
  let pending = Buffer.alloc(0);
  let i = 0;
  return {
    fillBuf() {
      clock.t += tickMs;
      if (pending.length) return pending;
      for (;;) {
        const step = script[i++];
        if (!step) return Buffer.alloc(0); // script exhausted = EOF
        if (step.d !== undefined) {
          pending = Buffer.concat([pending, Buffer.from(step.d, "utf8")]);
          return pending;
        }
        if (step.r) { const e = new Error(step.r); e.kind = step.r; throw e; }
        if (step.io) { const e = new Error(step.io); e.kind = step.io; throw e; }
        if (step.do) { step.do(); continue; }
        if (step.eof) return Buffer.alloc(0);
      }
    },
    consume(n) { pending = pending.subarray(n); },
    get pending() { return pending; },
  };
}

const results = [];
const check = (id, fn) => {
  try { const v = fn(); emit("frame", id, "PASS", null, v); }
  catch (e) { emit("frame", id, "FAIL", `${e.code || ""} ${e.message}`); }
};
const expectMsg = (id, msg, fn) => check(id, () => {
  try { fn(); } catch (e) {
    if (e.message === msg) return { err: msg };
    throw new Error(`expected "${msg}" got "${e.message}"`);
  }
  throw new Error(`expected "${msg}" but completed`);
});

const LIMIT = 8, BUDGET = 100;

check("complete_single_chunk", () => {
  const c = { t: 0 };
  const f = fakeBuf([{ d: '{"a":1}\n' }, { eof: 1 }], c, 10);
  const out = readFrame(f, () => false, LIMIT + 10, BUDGET, () => c.t);
  if (out.toString() !== '{"a":1}\n') throw new Error("bad frame");
  return { frame: out.toString().trim() };
});

check("split_across_chunks", () => {
  const c = { t: 0 };
  const f = fakeBuf([{ d: '{"a' }, { d: '":1}\n' }, { eof: 1 }], c, 10);
  const out = readFrame(f, () => false, LIMIT + 10, BUDGET, () => c.t);
  return { frame: out.toString().trim() };
});

check("remainder_stays_buffered_for_next_frame", () => {
  const c = { t: 0 };
  const f = fakeBuf([{ d: "ab\ncd" }, { d: "\n" }, { eof: 1 }], c, 10);
  const f1 = readFrame(f, () => false, LIMIT, BUDGET, () => c.t);
  if (f1.toString() !== "ab\n") throw new Error(`frame1 ${f1}`);
  if (f.pending.toString() !== "cd")
    throw new Error(`pending "${f.pending}"`);
  const f2 = readFrame(f, () => false, LIMIT, BUDGET, () => c.t);
  return { frame1: "ab\\n", frame2: f2.toString().replace("\n", "\\n") };
});

expectMsg("over_limit_rejected", "IPC frame exceeds limit", () => {
  const c = { t: 0 };
  const f = fakeBuf([{ d: "x".repeat(LIMIT + 1) }], c, 10);
  readFrame(f, () => false, LIMIT, BUDGET, () => c.t);
});

check("exactly_at_limit_accepted", () => {
  const c = { t: 0 };
  const f = fakeBuf([{ d: "1234567\n" }], c, 10); // 8 bytes incl \n
  const out = readFrame(f, () => false, LIMIT, BUDGET, () => c.t);
  return { len: out.length, limit: LIMIT };
});

expectMsg("newline_beyond_limit_rejected", "IPC frame exceeds limit", () => {
  const c = { t: 0 };
  const f = fakeBuf([{ d: "1234\n" }], c, 10); // 5 bytes > limit 4
  readFrame(f, () => false, 4, BUDGET, () => c.t);
});

check("eof_empty_returns_null", () => {
  const c = { t: 0 };
  const f = fakeBuf([{ eof: 1 }], c, 10);
  const out = readFrame(f, () => false, LIMIT, BUDGET, () => c.t);
  if (out !== null) throw new Error("expected null");
  return { out };
});

expectMsg("eof_inside_frame_rejected", "EOF inside IPC frame", () => {
  const c = { t: 0 };
  const f = fakeBuf([{ d: "ab" }, { eof: 1 }], c, 10);
  readFrame(f, () => false, LIMIT, BUDGET, () => c.t);
});

check("idle_retries_never_start_deadline", () => {
  // 'Bound incomplete frames, not the lifetime of an idle MCP session':
  // retries BEFORE the first data byte must not trip the budget even
  // though virtual elapsed time exceeds it.
  const c = { t: 0 };
  const f = fakeBuf(
    [{ r: "timed_out" }, { r: "would_block" }, { r: "interrupted" },
     { r: "timed_out" }, { r: "timed_out" }, // 5*60ms idle — over budget
     { d: "ok\n" }, { eof: 1 }], c, 60);
  const out = readFrame(f, () => false, LIMIT, BUDGET, () => c.t);
  if (out.toString() !== "ok\n") throw new Error("bad frame");
  return { idle_ms_before_data: 300, budget_ms: BUDGET, frame: "ok\\n" };
});

expectMsg("incomplete_deadline_enforced",
  "incomplete IPC frame deadline expired", () => {
    // data at t=60 -> started=60; retries advance t; deadline trips at
    // top-of-loop when t-started >= 100.
    const c = { t: 0 };
    const f = fakeBuf(
      [{ d: "ab" }, { r: "timed_out" }, { r: "timed_out" },
       { r: "timed_out" }, { r: "timed_out" }], c, 60);
    readFrame(f, () => false, LIMIT, BUDGET, () => c.t);
  });

check("stop_flag_returns_null_before_and_mid_frame", () => {
  const c = { t: 0 };
  let stop = true;
  const f1 = fakeBuf([{ d: "ab\n" }], c, 10);
  if (readFrame(f1, () => stop, LIMIT, BUDGET, () => c.t) !== null)
    throw new Error("pre-set stop not honored");
  stop = false;
  const f2 = fakeBuf(
    [{ d: "ab" }, { do: () => { stop = true; } }, { d: "cd\n" }],
    c, 10);
  const out = readFrame(f2, () => stop, LIMIT, BUDGET, () => c.t);
  if (out !== null) throw new Error("mid-frame stop not honored");
  return { pre: null, mid: null };
});

check("fatal_io_error_propagates", () => {
  const c = { t: 0 };
  const f = fakeBuf([{ d: "ab" }, { io: "permission_denied" }], c, 10);
  try {
    readFrame(f, () => false, LIMIT, BUDGET, () => c.t);
    throw new Error("not thrown");
  } catch (e) {
    if (e.kind === "permission_denied") return { kind: e.kind };
    throw e;
  }
});

finish();
