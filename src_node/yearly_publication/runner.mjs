// yearly_publication/runner.rs — receipt-aware execution decision flow.
// The pure/verification half is ported verbatim; every storage arm is
// an injected repository call (`commit_yearly` remains a typed seam).
import * as RC from "../market/record_codec.mjs";
import { StorageError } from "../storage/sqlite_adapter.mjs";
import { createHash } from "node:crypto";
import { aggregate } from "../algorithms/yearly.mjs";
import { aggregateKey, aggregateMemoized } from "./memo.mjs";
import {
  CONTRACT_VERSION, DATASET, mBase, mConflict,
  mIntegrity, mInvalid, mOutcomeUnknown, parseIntent, statusTerminal,
  Status, Stage, visibleStatus,
} from "./types.mjs";
import { deriveYearly, dailyView as dailyViewForAggregate } from "./derive.mjs";
import { prepareYearlyResult } from "./results.mjs";

const SH = 8 * 3600 * 1000;
const shanghaiDate = (v) => {
  const d = v instanceof Date ? v : new Date(v);
  return new Date(d.getTime() + SH).toISOString().slice(0, 10);
};
const sha256 = (s) => createHash("sha256").update(s).digest("hex");

// runner.rs::bound_evidence — pure DerivationEvidence construction.
// `source_label` is the verifier-supplied string verbatim.
export const boundEvidence = (base, years, annual, derivedAt, sourceLabel) => {
  const asOf = shanghaiDate(derivedAt);
  const annualYears = annual.records.map((r) =>
    typeof r.bar.date === "string" ? Number(r.bar.date.slice(0, 4)) : r.bar.date.y);
  const reqYears = [...new Set(years)].sort((a, b) => a - b);
  if (annual.base_daily_version_id !== base.version.version_id ||
      annual.as_of !== asOf ||
      annual.records.length !== reqYears.length ||
      !annualYears.every((y, i) => y === reqYears[i])) {
    throw new SourceNotVerified(
      "aggregated annual series does not cover the requested years");
  }
  const digest = base.raw.integrity_sha256;
  if (typeof digest !== "string") {
    throw new SourceNotVerified("fixed base carries no integrity digest");
  }
  return {
    base_integrity_sha256: digest,
    years: [...new Set(years)].sort((a, b) => a - b),
    as_of: asOf,
    coverage: {
      status: "Complete",
      reason: `selected years verified by re-aggregating fixed daily ${base.version.version_id} against ${sourceLabel} as of ${asOf}; not a listing-history claim`,
    },
  };
};

/** SourceError::NotVerified carrier — verifier seam failure type. */
export class SourceNotVerified extends Error {
  constructor(message) {
    super(message);
    this.name = "SourceNotVerified";
    this.kind = "not_verified";
  }
}

/**
 * Verifier seam — `OfficialYearlyVerifier` requires an env-installed
 * TDX source (not ported on Node); `BoundYearlyVerifier` requires a
 * bound generation session. Both are injected objects with
 * `verify(base, years, annual, derivedAt) -> DerivationEvidence`.
 * The two real gate implementations are provided verbatim-shaped here:
 * official-source absence and bound-session absence are the fail-closed
 * refusals; evidence itself flows through boundEvidence.
 */
export const officialYearlyVerifier = ({ installed }) => ({
  verify(base, years, annual, derivedAt) {
    if (!installed) {
      throw new SourceNotVerified("official-file source not installed");
    }
    return boundEvidence(base, years, annual, derivedAt,
      "env-installed official-file source");
  },
});
export const boundYearlyVerifier = ({ bound }) => ({
  verify(base, years, annual, derivedAt) {
    if (!bound) {
      throw new SourceNotVerified("no bound generation session");
    }
    return boundEvidence(base, years, annual, derivedAt,
      `bound generation ${bound.generation_id}`);
  },
});

export const YearlyExecution = {
  Published: (receipt) => ({ kind: "published", receipt }),
  NotPublished: (task) => ({ kind: "not_published", task }),
};

export class YearlyRunner {
  constructor(repository, runId, derivedAt) {
    this.repository = repository;
    this.run_id = runId;
    this.derived_at = derivedAt;
  }
  run_item(taskId, itemId, provider) {
    const repository = this.repository;
    let binding = repository.task_item_binding(taskId, itemId);
    if (binding === null) {
      repository.task_head(taskId);
      throw mInvalid("yearly item absent from accepted task");
    }
    const op = binding.head.key.operation;
    if ((op !== "update_data" && op !== "backfill_data") ||
        binding.head.key.dataset !== DATASET ||
        binding.head.key.contract_version !== CONTRACT_VERSION) {
      throw mInvalid("unsupported yearly execution contract");
    }
    const existing = repository.yearly_item_receipt(taskId, itemId);
    if (existing !== null) return YearlyExecution.Published(existing);
    if (visibleStatus(binding.head.status, binding.head.accepted_run_id,
        binding.head.owner_run_id, this.run_id) === "needs_verification") {
      // Error::Task(task_control::Error::NeedsVerification)
      throw new StorageError("needs_verification",
        "ACCEPTANCE_NEEDS_VERIFICATION: old run or unresolved execution");
    }
    if (binding.item.stage === Stage.Running) {
      throw mOutcomeUnknown(
        "running item has no verified result; inspect before any retry");
    }
    if (statusTerminal(binding.head.status) || binding.item.stage === Stage.Finished) {
      if (binding.item.outcome !== null && binding.item.outcome.success === true) {
        throw mIntegrity("successful yearly item has no receipt");
      }
      return this.#notPublished(taskId);
    }
    if (binding.head.status === Status.CancelRequested) {
      repository.transition_task_head(binding.head, null, this.run_id,
        { kind: "finishExecution" });
      return this.#notPublished(taskId);
    }
    if (binding.head.status === Status.Pending) {
      repository.transition_task_head(binding.head, null, this.run_id,
        { kind: "claim" });
    }
    repository.transition_task_head(binding.head, binding.item, this.run_id,
      { kind: "startItem", item_id: itemId });
    try {
      const receipt = this.#deriveAndCommit(binding, itemId, provider);
      return YearlyExecution.Published(receipt);
    } catch (error) {
      // A response can be lost after commit. Never overwrite success.
      const receipt = repository.yearly_item_receipt(taskId, itemId);
      if (receipt !== null) return YearlyExecution.Published(receipt);
      if (isOutcomeUnknown(error)) {
        const b = repository.task_item_binding(taskId, itemId);
        if (b === null) throw mIntegrity("bound task item missing");
        try {
          repository.transition_task_head(b.head, b.item, this.run_id, {
            kind: "requireVerification", item_id: itemId,
            reason: "item execution result not confirmed after acquisition/derivation/publication; committed receipt not yet established",
          });
        } catch (recording) {
          const receipt2 = repository.yearly_item_receipt(taskId, itemId);
          if (receipt2 !== null) return YearlyExecution.Published(receipt2);
          throw recording;
        }
        throw error;
      }
      const b = repository.task_item_binding(taskId, itemId);
      if (b === null) throw mIntegrity("bound task item missing");
      const cancelled = error instanceof RC.MarketError &&
        error.kind === "conflict" && b.head.status === Status.CancelRequested;
      repository.transition_task_head(b.head, b.item, this.run_id, {
        kind: "finishItem", item_id: itemId,
        outcome: {
          success: false,
          error: cancelled
            ? `cancelled before publication: ${error.message}`
            : error.message,
          evidence: null,
        },
      });
      if (b.head.counts.finished === b.head.counts.total ||
          b.head.status === Status.CancelRequested) {
        repository.transition_task_head(b.head, null, this.run_id, {
          kind: cancelled ? "finishCancellation" : "finishExecution",
        });
      }
      return this.#notPublished(taskId);
    }
  }
  #notPublished(taskId) {
    return YearlyExecution.NotPublished(this.repository.task(taskId));
  }
  #deriveAndCommit(binding, itemId, verifier) {
    const repository = this.repository;
    const frozenInputs = binding.item.spec.frozen_inputs;
    const baseId = frozenInputs.base_daily_version_id;
    if (typeof baseId !== "string") {
      throw mIntegrity("frozen base missing");
    }
    const yearsRaw = frozenInputs.years;
    if (!Array.isArray(yearsRaw)) {
      throw mIntegrity("invalid type: expected a sequence");
    }
    const years = [...new Set(yearsRaw.map((y) => {
      if (!Number.isInteger(y)) throw mIntegrity("invalid type: expected i32");
      return y;
    }))].sort((a, b) => a - b);
    if (years.length === 0) throw mIntegrity("frozen years empty");
    const firstDay = `${String(years[0]).padStart(4, "0")}-01-01`;
    const lastDay = `${String(years[years.length - 1]).padStart(4, "0")}-12-31`;
    if (years[0] < 1 || years[years.length - 1] > 9999) {
      throw mIntegrity("frozen years out of range");
    }
    const base = repository.fixed_window(baseId, firstDay, lastDay);
    if (base.raw.integrity_sha256 !== frozenInputs.base_integrity_sha256) {
      throw mIntegrity("fixed base differs from accepted digest");
    }
    let intent;
    try { intent = parseIntent(frozenInputs.intent); }
    catch (e) { throw mIntegrity(e.message); }
    const prior = frozenInputs.comparison?.version_id ?? null;
    if (prior !== null &&
        repository.fixed_facts(prior).integrity_sha256 !==
          frozenInputs.comparison.integrity_sha256) {
      throw mIntegrity("fixed comparison differs from accepted digest");
    }
    const asOf = shanghaiDate(this.derived_at);
    const key = aggregateKey(base.raw.integrity_sha256, years, asOf);
    const annual = aggregateMemoized(key, () => {
      try { return aggregate(dailyViewForAggregate(base.version), years, asOf); }
      catch (e) { throw mBase(e.message); }
    });
    let evidence;
    try { evidence = verifier.verify(base, years, annual, this.derived_at); }
    catch (e) { throw mBase(e instanceof Error ? e.message : String(e)); }
    const current = repository.task_head(binding.head.task_id);
    if (current.status !== "running" ||
        BigInt(current.state_revision) !== BigInt(binding.head.state_revision)) {
      throw mConflict("task changed or cancelled during yearly verification");
    }
    const id = sha256(JSON.stringify([binding.head.task_id, itemId]));
    const request = {
      symbol: base.version.symbol,
      base_daily_version_id: baseId,
      version_id: `yearly-version-${id}`,
      years,
      derived_at: this.derived_at,
    };
    const candidate = deriveYearly(repository, request, evidence, base, annual);
    const plan = prepareYearlyResult(repository, {
      publication_id: `yearly-result-${id}`,
      version_id: request.version_id,
      task_id: binding.head.task_id,
      item_id: itemId,
      run_id: this.run_id,
      expected_revision: Number(binding.head.state_revision),
      intent,
    }, candidate, prior);
    return repository.commit_yearly(plan);
  }
}
// runner.rs outcome guard: Error::OutcomeUnknown | Error::Storage |
// Error::Task(StorageUnavailable | OutcomeUnknown).
const isOutcomeUnknown = (e) =>
  (e instanceof RC.MarketError &&
    (e.kind === "outcome_unknown" || e.kind === "storage")) ||
  (e instanceof StorageError &&
    (e.code === "unavailable" || e.code === "outcome_unknown"));
