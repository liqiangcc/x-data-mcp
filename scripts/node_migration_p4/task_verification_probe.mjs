// NM-106 probe — runs the ported task_verification decision modules over
// the same scenario JSONL the Rust oracle consumes; diffs status, error
// code+text, decision detail, and repository call order.
//   node task_verification_probe.mjs scenarios.jsonl golden.jsonl
import { readFileSync, writeFileSync } from "node:fs";

import * as M from "../../src_node/task_verification/mod.mjs";
import * as WIRE from "../../src_node/task_verification/wire.mjs";
import * as ADM from "../../src_node/task_verification/admission.mjs";
import * as DEC from "../../src_node/task_verification/decision.mjs";
import * as NS from "../../src_node/task_verification/never_started.mjs";
import * as DET from "../../src_node/task_verification/determinate.mjs";
import * as EX from "../../src_node/task_verification/existing.mjs";


const digestOf = (snap) => M.snapshotDigest(snap);

const TC = new Set([
  "invalid_input", "request_conflict", "range_conflict", "needs_verification",
  "unavailable", "outcome_unknown", "revision_conflict", "unsupported_contract",
  "task_not_found", "invalid_retry_selection", "retry_request_id_conflict",
  "empty_retry_selection", "retry_selection_invalid", "empty_request_id",
]);
const errRow = (e) => {
  if (e && e.taskControl === true) return { status: "err", error: { code: e.code, message: e.message } };
  if (typeof e?.code === "string" && TC.has(e.code)) {
    return { status: "err", error: { code: e.code, message: e.message } };
  }
  return { status: "err", message: e.message ?? String(e) };
};

// ------------------------------------------------------------- stub repo
const KIND = (kind, msg) => {
  const m = {
    storage: ["unavailable", `STORAGE_UNAVAILABLE: STORAGE: ${msg}`],
    integrity: ["needs_verification", `ACCEPTANCE_NEEDS_VERIFICATION: MARKET_INTEGRITY: ${msg}`],
    base: ["needs_verification", `ACCEPTANCE_NEEDS_VERIFICATION: BASE_UNAVAILABLE: ${msg}`],
    conflict: ["needs_verification", `ACCEPTANCE_NEEDS_VERIFICATION: PUBLISH_PRECONDITION: ${msg}`],
    not_found: ["task_not_found", "TASK_NOT_FOUND"],
    invalid: ["invalid_input", `INVALID_COMMAND: INVALID_MARKET_COMMAND: ${msg}`],
    outcome_unknown: ["outcome_unknown", `OUTCOME_UNKNOWN: ${msg}; query the same request_id before retrying`],
    basis: ["needs_verification", `ACCEPTANCE_NEEDS_VERIFICATION: GAP_BASIS_NOT_VERIFIED: ${msg}`],
    tc_invalid: ["invalid_input", `INVALID_COMMAND: ${msg}`],
    tc_conflict: ["revision_conflict", "STATE_REVISION_CONFLICT"],
    tc_needs: ["needs_verification", `ACCEPTANCE_NEEDS_VERIFICATION: ${msg}`],
    tc_unavailable: ["unavailable", `STORAGE_UNAVAILABLE: ${msg}`],
    tc_outcome: ["outcome_unknown", `OUTCOME_UNKNOWN: ${msg}; query the same request_id before retrying`],
    tc_request: ["request_conflict", "REQUEST_CONFLICT"],
    tc_not_found: ["task_not_found", "TASK_NOT_FOUND"],
    tc_unsupported: ["unsupported_contract", "UNSUPPORTED_CONTRACT"],
  }[kind];
  if (!m) return Object.assign(new Error(`STORAGE_UNAVAILABLE: UNEXPECTED ${kind}`),
    { code: "unavailable", taskControl: true });
  return Object.assign(new Error(m[1]), { code: m[0], taskControl: true });
};

class StubRepo {
  constructor(cfg, calls) {
    this.cfg = cfg; this.calls = calls; this.reads = 0;
  }
  read_request(id) {
    this.calls.push({ call: "read_request", id });
    return this.cfg.request ?? null;
  }
  read_task(id) {
    this.calls.push({ call: "read_task", id });
    this.reads += 1;
    if (this.reads > 1 && this.cfg.task2 != null) return this.cfg.task2;
    return this.cfg.task ?? null;
  }
  accept(id, command, work, run) {
    this.calls.push({ call: "accept", id, run, command, work: workJson(work) });
    const c = this.cfg.accept ?? { receipt: {} };
    if (c.err) throw KIND(c.err.kind, c.err.message ?? "");
    return {
      record: { request_id: id, command, outcome: { Terminal: { accepted: true } } },
      reused: false,
    };
  }
  stop_receipt(id) {
    this.calls.push({ call: "stop_receipt", id });
    const r = this.cfg.receipt;
    if (r?.err) throw KIND(r.err.kind, r.err.message ?? "");
    return r ?? null;
  }
  bind_existing_stop(_plan) {
    this.calls.push({ call: "bind_existing_stop" });
    const c = this.cfg.bind ?? { ok: {} };
    if (c.err) throw KIND(c.err.kind, c.err.message ?? "");
    return {
      record: {
        request_id: "bound",
        command: { key: M.commandKey(), intent: { target_task_id: "?" } },
        outcome: { Terminal: { kind: "already_verified", receipt: "stub" } },
      },
      reused: false,
    };
  }
  never_started_facts(id) {
    this.calls.push({ call: "never_started_facts", id });
    const f = this.cfg.never_facts ?? null;
    if (f?.err) throw KIND(f.err.kind, f.err.message ?? "");
    return { snapshot: f?.snapshot ?? null,
      has_durable_effects: f?.has_durable_effects ?? false };
  }
  determinate_facts(id) {
    this.calls.push({ call: "determinate_facts", id });
    const f = this.cfg.det_facts ?? null;
    if (f?.err) throw KIND(f.err.kind, f.err.message ?? "");
    return { snapshot: f?.snapshot ?? null,
      started_items: f?.started_items ?? [],
      committed_items: f?.committed_items ?? [],
      has_durable_effects: f?.has_durable_effects ?? false };
  }
}

// Scripted inspector (prepare vectors).
class Scripted {
  constructor(obs, calls) { this.obs = obs; this.calls = calls; }
  inspect(_t) {
    this.calls.push({ call: "inspect" });
    const v = this.obs ?? null;
    if (v?.err) throw KIND(v.err.kind, v.err.message ?? "");
    if (v?.kind === "insufficient") return DEC.insufficient(v.reason ?? "");
    if (v?.kind === "stopped") return DEC.stopped(v.snapshot_sha256 ?? "", v.evidence_ref ?? "");
    throw new Error("bad scripted observation");
  }
}

// Mini replica of task_control::service::TaskControl::accept (NM-084's
// full port lands separately; identical decision surface, same call
// order, repository.accept canned by scenario).
const serviceAccept = (repo, runId, id, key, payload) => {
  if (id.trim() === "") throw M.tcInvalid("empty request_id");
  const existing = repo.read_request(id);
  if (existing !== null && !eqAny(existing.command.key, key)) throw M.tcRequestConflict();
  if (!eqAny(key, M.commandKey())) throw M.tcUnsupported();
  const contract = new ADM.StopVerification(repo, runId);
  const command = { key, intent: contract.normalize(payload) };
  if (existing !== null) {
    const k = existing.outcome.Task ? existing.outcome.Task.task.key
      : existing.command.key;
    if (!eqAny(k, M.commandKey())) throw M.tcUnsupported();
    if (!eqAny(existing.command, command)) throw M.tcRequestConflict();
    return { record: existing, reused: true };
  }
  const work = contract.resolve(command.intent);
  M.validateWork(work);
  return repo.accept(id, command, work, runId, null);
};
const eqAny = (a, b) =>
  a === b || (typeof a === "object" && typeof b === "object" && a !== null &&
    b !== null && Object.keys(a).length === Object.keys(b).length &&
    Object.keys(a).every((k) => eqAny(a[k], b[k])));

// ------------------------------------------------------------- emission
const obsJson = (o) => o.kind === "insufficient"
  ? { Insufficient: { reason: o.reason } }
  : { Stopped: { snapshot_sha256: o.snapshot_sha256, evidence_ref: o.evidence_ref } };
const workJson = (w) => ({
  items: w.items, scopes: [...w.scopes],
  ...(w.shared_evidence != null ? { shared_evidence: w.shared_evidence } : {}),
});

const runCase = (c) => {
  const calls = [];
  const repo = new StubRepo(c.repo ?? {}, calls);
  const out = (() => {
    switch (c.op) {
      case "verify_decode": {
        try { return { status: "ok", value: WIRE.tVerifyStopped(c.value) }; }
        catch (e) { return { status: "err", message: e.message }; }
      }
      case "admission_new": {
        try { new ADM.StopVerification(repo, c.run ?? ""); return { status: "ok" }; }
        catch (e) { return errRow(e); }
      }
      case "admission_normalize": {
        try {
          const sv = new ADM.StopVerification(repo, "R");
          return { status: "ok", normalized: sv.normalize(c.payload) };
        } catch (e) { return errRow(e); }
      }
      case "admission_resolve": {
        try {
          const sv = new ADM.StopVerification(repo, "R");
          return { status: "ok", work: workJson(sv.resolve(c.intent)) };
        } catch (e) { return errRow(e); }
      }
      case "inspect": {
        const target = c.target;
        const which = c.inspector ?? "audited";
        const inspector = which === "unverified" ? new DEC.UnverifiedStopInspector()
          : which === "never_started" ? new NS.NeverStarted(repo)
          : which === "determinate" ? new DET.Determinate(repo)
          : new DET.Audited(repo);
        try { return { status: "ok", observation: obsJson(inspector.inspect(target)) }; }
        catch (e) { return errRow(e); }
      }
      case "prepare": {
        const which = c.inspector ?? "scripted";
        const inspector = which === "unverified" ? new DEC.UnverifiedStopInspector()
          : which === "audited" ? new DET.Audited(repo)
          : which === "never_started" ? new NS.NeverStarted(repo)
          : which === "determinate" ? new DET.Determinate(repo)
          : new Scripted(c.observation, calls);
        try {
          const plan = DEC.prepare(c.verification, c.target, c.run ?? "R",
            c.actor ?? "actor", c.now, inspector);
          return { status: "ok", receipt: plan.audit.receipt };
        } catch (e) { return errRow(e); }
      }
      case "validate_audit": {
        // The oracle's Recorded-inspector replay logs `inspect` when the
        // target's visible status is needs_verification (run = vb owner).
        const vb = c.audit?.verification_before;
        const tb = c.audit?.target_before;
        const run = vb?.task?.owner_run_id ?? "";
        if (tb && M.snapVisible(tb, run) === "needs_verification") {
          calls.push({ call: "inspect" });
        }
        try {
          DEC.validateAudit(c.audit);
          return { status: "ok" };
        } catch (e) { return errRow(e); }
      }
      case "existing_accept": {
        const control = {
          repository: repo, run_id: "R",
          accept: (id, key, payload) => serviceAccept(repo, "R", id, key, payload),
        };
        try {
          const acc = EX.accept(control, c.id ?? "req-1", c.target_id ?? "T");
          return { status: "ok", acceptance: { record: acc.record, reused: acc.reused } };
        } catch (e) { return errRow(e); }
      }
      default: throw new Error(`unknown op ${c.op}`);
    }
  })();
  return { ...out, calls };
};

// ------------------------------------------------------------- diff loop
const eq = (a, b) => {
  if (a === b) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  if (Array.isArray(a)) return a.length === b.length && a.every((x, i) => eq(x, b[i]));
  const ka = Object.keys(a).filter((k) => a[k] !== undefined);
  const kb = Object.keys(b).filter((k) => b[k] !== undefined);
  if (ka.length !== kb.length) return false;
  return ka.every((k) => k in b && eq(a[k], b[k]));
};

const scenarios = readFileSync(process.argv[2], "utf8").trim().split("\n")
  .map((l) => JSON.parse(l));
const golden = readFileSync(process.argv[3], "utf8").trim().split("\n")
  .map((l) => JSON.parse(l));
let fail = 0;
const fails = [];
const rows = [];
scenarios.forEach((c, i) => {
  let got;
  try { got = { seq: i, op: c.op, ...runCase(c) }; }
  catch (e) { got = { seq: i, op: c.op, status: "probe_panic", message: e.stack?.split("\n")[0] ?? String(e) }; }
  rows.push(got);
  const want = golden[i];
  if (!eq(got, want)) { fail++; fails.push({ i, op: c.op, got, want }); }
});
if (process.env.RESULTS_PATH) {
  writeFileSync(process.env.RESULTS_PATH,
    rows.map((r) => JSON.stringify(r)).join("\n") + "\n");
}
console.log(`NM-106 probe: ${scenarios.length - fail}/${scenarios.length} match, ${fail} diffs`);
for (const f of fails.slice(0, 20)) {
  console.log(`\n=== seq ${f.i} op ${f.op}`);
  console.log("GOT :", JSON.stringify(f.got).slice(0, 1600));
  console.log("WANT:", JSON.stringify(f.want).slice(0, 1600));
}
process.exit(fail === 0 ? 0 : 1);
