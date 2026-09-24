// NM-111 — byte-identity differ for day_parse round 2.
// Compares the current parse() against a frozen pre-change copy on real
// retained members + edge fixtures; emits identical rows/errors or diffs.
// Usage: node day_parse_golden.mjs <old-module> <workdir> [member...]
import fs from "node:fs";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";

const [oldPath, dir, ...members] = process.argv.slice(2);
const OLD = await import(pathToFileURL(oldPath).href);
const NEW = await import("../../src_node/parsers/day_file.mjs");

const ZIP = "/home/box/retained-inputs/official-hsjday.zip";
const inputs = [];
for (const m of (members.length ? members : ["sh\\lday\\sh000001.day",
    "sh\\lday\\sh000002.day", "sz\\lday\\sz000001.day"])) {
  const out = execFileSync("python3", ["-c",
    "import zipfile,sys;sys.stdout.buffer.write(" +
    "zipfile.ZipFile(sys.argv[1]).read(sys.argv[2]))", ZIP, m],
    { maxBuffer: 32 * 1024 * 1024 });
  inputs.push({ name: m, bytes: out });
}
// Edge fixtures — every arm: truncation, invalid dates (month/day/leap),
// ordering (equal, descending), empty, year bounds.
const rec = (d, o = 1, h = 2, l = 0, c = 1, a = 0, v = 0, r7 = 0) => {
  const b = Buffer.alloc(32);
  [d, o, h, l, c, a, v, r7].forEach((x, i) => b.writeUInt32LE(x, i * 4));
  return b;
};
const cat = (...rs) => Buffer.concat(rs);
inputs.push(
  { name: "empty", bytes: Buffer.alloc(0) },
  { name: "truncated", bytes: Buffer.alloc(33) },
  { name: "single", bytes: rec(20240102) },
  { name: "leap_ok", bytes: rec(20240229) },
  { name: "leap_bad", bytes: rec(20230229) },
  { name: "year_1", bytes: rec(10101) },
  { name: "year_9999", bytes: rec(99991231) },
  { name: "year_10000", bytes: rec(100000101) },
  { name: "month_0", bytes: rec(20240001) },
  { name: "month_13", bytes: rec(20241301) },
  { name: "day_0", bytes: rec(20240100) },
  { name: "day_32", bytes: rec(20240132) },
  { name: "equal_dates", bytes: cat(rec(20240102), rec(20240102)) },
  { name: "descending", bytes: cat(rec(20240103), rec(20240102)) },
  { name: "ascending", bytes: cat(rec(20240101), rec(20240102), rec(20240103)) },
);
// Misaligned view — exercises the byte-assembly fallback path.
{
  const padded = Buffer.concat([Buffer.alloc(1), inputs[0].bytes]);
  inputs.push({ name: "misaligned", bytes: padded.subarray(1) });
}

const rows = [];
for (const { name, bytes } of inputs) {
  for (const [tag, mod] of [["old", OLD], ["new", NEW]]) {
    let row;
    try {
      const recs = mod.parse(bytes);
      row = { tag, n: recs.length,
        sha: createHash("sha256").update(JSON.stringify(recs)).digest("hex") };
    } catch (e) {
      row = { tag, err: `${e.name}:${e.kind ?? ""}:${e.message}` };
    }
    rows.push({ name, ...row });
  }
}
let diffs = 0;
for (let i = 0; i < rows.length; i += 2) {
  const [a, b] = [rows[i], rows[i + 1]];
  const ok = a.sha === b.sha && a.err === b.err && a.n === b.n;
  if (!ok) diffs++;
  console.log(JSON.stringify({ input: rows[i].name, match: ok,
    old: { n: a.n, sha: a.sha, err: a.err },
    new: { n: b.n, sha: b.sha, err: b.err } }));
}
console.log(`day_parse diff: ${rows.length / 2 - diffs}/${rows.length / 2} identical, ${diffs} diffs`);
process.exit(diffs ? 1 : 0);
