// Node port of src/acquire/wire.rs — typed wire contract for
// x-data-acquire. Single-action tools only: outputs are
// state/outcome/refs/reason codes — never next actions, other tool
// names, or orchestration hints.
//
// serde contract preserved:
//   * deny_unknown_fields on every struct — extra keys reject;
//   * non-Option fields required; Option fields absent-or-null -> null;
//   * ContractVersion enum "v1" only; Change -> snake_case strings;
//   * u16/u64 range-checked (u64 accepts number|bigint, emitted as
//     exact digits);
//   * serializers emit fields in declaration order (serde order).
//
// Errors carry serde_json-phrased messages on .kind="json" so callers
// mapping to ApiError keep the contract ("json: ..." via store's
// outcome<T> path, or the service's invalid_tool_call wrap).

export const CONTRACT_VERSION = "v1"; // ContractVersion::V1

export class WireError extends Error {
  constructor(message) {
    super(message);
    this.name = "WireError";
    this.kind = "json";
    this.code = "json";
  }
}
const err = (m) => new WireError(m);

// ---- serde_json-ish type phrasing ----
const describe = (v) =>
  v === null ? "null" :
  Array.isArray(v) ? "sequence" :
  typeof v === "string" ? `string "${v}"` :
  typeof v === "boolean" ? `boolean \`${v}\`` :
  typeof v === "bigint" ? `integer \`${v}\`` :
  typeof v === "number" && Number.isInteger(v) ? `integer \`${v}\`` :
  typeof v === "number" ? `floating point \`${v}\`` :
  typeof v === "object" ? "map" : `unsupported ${typeof v}`;

// serde "expected" list: `a` / `a` or `b` / `a`, `b` or `c`.
const expectList = (names) => {
  const q = names.map((n) => `\`${n}\``);
  if (q.length === 1) return q[0];
  return q.slice(0, -1).join(", ") + " or " + q[q.length - 1];
};

// ---- field type checkers ----
const isString = (v) => typeof v === "string" && v.isWellFormed();
const isU64 = (v) =>
  (typeof v === "bigint" && v >= 0n && v <= 18446744073709551615n) ||
  (typeof v === "number" && Number.isInteger(v) && v >= 0 &&
   v <= 18446744073709551615);
const isU16 = (v) =>
  typeof v === "number" && Number.isInteger(v) && v >= 0 && v <= 65535;

const CHECKS = {
  version: (v) => {
    if (v !== CONTRACT_VERSION)
      throw err(`unknown variant \`${JSON.stringify(v)}\`, expected \`v1\``);
    return v;
  },
  string: (v, k) => {
    if (!isString(v))
      throw err(`invalid type: ${describe(v)}, expected a string`);
    return v;
  },
  bool: (v) => {
    if (typeof v !== "boolean")
      throw err(`invalid type: ${describe(v)}, expected a boolean`);
    return v;
  },
  u16: (v) => {
    if (!isU16(v))
      throw err(`invalid type: ${describe(v)}, expected u16`);
    return v;
  },
  u64: (v) => {
    if (!isU64(v))
      throw err(`invalid type: ${describe(v)}, expected u64`);
    return v;
  },
  opt_string: (v) => {
    if (v === null) return null; // caller guarantees absent-or-null here
    if (!isString(v))
      throw err(`invalid type: ${describe(v)}, expected option`);
    return v;
  },
  opt_u64: (v) => {
    if (v === null) return null;
    if (!isU64(v))
      throw err(`invalid type: ${describe(v)}, expected option`);
    return v;
  },
  vec_string: (v) => {
    if (!Array.isArray(v))
      throw err(`invalid type: ${describe(v)}, expected a sequence`);
    for (const x of v) {
      if (!isString(x))
        throw err(`invalid type: ${describe(x)}, expected a string`);
    }
    return v;
  },
  opt_vec_string: (v) => {
    if (v === null) return null;
    return CHECKS.vec_string(v);
  },
  any: (v) => v, // serde_json::Value — passthrough
};

// spec: {fields:[(name,type)] in declaration order}; Option types are
// named opt_* — absent-or-null -> null; everything else required.
function parseStruct(name, spec, obj) {
  if (obj === null || typeof obj !== "object" || Array.isArray(obj))
    throw err(`invalid type: ${describe(obj)}, expected struct ${name}`);
  const known = spec.fields.map(([k]) => k);
  for (const k of Object.keys(obj)) {
    if (!known.includes(k)) {
      throw err(known.length === 0
        ? `unknown field \`${k}\`, there are no fields`
        : `unknown field \`${k}\`, expected ${expectList(known)}`);
    }
  }
  const out = {};
  for (const [k, t] of spec.fields) {
    const optional = t.startsWith("opt_");
    if (!(k in obj)) {
      if (optional) { out[k] = null; continue; }
      throw err(`missing field \`${k}\``);
    }
    const v = obj[k];
    if (v === null && !optional) {
      throw err(`invalid type: null, expected ${expectName(t)}`);
    }
    out[k] = CHECKS[t](v, k);
  }
  return out;
}
const expectName = (t) =>
  t === "version" ? "`v1`" :
  t === "string" ? "a string" :
  t === "bool" ? "a boolean" :
  t === "u16" ? "u16" :
  t === "u64" ? "u64" :
  t === "vec_string" ? "a sequence" : "a value";

// ---- request structs (Deserialize + deny_unknown_fields) ----
const REQUEST_SPECS = {
  CheckOfficialUpdates: { fields: [["contract_version", "version"]] },
  AcquireGeneration: { fields: [["contract_version", "version"],
    ["request_id", "string"]] },
  GetAcquisitionTask: { fields: [["contract_version", "version"],
    ["task_id", "string"]] },
  GetAcquisitionRequest: { fields: [["contract_version", "version"],
    ["request_id", "string"]] },
  VerifyGeneration: { fields: [["contract_version", "version"],
    ["request_id", "string"], ["generation_id", "string"]] },
  ActivateGeneration: { fields: [["contract_version", "version"],
    ["request_id", "string"], ["generation_id", "string"],
    ["expected_previous", "opt_string"]] },
  SyncStatus: { fields: [["contract_version", "version"]] },
  RetireGeneration: { fields: [["contract_version", "version"],
    ["request_id", "string"], ["generation_id", "string"],
    ["confirm_token", "string"]] },
  WorkerOnce: { fields: [] },
};

export const parse = {};
for (const [name, spec] of Object.entries(REQUEST_SPECS)) {
  parse[name] = (obj) => parseStruct(name, spec, obj);
}

// ---- reply structs: Deserialize too (outcome<T> round-trip) ----
const REPLY_SPECS = {
  VerifyReply: { fields: [["request_id", "string"],
    ["generation_id", "string"], ["state", "string"],
    ["consistency_status", "opt_string"],
    ["consistency_reasons", "vec_string"],
    ["decision_sha256", "opt_string"], ["reason_code", "opt_string"],
    ["reused", "bool"]] },
  ActivateReply: { fields: [["request_id", "string"],
    ["generation_id", "string"], ["state", "string"],
    ["reason_code", "opt_string"], ["detail", "opt_string"],
    ["manifest_sha256", "opt_string"], ["activated_at", "opt_string"],
    ["previous_generation_id", "opt_string"], ["reused", "bool"]] },
  RetireReply: { fields: [["request_id", "string"],
    ["generation_id", "string"], ["state", "string"],
    ["reason_code", "opt_string"], ["detail", "opt_string"],
    ["reused", "bool"]] },
};
for (const [name, spec] of Object.entries(REPLY_SPECS)) {
  parse[name] = (obj) => parseStruct(name, spec, obj);
}

// ---- Change enum (rename_all snake_case) ----
export const Change = Object.freeze({
  Changed: "changed", Unchanged: "unchanged", Unknown: "unknown",
});
const parseChange = (v) => {
  if (v === "changed" || v === "unchanged" || v === "unknown") return v;
  throw err(`unknown variant \`${JSON.stringify(v)}\`, expected \`changed\`, \`unchanged\` or \`unknown\``);
};

// ---- serializers (Serialize-only types) — declaration order ----
// serJson is serde_json::to_string: keys in insertion order, u64/bigint
// emitted as exact digits.
export const serJson = (v) => {
  if (v === null) return "null";
  if (typeof v === "bigint") return v.toString();
  if (typeof v === "number" || typeof v === "string" ||
      typeof v === "boolean") return JSON.stringify(v);
  if (Array.isArray(v)) return "[" + v.map(serJson).join(",") + "]";
  if (typeof v === "object") {
    return "{" + Object.entries(v)
      .map(([k, x]) => JSON.stringify(k) + ":" + serJson(x))
      .join(",") + "}";
  }
  throw err(`unsupported type in wire serialize: ${typeof v}`);
};

const req = (name, k, v, t) => {
  if (v === undefined) throw err(`wire serialize: missing field \`${k}\``);
  return CHECKS[t](v, k);
};

// wire.rs:78-86 RemoteFile
export const remoteFile = (f) => ({
  role: req("RemoteFile", "role", f.role, "string"),
  url: req("RemoteFile", "url", f.url, "string"),
  status: req("RemoteFile", "status", f.status, "u16"),
  etag: f.etag ?? null,
  last_modified: f.last_modified ?? null,
  content_length: f.content_length ?? null,
  change: parseChange(f.change),
});

// wire.rs:89-94 UpdateCheck
export const updateCheck = (u) => ({
  checked_at: req("UpdateCheck", "checked_at", u.checked_at, "string"),
  files: (u.files ?? []).map(remoteFile),
  changed: CHECKS.vec_string(u.changed),
  unknown: CHECKS.vec_string(u.unknown),
});

// wire.rs:98-103 AcquisitionAcceptance
export const acquisitionAcceptance = (a) => ({
  request_id: req("AcquisitionAcceptance", "request_id", a.request_id, "string"),
  task_id: req("AcquisitionAcceptance", "task_id", a.task_id, "string"),
  state: req("AcquisitionAcceptance", "state", a.state, "string"),
  reused: req("AcquisitionAcceptance", "reused", a.reused, "bool"),
});

// wire.rs:106-118 AcquisitionTaskView
export const acquisitionTaskView = (t) => ({
  task_id: req("AcquisitionTaskView", "task_id", t.task_id, "string"),
  request_id: req("AcquisitionTaskView", "request_id", t.request_id, "string"),
  kind: req("AcquisitionTaskView", "kind", t.kind, "string"),
  state: req("AcquisitionTaskView", "state", t.state, "string"),
  generation_id: t.generation_id ?? null,
  stage: t.stage ?? null,
  bytes_done: req("AcquisitionTaskView", "bytes_done", t.bytes_done, "u64"),
  error: t.error ?? null,
  detail: t.detail ?? null,
  created_at: req("AcquisitionTaskView", "created_at", t.created_at, "string"),
  updated_at: req("AcquisitionTaskView", "updated_at", t.updated_at, "string"),
});

// wire.rs:121-126 AcquisitionRequestView
export const acquisitionRequestView = (r) => ({
  request_id: req("AcquisitionRequestView", "request_id", r.request_id, "string"),
  bound: req("AcquisitionRequestView", "bound", r.bound, "bool"),
  task: r.task == null ? null : acquisitionTaskView(r.task),
  terminal_result: r.terminal_result === undefined ? null : r.terminal_result,
});

// wire.rs:129-138 GenerationView
export const generationView = (g) => ({
  generation_id: req("GenerationView", "generation_id", g.generation_id, "string"),
  state: req("GenerationView", "state", g.state, "string"),
  manifest_sha256: req("GenerationView", "manifest_sha256", g.manifest_sha256, "string"),
  consistency_status: g.consistency_status ?? null,
  consistency_reasons: g.consistency_reasons === null ||
    g.consistency_reasons === undefined ? null
      : CHECKS.vec_string(g.consistency_reasons),
  decision_sha256: g.decision_sha256 ?? null,
  acquisition_request_id: g.acquisition_request_id ?? null,
  installed_at: req("GenerationView", "installed_at", g.installed_at, "string"),
});

// wire.rs:145-154 VerifyReply
export const verifyReply = (r) => ({
  request_id: req("VerifyReply", "request_id", r.request_id, "string"),
  generation_id: req("VerifyReply", "generation_id", r.generation_id, "string"),
  state: req("VerifyReply", "state", r.state, "string"),
  consistency_status: r.consistency_status ?? null,
  consistency_reasons: CHECKS.vec_string(r.consistency_reasons ?? []),
  decision_sha256: r.decision_sha256 ?? null,
  reason_code: r.reason_code ?? null,
  reused: req("VerifyReply", "reused", r.reused, "bool"),
});

// wire.rs:161-171 ActivateReply
export const activateReply = (r) => ({
  request_id: req("ActivateReply", "request_id", r.request_id, "string"),
  generation_id: req("ActivateReply", "generation_id", r.generation_id, "string"),
  state: req("ActivateReply", "state", r.state, "string"),
  reason_code: r.reason_code ?? null,
  detail: r.detail ?? null,
  manifest_sha256: r.manifest_sha256 ?? null,
  activated_at: r.activated_at ?? null,
  previous_generation_id: r.previous_generation_id ?? null,
  reused: req("ActivateReply", "reused", r.reused, "bool"),
});

// wire.rs:180-183 WorkerTurn
export const workerTurn = (t) => ({
  idle: req("WorkerTurn", "idle", t.idle, "bool"),
  task: t.task == null ? null : acquisitionTaskView(t.task),
});

// wire.rs:190-197 RetireReply
export const retireReply = (r) => ({
  request_id: req("RetireReply", "request_id", r.request_id, "string"),
  generation_id: req("RetireReply", "generation_id", r.generation_id, "string"),
  state: req("RetireReply", "state", r.state, "string"),
  reason_code: r.reason_code ?? null,
  detail: r.detail ?? null,
  reused: req("RetireReply", "reused", r.reused, "bool"),
});

// wire.rs:200-206 StatusView — `active` is the generation ActivePointer
// serde shape (src/governance — passthrough, its validator owns it).
export const statusView = (s) => ({
  active: s.active ?? null,
  generations: (s.generations ?? []).map(generationView),
  inflight_tasks: (s.inflight_tasks ?? []).map(acquisitionTaskView),
  generation_contract_version:
    req("StatusView", "generation_contract_version", s.generation_contract_version, "string"),
  consistency_contract_version:
    req("StatusView", "consistency_contract_version", s.consistency_contract_version, "string"),
});
