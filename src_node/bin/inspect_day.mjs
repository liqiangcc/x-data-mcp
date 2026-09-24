#!/usr/bin/env node
// Node port of src/bin/inspect_day.rs — explicit local-file structural
// inspection; no network, normalization or publish.
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { parse } from "../parsers/day_file.mjs";
import { die, serPretty } from "./_shared.mjs";

function run() {
  const args = process.argv.slice(2);
  if (args.length === 0) throw new Error("usage: inspect_day PATH_TO_DAY_FILE");
  if (args.length > 1) throw new Error("expected exactly one local .day file");
  const bytes = readFileSync(args[0]);
  const records = parse(bytes);
  // serde_json::to_string_pretty(json!(...)) — Value => keys sorted
  process.stdout.write(serPretty({
    schema: "x-data-mcp.day-structural-inspection.v1",
    sha256: createHash("sha256").update(bytes).digest("hex"),
    bytes: bytes.length,
    record_count: records.length,
    first_date: records.length ? records[0].date : null,
    last_date: records.length ? records[records.length - 1].date : null,
    business_semantics: "NOT_VERIFIED",
    production_ready: false,
    evidence: "STRUCTURAL_ONLY",
    live_source: "NOT_RUN",
  }, { sorted: true }) + "\n");
}

try {
  run();
} catch (e) {
  die(e);
}
