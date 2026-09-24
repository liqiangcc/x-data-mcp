#!/usr/bin/env node
// NM-112 — gbbq decode bench + byte-identity parity harness.
// Usage: node gbbq_bench.mjs [--parity] [iters]
//   --parity: decode the real fixtures with the OLD impl
//             (/tmp/gbbq_decode_old.mjs) and the CURRENT impl, compare
//             canonical serialization digests.
import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { parse } from "../../src_node/parsers/gbbq_file.mjs";
import * as cur from "../../src_node/parsers/gbbq_decode.mjs";
import { serValue } from "../../src_node/mcp/serde_json.mjs";

const zipMember = (zip, name) => Buffer.from(
  spawnSync("python3", ["-c",
    `import zipfile,sys;sys.stdout.buffer.write(zipfile.ZipFile("${zip}").read("${name}"))`],
    { maxBuffer: 1 << 28 }).stdout);

const FIXTURES = [
  "/home/box/retained-inputs/official-gbbq.zip",
  "/home/box/retained-inputs/gen3/gbbq.zip",
];

const load = (zip) => parse(zipMember(zip, "gbbq"));

const digest = (events) => {
  const h = createHash("sha256");
  for (const e of events) h.update(serValue(e));
  return h.digest("hex");
};

const parity = process.argv.includes("--parity");
if (parity) {
  const old = await import("/tmp/gbbq_decode_old.mjs");
  for (const f of FIXTURES) {
    const file = load(f);
    const a = digest(cur.decodeEvents(file));
    const b = digest(old.decodeEvents(file));
    console.log(JSON.stringify({ fixture: f.split("/").pop(),
      records: file.records.length, cur: a, old: b, equal: a === b }));
  }
  // edge cases: invalid-length + invalid-UTF8 record error text parity
  const edge = [
    new Uint8Array(29),                       // all-NUL code
    new Uint8Array(29).fill(0x41),            // 'A'*29
    new Uint8Array([1,0xff,0xfe,0xfd,0,0,0,0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23]), // bad utf8 in code
    (() => { const b = new Uint8Array(29); b[0]=2; b[1]=0x41; b[2]=0; b[3]=0x42; return b; })(), // NUL mid-code
  ];
  let ok = true;
  for (const [i, rec] of edge.entries()) {
    let a, b;
    try { a = { v: cur.decodeRecord(i, rec) }; }
    catch (e) { a = { err: e.message, kind: e.kind }; }
    try { b = { v: old.decodeRecord(i, rec) }; }
    catch (e) { b = { err: e.message, kind: e.kind }; }
    const same = JSON.stringify(a) === JSON.stringify(b);
    if (!same) ok = false;
    console.log(JSON.stringify({ edge: i, same, a, b }));
  }
  console.log(JSON.stringify({ parity: ok ? "PASS" : "FAIL" }));
  process.exit(ok ? 0 : 1);
}

// bench mode
const iters = Number(process.argv[2] || 20);
const file = load(FIXTURES[0]);
const n = file.records.length;
// warmup
for (let i = 0; i < 3; i++) cur.decodeEvents(file);
const lat = [];
for (let i = 0; i < iters; i++) {
  const t0 = performance.now();
  cur.decodeEvents(file);
  lat.push(performance.now() - t0);
}
lat.sort((a, b) => a - b);
console.log(JSON.stringify({ records: n, iters,
  p50_ms: +lat[Math.floor(iters / 2)].toFixed(1),
  p95_ms: +lat[Math.floor(iters * 0.95)].toFixed(1),
  min_ms: +lat[0].toFixed(1),
  digest: digest(cur.decodeEvents(file)) }));
