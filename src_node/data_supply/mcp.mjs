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
import { Access, StorageError, invalid } from "./access.mjs";
import { Observer } from "./observe.mjs";
import { MAX_BYTES, MAX_ROWS, Queries } from "./query.mjs";
import {
  CancelV1, CapabilitiesQuery, Collection, Compute, Decode,
  InspectVerification, ProgressQuery, Query, RangeWrite, RepublishWrite,
  RetryItems, TaskV1, RequestV1, TasksQuery, VerifyDataGap, VerifyV1,
  VersionsQuery, WorkerInput, securityValidate,
  DATASET_CATALOG, DATASET_DAILY, DATASET_YEARLY, Dataset,
} from "./wire.mjs";
import { entries, capabilityExample, Handler } from "./registry.mjs";

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
export class RepositorySeam {
  // Queries/Observer read surface (SqliteSupply)
  query() { throw new StorageError("storage", seamMsg("repository.query")); }
  versions() { throw new StorageError("storage", seamMsg("repository.versions")); }
  verify() { throw new StorageError("storage", seamMsg("repository.verify")); }
  verifyVersions() { throw new StorageError("storage", seamMsg("repository.verify_versions")); }
  task() { throw new StorageError("storage", seamMsg("repository.task")); }
  tasks() { throw new StorageError("storage", seamMsg("repository.tasks")); }
  availability() { throw new StorageError("storage", seamMsg("repository.availability")); }
  // task_control::TaskRepository + service surface
  readRequest() { throw seamErr("read_request"); }
  accept() { throw seamErr("accept"); }
  cancel() { throw seamErr("cancel"); }
  retry() { throw seamErr("retry"); }
  inspectVerification() { throw seamErr("verification_inspection::inspect"); }
  publicationVerify() { throw seamErr("publication_verification::accept"); }
  stoppedVerify() { throw seamErr("task_verification::accept"); }
  gapAccept() { throw seamErr("gap_verification::accept"); }
  workerRunOnce() { throw seamErr("worker::run_once"); }
  metricsCompute() { throw new StorageError("storage", seamMsg("metrics::compute")); }
}

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
  // DataApi::new tasks/run_id/access — skeleton takes the wired seams:
  //   { access, runId?, seam?, generation?, marketSource? }
  // generation/marketSource default to env-presence; the seam defaults to
  // RepositorySeam (all repository ops -> typed storage_error).
  constructor({ access, runId, seam, generation, marketSource } = {}) {
    const repo = seam ?? new RepositorySeam();
    this.access = access ?? Access.local();
    this.generation = generation === undefined ? generationFromEnv() : generation;
    this.market_source = marketSource === undefined ? null : marketSource;
    this.repository = repo;
    this.observer = new Observer(repo, runId ?? `data-${process.pid}-0`);
    this.queries = new Queries(repo, this.observer);
    this.legacy = { control: repo, run_id: this.observer.run_id };
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
  // WriteAcceptance mapping — only reachable behind the control seam;
  // visible_status is the task_control codec (lands with S12c).
  acceptance(id, a) {
    if (a.record.outcome.kind !== "task")
      throw new ApiError("unsupported_request_outcome", "task binding required");
    const f = a.record.outcome.task;
    const block = this.#taskRuntimeBlock(f.task.key);
    return encode({
      request_id: id,
      task_id: f.task.task_id,
      dataset: f.task.key.dataset,
      contract_version: f.task.key.contract_version,
      reused: a.reused,
      status: f.visibleStatus(this.legacy.run_id),
      item_ids: f.items.map((i) => i.spec.item_id),
      runtime_readiness: block ? "blocked" : "ready",
      blocking_reasons: block ? [block] : [],
    });
  }
  #taskRuntimeBlock(key) {
    const dailyWrite = key.dataset === DATASET_DAILY &&
      ["start_collection", "update_data", "backfill_data", "republish_data"]
        .includes(key.operation);
    if ((dailyWrite || key.dataset === "market.data_gap") && !this.sourceReady())
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
      this.repository.task(a.task_id, this.access); // seam
      throw new ApiError("internal_error", "seam returned"); // unreachable
    }
    if (name === "get_request") {
      const a = decode(RequestV1, input);
      this.legacy.control.readRequest(a.request_id); // seam
      throw new ApiError("internal_error", "seam returned");
    }
    if (name === "cancel_task") {
      const a = decode(CancelV1, input);
      this.legacy.control.cancel(a.task_id, a.expected_revision); // seam
      throw new ApiError("internal_error", "seam returned");
    }
    if (name === "start_collection") {
      const command = decode(Collection, input);
      if (command.request_id.trim() === "")
        throw new ApiError("empty_request_id", "request_id required");
      this.legacy.control.accept(command.request_id, command); // seam
      throw new ApiError("internal_error", "seam returned");
    }
    if (name === "retry_task") {
      const a = decode(RetryItems, input);
      this.legacy.control.retry(a.original_task_id, a.new_request_id, a.item_ids); // seam
      throw new ApiError("internal_error", "seam returned");
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
        this.legacy.control.inspectVerification(command); // seam
        break;
      }
      case Handler.VerifyPublication: {
        localOnly("verification requires local owner access");
        const command = decode(VerifyV1, input);
        this.legacy.control.publicationVerify(command); // seam
        break;
      }
      case Handler.VerifyStopped: {
        localOnly("verification requires local owner access");
        const command = decode(VerifyV1, input);
        this.legacy.control.stoppedVerify(command); // seam
        break;
      }
      case Handler.VerifyGap: {
        localOnly("write controls require local owner access");
        const command = decode(VerifyDataGap, input);
        this.legacy.control.gapAccept(command); // seam
        break;
      }
      case Handler.RangeUpdate:
      case Handler.RangeBackfill: {
        localOnly("write controls require local owner access");
        const command = decode(RangeWrite, input);
        this.legacy.control.accept(command.request_id, command); // seam
        break;
      }
      case Handler.Republish: {
        localOnly("write controls require local owner access");
        const command = decode(RepublishWrite, input);
        this.legacy.control.accept(command.request_id, command); // seam
        break;
      }
      case Handler.Compute: {
        const request = decode(Compute, input);
        // metrics/service.rs pre-repository validation — ported verbatim.
        if (request.inputs.length !== 1 || request.metrics.length !== 1)
          throw invalid("exactly one inline input and metric supported");
        const inputRef = request.inputs[0];
        const metric = request.metrics[0];
        if (inputRef.input_id.trim() === "" || metric.metric_id.trim() === "" ||
            metric.input_ids.length !== 1 ||
            metric.input_ids[0] !== inputRef.input_id ||
            inputRef.version_ref.version_id.trim() === "" ||
            inputRef.version_ref.version_id === "latest" ||
            inputRef.version_ref.integrity_sha256.length !== 64)
          throw invalid("fixed yearly identity/digest/input reference required");
        const entity = inputRef.version_ref.entity;
        const bad = securityValidate(entity);
        if (bad) throw invalid(bad);
        this.access.require(DATASET_YEARLY, entity);
        const range = request.range;
        if (range.start_year < 1 || range.end_year > 9999 ||
            range.end_year < range.start_year ||
            range.end_year - range.start_year >= 1000 ||
            !(metric.parameters.min_periods >= 1 && metric.parameters.min_periods <= 1000))
          throw invalid("computation resource/range budget");
        this.repository.metricsCompute(); // seam — repository.observations
        break;
      }
      case Handler.Worker: {
        localOnly("worker requires local owner access");
        decode(WorkerInput, input);
        this.legacy.control.workerRunOnce(); // seam
        break;
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
