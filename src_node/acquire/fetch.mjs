// Node port of src/acquire/fetch.rs — bounded fetch surface.
//
// Fetcher contract (the trait): an object with
//   head(url) -> {status u16, etag?, last_modified?, content_length?}
//   fetch(url, destPath) -> {head, bytes u64, sha256 hex}
// Errors throw FetchError with .kind "failed" | "output" (verbatim
// Display: "fetch failed: …" / "fetch helper output invalid: …").
//
// `Helper` is the real spawn port: `python3 <script> head|fetch <url>
// [dest]` — script is X_DATA_MCP_TDX_FETCH or <repo>/scripts/
// tdx_fetch.py; stdout must be the JSON wire shape below
// (deny_unknown_fields), nonzero exit -> Failed(trimmed stderr).
// The helper program itself is not ported (S14c/bin scope).

import { spawnSync } from "node:child_process";
import { statSync, readFileSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

export class FetchError extends Error {
  constructor(kind, message) {
    super(message);
    this.name = "FetchError";
    this.kind = kind;
    this.code = kind;
  }
}
const failed = (m) => new FetchError("failed", `fetch failed: ${m}`);
const outputErr = (m) =>
  new FetchError("output", `fetch helper output invalid: ${m}`);

const sha256hex = (bytes) =>
  createHash("sha256").update(bytes).digest("hex");

// HeadJson/FetchJson — serde deny_unknown_fields; numeric status /
// content_length / bytes are the contract (header strings are
// normalized inside tdx_fetch.py, not here — fetch.rs:172 test).
const checkJson = (name, fields, obj) => {
  if (obj === null || typeof obj !== "object" || Array.isArray(obj))
    throw outputErr(`invalid type: expected struct ${name}`);
  for (const k of Object.keys(obj))
    if (!(k in fields)) throw outputErr(`unknown field \`${k}\``);
  const out = {};
  for (const [k, required, num] of fields) {
    const v = obj[k];
    if (v === undefined || v === null) {
      if (required) throw outputErr(`missing field \`${k}\``);
      out[k] = null;
      continue;
    }
    if (num) {
      const isU64 = (typeof v === "number" && Number.isInteger(v) &&
                     v >= 0) || (typeof v === "bigint" && v >= 0n);
      if (!isU64) throw outputErr(`invalid type for field \`${k}\``);
      out[k] = v;
    } else {
      if (typeof v !== "string" && typeof v !== "number")
        throw outputErr(`invalid type for field \`${k}\``);
      out[k] = v;
    }
  }
  return out;
};
const HEAD_FIELDS = [
  ["status", true, true],
  ["etag", false, false],
  ["last_modified", false, false],
  ["content_length", false, true],
];
const FETCH_FIELDS = [
  ["status", true, true],
  ["etag", false, false],
  ["last_modified", false, false],
  ["content_length", false, true],
  ["bytes", true, true],
  ["sha256", true, false],
];

export class Helper {
  constructor() {
    // fetch.rs:40-48 — X_DATA_MCP_TDX_FETCH override, else the repo's
    // pinned script path.
    this.script = process.env.X_DATA_MCP_TDX_FETCH ??
      join(fileURLToPath(new URL(".", import.meta.url)),
           "../../scripts/tdx_fetch.py");
  }
  _run(args) {
    // fetch.rs:49-61 — Command::new("python3") + args, JSON on stdout.
    const res = spawnSync("python3", [this.script, ...args], {
      encoding: "utf8", maxBuffer: 16 * 1024 * 1024,
    });
    if (res.error) throw failed(res.error.message);
    if (res.status !== 0) {
      throw failed(String(res.stderr ?? "").trim());
    }
    try {
      return JSON.parse(res.stdout);
    } catch (e) {
      throw outputErr(e.message);
    }
  }
  head(url) {
    return checkJson("HeadJson", HEAD_FIELDS, this._run(["head", url]));
  }
  fetch(url, dest) {
    const f = checkJson("FetchJson", FETCH_FIELDS,
                        this._run(["fetch", url, String(dest)]));
    return {
      head: {
        status: f.status, etag: f.etag, last_modified: f.last_modified,
        content_length: f.content_length,
      },
      bytes: f.bytes,
      sha256: f.sha256,
    };
  }
}

// fetch.rs:119-162 — local mirror: maps a declared role URL to a file
// under one directory by canonical artifact name; pinned snapshots and
// isolated acceptance flow through the identical byte-verified
// pipeline while source_url stays the declared TDX URL.
export class Mirror {
  constructor(dir) {
    this.dir = dir;
  }
  _file(url) {
    const name = String(url).split("/").pop() ?? "";
    if (!["base.zip", "gbbq.zip", "hsjday.zip"].includes(name)) {
      throw failed(`undeclared url ${url}`);
    }
    return join(this.dir, name);
  }
  head(url) {
    const path = this._file(url);
    let meta;
    try {
      meta = statSync(path);
    } catch (e) {
      throw failed(e.message);
    }
    const mtime = Number.isFinite(meta.mtimeMs)
      ? Math.max(0, Math.floor(meta.mtimeMs / 1000)) : 0;
    return {
      status: 200,
      etag: `"${meta.size.toString(16)}-${mtime.toString(16)}"`,
      last_modified: null,
      content_length: BigInt(meta.size),
    };
  }
  fetch(url, dest) {
    const path = this._file(url);
    let bytes;
    try {
      bytes = readFileSync(path);
    } catch (e) {
      throw failed(e.message);
    }
    try {
      writeFileSync(dest, bytes);
    } catch (e) {
      throw failed(e.message);
    }
    return {
      head: this.head(url),
      bytes: BigInt(bytes.length),
      sha256: sha256hex(bytes),
    };
  }
}
