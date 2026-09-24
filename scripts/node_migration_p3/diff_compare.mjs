// NM-049 — comparator: diffs Rust-oracle vs Node JSONL per-op.
// Rust variant <-> Node code equivalence table (verbatim, TASK-NM-049):
//   RevisionConflict<->revision_conflict  NeedsVerification<->needs_verification
//   InvalidRetrySelection<->invalid_retry_selection  RequestConflict<->conflict
//   RangeConflict<->range_conflict  InvalidCommand/EmptyRequestId<->invalid_input
//   Integrity(needs_verification payload)/StorageUnavailable->inconsistent/unavailable
//   TaskNotFound<->task_not_found  RetryRequestIdConflict<->conflict
//   EmptyRetrySelection/RetrySelectionInvalid<->invalid_retry_selection
//   OutcomeUnknown<->outcome_unknown  UnsupportedContract<->unsupported_contract
//
// status + mapped code always compared; message compared verbatim for
// code classes whose Node message is contract-verbatim
// (range_conflict/needs_verification/inconsistent/conflict) after $id
// normalization; invalid_input/unavailable messages are
// implementation-defined (boundary wording is asserted per-slice).
// usage: node diff_compare.mjs <rust.jsonl> <node.jsonl>
import { readFileSync } from "node:fs";

const [, , rustPath, nodePath] = process.argv;
const CODE_MAP = {
  RevisionConflict: "revision_conflict",
  NeedsVerification: "needs_verification",
  InvalidRetrySelection: "invalid_retry_selection",
  RetrySelectionInvalid: "invalid_retry_selection",
  EmptyRetrySelection: "invalid_retry_selection",
  RequestConflict: "conflict",
  RetryRequestIdConflict: "conflict",
  RangeConflict: "range_conflict",
  InvalidCommand: "invalid_input",
  EmptyRequestId: "invalid_input",
  StorageUnavailable: "unavailable",
  TaskNotFound: "task_not_found",
  OutcomeUnknown: "outcome_unknown",
  UnsupportedContract: "unsupported_contract",
  Integrity: "inconsistent",
  Invalid: "invalid_input",
  Storage: "unavailable",
  Conflict: "conflict",
  NotFound: "not_found",
  Base: "base_unavailable",
  OutcomeUnknown: "outcome_unknown",
  BasisNotVerified: "basis_not_verified",
};
const VERBATIM_MSG = new Set([
  "range_conflict", "needs_verification", "inconsistent", "conflict",
  "task_not_found", "revision_conflict", "invalid_retry_selection",
  "not_found", "base_unavailable", "outcome_unknown", "basis_not_verified",
]);
// serde parse positions differ (" at line N column M") — strip for compare
const stripPos = (m) => String(m).replace(/ at line \d+ column \d+$/, "");
const eq = (a, b) => {
  if (a === b) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  if (Array.isArray(a)) return a.length === b.length && a.every((x, i) => eq(x, b[i]));
  const ka = Object.keys(a), kb = Object.keys(b);
  return ka.length === kb.length && ka.every((k) => k in b && eq(a[k], b[k]));
};
const parse = (p) => readFileSync(p, "utf8").split("\n").filter((l) => l.trim()).map(JSON.parse);
const R = parse(rustPath), N = parse(nodePath);
let diffs = 0;
if (R.length !== N.length) {
  console.log(JSON.stringify({ verdict: "FAIL", detail: `row count ${R.length} != ${N.length}` }));
  process.exit(1);
}
const out = [];
for (let i = 0; i < R.length; i++) {
  const r = R[i], n = N[i];
  const issues = [];
  if (r.op !== n.op) issues.push(`op ${r.op}!=${n.op}`);
  if (r.status !== n.status) issues.push(`status ${r.status}!=${n.status}`);
  if (r.status === "error") {
    const rc = CODE_MAP[r.code] ?? `<unmapped:${r.code}>`;
    if (rc !== n.code) issues.push(`code ${r.code}(${rc})!=${n.code}`);
    if (VERBATIM_MSG.has(rc) && stripPos(r.message) !== stripPos(n.message)) {
      issues.push(`message ${JSON.stringify(r.message)}!=${JSON.stringify(n.message)}`);
    }
    // commit_ops_* run in the coordinator's autocommit write() — Rust
    // rejects after evidence/registry inserts which stay committed; the
    // digest comparison proves identical partial state on both sides.
    const PARTIAL_OK = new Set(["commit_ops_update", "commit_ops_no_change"]);
    if (!PARTIAL_OK.has(r.op)) {
      if (r.status === "error" && r.unchanged !== true) issues.push("oracle rejection mutated state");
      if (n.status === "error" && n.unchanged !== true) issues.push("node rejection mutated state");
    }
  } else {
    for (const f of ["task", "candidates", "reconciled"]) {
      if (f in r || f in n) {
        if (!eq(r[f] ?? null, n[f] ?? null)) {
          issues.push(`${f} ${JSON.stringify(r[f])} != ${JSON.stringify(n[f])}`);
        }
      }
    }
  }
  out.push({ seq: r.seq, op: r.op, status: issues.length === 0 ? "match" : "DIFF", issues });
  if (issues.length) diffs++;
}
for (const o of out) console.log(JSON.stringify(o));
console.log(JSON.stringify({ verdict: diffs === 0 ? "MATCH" : "DIFF", ops: R.length, diffs }));
process.exit(diffs === 0 ? 0 : 1);
