// NM-082 — src/market_update/runner.rs pure-decision halves:
// update_candidate (:319-451), binding (:452-467), run_items (:253-311),
// ensure_default_coverings (:542-570). The storage-commit orchestration
// (run_item and the acquire_and_commit*/commit_acquired_range arms) is
// NOT ported — it belongs to the repository-objects lane (S4/S5); every
// repository/provider call goes through injected seams that throw typed
// errors when unwired — never a silent success.
import {
  MarketError, invalid, integrity, sha256Hex, SourceError,
  reasonDebug, rangeEq, rangeValue, tcError,
} from "./mod.mjs";
import {
  parseDefaultFrozen, union, frozenDayRange, CONTRACT_VERSION,
  DEFAULT_CONTRACT_VERSION, REPUBLISH_CONTRACT_VERSION, scopeId,
} from "./tasks.mjs";
import { tDate, serValue, dateCmp, evidenceValue } from "./serde.mjs";
import { epochDay } from "../market/compact_market.mjs";

const TASK_TERMINAL = ["succeeded", "partially_failed", "failed", "cancelled", "interrupted"];
const visibleStatus = (status, accepted, owner, run) =>
  !TASK_TERMINAL.includes(status) && (accepted !== run || (owner !== null && owner !== undefined && owner !== run))
    ? "needs_verification" : status;

// Typed seam error — the storage-side RangeRepository/MarketRepository
// surface is owned by the S4/S5 lane; every unwired call fails closed.
export const seamError = (op) =>
  new MarketError("storage", `MARKET_STORAGE: market_update repository seam not wired (S4/S5 pending): ${op}`);

// RangeRepository seam — every method throws a typed error by default.
// The probe and the future storage lane supply concrete implementations.
export class RangeRepositorySeam {
  stage() { throw seamError("stage"); }
  candidate() { throw seamError("candidate"); }
  mark_validated() { throw seamError("mark_validated"); }
  fixed() { throw seamError("fixed"); }
  fixed_many() { throw seamError("fixed_many"); }
  fixed_facts() { throw seamError("fixed_facts"); }
  fixed_window() { throw seamError("fixed_window"); }
  fixed_window_years() { throw seamError("fixed_window_years"); }
  task() { throw seamError("task"); }
  task_head() { throw seamError("task_head"); }
  task_item_binding() { throw seamError("task_item_binding"); }
  transition_task_head() { throw seamError("transition_task_head"); }
  commit() { throw seamError("commit"); }
  receipt() { throw seamError("receipt"); }
  range() { throw seamError("range"); }
  versions() { throw seamError("versions"); }
  commit_range() { throw seamError("commit_range"); }
  commit_range_ops() { throw seamError("commit_range_ops"); }
  update_base_facts() { throw seamError("update_base_facts"); }
  base_bars_in_range() { throw seamError("base_bars_in_range"); }
  commit_range_ops_typed() { throw seamError("commit_range_ops_typed"); }
  commit_range_no_change_typed() { throw seamError("commit_range_no_change_typed"); }
  commit_update_batch() { throw seamError("commit_update_batch"); }
  range_receipt() { throw seamError("range_receipt"); }
  item_receipt() { throw seamError("item_receipt"); }
  transition_task() { throw seamError("transition_task"); }
  cover_gap() { throw seamError("cover_gap"); }
  gap_coverings() { throw seamError("gap_coverings"); }
}
// RangeProvider seam — acquire/acquire_typed throw SourceError; the
// update path maps them to Error::Invalid per runner.rs:397-399.
export class RangeProviderSeam {
  acquire() { throw new SourceError("invocation", "range provider seam not wired (S8)"); }
  acquire_typed() { throw new SourceError("invocation", "range provider seam not wired (S8)"); }
}

// RangeRunner { repository, run_id } — run_item is injected (the
// storage-commit driver, S4/S5 lane); default throws the typed seam.
export class RangeRunner {
  constructor({ repository = new RangeRepositorySeam(), run_id, runItem = null }) {
    this.repository = repository;
    this.run_id = run_id;
    this.runItem = runItem ?? (() => { throw seamError("run_item"); });
  }

  // binding (:452-467) — O(1) head+item binding; a missing item produces
  // the same invalid-command error the snapshot path did.
  binding(taskId, itemId, kind) {
    const b = this.repository.task_item_binding(taskId, itemId);
    if (b != null) return b;
    this.repository.task_head(taskId); // propagate task-level errors verbatim
    throw invalid(`${kind} item absent from accepted task`);
  }

  // update_candidate (:319-451) — verbatim. Returns null to defer the
  // item to the per-item path; returns [UpdateBatchItem, frozenOrNull].
  updateCandidate(taskId, itemId, provider) {
    let binding = this.repository.task_item_binding(taskId, itemId);
    if (binding == null) {
      this.repository.task_head(taskId);
      throw invalid("range item absent from accepted task");
    }
    const defaultContract = binding.head.key.contract_version === DEFAULT_CONTRACT_VERSION;
    const republishContract = binding.head.key.contract_version === REPUBLISH_CONTRACT_VERSION;
    if (!["update_data", "backfill_data", "republish_data"].includes(binding.head.key.operation) ||
        binding.head.key.dataset !== "market.daily" ||
        (!defaultContract && !republishContract &&
         binding.head.key.contract_version !== CONTRACT_VERSION)) {
      throw invalid("unsupported range execution contract");
    }
    if (this.repository.item_receipt(taskId, itemId) != null ||
        visibleStatus(binding.head.status, binding.head.accepted_run_id,
                      binding.head.owner_run_id ?? null, this.run_id) === "needs_verification" ||
        binding.item.stage !== "pending" ||
        TASK_TERMINAL.includes(binding.head.status) ||
        binding.head.status === "cancel_requested") {
      return null;
    }
    let baseId, range;
    if (defaultContract) {
      const parsed = parseDefaultFrozen(binding.item.spec.frozen_inputs);
      if (binding.head.key.operation === "update_data" && parsed.refresh_range == null) {
        throw integrity("default update item missing frozen refresh range");
      }
      if (binding.head.key.operation === "backfill_data" && parsed.refresh_range != null) {
        throw integrity("default backfill item carries a refresh range");
      }
      baseId = parsed.base_version_id;
      range = union(parsed);
    } else {
      const frozen = binding.item.spec.frozen_inputs;
      const baseIdV = frozen != null && typeof frozen === "object" && !Array.isArray(frozen)
        ? frozen.base_version_id : undefined;
      if (typeof baseIdV !== "string") throw integrity("frozen base missing");
      baseId = baseIdV;
      const rangeV = typeof frozen === "object" && frozen !== null && !Array.isArray(frozen)
        ? frozen.range : undefined;
      const startV = rangeV != null && typeof rangeV === "object" && !Array.isArray(rangeV)
        ? rangeV.start : undefined;
      const endV = rangeV != null && typeof rangeV === "object" && !Array.isArray(rangeV)
        ? rangeV.end : undefined;
      try { range = { start: tDate(startV) }; } catch (e) { throw integrity(e.message); }
      try { range.end = tDate(endV); } catch (e) { throw integrity(e.message); }
    }
    const facts = this.repository.update_base_facts(baseId);
    if (facts == null) return null;
    const baseWindow = this.repository.base_bars_in_range(baseId, range.start, range.end);
    if (baseWindow == null) return null;
    let acquired;
    try { acquired = provider.acquire_typed(facts, baseWindow, range); }
    catch (e) { throw invalid(e.message); }
    if (acquired == null) return null;
    // The batch transaction's revision CAS subsumes the per-item guard.
    if (acquired.evidence.base_version_id !== baseId ||
        acquired.evidence.base_integrity_sha256 !== facts.integrity_sha256 ||
        !rangeEq(acquired.evidence.range, range)) {
      throw invalid(`NEEDS_VERIFICATION: ${reasonDebug({ kind: "basis_unknown" })}`);
    }
    const id = sha256Hex(Buffer.from(JSON.stringify([taskId, itemId]), "utf8"));
    const scope = { symbol: facts.symbol, period: "Day" };
    const continuity = evidenceValue(acquired.evidence);
    const prepared = acquired.decision.kind === "unchanged"
      ? { kind: "no_change", base_version_id: baseId,
          range_start_day: epochDay(range.start.s), range_end_day: epochDay(range.end.s),
          continuity }
      : { kind: "append", base_version_id: baseId,
          result_version_id: `range-version-${id}`, upserts: acquired.decision.upserts,
          continuity };
    return [{
      task_id: taskId, item_id: itemId, publication_id: `range-result-${id}`,
      run_id: this.run_id, symbol: facts.symbol,
      occupancy_key: scopeId(scope.symbol, "day"),
      prepared,
    }, defaultContract ? binding.item.spec.frozen_inputs : null];
  }

  // ensure_default_coverings (:542-570) — covering bindings for frozen
  // gaps fully contained in the committed union range.
  ensureDefaultCoverings(frozenV, versionId, coveredAt) {
    const parsed = parseDefaultFrozen(frozenV);
    const u = union(parsed);
    for (const gap of parsed.proven_gap_ranges) {
      const range = frozenDayRange(gap.range); // "frozen range inverted"
      if (dateCmp(range.start, u.start) < 0 || dateCmp(range.end, u.end) > 0) {
        throw integrity("frozen gap outside committed verified range");
      }
      if (this.repository.gap_coverings(gap.gap_id).some((c) => c.version_id === versionId)) {
        continue;
      }
      this.repository.cover_gap({
        gap_id: gap.gap_id, version_id: versionId,
        covered_at: coveredAt ?? new Date().toISOString(),
      });
    }
  }

  // run_items (:253-311) — verbatim: prepare every item outside the
  // writer, commit in one transaction, per-item fallback on the per-item
  // path (injected runItem — the S4/S5 lane owns the real one).
  runItems(taskId, itemIds, provider) {
    const items = [];
    const fallback = [];
    const coverings = []; // [item_id, frozen_inputs]
    for (const itemId of itemIds) {
      let r;
      try { r = this.updateCandidate(taskId, itemId, provider); }
      catch { fallback.push(itemId); continue; }
      if (r != null) {
        const [item, frozen] = r;
        if (frozen != null) coverings.push([item.item_id, frozen]);
        items.push(item);
      } else {
        fallback.push(itemId);
      }
    }
    if (items.length > 0) {
      let results;
      try {
        results = this.repository.commit_update_batch(taskId, items);
      } catch {
        // Structural failure rolled the batch back — isolate per item.
        fallback.push(...items.map((i) => i.item_id));
        results = undefined;
      }
      if (results === null) return null; // backend lacks the batch surface
      if (results !== undefined) {
        if (results.length !== items.length) {
          throw integrity("update batch result count does not match items");
        }
        for (let i = 0; i < items.length; i++) {
          const item = items[i], result = results[i];
          if (result.ok !== undefined) {
            const covering = coverings.find(([id]) => id === item.item_id);
            if (covering != null) {
              this.ensureDefaultCoverings(covering[1], result.ok.version_id);
            }
          } else {
            fallback.push(item.item_id);
          }
        }
      }
    }
    for (const itemId of fallback) {
      this.runItem(taskId, itemId, provider);
    }
    return true; // Ok(Some(()))
  }
}

export { rangeValue };
