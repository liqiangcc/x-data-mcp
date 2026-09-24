#!/usr/bin/env node
// Node port of src/bin/inspect_snapshot.rs — read at most 1 MiB + 1 of the
// manifest (the over-limit check lives in snapshot::inspect), then inspect.
import { openSync, readSync, closeSync } from "node:fs";
import { inspect } from "../governance/snapshot.mjs";
import { die, serPretty } from "./_shared.mjs";

function run() {
  const args = process.argv.slice(2);
  if (args.length !== 2)
    throw new Error("usage: inspect_snapshot <manifest.json> <trusted-input-directory>");
  // File::open + .take(1MiB+1).read_to_end — bounded read, not whole file
  const buf = Buffer.alloc(1024 * 1024 + 1);
  const fd = openSync(args[0], "r");
  let n;
  try {
    n = readSync(fd, buf, 0, buf.length, 0);
  } finally {
    closeSync(fd);
  }
  const bytes = buf.subarray(0, n);
  const result = inspect(bytes, args[1], new Date());
  process.stdout.write(serPretty(result) + "\n");
}

try {
  run();
} catch (e) {
  die(e);
}
