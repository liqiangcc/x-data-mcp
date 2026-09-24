#!/usr/bin/env node
// Node port of src/bin/consistency_check.rs — read input.json, run
// file_consistency::validate, print the verbatim decision.
import { readFileSync } from "node:fs";
import { validate } from "../governance/file_consistency.mjs";
import { parseTs } from "../governance/ts.mjs";
import { decodeInput } from "./_consistency_input.mjs";
import { die, serPretty } from "./_shared.mjs";

// FileFact field order = Rust struct declaration order; timestamps are
// re-serialized to canonical RFC3339 (DateTime<Utc> -> "Z" form).
const FACT_FIELDS = [
  "role", "bytes", "sha256", "source_url", "acquisition_started_at",
  "acquisition_completed_at", "last_modified", "etag", "byte_integrity",
  "source_revision", "source_identity",
];
const TS_FIELDS = new Set(["acquisition_started_at", "acquisition_completed_at"]);
const factOut = (f) => {
  const o = {};
  for (const k of FACT_FIELDS) o[k] = TS_FIELDS.has(k) ? parseTs(f[k]).out : f[k];
  return o;
};

function run() {
  const args = process.argv.slice(2);
  if (args.length !== 1)
    throw new Error("usage: consistency_check <input.json> — evaluates " +
      "file_consistency::validate and prints the verbatim decision");
  const bytes = readFileSync(args[0]);
  const input = decodeInput(bytes); // serde-compatible Input decode
  const decision = validate({
    files: input.files,
    compatibility: input.compatibility,
  });
  process.stdout.write(serPretty({
    contract_version: decision.contract_version,
    status: decision.status,
    reasons: decision.reasons,
    evidence_refs: decision.evidence_refs,
    files: decision.files.map(factOut),
  }) + "\n");
}

try {
  run();
} catch (e) {
  die(e);
}
