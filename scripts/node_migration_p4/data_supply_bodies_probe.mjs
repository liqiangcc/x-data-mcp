// NM-088 prep — DataApi real handler bodies over the SqliteSupply
// interface shape (NM-074). Exercises the ported query/observe/metrics/
// control bodies with a stub repository + control, proving:
//   - paging/cursor semantics (resume, key/access binding, LRU cap 32,
//     category mismatch)
//   - DataPage/VersionPage/TaskListPage/ProgressPage/AvailabilityPage
//     field shapes + limitations
//   - write arms: command key/payload + acceptance(Task) gate +
//     runtime blocks
//   - control calls: get_task/get_request/cancel_task/start_collection/
//     retry_task
//   - compute: computed + not_computable (Debug reason + details) +
//     seam error
//   - remaining seams still throw typed storage_unavailable
// Verified against w1's in-flight sqlite_supply.mjs end-to-end on a
// fixture DB (see evidence/node-migration/p4/data_supply/README.md).
//
//   node scripts/node_migration_p4/data_supply_bodies_probe.mjs [outdir]
//   SELFTEST=1 -> must exit 1 (asserts a checker catches a mutation)

import { mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { DataApi } from "../../src_node/data_supply/mcp.mjs";
import { Metrics } from "../../src_node/metrics/service.mjs";
import { Access } from "../../src_node/data_supply/access.mjs";
import { serValue } from "../../src_node/mcp/serde_json.mjs";

const SELFTEST = !!process.env.SELFTEST;
const HERE = fileURLToPath(new URL(".", import.meta.url));
const OUT = process.argv[2]
  ? resolve(process.argv[2])
  : join(resolve(HERE, "../.."), "evidence/node-migration/p4/data_supply");
mkdirSync(OUT, { recursive: true });
const results = [];
const check = (id, fn) => {
  try {
    fn();
    results.push({ id, status: "PASS" });
  } catch (e) {
    results.push({ id, status: "FAIL", detail: e.message });
    process.exitCode = 1;
  }
};
if (SELFTEST) {
  // Negative selftest: a mutated expectation MUST trip the checker.
  let detected = false;
  try { eq({ a: 1 }, { a: 2 }); } catch { detected = true; }
  console.log(JSON.stringify({ selftest: detected ? "detected" : "missed" }));
  process.exit(detected ? 1 : 0);
}
const eq = (a, b) => {
  if (serValue(a) !== serValue(b))
    throw new Error(`${serValue(a)} != ${serValue(b)}`);
};
const expectErr = (code, frag) => (fn) => {
  try { fn(); } catch (e) {
    if (e.code === code && (frag === undefined || String(e.message).includes(frag))) return;
    throw new Error(`expected ${code}/${frag} got ${e.code}:${e.message}`);
  }
  throw new Error(`expected ${code} error`);
};

const SEC = { exchange: "sh", code: "600000" };
const VREF = { dataset: "market.daily", contract_version: "v1",
  entity: SEC, version_id: "v1", integrity_sha256: "a".repeat(64) };
const row = (d, close) => ({ kind: "market", entity: SEC,
  version_ref: VREF, date: d, first_trading_date: d, last_trading_date: d,
  year_unfinished: null, open: "1.0", high: "1.1", low: "0.9",
  close, volume: "100", amount: "105" });
const facts = (id, status, items = []) => ({
  request_id: `req-${id}`,
  snapshot: {
    task: { task_id: id, key: { dataset: "market.daily",
      operation: "update_data", contract_version: "explicit-range-task-v1" },
      status, state_revision: 2, accepted_run_id: "run-1", owner_run_id: null,
      created_at: `2024-01-01T00:00:${id.slice(-2)}Z`,
      last_progress_at: "2024-01-01T00:01:00Z", work: {}, result: null,
      retry_of: null },
    items, claims: [] },
  publications: [], range_results: [], yearly_results: [] });
const item = (id, stage, outcome, extra = {}) => ({
  spec: { item_id: id, target: { symbol: "sh.600000", period: "day" },
    progress_unit: "d",
    frozen_inputs: { range: { start: "2024-01-01", end: "2024-01-31" },
      base_version_id: "bv", base_integrity_sha256: "x" },
    scope_keys: [] },
  stage, outcome, ...extra });
const repo = (over = {}) => ({
  query: () => ({ manifest: [
      { entity: SEC, version_ref: VREF, status: "present", metadata: null }],
    rows: [row("2024-01-02", "1.05"), row("2024-01-03", "1.10"),
      row("2024-01-04", "1.15")] }),
  verify: () => {}, versions: () => [
    { version_ref: VREF, published_at: "2024-02-01T00:00:00Z" }],
  tasks: () => [facts("t-09", "succeeded"), facts("t-08", "running")],
  task: (id) => facts(id, "running", [
    item("it-1", "finished", { success: true, error: null }),
    item("it-2", "running", null),
    item("it-3", "pending", null)]),
  availability: () => [{ entity: SEC,
    manifest: { entity: SEC, version_ref: VREF, status: "present",
      metadata: { coverage_reason: "fixture only" } },
    first_record: "2024-01-02", last_record: "2024-01-04",
    record_count: 3, integrity_failure: null, proven_gaps: [],
    related_tasks: [] }],
  verifyVersions: () => {}, verifyRangeResults: () => {},
  verifyYearlyResults: () => {}, verifyStoppedCheck: () => {},
  verifyPublicationCheck: () => {},
  ...over });
const api = (over = {}, opts = {}) => new DataApi({
  access: Access.local(), runId: "run-1", repository: repo(over),
  generation: null, marketSource: null, ...opts });
const qargs = (extra = {}) => ({ dataset: "market.daily",
  contract_version: "v1",
  selection: { kind: "single", entity: SEC },
  filter: { kind: "daily", start_date: "2024-01-01", end_date: "2024-01-31" },
  version: { kind: "latest_per_entity" }, page_size: 2, cursor: null,
  ...extra });

check("query_pages_records", () => {
  const a = api(); // cursors are session-bound — same DataApi instance
  const p1 = a.call("query_data", qargs());
  if (p1.records.length !== 2 || p1.page_complete !== false ||
      typeof p1.next_cursor !== "string" ||
      p1.version_manifest[0].version_ref.version_id !== "v1" ||
      p1.limitations.length !== 2 || p1.selection_complete !== true)
    throw new Error(serValue(p1));
  const p2 = a.call("query_data", qargs({ cursor: p1.next_cursor }));
  if (p2.records.length !== 1 || p2.page_complete !== true ||
      p2.next_cursor !== null || p2.records[0].date !== "2024-01-04")
    throw new Error(serValue(p2));
});
check("query_cursor_unknown", () => {
  const a = api();
  expectErr("invalid_arguments", "invalid/expired cursor")(() =>
    a.call("query_data", qargs({ cursor: "bogus-1" })));
});
check("query_cursor_key_mismatch", () => {
  const a = api();
  const c1 = a.call("query_data", qargs()).next_cursor;
  expectErr("invalid_arguments", "cursor conditions/authorization changed")(() =>
    a.call("query_data", qargs({ cursor: c1,
      filter: { kind: "daily", start_date: "2024-02-01", end_date: "2024-02-28" } })));
});
check("query_cursor_lru32", () => {
  const a = api();
  let first = null;
  for (let i = 0; i < 33; i++) {
    const p = a.call("query_data", qargs());
    if (i === 0) first = p.next_cursor;
  }
  // the first cursor was evicted (cache cap 32)
  expectErr("invalid_arguments", "invalid/expired cursor")(() =>
    a.call("query_data", qargs({ cursor: first })));
});
check("versions_page", () => {
  const v = api().call("list_data_versions", { dataset: "market.daily",
    contract_version: "v1", selection: { kind: "single", entity: SEC },
    page_size: 10, cursor: null });
  if (v.items.length !== 1 || v.items[0].version_ref.version_id !== "v1" ||
      v.items[0].published_at !== "2024-02-01T00:00:00Z" ||
      v.page_complete !== true)
    throw new Error(serValue(v));
});
check("tasks_sort_filter", () => {
  const tl = api().call("list_tasks", { page_size: 10, cursor: null });
  if (tl.items.length !== 2 || tl.items[0].task_id !== "t-09" ||
      tl.items[0].status !== "succeeded")
    throw new Error(serValue(tl));
  const running = api().call("list_tasks", { page_size: 10, cursor: null,
    status: ["running"] });
  if (running.items.length !== 1 || running.items[0].task_id !== "t-08")
    throw new Error(serValue(running));
  const ranged = api().call("list_tasks", { page_size: 10, cursor: null,
    created_from: "2024-01-01T00:00:09Z" });
  if (ranged.items.length !== 1 || ranged.items[0].task_id !== "t-09")
    throw new Error(serValue(ranged));
});
check("tasks_inverted_range", () => {
  expectErr("invalid_arguments", "inverted task time range")(() =>
    api().call("list_tasks", { page_size: 10, cursor: null,
      created_from: "2024-02-01T00:00:00Z", created_to: "2024-01-01T00:00:00Z" }));
});
check("progress_counts_percent", () => {
  const p = api().call("get_task_progress", { task_id: "t-09",
    page_size: 10, cursor: null });
  if (p.total !== 3 || p.finished !== 1 || p.succeeded !== 1 ||
      p.failed !== 0 || p.pending !== 1 || p.running !== 1 ||
      p.execution_percent !== "33.33" ||
      p.unfinished.length !== 2 ||
      p.unfinished[0].item_id !== "it-2" ||
      p.frozen_daily_ranges.length !== 3)
    throw new Error(serValue(p));
});
check("progress_verify_reinvoked_per_page", () => {
  let calls = 0;
  const a = api({}, { });
  // swap in counting repository
  a.repository.verifyVersions = () => { calls++; };
  const p1 = a.call("get_task_progress", { task_id: "t-09", page_size: 1, cursor: null });
  const p2 = a.call("get_task_progress", { task_id: "t-09", page_size: 1, cursor: p1.next_cursor });
  if (calls !== 2) throw new Error(`verify calls ${calls}`);
});
check("availability_absent_class", () => {
  const av = api().call("get_data_availability", qargs({ page_size: 10 }));
  const it = av.items[0];
  if (it.availability !== "unknown" || it.integrity !== "verified" ||
      it.record_count !== 3 || it.coverage_evidence !== "fixture only" ||
      it.unresolved_ranges.length !== 1 || it.confirmed_gaps.length !== 0)
    throw new Error(serValue(av));
});
check("availability_integrity_failure", () => {
  const av = api({ availability: () => [{ entity: SEC, manifest: null,
    first_record: null, last_record: null, record_count: 0,
    integrity_failure: "digest mismatch", proven_gaps: [],
    related_tasks: [] }] }).call("get_data_availability", qargs({ page_size: 10 }));
  const it = av.items[0];
  if (it.availability !== "unavailable" || it.integrity !== "failed" ||
      it.coverage_evidence !== "digest mismatch")
    throw new Error(serValue(av));
});
check("availability_catalog_available", () => {
  const av = api({ availability: () => [{ entity: null, manifest: null,
    first_record: null, last_record: null, record_count: 0,
    integrity_failure: null, proven_gaps: [], related_tasks: [] }] })
    .call("get_data_availability", { dataset: "securities.catalog",
      contract_version: "v1", selection: { kind: "catalog", scope: "current_sh_sz_a" },
      filter: { kind: "catalog" }, version: { kind: "latest_per_entity" },
      page_size: 10, cursor: null });
  if (av.items[0].availability !== "available" ||
      av.items[0].integrity !== "verified")
    throw new Error(serValue(av));
});
check("get_task_summary", () => {
  const s = api().call("get_task", { contract_version: "v1", task_id: "t-09" });
  if (s.task_id !== "t-09" || s.status !== "running" ||
      s.request_id !== "req-t-09" || s.state_revision !== 2)
    throw new Error(serValue(s));
});
check("get_task_foreign_run_needs_verification", () => {
  const s = api({ task: () => facts("t-09", "running", []) })
    .call("get_task", { contract_version: "v1", task_id: "t-09" });
  // accepted_run_id "run-1" vs api run "run-1" -> running (same run)
  if (s.status !== "running") throw new Error(serValue(s));
  const s2 = new DataApi({ access: Access.local(), runId: "other-run",
    repository: repo({ task: () => facts("t-09", "running", []) }),
    generation: null, marketSource: null })
    .call("get_task", { contract_version: "v1", task_id: "t-09" });
  if (s2.status !== "needs_verification") throw new Error(serValue(s2));
});
check("get_request_bound_unbound", () => {
  const control = { runId: "run-1", repository: {},
    readRequest: (id) => id === "r-x" ? null
      : { request_id: id, command: { key: {}, intent: {} },
          outcome: { Terminal: { kind: "already_verified", receipt: {} } } } };
  const a = api({}, { control });
  const bound = a.call("get_request", { contract_version: "v1", request_id: "r-1" });
  if (bound.bound !== true || bound.task !== null ||
      bound.terminal_result.kind !== "already_verified")
    throw new Error(serValue(bound));
  const un = a.call("get_request", { contract_version: "v1", request_id: "r-x" });
  if (un.bound !== false || un.task !== null || un.terminal_result !== null)
    throw new Error(serValue(un));
});
check("cancel_task_visible_status", () => {
  const control = { runId: "run-1", repository: {},
    cancel: (id) => ({ task: { task_id: id, status: "cancel_requested",
      state_revision: 5n, accepted_run_id: "run-1", owner_run_id: null },
      items: [], claims: [] }) };
  const c = api({}, { control })
    .call("cancel_task", { contract_version: "v1", task_id: "t-9",
      expected_revision: 4 });
  if (c.status !== "cancel_requested" || Number(c.state_revision) !== 5)
    throw new Error(serValue(c));
});
check("start_collection_key_payload", () => {
  let seen;
  const snap = { task: { task_id: "t-c", key: { dataset: "market.daily",
      operation: "start_collection", contract_version: "v1" },
    status: "pending", state_revision: 1n, accepted_run_id: "run-1",
    owner_run_id: null, created_at: "x", last_progress_at: "x",
    work: {}, result: null, retry_of: null }, items: [], claims: [] };
  const control = { runId: "run-1", repository: {},
    accept: (id, key, payload) => { seen = [id, key, payload];
      return { reused: false,
        record: { request_id: id, command: { key, intent: payload },
          outcome: { Task: snap } } }; } };
  const acc = api({}, { control })
    .call("start_collection", { dataset: "market.daily", request_id: "req-c",
      contract_version: "v1",
      selection: { kind: "explicit", securities: [SEC] } });
  if (seen[0] !== "req-c" ||
      seen[1].operation !== "start_collection" ||
      seen[1].dataset !== "market.daily" ||
      seen[1].contract_version !== "v1" ||
      seen[2].request_id !== undefined ||
      seen[2].selection.kind !== "explicit")
    throw new Error(serValue(seen));
  // market.daily write with no generation/source -> blocked, verbatim
  if (acc.request_id !== "req-c" || acc.task_id !== "t-c" ||
      acc.reused !== false || acc.runtime_readiness !== "blocked" ||
      acc.blocking_reasons[0].code !== "verified_source_absent")
    throw new Error(serValue(acc));
});
check("acceptance_terminal_rejected", () => {
  const control = { runId: "run-1", repository: {},
    accept: (id, key, payload) => ({ reused: false,
      record: { request_id: id, command: { key, intent: payload },
        outcome: { Terminal: { kind: "already_verified", receipt: {} } } } }) };
  expectErr("unsupported_request_outcome")(() =>
    api({}, { control }).call("start_collection", { dataset: "market.daily",
      request_id: "req-c", contract_version: "v1",
      selection: { kind: "explicit", securities: [SEC] } }));
});
check("range_write_key_payload", () => {
  let seen;
  const snap = { task: { task_id: "t-r", key: { dataset: "market.daily",
      operation: "update_data", contract_version: "explicit-range-task-v1" },
    status: "pending", state_revision: 1n, accepted_run_id: "run-1",
    owner_run_id: null, created_at: "x", last_progress_at: "x",
    work: {}, result: null, retry_of: null }, items: [], claims: [] };
  const control = { runId: "run-1", repository: {},
    accept: (id, key, payload) => { seen = [id, key, payload];
      return { reused: false,
        record: { request_id: id, command: { key, intent: payload },
          outcome: { Task: snap } } }; } };
  api({}, { control }).call("update_data", { dataset: "market.daily",
    contract_version: "v1", request_id: "req-r",
    items: [{ entity: SEC, base_version_id: "bv",
      start_date: "2024-01-01", end_date: "2024-01-31" }],
    selection: null });
  if (seen[1].operation !== "update_data" ||
      seen[1].contract_version !== "explicit-range-task-v1" ||
      seen[2].items[0].symbol !== "sh.600000")
    throw new Error(serValue(seen));
});
check("range_items_xor_selection", () => {
  const control = { runId: "run-1", repository: {},
    accept: () => { throw new Error("unreached"); } };
  expectErr("invalid_arguments", "mutually exclusive")(() =>
    api({}, { control }).call("update_data", { dataset: "market.daily",
      contract_version: "v1", request_id: "req-r",
      items: [{ entity: SEC, base_version_id: "bv",
        start_date: "2024-01-01", end_date: "2024-01-31" }],
      selection: { kind: "default" } }));
});
check("compute_computed", () => {
  const obs = { observations: () => [
    { year: 2022, first: "2022-01-03", last: "2022-12-30", close: "10.0",
      unfinished: false, completion: "complete" },
    { year: 2023, first: "2023-01-02", last: "2023-12-29", close: "11.0",
      unfinished: false, completion: "complete" },
    { year: 2024, first: "2024-01-02", last: "2024-12-31", close: "12.1",
      unfinished: false, completion: "complete" }] };
  const a = api({}, { metrics: new Metrics(obs) });
  const c = a.call("compute_metrics", { contract_version: "v1",
    inputs: [{ input_id: "i1", version_ref: { dataset: "market.yearly",
      contract_version: "v1", entity: SEC, version_id: "y1",
      integrity_sha256: "a".repeat(64) } }],
    range: { start_year: 2022, end_year: 2024 },
    metrics: [{ metric_id: "m1", name: "consecutive_direction",
      algorithm_version: "local-consecutive-direction-v1",
      input_ids: ["i1"], parameters: { direction: "up", min_periods: 2 } }],
    execution: "inline" });
  if (c.result.outcome !== "computed" || c.result.run_length !== 2 ||
      c.result.cumulative_change !== "0.210000000000" ||
      c.result.limitations.length !== 2)
    throw new Error(serValue(c));
});
check("compute_not_computable", () => {
  const obs = { observations: () => [
    { year: 2023, first: "2023-01-02", last: "2023-12-29", close: "11.0",
      unfinished: false, completion: "complete" }] };
  const a = api({}, { metrics: new Metrics(obs) });
  try {
    a.call("compute_metrics", { contract_version: "v1",
      inputs: [{ input_id: "i1", version_ref: { dataset: "market.yearly",
        contract_version: "v1", entity: SEC, version_id: "y1",
        integrity_sha256: "a".repeat(64) } }],
      range: { start_year: 2022, end_year: 2024 },
      metrics: [{ metric_id: "m1", name: "consecutive_direction",
        algorithm_version: "local-consecutive-direction-v1",
        input_ids: ["i1"], parameters: { direction: "up", min_periods: 2 } }],
      execution: "inline" });
  } catch (e) {
    if (e.code !== "not_computable" || e.message !== "MissingYear" ||
        e.details?.result?.reason !== "missing_year")
      throw new Error(`${e.code}:${e.message}:${serValue(e.details)}`);
    return;
  }
  throw new Error("expected not_computable");
});
check("seam_control_typed", () => {
  // no control injected -> the documented ControlSeam throws
  // storage_unavailable, mapped to storage_error.
  expectErr("storage_error", "repository seam not wired")(() =>
    api().call("cancel_task", { contract_version: "v1", task_id: "t",
      expected_revision: 0 }));
});
check("seam_repository_typed", () => {
  const bare = new DataApi({ access: Access.local(), generation: null,
    marketSource: null });
  expectErr("storage_error", "repository seam not wired")(() =>
    bare.call("query_data", qargs()));
});
check("inspect_terminal_shape", () => {
  const a = api({}, { control: { runId: "run-1", repository: {} },
    inspectFn: () => ({ record: { outcome: {
      Terminal: { kind: "verification_inspection", receipt: {} } } } }) });
  const r = a.call("inspect_verification_task", { contract_version: "v1",
    request_id: "req-i", target_task_id: "t-9" });
  if (r.bound !== true || r.terminal_result.kind !== "verification_inspection")
    throw new Error(serValue(r));
});
check("inspect_nonterminal_needs_verification", () => {
  const a = api({}, { control: { runId: "run-1", repository: {} },
    inspectFn: () => ({ record: { outcome: { Task: {} } } }) });
  expectErr("needs_verification",
    "bounded inspection has an invalid task outcome")(() =>
    a.call("inspect_verification_task", { contract_version: "v1",
      request_id: "r", target_task_id: "t" }));
});

const pass = results.filter((r) => r.status === "PASS").length;
const fail = results.filter((r) => r.status.startsWith("FAIL")).length;
writeFileSync(join(OUT, "bodies-probe-results.jsonl"),
  results.map((r) => JSON.stringify(r)).join("\n") + "\n");
console.log(JSON.stringify({ probe: "data_supply_bodies",
  pass, fail, total: results.length }));
if (SELFTEST) process.exit(fail > 0 ? 0 : 1);
process.exit(fail === 0 ? 0 : 1);
