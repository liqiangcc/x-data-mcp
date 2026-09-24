// NM-068 probe — S14d AcquireApi MCP adapter: ENTRIES byte-parity vs
// the captured Rust golden, per-tool ok/error paths, capability
// blocking reasons, contract_version gate, mapped() codes. JSONL out.
// Usage: node acquire_api_probe.mjs <dir>
import fs from "node:fs";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { crc32 } from "node:zlib";
import {
  AcquireService, AcquireApi, Source,
} from "../../src_node/acquire/index.mjs";
import { serJson } from "../../src_node/acquire/wire.mjs";
import { decodeRecord } from "../../src_node/parsers/gbbq_decode.mjs";

const dir = process.argv[2];
const GOLDEN = new URL(
  "../../evidence/node-migration/p4/acquire_api/tools-golden.json",
  import.meta.url);
let failures = 0;
const emit = (id, status, detail, evidence = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ id, status, detail, evidence }));
};
const acheck = async (id, fn) => {
  try { emit(id, "PASS", null, await fn()); }
  catch (e) { emit(id, "FAIL", `${e.code ?? e.kind ?? ""} ${e.message}`); }
};

// ---------- serde helpers ----------

// deep BTreeMap key sort — serde_json::Value object semantics.
const sortDeep = (v) => {
  if (Array.isArray(v)) return v.map(sortDeep);
  if (v && typeof v === "object") {
    return Object.fromEntries(Object.keys(v).sort()
      .map((k) => [k, sortDeep(v[k])]));
  }
  return v;
};
// serde_json::to_string — compact + sorted (serJson emits insertion
// order, so deep-sort first).
const serdeCompact = (v) => serJson(sortDeep(v));

// ---------- fixtures (same recipe as acquire_govern_probe) ----------

function writeZip(path, members) {
  const chunks = [], cd = [];
  let off = 0;
  for (const [name, data0] of members) {
    const data = Buffer.isBuffer(data0) ? data0 : Buffer.from(data0);
    const nb = Buffer.from(name, "latin1");
    const crc = crc32(data);
    const lh = Buffer.alloc(30);
    lh.writeUInt32LE(0x04034b50, 0); lh.writeUInt16LE(20, 4);
    lh.writeUInt16LE(0, 6); lh.writeUInt16LE(0, 8);
    lh.writeUInt32LE(crc, 14);
    lh.writeUInt32LE(data.length, 18); lh.writeUInt32LE(data.length, 22);
    lh.writeUInt16LE(nb.length, 26); lh.writeUInt16LE(0, 28);
    chunks.push(lh, nb, data);
    const ce = Buffer.alloc(46);
    ce.writeUInt32LE(0x02014b50, 0);
    ce.writeUInt16LE(20, 4); ce.writeUInt16LE(20, 6);
    ce.writeUInt16LE(0, 8); ce.writeUInt16LE(0, 10);
    ce.writeUInt32LE(crc, 16);
    ce.writeUInt32LE(data.length, 20); ce.writeUInt32LE(data.length, 24);
    ce.writeUInt16LE(nb.length, 28); ce.writeUInt32LE(off, 42);
    cd.push(Buffer.concat([ce, nb]));
    off += 30 + nb.length + data.length;
  }
  const cdBuf = Buffer.concat(cd);
  const eocd = Buffer.alloc(22);
  eocd.writeUInt32LE(0x06054b50, 0);
  eocd.writeUInt16LE(members.length, 8); eocd.writeUInt16LE(members.length, 10);
  eocd.writeUInt32LE(cdBuf.length, 12); eocd.writeUInt32LE(off, 16);
  fs.writeFileSync(path, Buffer.concat([...chunks, cdBuf, eocd]));
}
const dayRecord = (d) => {
  const r = Buffer.alloc(32);
  r.writeUInt32LE(d, 0); r.writeUInt32LE(100, 24); return r;
};
const dayMember = (ds) => Buffer.concat(ds.map(dayRecord));
function gbbqRecord() {
  for (let i = 0; ; i++) {
    const rec = createHash("sha256")
      .update(`gbbq-fixture-${i}`).digest().subarray(0, 29);
    try {
      if (typeof decodeRecord(0, new Uint8Array(rec)).code === "string")
        return new Uint8Array(rec);
    } catch { /* next */ }
    if (i > 200000) throw new Error("no decryptable record");
  }
}
const gbbqMember = (recs) => {
  const b = Buffer.alloc(4); b.writeUInt32LE(recs.length, 0);
  return Buffer.concat([b, ...recs.map((r) => Buffer.from(r))]);
};
const todayU32 = () => {
  const d = new Date(Date.now() + 8 * 3600_000);
  return d.getUTCFullYear() * 10000 + (d.getUTCMonth() + 1) * 100 + d.getUTCDate();
};
function writeMirror(dir2, lastBar) {
  fs.mkdirSync(dir2, { recursive: true });
  const tdxhy = Buffer.from(
    "1|600006|T090302|||X6102\r\n0|000001|T1001|||X500102\r\n", "latin1");
  const ex = Buffer.from("600006|n1|kw\r\n000001|n2|kw\r\n", "latin1");
  writeZip(join(dir2, "base.zip"),
    [["tdxhy.cfg", tdxhy], ["infoharbor_ex.code", ex]]);
  writeZip(join(dir2, "gbbq.zip"), [["gbbq", gbbqMember([gbbqRecord()])]]);
  writeZip(join(dir2, "hsjday.zip"), [
    ["sh\\lday\\sh600006.day", dayMember([lastBar])],
    ["sz\\lday\\sz000001.day", dayMember([lastBar])],
  ]);
}

// ---------- drivers ----------

const v1 = (extra = {}) => ({ contract_version: "v1", ...extra });
const apiErr = (fn) => {
  try { fn(); } catch (e) { return e; }
  throw new Error("call did not throw");
};

const mirror = join(dir, "mirror"); writeMirror(mirror, todayU32());
const svc = AcquireService.open(join(dir, "root"), Source.mirror(mirror));
const api = new AcquireApi(svc);

// ---------- tools/list byte parity ----------

await acheck("tools_list_byte_parity", async () => {
  const golden = fs.readFileSync(GOLDEN, "utf8").trim();
  const mine = serdeCompact(api.tools());
  if (mine !== golden) {
    const i = [...mine].findIndex((c, j) => c !== golden[j]);
    throw new Error(`byte diff at ${i}: …${mine.slice(i, i + 60)}`);
  }
  return { bytes: golden.length };
});

await acheck("tools_no_workflow_leakage", async () => {
  const listed = serdeCompact(api.tools());
  for (const banned of ["next_actions", "next_tool"]) {
    if (listed.includes(banned)) throw new Error(`leaked ${banned}`);
  }
  return { names: api.tools().length };
});

// ---------- contract_version gate + decode ----------

await acheck("contract_gate_variants", async () => {
  const a = apiErr(() => api.call("check_official_updates",
    { contract_version: "v2" }));
  if (a.code !== "unsupported_contract") throw new Error(a.code);
  const b = apiErr(() => api.call("check_official_updates",
    { contract_version: null }));
  if (b.code !== "unsupported_contract") throw new Error(b.code);
  // absent on a tool that requires it -> decode invalid_arguments
  const c = apiErr(() => api.call("check_official_updates", {}));
  if (c.code !== "invalid_arguments") throw new Error(c.code);
  // extra field -> deny_unknown_fields -> invalid_arguments
  const d = apiErr(() => api.call("check_official_updates",
    v1({ extra: 1 })));
  if (d.code !== "invalid_arguments") throw new Error(d.code);
  // run_worker_once has NO fields: even contract_version is unknown
  const e = apiErr(() => api.call("run_worker_once", v1()));
  if (e.code !== "invalid_arguments") throw new Error(e.code);
  return { gate: "unsupported_contract", decode: "invalid_arguments" };
});

await acheck("unknown_tool_code", async () => {
  const e = apiErr(() => api.call("bogus_tool", {}));
  if (e.code !== "unknown_tool" ||
      e.message !== "unsupported tool: bogus_tool") throw new Error(e.code);
  return { code: e.code };
});

// ---------- per-tool ok paths over the fixture service ----------

let gid;
await acheck("check_official_updates_ok", async () => {
  const r = api.call("check_official_updates", v1());
  if (r.files.length !== 3) throw new Error("files");
  return { files: r.files.map((f) => f.role) };
});

await acheck("acquire_and_read_tools", async () => {
  const acc = api.call("acquire_generation", v1({ request_id: "api-1" }));
  if (acc.reused !== false || !acc.task_id) throw new Error("accept");
  const replay = api.call("acquire_generation", v1({ request_id: "api-1" }));
  if (replay.reused !== true || replay.task_id !== acc.task_id)
    throw new Error("replay");
  const t = api.call("get_acquisition_task", v1({ task_id: acc.task_id }));
  if (t.task_id !== acc.task_id) throw new Error("task view");
  const r = api.call("get_acquisition_request", v1({ request_id: "api-1" }));
  if (r.bound !== true || r.task?.task_id !== acc.task_id)
    throw new Error("request view");
  // worker turns drive the install
  for (let i = 0; i < 16; i++) {
    const turn = api.call("run_worker_once", {});
    if (turn.idle) break;
    if (turn.task?.task_id !== acc.task_id) throw new Error("turn task");
  }
  const done = api.call("get_acquisition_task", v1({ task_id: acc.task_id }));
  if (done.state !== "succeeded") throw new Error(done.state);
  gid = done.generation_id;
  return { gid: gid.slice(0, 16) };
});

await acheck("worker_once_idle_turn", async () => {
  const turn = api.call("run_worker_once", {});
  if (turn.idle !== true || turn.task !== null)
    throw new Error(JSON.stringify(turn));
  return { idle: true };
});

await acheck("verify_and_activate_tools", async () => {
  const v = api.call("verify_generation",
    v1({ request_id: "api-v1", generation_id: gid }));
  if (v.state !== "consistency_verified" ||
      v.consistency_status !== "VERIFIED_COMPATIBLE")
    throw new Error(JSON.stringify(v));
  const a = api.call("activate_generation",
    v1({ request_id: "api-a1", generation_id: gid }));
  if (a.state !== "active") throw new Error(JSON.stringify(a));
  const st = api.call("sync_status", v1());
  if (st.active?.generation_id !== gid) throw new Error("pointer");
  return { verified: v.consistency_status, active: a.state };
});

// ---------- mapped() error codes ----------

await acheck("mapped_request_conflict", async () => {
  // same request_id, different kind+payload -> Storage(RequestConflict)
  const e = apiErr(() => api.call("verify_generation",
    v1({ request_id: "api-1", generation_id: gid })));
  if (e.code !== "request_conflict") throw new Error(e.code);
  return { code: e.code };
});

await acheck("mapped_refused_code", async () => {
  const e = apiErr(() => api.call("get_acquisition_task",
    v1({ task_id: "acq-missing" })));
  if (e.code !== "task_not_found") throw new Error(e.code);
  return { code: e.code };
});

// ---------- capabilities ----------

await acheck("capabilities_blocked_fresh", async () => {
  const svc2 = AcquireService.open(join(dir, "root2"), Source.mirror(mirror));
  const api2 = new AcquireApi(svc2);
  const c = api2.call("get_acquisition_capabilities", v1());
  if (c.runtime_readiness !== "blocked") throw new Error("readiness");
  if (!c.blocking_reasons.some((b) => b.code === "no_active_generation"))
    throw new Error("no blocking reason");
  if (c.implementation.capability_revision !== "x-data-acquire-v1")
    throw new Error("revision");
  if (c.implementation.server !== "x-data-acquire")
    throw new Error("server");
  if (!/^\d+\.\d+\.\d+/.test(c.implementation.version))
    throw new Error(`version ${c.implementation.version}`);
  return { readiness: c.runtime_readiness };
});

await acheck("capabilities_ready_after_activate", async () => {
  const c = api.call("get_acquisition_capabilities", v1());
  if (c.runtime_readiness !== "ready") throw new Error(JSON.stringify(c.blocking_reasons));
  if (c.active_generation_ref !== gid) throw new Error("ref");
  if (c.tools.length !== 10) throw new Error("tools len");
  return { readiness: "ready" };
});

await acheck("capabilities_schema_digest", async () => {
  // recompute from the golden file independently: serde to_vec of
  // [{name,input}] (Value BTreeMap order — input < name).
  const golden = JSON.parse(fs.readFileSync(GOLDEN, "utf8"));
  const expect = createHash("sha256").update(Buffer.from(
    serdeCompact(golden.map((t) =>
      ({ input: t.inputSchema, name: t.name }))), "utf8")).digest("hex");
  const c = api.call("get_acquisition_capabilities", v1());
  if (c.schema_digest !== expect)
    throw new Error(`${c.schema_digest} != ${expect}`);
  return { digest: expect.slice(0, 16) };
});

await acheck("capabilities_unresolved_generation", async () => {
  // a not_verified generation reports unresolved_generation
  const stale = join(dir, "m-stale"); writeMirror(stale, 20200103);
  const svc3 = AcquireService.open(join(dir, "root3"), Source.mirror(stale));
  const api3 = new AcquireApi(svc3);
  const acc = api3.call("acquire_generation", v1({ request_id: "s1" }));
  for (let i = 0; i < 16; i++) {
    if (api3.call("run_worker_once", {}).idle) break;
  }
  const t = api3.call("get_acquisition_task", v1({ task_id: acc.task_id }));
  const v = api3.call("verify_generation",
    v1({ request_id: "sv1", generation_id: t.generation_id }));
  if (v.state !== "not_verified") throw new Error(v.state);
  const c = api3.call("get_acquisition_capabilities", v1());
  if (c.runtime_readiness !== "blocked") throw new Error("readiness");
  const u = c.blocking_reasons.find((b) => b.code === "unresolved_generation");
  if (!u || u.generation_id !== t.generation_id) throw new Error("reason");
  if (!/not_verified/.test(u.detail)) throw new Error(u.detail);
  return { blocked: true, reason: u.detail.slice(0, 40) };
});

emit("summary", failures === 0 ? "PASS" : "FAIL", `${failures} failures`);
process.exit(failures ? 1 : 0);
