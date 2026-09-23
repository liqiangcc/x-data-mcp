// NM-018 P2: retained-input integration slice — gen-2 official bytes ->
// day_file.parse + gbbq parse/decode -> category-1 filter -> forwardAdjust
// + eventDayMarks, compared against a live Rust oracle on the same bytes.
// Usage: node scripts/node_migration_p2/retained_qfq_probe.mjs <worktree-root>
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { inflateRawSync } from "node:zlib";
import { join } from "node:path";
import { parse as parseDay } from "../../src_node/parsers/day_file.mjs";
import { parse as parseGbbq } from "../../src_node/parsers/gbbq_file.mjs";
import { decodeEvents } from "../../src_node/parsers/gbbq_decode.mjs";
import { forwardAdjust } from "../../src_node/algorithms/qfq_forward.mjs";
import { eventDayMarks } from "../../src_node/algorithms/qfq_event_marks.mjs";

const ROOT = process.argv[2] || ".";
const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/retained_qfq/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

const emit = (id, status, detail, expected = null, actual = null) =>
  console.log(JSON.stringify({ family: "retained_qfq", id, status, detail, expected, actual }));
const sha256 = (buf) => createHash("sha256").update(buf).digest("hex");

// ---------- bounded ZIP member reader (EOCD -> central dir -> local hdr) ----------
function zipMember(buf, name) {
  const eocd = buf.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06]));
  if (eocd < 0) throw new Error("no EOCD");
  const cdCount = buf.readUInt16LE(eocd + 10);
  let off = buf.readUInt32LE(eocd + 16);
  for (let i = 0; i < cdCount; i++) {
    if (buf.readUInt32LE(off) !== 0x02014b50) throw new Error("bad central header");
    const method = buf.readUInt16LE(off + 10);
    const compSize = buf.readUInt32LE(off + 20);
    const nameLen = buf.readUInt16LE(off + 28), extraLen = buf.readUInt16LE(off + 30), cmtLen = buf.readUInt16LE(off + 32);
    const lho = buf.readUInt32LE(off + 42);
    const entry = buf.subarray(off + 46, off + 46 + nameLen).toString("latin1");
    if (entry === name) {
      const lNameLen = buf.readUInt16LE(lho + 26), lExtraLen = buf.readUInt16LE(lho + 28);
      const data = buf.subarray(lho + 30 + lNameLen + lExtraLen, lho + 30 + lNameLen + lExtraLen + compSize);
      if (method === 0) return data;
      if (method === 8) return inflateRawSync(data);
      throw new Error(`unsupported method ${method}`);
    }
    off += 46 + nameLen + extraLen + cmtLen;
  }
  throw new Error(`member not found: ${name}`);
}

// ---------- MANIFEST verification ----------
const DAY_ZIP = "/home/box/retained-inputs/official-hsjday.zip";
const GBBQ_MEMBER = "/home/box/retained-inputs/official-gbbq-member";
const MANIFEST = {
  [DAY_ZIP]: "c82f852a7bc2e52c7e89347a1e0d1878b01ce997946c2745f7af323752270892",
  [GBBQ_MEMBER]: "4a3e9b862247a8a3fec406cc25b9a5a6ee3303f740780822519c99decaa09d77",
};
for (const [p, want] of Object.entries(MANIFEST)) {
  const got = sha256(readFileSync(p));
  emit(`manifest_${p.split("/").pop()}`, got === want ? "PASS" : "FAIL", got);
}

// ---------- pipeline ----------
const zipBuf = readFileSync(DAY_ZIP);
const dayBytes = zipMember(zipBuf, "sz\\lday\\sz000002.day");
emit("member_extract", dayBytes.length === 271328 ? "PASS" : "FAIL",
  `sz\\lday\\sz000002.day ${dayBytes.length}B sha256=${sha256(dayBytes)}`, null, sha256(dayBytes));

const bars = parseDay(dayBytes);
const gbbqFile = parseGbbq(readFileSync(GBBQ_MEMBER));
const allEvents = decodeEvents(gbbqFile);
// caller-boundary filter (Rust: official_market.rs — category==1 only,
// keyed by (market, code); other categories uninterpreted)
const events = allEvents.filter((e) => e.market === 0 && e.code === "000002" && e.category === 1);

const sel = oracle.find((r) => r.id === "selection");
emit("selection", bars.length === sel.bars && events.length === sel.cat1_events &&
  JSON.stringify(events.map((e) => e.date_raw)) === JSON.stringify(sel.dates) ? "PASS" : "FAIL",
  `bars=${bars.length} cat1=${events.length}`, { bars: sel.bars, cat1: sel.cat1_events, dates: sel.dates }, { bars: bars.length, cat1: events.length, dates: events.map((e) => e.date_raw) });

// raw record digest — same serialization as Rust
{
  const h = createHash("sha256");
  for (const b of bars) {
    h.update(`${b.date},${b.ohlc_raw.join(",")},${b.amount_float32_bits},${b.volume_raw},${b.r7_raw}\n`);
  }
  const want = oracle.find((r) => r.id === "raw_digest");
  const got = h.digest("hex");
  emit("raw_digest", got === want.sha256 && bars.length === want.bars ? "PASS" : "FAIL",
    got, want.sha256);
}

// forward adjust digest — same serialization as Rust
const out = forwardAdjust(bars, events);
{
  const h = createHash("sha256");
  for (const b of out) {
    h.update(`${b.date},${b.open},${b.high},${b.low},${b.close},${b.chain_event_count},${b.amount_float32_bits},${b.volume_raw},${b.r7_raw}\n`);
  }
  const want = oracle.find((r) => r.id === "fq_digest");
  const got = h.digest("hex");
  emit("fq_digest", got === want.sha256 && out.length === want.bars ? "PASS" : "FAIL",
    `${got} bars=${out.length}`, want.sha256);
}

// sampled bars (all oracle bar_* rows)
for (const o of oracle.filter((r) => r.id.startsWith("bar_"))) {
  const i = Number(o.id.slice(4));
  const ok = JSON.stringify(out[i]) === JSON.stringify(o.bar);
  emit(o.id, ok ? "PASS" : "FAIL", ok ? null : "mismatch", o.bar, out[i]);
}

// event-day marks
const marks = eventDayMarks(bars, events);
const markRows = oracle.filter((r) => r.id.startsWith("mark_"));
emit("marks_count", marks.length === markRows.length ? "PASS" : "FAIL", `${marks.length}`);
for (const o of markRows) {
  const i = Number(o.id.slice(5));
  const ok = JSON.stringify(marks[i]) === JSON.stringify(o.mark);
  emit(o.id, ok ? "PASS" : "FAIL", ok ? null : "mismatch", o.mark, marks[i]);
}
