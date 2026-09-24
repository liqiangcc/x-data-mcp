// NM-067 probe — S14c governance arm: verify/activate/retire over a
// Mirror fixture with parser-valid zips (mirrors tests/acquire_service.rs
// fixture recipe). JSONL results. Usage: node acquire_govern_probe.mjs <dir>
import fs from "node:fs";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { crc32 } from "node:zlib";
import { AcquireService, Source } from "../../src_node/acquire/service.mjs";
import { decodeRecord } from "../../src_node/parsers/gbbq_decode.mjs";

const dir = process.argv[2];
let failures = 0;
const emit = (id, status, detail, evidence = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ id, status, detail, evidence }));
};
const acheck = async (id, fn) => {
  try { emit(id, "PASS", null, await fn()); }
  catch (e) { emit(id, "FAIL", `${e.kind ?? e.code ?? e.name ?? ""} ${e.message}`); }
};

// ---------- fixture builders (tests/acquire_service.rs:34-85) ----------

// Minimal store-only zip writer (method 0) — members as [name, bytes].
function writeZip(path, members) {
  const chunks = [], cd = [];
  let off = 0;
  for (const [name, data0] of members) {
    const data = Buffer.isBuffer(data0) ? data0 : Buffer.from(data0);
    const nb = Buffer.from(name, "latin1");
    const crc = crc32(data);
    const lh = Buffer.alloc(30);
    lh.writeUInt32LE(0x04034b50, 0); lh.writeUInt16LE(20, 4);
    lh.writeUInt16LE(0, 6); lh.writeUInt16LE(0, 8); // flags, method 0
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

const dayRecord = (dateU32) => {
  const r = Buffer.alloc(32);
  r.writeUInt32LE(dateU32, 0); r.writeUInt32LE(100, 24);
  return r;
};
const dayMember = (dates) => Buffer.concat(dates.map(dayRecord));

// A decryptable gbbq record: deterministic counter-derived ciphertext,
// first candidate whose decrypted code bytes are valid UTF-8 wins
// (decodeRecord via the ported pinned key table — no table duplication).
function gbbqRecord() {
  for (let i = 0; ; i++) {
    const rec = createHash("sha256")
      .update(`gbbq-fixture-${i}`).digest().subarray(0, 29);
    try {
      const ev = decodeRecord(0, new Uint8Array(rec));
      if (ev && typeof ev.code === "string") return new Uint8Array(rec);
    } catch { /* try next */ }
    if (i > 200000) throw new Error("no decryptable fixture record found");
  }
}
const gbbqMember = (records) => {
  const b = Buffer.alloc(4);
  b.writeUInt32LE(records.length, 0);
  return Buffer.concat([b, ...records.map((r) => Buffer.from(r))]);
};

const todayU32 = () => {
  const d = new Date(Date.now() + 8 * 3600_000); // shanghai_date
  return d.getUTCFullYear() * 10000 +
    (d.getUTCMonth() + 1) * 100 + d.getUTCDate();
};

function writeMirror(dir2, lastBar) {
  fs.mkdirSync(dir2, { recursive: true });
  const tdxhy = Buffer.from(
    "1|600006|T090302|||X6102\r\n0|000001|T1001|||X500102\r\n", "latin1");
  const ex = Buffer.from(
    "600006|name-one|kw\r\n000001|name-two|kw\r\n", "latin1");
  writeZip(join(dir2, "base.zip"),
    [["tdxhy.cfg", tdxhy], ["infoharbor_ex.code", ex]]);
  writeZip(join(dir2, "gbbq.zip"),
    [["gbbq", gbbqMember([gbbqRecord()])]]);
  const bars = dayMember([lastBar]);
  writeZip(join(dir2, "hsjday.zip"), [
    ["sh\\lday\\sh600006.day", bars],
    ["sz\\lday\\sz000001.day", dayMember([lastBar])],
  ]);
}
function remirror(dir2, code, lastBar) {
  const tdxhy = Buffer.from(`1|${code}|T090302|||X6102\r\n`, "latin1");
  const ex = Buffer.from(`${code}|name|kw\r\n`, "latin1");
  writeZip(join(dir2, "base.zip"),
    [["tdxhy.cfg", tdxhy], ["infoharbor_ex.code", ex]]);
  writeZip(join(dir2, "hsjday.zip"),
    [[`sh\\lday\\sh${code}.day`, dayMember([lastBar])]]);
}

// ---------- drivers ----------
const openSvc = (root, mirror) =>
  AcquireService.open(root, Source.mirror(mirror));
const drive = (svc) => {
  for (let i = 0; i < 16; i++) if (svc.workerOnce() === null) break;
};
const install = (svc, req) => {
  const a = svc.acquireGeneration(req);
  drive(svc);
  return a;
};
const genOf = (svc, taskId) => svc.task(taskId).generation_id;
const verify = (svc, gid) =>
  svc.verifyGeneration(`verify-${gid.slice(0, 16)}`, gid);
const activate = (svc, gid, expected) =>
  svc.activateGeneration(`activate-${gid.slice(0, 16)}`, gid, expected);

// ================= scenario 1: lifecycle =================
const mir1 = join(dir, "m1"); writeMirror(mir1, todayU32());
const root1 = join(dir, "r1");
const svc1 = openSvc(root1, mir1);
let gid1, manifestSha1;

await acheck("lifecycle_check_updates", async () => {
  const first = svc1.checkUpdates();
  if (first.files.length !== 3) throw new Error("files");
  const second = svc1.checkUpdates();
  if (!second.files.every((f) => f.change === "unchanged"))
    throw new Error("etag compare not unchanged");
  return { files: 3 };
});

await acheck("lifecycle_install_verify", async () => {
  const a = install(svc1, "req-lifecycle");
  const t = svc1.task(a.task_id);
  if (t.state !== "succeeded") throw new Error(`state ${t.state}`);
  gid1 = t.generation_id;
  if (!/^[0-9a-f]{64}$/.test(gid1)) throw new Error(`gid ${gid1}`);
  const reply = verify(svc1, gid1);
  if (reply.state !== "consistency_verified")
    throw new Error(`verify ${JSON.stringify(reply)}`);
  if (reply.consistency_status !== "VERIFIED_COMPATIBLE")
    throw new Error(`status ${reply.consistency_status}`);
  if (!reply.decision_sha256 || reply.reused !== false)
    throw new Error("missing decision sha");
  // decision + evidence artifacts exist on disk
  const gdir = join(root1, "generations", gid1);
  for (const n of ["consistency-decision.json",
                   "compatibility-evidence.json", "snapshot-manifest.json"]) {
    if (!fs.statSync(join(gdir, n)).isFile())
      throw new Error(`missing ${n}`);
  }
  manifestSha1 = svc1.syncStatus().generations
    .find((g) => g.generation_id === gid1).manifest_sha256;
  return { gid: gid1.slice(0, 16), status: reply.consistency_status };
});

await acheck("lifecycle_verify_replay", async () => {
  const first = verify(svc1, gid1);
  const second = verify(svc1, gid1);
  if (second.reused !== true ||
      second.decision_sha256 !== first.decision_sha256)
    throw new Error("replay diverged");
  return { reused: true, sha: second.decision_sha256.slice(0, 12) };
});

await acheck("lifecycle_activate", async () => {
  const r = activate(svc1, gid1, null);
  if (r.state !== "active") throw new Error(JSON.stringify(r));
  const st = svc1.syncStatus();
  if (st.active?.generation_id !== gid1)
    throw new Error("pointer not set");
  const row = st.generations.find((g) => g.generation_id === gid1);
  if (row?.state !== "active") throw new Error(`row ${row?.state}`);
  if (!/\+00:00$/.test(r.activated_at))
    throw new Error(`activated_at ${r.activated_at} not to_rfc3339`);
  return { active: gid1.slice(0, 16), activated_at: r.activated_at };
});

// ============ scenario 2: corrupt bytes fail closed ============
await acheck("corrupt_artifact_refuses_activate", async () => {
  const gdir = join(root1, "generations", gid1);
  const f = join(gdir, "gbbq.zip");
  const b = fs.readFileSync(f);
  b[b.length - 1] ^= 1;
  fs.writeFileSync(f, b);
  const r = svc1.activateGeneration("act-corrupt", gid1, null);
  if (r.state !== "refused" || r.reason_code !== "generation_bytes_unverified")
    throw new Error(JSON.stringify(r));
  // restore for later scenarios
  b[b.length - 1] ^= 1;
  fs.writeFileSync(f, b);
  return { refused: r.reason_code };
});

// ============ scenario 3: stale mix stays not_verified ============
const mir3 = join(dir, "m3"); writeMirror(mir3, 20200103);
const svc3 = openSvc(join(dir, "r3"), mir3);

await acheck("stale_mix_not_verified", async () => {
  const a = install(svc3, "req-stale");
  const gid = genOf(svc3, a.task_id);
  const reply = verify(svc3, gid);
  if (reply.state !== "not_verified")
    throw new Error(`verify ${reply.state}`);
  const activated = svc3.activateGeneration("act-stale", gid, null);
  if (activated.state !== "refused" ||
      activated.reason_code !== "generation_not_consistency_verified")
    throw new Error(JSON.stringify(activated));
  return { state: reply.state, activate: activated.reason_code };
});

// ============ scenario 4: malformed member -> not_verified ============
const mir4 = join(dir, "m4"); writeMirror(mir4, todayU32());
const svc4 = openSvc(join(dir, "r4"), mir4);

await acheck("malformed_day_not_verified", async () => {
  const bad = dayMember([todayU32()]).subarray(0, 10);
  writeZip(join(mir4, "hsjday.zip"),
    [["sh\\lday\\sh600006.day", bad]]);
  const a = install(svc4, "req-malformed");
  const gid = genOf(svc4, a.task_id);
  const reply = verify(svc4, gid);
  if (reply.state !== "not_verified") throw new Error(reply.state);
  const act = svc4.activateGeneration("act-mal", gid, null);
  if (act.state !== "refused") throw new Error(act.state);
  return { state: reply.state };
});

// ============ scenario 5: activation conflict + replay ============
const mir5 = join(dir, "m5"); writeMirror(mir5, todayU32());
const svc5 = openSvc(join(dir, "r5"), mir5);
let genA5, genB5;

await acheck("activation_conflict_and_replay", async () => {
  const a = install(svc5, "req-a");
  genA5 = genOf(svc5, a.task_id);
  verify(svc5, genA5);
  if (activate(svc5, genA5, null).state !== "active")
    throw new Error("first activate");
  remirror(mir5, 600007, todayU32());
  const b = install(svc5, "req-b");
  genB5 = genOf(svc5, b.task_id);
  if (genA5 === genB5) throw new Error("same gid");
  verify(svc5, genB5);
  const refused = svc5.activateGeneration("act-b-stale", genB5, null);
  if (refused.state !== "refused" ||
      refused.reason_code !== "activation_conflict")
    throw new Error(JSON.stringify(refused));
  if (!/active generation is Some/.test(refused.detail ?? ""))
    throw new Error(`detail ${refused.detail}`);
  const replay = svc5.activateGeneration("act-b-stale", genB5, null);
  if (replay.reused !== true || replay.state !== "refused")
    throw new Error("replay");
  // same request id + different payload -> conflict error
  try {
    svc5.activateGeneration("act-b-stale", genB5, genA5);
    throw new Error("not refused");
  } catch (e) {
    if (e.kind !== "request_conflict" && !/already bound/.test(e.message))
      throw e;
  }
  const ok = svc5.activateGeneration("act-b-final", genB5, genA5);
  if (ok.state !== "active" || ok.previous_generation_id !== genA5)
    throw new Error(JSON.stringify(ok));
  const st = svc5.syncStatus();
  if (st.active?.generation_id !== genB5) throw new Error("pointer");
  const rowA = st.generations.find((g) => g.generation_id === genA5);
  if (rowA?.state !== "superseded") throw new Error(`genA ${rowA?.state}`);
  return { conflict_replay: true, activated: genB5.slice(0, 16) };
});

// ============ scenario 6: retire guards ============
const mir6 = join(dir, "m6"); writeMirror(mir6, todayU32());
const root6 = join(dir, "r6");
const svc6 = openSvc(root6, mir6);
let genA6, shaA6, genB6;

await acheck("retire_guard_order", async () => {
  const a = install(svc6, "req-pin-a");
  genA6 = genOf(svc6, a.task_id);
  verify(svc6, genA6);
  shaA6 = svc6.syncStatus().generations
    .find((g) => g.generation_id === genA6).manifest_sha256;
  // wrong token refuses before the pin check (verbatim order)
  svc6.generations().pin(genA6, "task:held");
  const wrong = svc6.retireGeneration("req-retire-1", genA6,
    createHash("sha256").update("wrong").digest("hex"));
  if (wrong.state !== "refused" ||
      wrong.reason_code !== "confirm_token_mismatch")
    throw new Error(JSON.stringify(wrong));
  const pinned = svc6.retireGeneration("req-retire-2", genA6, shaA6);
  if (pinned.state !== "refused" ||
      pinned.reason_code !== "generation_referenced" ||
      !/live pins: task:held/.test(pinned.detail))
    throw new Error(JSON.stringify(pinned));
  svc6.generations().unpin(genA6, "task:held");
  return { order: ["token", "pins"] };
});

await acheck("retire_active_refused", async () => {
  if (svc6.activateGeneration("act-a6", genA6, null).state !== "active")
    throw new Error("activate");
  const r = svc6.retireGeneration("req-retire-3", genA6, shaA6);
  if (r.state !== "refused" || r.reason_code !== "generation_active")
    throw new Error(JSON.stringify(r));
  return { refused: r.reason_code };
});

await acheck("retire_superseded_evidence_only", async () => {
  remirror(mir6, 600007, todayU32());
  const b = install(svc6, "req-pin-b");
  genB6 = genOf(svc6, b.task_id);
  verify(svc6, genB6);
  svc6.generations().pin(genA6, "task:held-2");
  if (svc6.activateGeneration("act-b6", genB6, genA6).state !== "active")
    throw new Error("activate b");
  const pinned = svc6.retireGeneration("req-retire-4", genA6, shaA6);
  if (pinned.reason_code !== "generation_referenced")
    throw new Error(JSON.stringify(pinned));
  svc6.generations().unpin(genA6, "task:held-2");
  const done = svc6.retireGeneration("req-retire-5", genA6, shaA6);
  if (done.state !== "evidence_only") throw new Error(JSON.stringify(done));
  // bytes gone; manifest + decision retained
  const gdir = join(root6, "generations", genA6);
  if (fs.existsSync(join(gdir, "base.zip"))) throw new Error("bytes kept");
  if (!fs.existsSync(join(gdir, "snapshot-manifest.json")))
    throw new Error("manifest removed");
  if (!fs.existsSync(join(gdir, "consistency-decision.json")))
    throw new Error("decision removed");
  const reverify = svc6.verifyGeneration("verify-retired", genA6);
  // durable decision replays even though bytes are retired
  if (reverify.state !== "evidence_only")
    throw new Error(`reverify ${reverify.state}`);
  const actRetired = svc6.activateGeneration("act-retired", genA6, genB6);
  if (actRetired.state !== "refused") throw new Error(actRetired.state);
  return { retired: true };
});

// ============ scenario 7: misc refusals ============
await acheck("verify_missing_generation", async () => {
  const r = svc6.verifyGeneration("verify-missing", "0".repeat(64));
  if (r.state !== "refused" ||
      r.reason_code !== "generation_not_installed")
    throw new Error(JSON.stringify(r));
  return { refused: r.reason_code };
});

await acheck("request_kind_conflict", async () => {
  try {
    svc6.verifyGeneration("req-pin-b", "x".repeat(64));
    throw new Error("not refused");
  } catch (e) {
    if (!/already bound|request_conflict/i.test(e.message) &&
        e.kind !== "storage") throw e;
    return { conflict: true };
  }
});

emit("summary", failures === 0 ? "PASS" : "FAIL", `${failures} failures`);
process.exit(failures ? 1 : 0);
