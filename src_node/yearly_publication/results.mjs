// yearly_publication/results.rs verbatim port — the publish/reuse
// decision (prepare_yearly_result) + YearlyResultPlan + equivalent.
// `commit_yearly` stays a typed seam on the injected repository; the
// audit envelope uses the ported market_write::pack.
import * as RC from "../market/record_codec.mjs";
import { pack } from "../market/market_write.mjs";
import { canonEq } from "../storage/task_repository.mjs";
import {
  intentBody, intentVariant, mBase, mConflict, mInvalid,
  mIntegrity,
} from "./types.mjs";
import { CONTRACT_VERSION } from "./types.mjs";
import { validateBinding } from "./tasks.mjs";
import {
  barEq, planRecord, prepareDerivedYearly, validateBaseFacts,
} from "./derive.mjs";

const integrity = mIntegrity;

// results::equivalent — compare the published/reuse facts subtree.
// serde PartialEq: typed fields compare by VALUE (Decimal "12.340" ==
// "12.34"), so bars need dec-aware comparison; the raw Value subtrees
// (annual_records, analysis_coverage_v1) compare structurally.
export const equivalent = (prior, next) => {
  const facts = (r) => {
    const annual = r.raw?.local_yearly_derivation_v1?.annual;
    if (!annual || annual.records === undefined) return null;
    return {
      symbol: r.version.symbol, source: r.version.source,
      adjustment: r.version.adjustment,
      adjustment_evidence: r.version.adjustment_evidence,
      units: r.version.unit_currency, bars: r.version.records,
      lineage: r.version.yearly_derivation,
      coverage: r.batch.coverage,
      annual_records: annual.records,
      analysis_coverage_v1: r.raw.analysis_coverage_v1,
    };
  };
  const a = facts(prior), b = facts(next);
  if (a === null || b === null) return false;
  return a.symbol === b.symbol && a.source === b.source &&
    a.adjustment === b.adjustment &&
    canonEq(a.adjustment_evidence, b.adjustment_evidence) &&
    canonEq(a.units, b.units) && canonEq(a.coverage, b.coverage) &&
    lineageEq(a.lineage, b.lineage) &&
    a.bars.length === b.bars.length &&
    a.bars.every((x, i) => barEq(x, b.bars[i])) &&
    canonEq(a.annual_records, b.annual_records) &&
    canonEq(a.analysis_coverage_v1 ?? null, b.analysis_coverage_v1 ?? null);
};
const lineageEq = (a, b) =>
  a === null || a === undefined ? b === null || b === undefined
    : b !== null && b !== undefined &&
      a.base_daily_version_id === b.base_daily_version_id &&
      a.algorithm_version === b.algorithm_version &&
      (typeof a.first_trading_date === "string" ? a.first_trading_date : a.first_trading_date?.s) ===
        (typeof b.first_trading_date === "string" ? b.first_trading_date : b.first_trading_date?.s) &&
      (typeof a.last_trading_date === "string" ? a.last_trading_date : a.last_trading_date?.s) ===
        (typeof b.last_trading_date === "string" ? b.last_trading_date : b.last_trading_date?.s);

/**
 * Publisher::prepare_yearly_result — verbatim decision:
 * binding validation -> candidate identity -> intent/prior binding ->
 * base facts re-read -> prior check -> equivalent reuse vs derived
 * publish -> audit envelope.
 */
export const prepareYearlyResult = (repository, command, candidate, priorId) => {
  const binding0 = repository.task_item_binding(command.task_id, command.item_id);
  if (binding0 !== null) {
    validateBinding(binding0, command, candidate);
  } else {
    const head = repository.task_head(command.task_id);
    const op = head.key.operation;
    if (op === "update_data" || op === "backfill_data") {
      throw mConflict("yearly publication differs from accepted frozen inputs");
    }
  }
  if (candidate.raw.version_id !== command.version_id) {
    throw mInvalid("yearly candidate identity");
  }
  if (intentVariant(command.intent) === "CurrentHead" &&
      (intentBody(command.intent).expected_yearly_head ?? null) !==
        (priorId ?? null)) {
    throw mConflict("reuse comparison must bind expected yearly head");
  }
  const baseFacts = repository.fixed_facts(candidate.base_daily_version_id);
  if (baseFacts.integrity_sha256 !== candidate.base_integrity_sha256) {
    throw mBase("fixed base changed after derivation");
  }
  // One-day window: full metadata with a single-segment read on compact
  // backends; legacy falls back to `fixed` (contract permits superset).
  const base = repository.fixed_window(
    candidate.base_daily_version_id, baseFacts.first_day, baseFacts.first_day);
  const prior = priorId === null || priorId === undefined
    ? null : repository.fixed(priorId);
  const binding = repository.task_item_binding(command.task_id, command.item_id);
  if (binding === null) {
    throw mConflict("comparison item absent from accepted task");
  }
  if (binding.head.key.contract_version === CONTRACT_VERSION) {
    const comparison = prior === null ? null : {
      version_id: prior.version.version_id,
      integrity_sha256: prior.raw.integrity_sha256,
    };
    if (binding.item.spec.item_id !== command.item_id ||
        !RC.__jsonEq(binding.item.spec.frozen_inputs.comparison ?? null,
          comparison)) {
      throw mConflict("comparison differs from accepted fixed version");
    }
  }
  const next = candidate.checked;
  validateBaseFacts(repository, next, baseFacts);
  if (prior !== null &&
      (prior.version.symbol !== next.version.symbol ||
       prior.version.period !== next.version.period)) {
    throw mBase("prior yearly identity/period");
  }
  const noChange = prior !== null && equivalent(prior, next);
  let publication;
  if (noChange) {
    const c = { ...command, version_id: prior.version.version_id };
    publication = planRecord(repository, c, prior);
    publication.promote = null;
  } else {
    publication = prepareDerivedYearly(repository, command, candidate);
  }
  const [candidateEnv, auditPayloads] = pack(candidate.raw);
  const audit = {
    format: "local-yearly-result-v1", command: publication.command,
    candidate: candidateEnv, no_change: noChange,
    base_daily_version_id: candidate.base_daily_version_id,
    base_integrity_sha256: candidate.base_integrity_sha256,
    prior_version_id: priorId ?? null,
    prior_integrity_sha256: prior === null ? null : prior.raw.integrity_sha256,
    result_integrity_sha256: publication.record.raw.integrity_sha256,
  };
  return new YearlyResultPlan({
    publication, base, prior, no_change: noChange, audit, audit_payloads: auditPayloads,
  });
};

export class YearlyResultPlan {
  constructor({ publication, base, prior, no_change, audit, audit_payloads }) {
    this.publication = publication; this.base = base; this.prior = prior;
    this.no_change = no_change; this.audit = audit;
    this.audit_payloads = audit_payloads;
  }
  is_no_change() { return this.no_change; }
  command() { return this.publication.command; }
}
