// NM-106 — scenario generator for the task_verification decision probe.
// Emits scenario JSONL on stdout. Fixtures are canonical serde-shaped
// TaskSnapshot objects; digests inside evidence markers are computed via
// the ported snapshotDigest so both sides derive identical bytes.
import { createHash } from "node:crypto";
import { serSnap } from "../../src_node/task_verification/mod.mjs";


const sha = (s) => createHash("sha256").update(s).digest("hex");
const digestOf = (snap) => sha(serSnap(snap));

const cases = [];
const c = (x) => cases.push(x);

// ------------------------------------------------------------- fixtures
const TS = "2024-01-01T00:00:00Z";
const key = (over = {}) => ({
  operation: "update_data", dataset: "market.daily",
  contract_version: "explicit-range-task-v1", ...over,
});
const spec = (item_id, over = {}) => ({
  item_id, target: { t: 1 }, progress_unit: "security_period",
  frozen_inputs: {}, scope_keys: [`["market","sh.600000","day"]`], ...over,
});
const item = (over = {}) => ({ spec: spec("i1"), stage: "pending", outcome: null, ...over });
// canonical ItemOutcome: serde always emits `error`; `evidence` skipped when null
const outc = (success, error = null, evidence = null) => ({
  success, error, ...(evidence !== null ? { evidence } : {}) });
const task = (over = {}) => ({
  task_id: "T", key: key(), status: "pending", state_revision: 0,
  accepted_run_id: "R", owner_run_id: null, created_at: TS,
  last_progress_at: TS,
  work: { items: [], scopes: [], shared_evidence: null },
  result: null, retry_of: null, ...over,
});
const snap = (over = {}) => ({
  task: task(), items: [item()], claims: [], ...over,
});

// A verification task snapshot owned by run R, item running.
const vtask = (targetId, over = {}) => task({
  task_id: "V",
  key: { operation: "verify_stopped_writes", dataset: "task.verification",
    contract_version: "stopped-writes-task-v1" },
  status: "running", state_revision: 3, accepted_run_id: "R",
  owner_run_id: "R",
  work: { items: [], scopes: [`verification:task:${targetId}`],
    shared_evidence: null },
  ...over,
});
const vitem = (targetId, accepted, over = {}) => ({
  spec: {
    item_id: "stopped-writes", target: { task_id: targetId },
    progress_unit: "evidence_check",
    frozen_inputs: {
      target_task_id: targetId, admission_revision: 0,
      admission_snapshot_sha256: "x",
      accepted_run_id: accepted, owner_run_id: null,
    },
    scope_keys: [`verification:task:${targetId}`],
  },
  stage: "running", outcome: null, ...over,
});
const vsnap = (targetId, accepted, over = {}) => snap({
  task: vtask(targetId, over),
  items: [vitem(targetId, accepted)],
  claims: [{ scope_key: `verification:task:${targetId}`, task_id: "V",
    state_revision: 3 }],
});

// A target that reads as needs_verification for run R.
const nvTarget = (over = {}) => snap({
  task: task({ status: "running", state_revision: 2,
    accepted_run_id: "OLD", owner_run_id: "OLD", ...over }),
});

// ---------------------------------------------------------- verify_decode
const VD = (value) => c({ op: "verify_decode", value });
VD({ contract_version: "v1", request_id: "r1", target_task_id: "T" });
VD({ contract_version: "v1", request_id: "r1" });                     // missing
VD({ contract_version: "v1", request_id: "r1", target_task_id: "T", x: 1 }); // unknown
VD({ contract_version: "v2", request_id: "r1", target_task_id: "T" });       // bad variant
VD({ contract_version: "v1", request_id: 5, target_task_id: "T" });
VD(null); VD("v1"); VD(["v1", "r1", "T"]);                            // seq form
VD({}); VD({ target_task_id: "T", request_id: "r1", contract_version: "v1" }); // reorder
VD({ contract_version: { v1: null }, request_id: "r1", target_task_id: "T" }); // map form
VD({ contract_version: "v1", request_id: "r1", target_task_id: "T" },
  ); // dup line guard
VD(["v1", "r1"]);                                                     // seq short

// ------------------------------------------------------- admission_new
for (const run of ["R", "", "  ", " R "]) c({ op: "admission_new", run });

// --------------------------------------------------- admission_normalize
const AN = (payload) => c({ op: "admission_normalize", payload });
AN({ target_task_id: "T" });
AN({ target_task_id: "" }); AN({ target_task_id: "  " });
AN({}); AN({ target_task_id: "T", extra: 1 });
AN({ target_task_id: 42 }); AN(null); AN("x");
AN(["T"]);

// ----------------------------------------------------- admission_resolve
const AR = (intent, repo = {}) =>
  c({ op: "admission_resolve", intent, repo });
AR({ target_task_id: "T" }, { task: nvTarget() });                    // ok
AR({ target_task_id: "T" }, { task: snap() });                        // pending+R -> not NV
AR({ target_task_id: "T" }, { task: null });                          // missing
AR({ target_task_id: "T" }, { task: snap({ task: task({ key: {
  operation: "verify_stopped_writes", dataset: "task.verification",
  contract_version: "stopped-writes-task-v1" }, status: "running",
  state_revision: 1, accepted_run_id: "OLD", owner_run_id: "OLD" }) }) }); // verification dataset
AR({ target_task_id: "T" }, { task: snap({ task: task({
  status: "cancel_requested", state_revision: 4, accepted_run_id: "OLD",
  owner_run_id: null }) }) });                                        // cancel-requested NV
AR({ target_task_id: "T" }, { task: snap({ task: task({
  status: "running", state_revision: 5, accepted_run_id: "R",
  owner_run_id: "Q" }) }) });                                         // owner != run -> NV
AR({ target_task_id: "T" }, { task: snap({ task: task({
  status: "running", state_revision: 5, accepted_run_id: "R",
  owner_run_id: null }) }) });                                        // running+R -> not NV
AR({ target_task_id: "  " }, { task: nvTarget() });                   // resolve decodes raw
AR({ nope: 1 }, { task: nvTarget() });
AR({ target_task_id: "T" }, {});                                      // task absent -> not_found

// -------------------------------------------------------------- inspect
const IN = (inspector, target, repo = {}) =>
  c({ op: "inspect", inspector, target, repo });
// never_started
IN("never_started", snap(), {
  never_facts: { snapshot: snap(), has_durable_effects: false } });
IN("never_started", snap(), {
  never_facts: { snapshot: snap(), has_durable_effects: false } });     // stopped
IN("never_started", snap(), {
  never_facts: { snapshot: snap({ task: task({ state_revision: 9 }) }),
    has_durable_effects: false } });                                  // snapshot moved -> conflict
IN("never_started", snap(), {
  never_facts: { snapshot: snap(), has_durable_effects: true } });      // durable effects
IN("never_started", snap(), {
  never_facts: { err: { kind: "tc_unavailable", message: "disk gone" } } });
IN("never_started", snap({ items: [] }));                             // no items
IN("never_started", snap({ task: task({ status: "running" }) }));
IN("never_started", snap({ task: task({ owner_run_id: "R" }) }));
IN("never_started", snap({ task: task({ result: { x: 1 } }) }));
IN("never_started", snap({ task: task({ state_revision: 1 }) }));
IN("never_started", snap({ items: [item({ stage: "running" })] }));
IN("never_started", snap({ items: [item({ outcome: outc(true) })] }));
IN("never_started", snap({ task: task({ key: key({
  operation: "collect_data" }) }) }));                                // unsupported op
IN("never_started", snap({ task: task({ key: key({
  operation: "start_collection", dataset: "legacy_market_compound",
  contract_version: "task-acceptance-v1" }) }) }), {
  never_facts: { snapshot: snap({ task: task({ key: key({
    operation: "start_collection", dataset: "legacy_market_compound",
    contract_version: "task-acceptance-v1" }) }) }),
    has_durable_effects: false } });                                  // legacy key ok
IN("never_started", snap({ task: task({ key: key({
  operation: "start_collection", dataset: "market.yearly",
  contract_version: "v1" }) }) }), {
  never_facts: { snapshot: snap({ task: task({ key: key({
    operation: "start_collection", dataset: "market.yearly",
    contract_version: "v1" }) }) }), has_durable_effects: false } });  // v1 collect
IN("never_started", snap({ task: task({ key: key({
  operation: "start_collection", dataset: "market.daily",
  contract_version: "v1" }) }) }), {
  never_facts: { snapshot: snap({ task: task({ key: key({
    operation: "start_collection", dataset: "market.daily",
    contract_version: "v1" }) }) }), has_durable_effects: false } });
IN("never_started", snap({ task: task({ key: key({
  operation: "start_collection", dataset: "bad.ds",
  contract_version: "v1" }) }) }));                                   // v1 bad dataset
IN("never_started", snap({ task: task({ key: key({
  dataset: "market.yearly" }) }) }), {
  never_facts: { snapshot: snap({ task: task({ key: key({
    dataset: "market.yearly" }) }) }), has_durable_effects: false } });// yearly contract
IN("never_started", snap({ task: task({ key: key({
  operation: "republish_data" }) }) }));                              // republish unsupported
// determinate
const DET = (target, facts, repo = {}) =>
  IN("determinate", target, { det_facts: facts, ...repo });
const dFacts = (target, over = {}) => ({
  snapshot: target, started_items: [], committed_items: [],
  has_durable_effects: false, ...over });
DET(snap(), dFacts(snap()));                                          // all pending -> stopped
DET(snap({ task: task({ key: key({ operation: "collect_data" }) }) }));// unsupported
DET(snap({ items: [] }));                                             // empty items
DET(snap({ items: [item({ stage: "finished", outcome: null })] }),
    dFacts(snap({ items: [item({ stage: "finished", outcome: null })] }),
      { started_items: ["i1"] }));                                    // finished no outcome
DET(snap({ items: [item({ stage: "finished",
      outcome: outc(true) })] }),
    dFacts(snap({ items: [item({ stage: "finished",
      outcome: outc(true) })] })));                            // finished, not started
DET(snap({ items: [item({ stage: "finished",
      outcome: outc(true) })] }),
    dFacts(snap({ items: [item({ stage: "finished",
      outcome: outc(true) })] }), { started_items: ["i1"] })); // finished+started ok
DET(snap(), dFacts(snap(), { started_items: ["i1"] }));               // pending but started
DET(snap({ items: [item({ stage: "running" })] }),
    dFacts(snap({ items: [item({ stage: "running" })] })));           // running, no start marker
DET(snap({ items: [item({ stage: "running" })] }),
    dFacts(snap({ items: [item({ stage: "running" })] }),
      { started_items: ["i1"], committed_items: ["i1"] }));           // landed
DET(snap({ items: [item({ stage: "running" })] }),
    dFacts(snap({ items: [item({ stage: "running" })] }),
      { started_items: ["i1"] }));                                    // uncommitted
DET(snap(), dFacts(snap(), { has_durable_effects: true }));           // effects, no writes
DET(snap({ items: [item({ stage: "finished",
      outcome: outc(true) })] }),
    dFacts(snap({ items: [item({ stage: "finished",
      outcome: outc(true) })] }), { started_items: ["i1"],
      has_durable_effects: true }));                                  // landed + effects ok
DET(snap(), dFacts(snap({ task: task({ state_revision: 7 }) })));     // facts mismatch
DET(snap(), { err: { kind: "tc_unavailable", message: "x" } });       // repo err
DET(snap({ task: task({ key: key({ operation: "republish_data",
  contract_version: "explicit-republish-task-v1" }) }) }),
    dFacts(snap({ task: task({ key: key({ operation: "republish_data",
      contract_version: "explicit-republish-task-v1" }) }) })));      // republish supported
DET(snap({ items: [item({ stage: "running" }),
      item({ spec: spec("i2"), stage: "finished",
        outcome: outc(true) }),
      item({ spec: spec("i3") })] }),
    dFacts(snap({ items: [item({ stage: "running" }),
      item({ spec: spec("i2"), stage: "finished",
        outcome: outc(true) }), item({ spec: spec("i3") })] }),
      { started_items: ["i1", "i2"], committed_items: ["i1"] }));     // mixed counts
// audited chain
IN("audited", snap(), {
  never_facts: { snapshot: snap(), has_durable_effects: false } });   // first proof wins
IN("audited", snap({ task: task({ status: "running", state_revision: 2 }) }), {
  det_facts: dFacts(snap({ task: task({ status: "running",
    state_revision: 2 }) }), { started_items: [] }) });               // falls through
IN("audited", snap({ task: task({ status: "running", state_revision: 2 }),
    items: [item({ stage: "running" })] }), {
  det_facts: dFacts(snap({ task: task({ status: "running",
    state_revision: 2 }), items: [item({ stage: "running" })] }),
    { started_items: ["i1"], committed_items: ["i1"] }) });           // landed via determinate
IN("unverified", nvTarget());                                          // unverified inspector

// -------------------------------------------------------------- prepare
const PR = (over = {}) => c({ op: "prepare",
  verification: vsnap("T", "OLD"),
  target: nvTarget(),
  run: "R", actor: "actor", now: "2024-02-01T00:00:00Z",
  inspector: "scripted",
  observation: { kind: "insufficient",
    reason: "STOP_EVIDENCE_NOT_VERIFIED: no installed writer/effect evidence protocol" },
  ...over });
PR({});
PR({ observation: { kind: "stopped",
    snapshot_sha256: digestOf(nvTarget()), evidence_ref: "ev-1" } });
PR({ observation: { kind: "stopped",
    snapshot_sha256: "bad", evidence_ref: "ev-1" } });                // digest mismatch
PR({ observation: { kind: "stopped",
    snapshot_sha256: digestOf(nvTarget()), evidence_ref: "  " } });   // empty ref
PR({ observation: { kind: "insufficient", reason: "  " } });          // empty reason
PR({ observation: { kind: "insufficient", reason: "" } });
PR({ observation: { err: { kind: "tc_unavailable", message: "probe died" } } });
PR({ observation: { err: { kind: "tc_conflict" } } });
// no-observation arms: target not NV
PR({ target: snap({ task: task({ status: "interrupted",
    accepted_run_id: "OLD", owner_run_id: null, state_revision: 4 }) }) });
PR({ target: snap() });                                                // pending -> not_applicable
PR({ target: snap({ task: task({ status: "succeeded",
    accepted_run_id: "OLD", owner_run_id: null }) }) });
// gate violations
for (const [k, v] of [
  ["dataset", "other.ds"], ["operation", "noop"],
  ["contract_version", "vX"],
]) {
  PR({ verification: vsnap("T", "OLD", { key: { ...key(), [k]: v } }) });
}
PR({ verification: vsnap("T", "OLD", { work: { items: [], scopes: [],
    shared_evidence: null } }) });                                     // scopes mismatch
PR({ verification: vsnap("T", "OLD", { owner_run_id: null }) });      // owner != run
PR({ verification: vsnap("T", "OLD", { owner_run_id: "Q" }) });
PR({ verification: vsnap("T", "OLD", { status: "pending",
    owner_run_id: "R" }) });                                          // vis != running
PR({ verification: snap({ task: vtask("T"), items: [] }) });          // items 0
PR({ verification: snap({ task: vtask("T"),
    items: [vitem("T", "OLD"), vitem("T", "OLD")] }) });              // items 2
PR({ verification: snap({ task: vtask("T"),
    items: [vitem("T", "OLD", { stage: "pending" })] }) });           // stage
PR({ verification: snap({ task: vtask("T"),
    items: [vitem("T", "OLD", { spec: { ...vitem("T","OLD").spec,
      item_id: "other" } })] }) });                                   // item_id
PR({ verification: snap({ task: vtask("T"),
    items: [vitem("T", "OLD", { spec: { ...vitem("T","OLD").spec,
      progress_unit: "x" } })] }) });                                 // progress_unit
PR({ verification: snap({ task: vtask("T"),
    items: [vitem("T", "OLD", { spec: { ...vitem("T","OLD").spec,
      scope_keys: ["other"] } })] }) });                              // scope_keys
PR({ verification: snap({ task: vtask("T"),
    items: [vitem("T", "NEW", { })] }) });                            // accepted_run_id mismatch
PR({ verification: snap({ task: vtask("X"),
    items: [vitem("X", "OLD")] }) });                                 // target_task_id mismatch
PR({ target: snap({ task: task({ key: {
    operation: "verify_stopped_writes", dataset: "task.verification",
    contract_version: "stopped-writes-task-v1" }, status: "running",
    state_revision: 2, accepted_run_id: "OLD", owner_run_id: "OLD" }) }) }); // target is verification task
PR({ actor: "" }); PR({ actor: " " }); PR({ run: "" });
PR({ inspector: "unverified" });
PR({ inspector: "audited", repo: { never_facts: { snapshot: nvTarget(),
    has_durable_effects: false },
    det_facts: dFacts(nvTarget()) } });                               // real chain in prepare
PR({ inspector: "audited", repo: { det_facts: dFacts(nvTarget(), {
    started_items: [], committed_items: [] }) } });                   // audited->determinate

// ------------------------------------------------------- validate_audit
// Inputs carry the audit JSON; the oracle replays prepare via a Recorded
// inspector and compares the receipt projection. Audits below replicate
// the Rust audit assembly (verification_after = succeeded+rev+1+item
// finished+result receipt; target_after = interrupted transform or null).
const NOW = "2024-02-01T00:00:00Z";
const clone = (o) => JSON.parse(JSON.stringify(o));
const mkAudit = (ver, tgt, receipt, evidence_ref, after = null) => {
  const va = clone(ver);
  delete va.task.work.shared_evidence;   // serde clone drops skip_none keys
  va.task.status = "succeeded";
  va.task.state_revision += 1;
  va.task.last_progress_at = NOW;
  va.claims = [];
  va.items[0].stage = "finished";
  va.items[0].outcome = { success: true, error: null };
  va.task.result = { stopped_write_check: receipt };
  return {
    actor: "actor", checked_at: NOW,
    target_before: tgt, target_after: after,
    verification_before: ver, verification_after: va,
    receipt, evidence_ref,
  };
};
const interruptedAfter = (tgt, receipt, verId = "V") => {
  const t = clone(tgt);
  delete t.task.work.shared_evidence;
  t.task.status = "interrupted";
  for (const i of t.items) if (i.stage === "running") i.stage = "pending";
  t.task.result = {
    interruption: { verification_task_id: verId, evidence_ref: "ev-1" },
    unfinished_items: t.items.filter((i) => !(i.outcome && i.outcome.success === true))
      .map((i) => i.spec.item_id),
  };
  t.task.state_revision += 1;
  t.task.last_progress_at = NOW;
  t.claims = [];
  t.task.result.stopped_write_check = receipt;
  return t;
};
const VA = (audit) => c({ op: "validate_audit", audit });
{
  const ver = vsnap("T", "OLD");
  const tgt = nvTarget();
  const receipt = {
    verification_task_id: "V", target_task_id: "T",
    conclusion: "insufficient_evidence", verification_revision: 4,
    target_revision: 2,
    reason: "STOP_EVIDENCE_NOT_VERIFIED: no installed writer/effect evidence protocol",
  };
  VA(mkAudit(ver, tgt, receipt, null));                                // valid
  VA(mkAudit(ver, tgt, { ...receipt, reason: "tampered" }, null));     // receipt tamper
  VA(mkAudit(ver, tgt, { ...receipt, conclusion: "interrupted" }, null));
  VA(mkAudit(ver, tgt, { ...receipt, verification_revision: 9 }, null));
  VA(mkAudit(ver, tgt, receipt, "unexpected-evidence"));               // evidence_ref flip
  VA(mkAudit(ver, { ...tgt, task: { ...tgt.task, state_revision: 5 } },
      receipt, null));                                                // target_before tamper
}
// validate_audit — stopped conclusion variant
{
  const ver = vsnap("T", "OLD");
  const tgt = nvTarget();
  const receipt = {
    verification_task_id: "V", target_task_id: "T",
    conclusion: "interrupted", verification_revision: 4,
    target_revision: 3,
    reason: "verified stopped; original task not resumed",
  };
  VA(mkAudit(ver, tgt, receipt, "ev-1", interruptedAfter(tgt, receipt)));
  VA(mkAudit(ver, tgt, { ...receipt, target_revision: 2 }, "ev-1",
      interruptedAfter(tgt, { ...receipt, target_revision: 2 })));    // wrong rev
  VA(mkAudit(ver, tgt, receipt, null, interruptedAfter(tgt, receipt))); // ref missing
}

// -------------------------------------------------------- existing_accept
const EA = (id, target_id, repo = {}) =>
  c({ op: "existing_accept", id, target_id, repo });
const interruptedTarget = (receiptFields = {}, over = {}) => snap({
  task: task({ status: "interrupted", state_revision: 7,
    accepted_run_id: "OLD", owner_run_id: null,
    result: { interruption: { verification_task_id: "V1",
      evidence_ref: "ev" }, unfinished_items: [] }, ...over }),
});
const goodReceipt = (over = {}) => ({
  verification_task_id: "V1", target_task_id: "T",
  conclusion: "interrupted", verification_revision: 2,
  target_revision: 7, reason: "verified stopped; original task not resumed",
  ...over,
});
EA("", "T");                                                          // empty id
EA("r1", "T", { request: { request_id: "r1",
    command: { key: { operation: "verify_stopped_writes",
      dataset: "task.verification", contract_version: "stopped-writes-task-v1" },
      intent: { target_task_id: "T" } },
    outcome: { Task: { task: task(), items: [], claims: [] } } } }); // replay -> accept
EA("r1", "T", { task: null });                                       // target missing
EA("r1", "T", { task: interruptedTarget(),
    receipt: goodReceipt() });                                       // existing path -> bind
EA("r1", "T", { task: snap({ task: task({ status: "interrupted",
      state_revision: 7, accepted_run_id: "OLD", owner_run_id: null,
      result: null }) }), receipt: goodReceipt() });                  // link missing
EA("r1", "T", { task: interruptedTarget(), receipt: null });         // receipt missing
EA("r1", "T", { task: interruptedTarget(),
    receipt: { err: { kind: "tc_unavailable", message: "disk gone" } } }); // receipt read err
EA("r1", "T", { task: interruptedTarget(),
    receipt: goodReceipt({ conclusion: "insufficient_evidence" }) });
EA("r1", "T", { task: interruptedTarget(),
    receipt: goodReceipt({ target_task_id: "X" }) });
EA("r1", "T", { task: interruptedTarget(),
    receipt: goodReceipt({ target_revision: 9 }) });
EA("r1", "T", { task: interruptedTarget(),
    receipt: goodReceipt(), bind: { err: { kind: "tc_unavailable",
      message: "disk gone" } } });                                   // bind seam err
EA("r1", "T", { task: nvTarget(),
    accept: { receipt: {} } });                                      // normal accept ok
EA("r1", "T", { task: nvTarget(),
    accept: { err: { kind: "tc_conflict" } } });                     // non-invalid -> no recheck
EA("r1", "T", { task: nvTarget(),
    task2: interruptedTarget(),
    accept: { err: { kind: "tc_invalid", message: "x" } },
    receipt: goodReceipt() });                                       // bounded recheck -> existing
EA("r1", "T", { task: nvTarget(),
    task2: nvTarget(),
    accept: { err: { kind: "tc_invalid", message: "x" } } });        // recheck not interrupted
EA("r1", "T", { task: nvTarget(),
    task2: null,
    accept: { err: { kind: "tc_invalid", message: "x" } } });        // recheck missing -> notfound
EA("  ", "T");                                                        // whitespace id
EA("r1", "", { task: nvTarget() });                                  // empty target -> normalize err
EA("r1", "T", { request: { request_id: "r1",
    command: { key: { operation: "other_op",
      dataset: "task.verification", contract_version: "stopped-writes-task-v1" },
      intent: { target_task_id: "T" } },
    outcome: { Terminal: { done: true } } } });                      // replay key mismatch -> conflict
EA("r1", "T", { request: { request_id: "r1",
    command: { key: { operation: "verify_stopped_writes",
      dataset: "task.verification", contract_version: "stopped-writes-task-v1" },
      intent: { target_task_id: "OTHER" } },
    outcome: { Terminal: { done: true } } } });                      // replay intent mismatch

process.stdout.write(cases.map((x) => JSON.stringify(x)).join("\n") + "\n");
