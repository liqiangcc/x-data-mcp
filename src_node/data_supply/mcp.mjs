// src/data_supply/mcp.rs — DataApi ToolHandler skeleton (S12a):
//   tools/list -> registry entries (access-filtered)
//   tools/call -> dispatch: dataset/contract prechecks -> entry lookup ->
//     handler match -> serde decode -> access gate -> repository seam.
// Repository calls land on `RepositorySeam` — every read/write throws the
// typed `storage_error` MARKET_STORAGE error instead of silently
// succeeding; SqliteSupply is S12c. `get_data_capabilities` is fully
// implemented (pure registry + access + env-presence facts).
//
// Error mapping parity (mcp.rs mapped/task_error + the call() overlay):
//   market_storage::Error -> forbidden/invalid_arguments/integrity_error/
//     version_not_found/precondition_conflict/outcome_unknown/storage_error
//   task_control::Error   -> task_not_found/unsupported_contract/
//     needs_verification/request_conflict/precondition_conflict/
//     range_conflict/retry_request_id_conflict/empty_request_id/
//     invalid_retry_selection/invalid_arguments/outcome_unknown/
//     storage_error
//   decode()              -> invalid_arguments with the serde text
//   call()                -> outcome_certainty overlay (unknown /
//     not_committed / not_applicable) on the generic tool surface.

import { createHash } from "node:crypto";
import { serValue } from "../mcp/serde_json.mjs";
import { ApiError } from "../mcp/transport.mjs";
import { Access, StorageError } from "./access.mjs";
import { Observer, summary } from "./observe.mjs";
import { MAX_BYTES, MAX_ROWS, Queries } from "./query.mjs";
import {
  CancelV1, CapabilitiesQuery, Collection, Decode,
  InspectVerification, ProgressQuery, Query, RangeWrite, RepublishWrite,
  RetryItems, TaskV1, RequestV1, TasksQuery, VerifyDataGap, VerifyV1,
  VersionsQuery, WorkerInput, securitySymbol,
  DATASET_CATALOG, DATASET_DAILY, DATASET_YEARLY, Dataset,
} from "./wire.mjs";
import { Compute } from "../metrics/wire.mjs";
import { Metrics } from "../metrics/service.mjs";
import { entries, capabilityExample, Handler } from "./registry.mjs";
import { visibleStatusOf } from "../storage/task_repository.mjs";
import { accept as pubAccept } from "../publication_verification/existing.mjs";

const DECLARED_SCOPE =
  "DECLARED_SCOPE: owner personal research, local-only, no external exposure (#71 comment-5749934485); overall production certification pending #225";
const GENERATION_ROOT_ENV = "X_DATA_MCP_GENERATION_ROOT";

export class TaskControlError extends Error {
  // task_control::Error Display subset reachable at the seam boundary.
  constructor(kind, text) {
    super(text);
    this.kind = kind;
  }
  toString() {
    const tag = {
      storage_unavailable: "STORAGE_UNAVAILABLE",
      task_not_found: "TASK_NOT_FOUND",
      unsupported_contract: "UNSUPPORTED_CONTRACT",
      needs_verification: "ACCEPTANCE_NEEDS_VERIFICATION",
      request_conflict: "REQUEST_CONFLICT",
      revision_conflict: "STATE_REVISION_CONFLICT",
      range_conflict: "RANGE_CONFLICT",
      retry_request_id_conflict: null,
      empty_retry_selection: "retry selected_scopes must not be empty",
      empty_request_id: "request_id must not be empty",
      invalid_retry_selection: "INVALID_RETRY_SELECTION",
      retry_selection_invalid: null,
      invalid_command: "INVALID_COMMAND",
      outcome_unknown: "OUTCOME_UNKNOWN",
    }[this.kind];
    if (this.kind === "retry_request_id_conflict")
      return "retry request_id must differ from the original task request_id";
    if (this.kind === "retry_selection_invalid")
      return "retry selected scopes are invalid, outside the original range, or already completed";
    if (this.kind === "empty_retry_selection" || this.kind === "empty_request_id")
      return this.message;
    if (this.kind === "outcome_unknown")
      return `${tag}: ${this.message}; query the same request_id before retrying`;
    if (this.kind === "range_conflict")
      return `${tag}: ${JSON.stringify(this.message)}`;
    if (this.kind === "needs_verification" || this.kind === "invalid_command" ||
        this.kind === "storage_unavailable")
      return `${tag}: ${this.message}`;
    return tag ?? this.message;
  }
}

// The documented repository seam: every storage-facing call fails closed
// with the market_storage/task_control StorageUnavailable text so an
// unwired call is a typed error, never a silent success.
const seamMsg = (op) => `repository seam not wired (S12c pending): ${op}`;
const seamErr = (op) => new TaskControlError("storage_unavailable", seamMsg(op));

// range.rs — RangeWrite::key/payload + RepublishWrite::key/payload.
// market_update::tasks Operation::key() contract versions.
const RANGE_CONTRACT_VERSION = "explicit-range-task-v1";
const RANGE_DEFAULT_CONTRACT_VERSION = "default-range-task-v1";
const REPUBLISH_CONTRACT_VERSION = "explicit-republish-task-v1";
const YEARLY_CONTRACT_VERSION = "explicit-yearly-task-v1";

const commandKey = (operation, command) => {
  const key = {
    operation, // "update_data" | "backfill_data" | "republish_data"
    dataset: "market.daily",
    contract_version: operation === "republish_data"
      ? REPUBLISH_CONTRACT_VERSION : RANGE_CONTRACT_VERSION,
  };
  if (command.dataset === "market.yearly") {
    key.dataset = "market.yearly";
    key.contract_version = YEARLY_CONTRACT_VERSION;
  } else if (command.selection !== null && command.selection !== undefined &&
             operation !== "republish_data") {
    key.contract_version = RANGE_DEFAULT_CONTRACT_VERSION;
  }
  return key;
};
const rangeWritePayload = (command) => {
  const invalidCommand = (m) => new TaskControlError("invalid_command", m);
  if (command.dataset === "market.daily") {
    const { items, selection } = command;
    if (items !== null && selection === null)
      return { items: items.map((i) => ({
        symbol: securitySymbol(i.entity),
        base_version_id: i.base_version_id,
        start_date: i.start_date,
        end_date: i.end_date,
      })) };
    if (items === null && selection !== null)
      return { selection: { kind: "default" } };
    if (items !== null && selection !== null)
      throw invalidCommand("items and selection are mutually exclusive");
    throw invalidCommand("items or selection required");
  }
  // market.yearly — ExplicitYears
  return { items: command.items.map((i) => ({
    symbol: securitySymbol(i.entity),
    base_daily_version_id: i.base_daily_version_id,
    years: [...new Set(i.years)].sort((a, b) => a - b),
    intent: i.publication.mode === "historical_fixed_base"
      ? "HistoricalFixedBase"
      : { CurrentHead: { expected_daily_head: i.base_daily_version_id,
                         expected_yearly_head: i.publication.expected_yearly_head } },
    comparison_version_id: i.publication.mode === "historical_fixed_base"
      ? i.publication.comparison_version_id : null,
  })) };
};
const republishPayload = (command) => ({
  items: command.items.map((i) => ({
    symbol: securitySymbol(i.entity),
    base_version_id: i.base_version_id,
    start_date: i.start_date,
    end_date: i.end_date,
  })),
});
// The documented repository seam (NM-074 SqliteSupply shape):
// ReadRepository + ObservationRepository. Every call fails closed with
// the typed storage error — an unwired call is never a silent success.
export class RepositorySeam {
  query() { throw new StorageError("storage", seamMsg("repository.query")); }
  verify() { throw new StorageError("storage", seamMsg("repository.verify")); }
  versions() { throw new StorageError("storage", seamMsg("repository.versions")); }
  tasks() { throw new StorageError("storage", seamMsg("repository.tasks")); }
  task() { throw new StorageError("storage", seamMsg("repository.task")); }
  availability() { throw new StorageError("storage", seamMsg("repository.availability")); }
  verifyVersions() { throw new StorageError("storage", seamMsg("repository.verify_versions")); }
  verifyRangeResults() { throw new StorageError("storage", seamMsg("repository.verify_range_results")); }
  verifyYearlyResults() { throw new StorageError("storage", seamMsg("repository.verify_yearly_results")); }
  verifyPublicationCheck() { throw new StorageError("storage", seamMsg("repository.verify_publication_check")); }
  verifyStoppedCheck() { throw new StorageError("storage", seamMsg("repository.verify_stopped_check")); }
  // metrics::service InputRepository::observations (SqliteMetrics)
  observations() { throw new StorageError("storage", seamMsg("metrics::observations")); }
}

// task_control::service::TaskControl-shaped seam (S6/S7): the service
// surface the handler bodies call — read_request/accept/cancel/retry
// plus `repository` + `runId` for the domain verifiers.
export class ControlSeam {
  constructor(runId) { this.runId = runId; this.repository = this; }
  readRequest() { throw seamErr("task_control::read_request"); }
  readTask() { throw seamErr("task_control::read_task"); }
  accept() { throw seamErr("task_control::accept"); }
  cancel() { throw seamErr("task_control::cancel"); }
  retry() { throw seamErr("task_control::retry"); }
  // task_control::TaskRepository — the domain-verifier surface the S6/S7
  // SqliteTasks binding satisfies (NM-056/NM-059 storage fns behind it).
  knownPublication() { throw seamErr("repository::known_publication"); }
  bindKnownPublication() { throw seamErr("repository::bind_known_publication"); }
  readFacts() { throw seamErr("repository::read_facts"); }
  checkReceipt() { throw seamErr("repository::check_receipt"); }
  commitCheck() { throw seamErr("repository::commit_check"); }
  transition() { throw seamErr("repository::transition"); }
  claim() { throw seamErr("repository::claim"); }
  candidates() { throw seamErr("repository::candidates"); }
}

// metrics Reason Debug format (mcp.rs `format!("{reason:?}")`).
const reasonDebug = (r) => ({
  window_too_short: "WindowTooShort",
  missing_year: "MissingYear",
  unknown_completion: "UnknownCompletion",
  incomplete_period: "IncompletePeriod",
  non_positive_price: "NonPositivePrice",
}[r] ?? String(r));

// GenerationSource::from_env + TdxMarketSource::from_env presence rules
// (generation.active_verified_id stays null until the generation store
// is ported — the env-set-unverified case still reports the generation
// variant of the source block, matching Rust's source_ready == false).
export function generationFromEnv(env = process.env) {
  return env[GENERATION_ROOT_ENV] ? { activeVerifiedId: () => null } : null;
}
export function marketSourceFromEnv(env = process.env, inspect) {
  // official_market::TdxMarketSource::from_env — env + passing inspect.
  if (!env.X_DATA_MCP_OFFICIAL_MANIFEST || !env.X_DATA_MCP_OFFICIAL_DIR) return null;
  try {
    const m = inspect(env.X_DATA_MCP_OFFICIAL_MANIFEST, env.X_DATA_MCP_OFFICIAL_DIR);
    return { manifest_sha256: m.manifest_sha256 };
  } catch {
    return null;
  }
}

const mapped = (e) => {
  if (e instanceof TaskControlError) return taskError(e);
  const code =
    e instanceof StorageError && e.kind === "invalid" && e.message === "forbidden" ? "forbidden" :
    e instanceof StorageError && e.kind === "invalid" ? "invalid_arguments" :
    e instanceof StorageError && (e.kind === "integrity" || e.kind === "base") ? "integrity_error" :
    e instanceof StorageError && e.kind === "notfound" ? "version_not_found" :
    e instanceof StorageError && e.kind === "conflict" ? "precondition_conflict" :
    e instanceof StorageError && e.kind === "outcome" ? "outcome_unknown" :
    "storage_error";
  return new ApiError(code, String(e));
};
const taskError = (e) => {
  const code = {
    task_not_found: "task_not_found",
    unsupported_contract: "unsupported_contract",
    needs_verification: "needs_verification",
    request_conflict: "request_conflict",
    revision_conflict: "precondition_conflict",
    range_conflict: "range_conflict",
    retry_request_id_conflict: "retry_request_id_conflict",
    empty_request_id: "empty_request_id",
    empty_retry_selection: "invalid_retry_selection",
    retry_selection_invalid: "invalid_retry_selection",
    invalid_retry_selection: "invalid_retry_selection",
    invalid_command: "invalid_arguments",
    outcome_unknown: "outcome_unknown",
  }[e.kind] ?? "storage_error";
  return new ApiError(code, String(e));
};
// RequestTerminal is untagged: Stopped/Publication are
// {kind:"already_verified", receipt}, Inspection is
// {kind:"verification_inspection", receipt}. Structural gate mirrors
// serde untagged decode; per-variant receipt field validation lands
// with the domain decoders (S6/S7) — stored terminal values are
// produced by our own commit paths and always well-formed.
const UNTAGGED_MISS =
  "data did not match any variant of untagged enum RequestTerminal";
const terminalShape = (v) =>
  v !== null && typeof v === "object" && !Array.isArray(v) &&
  (v.kind === "already_verified" || v.kind === "verification_inspection") &&
  v.receipt !== null && typeof v.receipt === "object";
const decodeRequestTerminal = (v) => {
  if (!terminalShape(v))
    throw new ApiError("unsupported_request_outcome",
      `terminal contract: ${UNTAGGED_MISS}`);
  return v;
};
const decodeInspectionTerminal = (v) => {
  if (!terminalShape(v))
    throw new ApiError("needs_verification",
      `inspection terminal contract: ${UNTAGGED_MISS}`);
  return v;
};
const decode = (dec, input) => {
  try {
    return dec(input);
  } catch (e) {
    if (e instanceof Decode) throw new ApiError("invalid_arguments", e.message);
    throw e;
  }
};
const encode = (v) => v; // serde to_value is infallible for these shapes

export class DataApi {
  // DataApi::new_in registry/tasks/market/access/market_source/
  //   generation_source + the seam split documented for the dep wall:
  //   repository  — SqliteSupply shape (NM-074): query/verify/versions/
  //                 tasks/task/availability + verify_* re-checks.
  //   control     — task_control::service::TaskControl shape (S6/S7):
  //                 {runId, repository, readRequest, accept, cancel, retry}.
  //   metrics     — Metrics over SqliteMetrics (metrics/sqlite_metrics.mjs);
  //                 defaults to the seam until the market backend is wired.
  //   inspectFn   — verification_inspection::service::inspect (S6/S7).
  //   stoppedVerify — task_verification::existing::accept (unported domain —
  //                 lands with its own task; seam until then).
  //   workerRun   — worker::run_once (S6/S7); workerGate = WorkersGate.
  //   pubVerify   — publication_verification::existing::accept (NM-098,
  //                 landed — real default).
  constructor({ access, runId, repository, control, metrics, generation,
                marketSource, workerRun, inspectFn, stoppedVerify,
                pubVerify, workerGate, market } = {}) {
    const repo = repository ?? new RepositorySeam();
    this.access = access ?? Access.local();
    this.generation = generation === undefined ? generationFromEnv() : generation;
    this.market_source = marketSource === undefined ? null : marketSource;
    this.repository = repo;
    this.market = market ?? null; // backend for worker::run_once
    this.observer = new Observer(repo, runId ?? `data-${process.pid}-0`);
    this.queries = new Queries(repo);
    // Metrics' InputRepository is SqliteMetrics{market} — a different
    // object from the supply; post-NM-074 the caller wires
    // new Metrics(sqliteMetrics(market)) over the same backend.
    this.metrics = metrics ?? new Metrics({
      observations: () => {
        throw new StorageError("storage", seamMsg("metrics::observations"));
      },
    });
    const ctl = control ?? new ControlSeam(
      runId ?? this.observer.run_id);
    this.legacy = { control: ctl, run_id: ctl.runId };
    this.inspectFn = inspectFn ??
      (() => { throw seamErr("verification_inspection::service::inspect"); });
    this.stoppedVerify = stoppedVerify ??
      (() => { throw seamErr("task_verification::existing::accept"); });
    this.pubVerify = pubVerify ?? pubAccept;
    this.workerRun = workerRun ??
      (() => { throw seamErr("worker::run_once"); });
    this.workerGate = workerGate ?? { acquire: () => null };
  }
  sourceReady() {
    return this.generation
      ? this.generation.activeVerifiedId() !== null
      : this.market_source !== null;
  }
  sourceBlock() {
    // BlockingReason.evidence_ref has skip_serializing_if — None omits.
    return this.generation
      ? { code: "verified_source_absent", detail: "no verified active generation; executions record per-item source refusals and never advance heads (acquire+verify+activate through x-data-acquire)" }
      : { code: "verified_source_absent", detail: "no installed inspected official snapshot; executions record per-item source refusals and never advance heads (X_DATA_MCP_OFFICIAL_MANIFEST + X_DATA_MCP_OFFICIAL_DIR with a passing manifest)" };
  }
  runtimeBlock(e, dataset) {
    const needsInspected =
      (e.handler === Handler.Collection && dataset === DATASET_DAILY) ||
      ((e.handler === Handler.RangeUpdate || e.handler === Handler.RangeBackfill ||
        e.handler === Handler.Republish) && dataset === DATASET_DAILY) ||
      e.handler === Handler.VerifyGap;
    const needsCatalogInput =
      e.handler === Handler.Collection && dataset === DATASET_CATALOG;
    if (needsInspected && !this.sourceReady()) return this.sourceBlock();
    if (needsCatalogInput && !this.sourceReady())
      return this.generation
        ? this.sourceBlock()
        : { code: "catalog_source_unconfigured", detail: "official input location is not configured; catalog execution keeps the source refusal" };
    return null;
  }
  capabilities(args) {
    if (args.dataset !== null) this.access.require(args.dataset, null);
    if (args.operation !== null &&
        !entries().some((e) => e.name === args.operation))
      throw new ApiError("unsupported_operation", "operation not registered");
    const items = [];
    for (const e of entries()) {
      if (e.handler === Handler.Compute &&
          (!this.access.allows(DATASET_YEARLY, null) ||
           (args.dataset !== null && args.dataset !== DATASET_YEARLY)))
        continue;
      if (!this.access.equalsLocal() && e.requires_local) continue;
      if (args.operation !== null && args.operation !== e.name) continue;
      for (const dataset of e.datasets(this.access)) {
        if (dataset !== null && args.dataset !== null && args.dataset !== dataset) continue;
        const detailed = args.operation !== null;
        const block = this.runtimeBlock(e, dataset);
        items.push({
          tool_name: e.name,
          description: e.description,
          dataset,
          contract_version: "v1",
          implemented: true,
          operator_only: e.operator_only,
          runtime_readiness: block ? "blocked" : "ready",
          governance_readiness: "ready",
          blocking_reasons: block ? [block] : [],
          declared_scope: DECLARED_SCOPE,
          active_generation_ref: this.generation ? this.generation.activeVerifiedId() : null,
          input_schema: detailed ? e.input : null,
          output_schema: detailed ? e.output : null,
          example: detailed ? capabilityExample(e, dataset) : null,
          schema_digest: createHash("sha256")
            .update(serValue([e.input, e.output])).digest("hex"),
        });
      }
    }
    const productionBlocking = [{
      code: "declared_scope_non_production", detail: DECLARED_SCOPE,
    }];
    if (this.generation === null)
      productionBlocking.push({
        code: "generation_root_unconfigured",
        detail: "X_DATA_MCP_GENERATION_ROOT is not configured; source bindings use the legacy inspected-env path rather than immutable generation sessions",
      });
    else if (!this.sourceReady()) productionBlocking.push(this.sourceBlock());
    return encode({
      capability_revision: "generic-data-v2",
      items,
      max_batch_entities: 100,
      max_page_size: 1000,
      max_snapshot_rows: MAX_ROWS,
      max_snapshot_bytes: MAX_BYTES,
      snapshots_per_category: 32,
      declared_scope: DECLARED_SCOPE,
      production_ready: false,
      production_blocking_reasons: productionBlocking,
    });
  }
  // WriteAcceptance mapping — the RequestOutcome must bind a task
  // (serde externally-tagged {Task: snapshot} | {Terminal: value}).
  acceptance(id, a) {
    const f = a.record.outcome.Task;
    if (f === undefined)
      throw new ApiError("unsupported_request_outcome", "task binding required");
    const block = this.#taskRuntimeBlock(f.task.key);
    return encode({
      request_id: id,
      task_id: f.task.task_id,
      dataset: f.task.key.dataset,
      contract_version: f.task.key.contract_version,
      reused: a.reused,
      status: visibleStatusOf(f.task.status, f.task.accepted_run_id,
        f.task.owner_run_id, this.legacy.control.runId),
      item_ids: f.items.map((i) => i.spec.item_id),
      runtime_readiness: block ? "blocked" : "ready",
      blocking_reasons: block ? [block] : [],
    });
  }
  #taskRuntimeBlock(key) {
    const dailyWrite = key.dataset === DATASET_DAILY &&
      ["start_collection", "update_data", "backfill_data", "republish_data"]
        .includes(key.operation);
    // crate::gap_verification::DATASET — "market.gap_verification".
    if ((dailyWrite || key.dataset === "market.gap_verification") && !this.sourceReady())
      return this.sourceBlock();
    if (key.dataset === DATASET_CATALOG && !this.sourceReady())
      return this.generation
        ? this.sourceBlock()
        : { code: "catalog_source_unconfigured", detail: "official input location is not configured; catalog execution keeps the source refusal" };
    return null;
  }
  controlCall(name, input) {
    if (!this.access.equalsLocal())
      throw new ApiError("forbidden", "control requires local-owner context");
    if (name === "get_task") {
      const a = decode(TaskV1, input);
      // ObservationRepository::task -> TaskSummary (observer run id).
      return encode(summary(
        this.repository.task(a.task_id, this.access),
        this.observer.run_id));
    }
    if (name === "get_request") {
      const a = decode(RequestV1, input);
      const r = this.legacy.control.readRequest(a.request_id);
      // RequestOutcome {Task: snapshot} | {Terminal: value}; an unknown
      // request is bound:false — never an error.
      let task = null, terminalResult = null;
      if (r !== null && r !== undefined) {
        if (r.outcome.Task !== undefined) {
          task = summary({
            request_id: a.request_id, snapshot: r.outcome.Task,
            publications: [], range_results: [], yearly_results: [],
          }, this.legacy.control.runId);
        } else {
          // Untagged RequestTerminal decode: kind-tagged terminal shapes
          // (already_verified | verification_inspection + receipt).
          terminalResult = decodeRequestTerminal(r.outcome.Terminal);
        }
      }
      return encode({
        request_id: a.request_id,
        bound: task !== null || terminalResult !== null,
        task,
        terminal_result: terminalResult,
      });
    }
    if (name === "cancel_task") {
      const a = decode(CancelV1, input);
      const s = this.legacy.control.cancel(a.task_id, a.expected_revision);
      return encode({
        task_id: s.task.task_id,
        status: visibleStatusOf(s.task.status, s.task.accepted_run_id,
          s.task.owner_run_id, this.observer.run_id),
        state_revision: s.task.state_revision,
      });
    }
    if (name === "start_collection") {
      const command = decode(Collection, input);
      if (command.request_id.trim() === "")
        throw new ApiError("empty_request_id", "request_id required");
      // CollectionContract::key + Collection::payload (request_id
      // dropped — the envelope never enters the normalized command).
      const { request_id, ...payload } = command;
      return this.acceptance(command.request_id,
        this.legacy.control.accept(command.request_id,
          { operation: "start_collection", dataset: command.dataset,
            contract_version: "v1" },
          payload));
    }
    if (name === "retry_task") {
      const a = decode(RetryItems, input);
      return this.acceptance(a.new_request_id,
        this.legacy.control.retry(
          a.original_task_id, a.new_request_id, new Set(a.item_ids)));
    }
    throw new ApiError("unknown_tool", `unsupported tool: ${name}`);
  }
  dispatch(name, input) {
    const isObj = input !== null && typeof input === "object" && !Array.isArray(input);
    if (isObj && "dataset" in input) {
      try { Dataset(input.dataset); }
      catch { throw new ApiError("unknown_dataset", "dataset is not registered"); }
    }
    if (isObj && "contract_version" in input && input.contract_version !== "v1")
      throw new ApiError("unsupported_contract", "unsupported contract version");
    const entry = entries().find((x) => x.name === name);
    if (!entry) return this.controlCall(name, input);
    const e = entry;
    const localOnly = (msg) => {
      if (!this.access.equalsLocal()) throw new ApiError("forbidden", msg);
    };
    switch (e.handler) {
      case Handler.InspectVerification: {
        localOnly("verification inspection requires local owner access");
        const command = decode(InspectVerification, input);
        // verification_inspection::service::inspect — one retry on a
        // known rollback precondition conflict is inside the service.
        const acceptance = this.inspectFn(this.legacy.control.repository,
          command.request_id, command.target_task_id,
          this.legacy.control.runId, this.access.principal);
        const term = acceptance.record.outcome.Terminal;
        if (term === undefined)
          throw new ApiError("needs_verification",
            "bounded inspection has an invalid task outcome");
        return encode({
          request_id: command.request_id, bound: true,
          task: null, terminal_result: decodeInspectionTerminal(term),
        });
      }
      case Handler.VerifyPublication: {
        localOnly("verification requires local owner access");
        const command = decode(VerifyV1, input);
        return this.acceptance(command.request_id,
          this.pubVerify(this.legacy.control, command.request_id,
            command.target_task_id));
      }
      case Handler.VerifyStopped: {
        localOnly("verification requires local owner access");
        const command = decode(VerifyV1, input);
        return this.acceptance(command.request_id,
          this.stoppedVerify(this.legacy.control, command.request_id,
            command.target_task_id));
      }
      case Handler.VerifyGap: {
        localOnly("write controls require local owner access");
        const command = decode(VerifyDataGap, input);
        // gap_verification::command_key + payload {target}.
        return this.acceptance(command.request_id,
          this.legacy.control.accept(command.request_id,
            { operation: "verify_data_gap",
              dataset: "market.gap_verification",
              contract_version: "gap-verification-task-v1" },
            { target: command.target }));
      }
      case Handler.RangeUpdate:
      case Handler.RangeBackfill: {
        localOnly("write controls require local owner access");
        const command = decode(RangeWrite, input);
        // RangeWrite::payload() (range.rs) — items XOR selection checked at
        // accept-admission before any repository work.
        const payload = rangeWritePayload(command);
        return this.acceptance(command.request_id,
          this.legacy.control.accept(command.request_id,
            commandKey(e.handler === Handler.RangeUpdate
              ? "update_data" : "backfill_data", command),
            payload));
      }
      case Handler.Republish: {
        localOnly("write controls require local owner access");
        const command = decode(RepublishWrite, input);
        return this.acceptance(command.request_id,
          this.legacy.control.accept(command.request_id,
            commandKey("republish_data", command),
            republishPayload(command)));
      }
      case Handler.Compute: {
        // metrics::compute(&*market, decode) — Metrics::compute over
        // SqliteMetrics; a NotComputable outcome maps to the typed
        // not_computable error with the computation as details.
        const computed = this.metrics.compute(
          decode(Compute, input), this.access);
        if (computed.result.outcome === "not_computable")
          throw new ApiError("not_computable",
            reasonDebug(computed.result.reason), { details: computed });
        return encode(computed);
      }
      case Handler.Worker: {
        localOnly("worker requires local owner access");
        decode(WorkerInput, input);
        this.workerGate.acquire();
        const s = this.workerRun(this.legacy.control, this.market,
          this.market_source, this.generation);
        return encode({
          idle: s === null || s === undefined,
          task_id: s?.task?.task_id ?? null,
          status: s === null || s === undefined ? null
            : visibleStatusOf(s.task.status, s.task.accepted_run_id,
              s.task.owner_run_id, this.legacy.control.runId),
        });
      }
      case Handler.Collection:
      case Handler.Retry:
      case Handler.Task:
      case Handler.Request:
      case Handler.Cancel:
        return this.controlCall(name, input);
      case Handler.Query:
        return encode(this.queries.query(decode(Query, input), this.access));
      case Handler.Versions:
        return encode(this.queries.versions(decode(VersionsQuery, input), this.access));
      case Handler.Availability:
        return encode(this.observer.availability(decode(Query, input), this.access));
      case Handler.Tasks:
        return encode(this.observer.tasks(decode(TasksQuery, input), this.access));
      case Handler.Progress:
        return encode(this.observer.progress(decode(ProgressQuery, input), this.access));
      case Handler.Capabilities:
        return this.capabilities(decode(CapabilitiesQuery, input));
    }
    throw new ApiError("unknown_tool", `unsupported tool: ${name}`);
  }
  tools() {
    return entries()
      .filter((e) => this.access.equalsLocal() || !e.requires_local)
      .map((e) => ({
        name: e.name, description: e.description,
        inputSchema: e.input, outputSchema: e.output,
      }));
  }
  call(name, input) {
    const GENERIC = new Set([
      "query_data", "list_data_versions", "get_data_availability",
      "list_tasks", "get_task_progress", "get_data_capabilities",
      "compute_metrics", "verify_publication_result",
      "inspect_verification_task", "verify_data_gap", "update_data",
      "backfill_data", "republish_data",
    ]);
    const WRITEISH = new Set([
      "start_collection", "retry_task", "cancel_task", "update_data",
      "backfill_data", "republish_data", "verify_publication_result",
      "inspect_verification_task", "verify_data_gap",
    ]);
    const UNCERTAIN = new Set([
      "update_data", "backfill_data", "republish_data",
      "verify_publication_result", "inspect_verification_task", "verify_data_gap",
    ]);
    const generic = (input && typeof input === "object" && !Array.isArray(input) &&
      ("contract_version" in input || "dataset" in input)) || GENERIC.has(name);
    let result;
    try {
      result = this.dispatch(name, input);
    } catch (e) {
      // market_storage::Error / task_control::Error -> mapped()/task_error()
      if (e instanceof StorageError || e instanceof TaskControlError) e = mapped(e);
      if (!(e instanceof ApiError)) throw e;
      if (generic)
        e.outcome_certainty =
          e.code === "outcome_unknown" ||
          (UNCERTAIN.has(name) &&
            (e.code === "storage_error" || e.code === "needs_verification"))
            ? "unknown"
            : WRITEISH.has(name) ? "not_committed" : "not_applicable";
      throw e;
    }
    return result;
  }
}
