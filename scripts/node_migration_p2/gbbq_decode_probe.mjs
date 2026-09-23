// NM-016 P2: validate src_node/parsers/gbbq_decode.mjs against a live Rust
// oracle — synthetic encrypt->decode round-trips and the retained corpus.
// Usage: node scripts/node_migration_p2/gbbq_decode_probe.mjs <worktree-root>
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { join } from "node:path";
import { parse as parseContainer } from "../../src_node/parsers/gbbq_file.mjs";
import { decryptRecord, decodeRecord, decodeEvents, GbbqDecodeError }
  from "../../src_node/parsers/gbbq_decode.mjs";

const ROOT = process.argv[2] || ".";
const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/gbbq_decode/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

const emit = (id, status, detail, expected = null, actual = null) =>
  console.log(JSON.stringify({ family: "gbbq_decode", id, status, detail, expected, actual }));

const hex = (u8) => Buffer.from(u8).toString("hex");
const evActual = (e) => ({
  market: e.market, code: e.code, date_raw: e.date_raw, category: e.category,
  c_raw: e.c_raw, c_raw_bits: e.c_raw_bits.map((b) => b.toString(16).padStart(8, "0")),
});
// oracle event comparison: numeric fields + code + bit-exact c_raw
const evEq = (a, o) =>
  a.market === o.market && a.code === o.code && a.date_raw === o.date_raw &&
  a.category === o.category &&
  a.c_raw_bits.every((b, i) => b === o.c_raw_bits[i]);

// ---------- synthetic encrypt->decode round trips ----------
for (const o of oracle.filter((r) => r.id.startsWith("synthetic_"))) {
  const enc = Buffer.from(o.enc, "hex");
  try {
    const clear = decryptRecord(enc);
    const clearOk = hex(clear) === o.clear;
    const e = decodeRecord(0, enc);
    const ok = clearOk && o.status === "ok" && evEq(evActual(e), o.event);
    emit(o.id, ok ? "PASS" : "FAIL", ok ? null : `clear=${clearOk}`,
      { clear: o.clear, event: o.event }, { clear: hex(clear), event: evActual(e) });
  } catch (err) {
    const ok = o.status === "error" && err instanceof GbbqDecodeError && err.message === o.error;
    emit(o.id, ok ? "PASS" : "FAIL", ok ? null : `expected ${JSON.stringify(o.error)} got ${err.message}`, o.error, err.message);
  }
}

// ---------- retained corpus: bounded records + full digest + histogram ----------
{
  const member = "/home/box/retained-inputs/official-gbbq-member";
  const buf = readFileSync(member);
  const sha = createHash("sha256").update(buf).digest("hex");
  const MANIFEST_SHA = "4a3e9b862247a8a3fec406cc25b9a5a6ee3303f740780822519c99decaa09d77";
  emit("member_sha256_manifest", sha === MANIFEST_SHA ? "PASS" : "FAIL", sha);

  const file = parseContainer(buf);
  const events = decodeEvents(file);
  const hasher = createHash("sha256");
  const hist = {};
  events.forEach((e, i) => {
    hasher.update(`${i},${e.market},${e.code},${e.date_raw},${e.category},` +
      e.c_raw_bits.map((b) => b.toString(16).padStart(8, "0")).join("") + "\n");
    hist[e.category] = (hist[e.category] ?? 0) + 1;
  });
  const digest = hasher.digest("hex");
  const want = oracle.find((r) => r.id === "member_digest");
  const histEq = JSON.stringify(Object.fromEntries(
    Object.entries(hist).sort(([a], [b]) => Number(a) - Number(b)))) === JSON.stringify(want.categories);
  emit("member_full_corpus_digest",
    digest === want.sha256 && events.length === want.count && histEq ? "PASS" : "FAIL",
    `digest=${digest} count=${events.length} histEq=${histEq}`,
    want.sha256, digest);

  for (const o of oracle.filter((r) => r.id.startsWith("member_event_"))) {
    const i = o.event.index;
    const ok = evEq(evActual(events[i]), o.event);
    emit(o.id, ok ? "PASS" : "FAIL", ok ? null : "mismatch", o.event, evActual(events[i]));
  }
}

// ---------- fail-closed typed boundary ----------
for (const [id, bad] of [
  ["reject_short_record", new Uint8Array(28)], ["reject_long_record", new Uint8Array(30)],
  ["reject_non_uint8", "x".repeat(29)], ["reject_array_input", new Array(29).fill(0)],
]) {
  try {
    decodeRecord(0, bad);
    emit(id, "FAIL", `accepted ${typeof bad}`);
  } catch (e) {
    emit(id, e instanceof GbbqDecodeError && e.kind === "invalid_input" ? "PASS" : "FAIL",
      `${e.kind}: ${e.message}`);
  }
}
// all-zero record decrypts to non-UTF-8 code bytes -> fail-closed invalid_code
try {
  decodeEvents({ records: [new Uint8Array(29)] });
  emit("decode_events_zero_record", "FAIL", "decoded a zero record");
} catch (e) {
  emit("decode_events_zero_record",
    e instanceof GbbqDecodeError && e.kind === "invalid_code" && e.index === 0 ? "PASS" : "FAIL",
    e.message);
}
try {
  decodeEvents(null);
  emit("decode_events_null_reject", "FAIL", "accepted null");
} catch (e) {
  emit("decode_events_null_reject", e instanceof GbbqDecodeError ? "PASS" : "FAIL", e.message);
}
