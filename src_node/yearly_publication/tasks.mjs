// yearly_publication/tasks.rs verbatim port — ExplicitYears/YearlyItem
// serde + YearlyContract (key/normalize/resolve) + validate_binding +
// frozen. Pure layer: repository access only through the injected
// `repository` object (fixed_facts/fixed_window_years reads).
import {
  CONTRACT_VERSION, DATASET, intentBody, intentVariant, mConflict,
  mIntegrity, parseIntent, repositoryError, scopeId, tcInvalid,
  tcNeedsVerification, valEq, asBTreeSet,
} from "./types.mjs";

const invalid = tcInvalid;
const ITEM_FIELDS = ["symbol", "base_daily_version_id", "years",
  "intent", "comparison_version_id"];

// serde YearlyItem — deny_unknown_fields, missing-field wording
// (serde_json::from_value errors carry no position — bare messages).
const parseYearlyItem = (v) => {
  const bad = (m) => { throw tcInvalid(m); };
  if (v === null || typeof v !== "object" || Array.isArray(v)) {
    bad("invalid type: expected struct YearlyItem");
  }
  for (const k of Object.keys(v)) {
    if (!ITEM_FIELDS.includes(k)) {
      bad(`unknown field \`${k}\`, expected ${ITEM_FIELDS.map((f) => `\`${f}\``).join(", ")}`);
    }
  }
  for (const k of ["symbol", "base_daily_version_id"]) {
    if (typeof v[k] !== "string") bad(`missing field \`${k}\``);
  }
  if (!Array.isArray(v.years)) bad("missing field `years`");
  const years = asBTreeSet(v.years, tcInvalid);
  if (v.intent === undefined) bad("missing field `intent`");
  const intent = parseIntent(v.intent);
  const cv = v.comparison_version_id;
  if (cv !== undefined && cv !== null && typeof cv !== "string") {
    bad("invalid type: expected option string");
  }
  return {
    symbol: v.symbol, base_daily_version_id: v.base_daily_version_id,
    years, intent, comparison_version_id: cv ?? null,
  };
};

const parseExplicitYears = (raw) => {
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) {
    throw tcInvalid("invalid type: expected struct ExplicitYears");
  }
  for (const k of Object.keys(raw)) {
    if (k !== "items") {
      throw tcInvalid(`unknown field \`${k}\`, expected \`items\``);
    }
  }
  if (!Array.isArray(raw.items)) {
    throw tcInvalid("missing field `items`");
  }
  return { items: raw.items.map(parseYearlyItem) };
};

/** yearly_publication::tasks::frozen — json! shape verbatim. */
export const frozen = (base, digest, years, intent) => ({
  base_daily_version_id: base, base_integrity_sha256: digest,
  years: [...years], intent,
});

export class YearlyContract {
  /** @param operation "update"|"backfill"|"republish" (market_update::tasks::Operation)
   *  @param repository MarketRepository-shaped object
   *  @param today () => "YYYY-MM-DD" — server Shanghai date, never input. */
  constructor(operation, repository, today) {
    this.operation = operation;
    this.repository = repository;
    this.today = today;
  }
  key() {
    // Operation::key() then yearly overrides dataset+contract_version.
    const op = {
      update: "update_data", backfill: "backfill_data",
      republish: "republish_data",
    }[this.operation];
    return {
      operation: op, dataset: DATASET, contract_version: CONTRACT_VERSION,
    };
  }
  normalize(raw) {
    const input = parseExplicitYears(raw);
    for (const item of input.items) {
      const dot = item.symbol.indexOf(".");
      const validSymbol = dot >= 0 &&
        ["sh", "sz"].includes(item.symbol.slice(0, dot)) &&
        item.symbol.slice(dot + 1).length === 6 &&
        /^[0-9]+$/.test(item.symbol.slice(dot + 1));
      if (item.comparison_version_id !== null &&
          item.comparison_version_id.trim() === "") {
        throw invalid("empty comparison version");
      }
      let validIntent = false;
      if (item.intent === "HistoricalFixedBase") {
        validIntent = true;
      } else if (intentVariant(item.intent) === "CurrentHead") {
        const b = intentBody(item.intent);
        validIntent =
          b.expected_daily_head === item.base_daily_version_id &&
          (item.comparison_version_id === null ||
           item.comparison_version_id === b.expected_yearly_head) &&
          (b.expected_yearly_head === null ||
           b.expected_yearly_head.trim() !== "");
      }
      if (!validSymbol || item.base_daily_version_id.trim() === "" ||
          item.years.length === 0 ||
          item.years.some((y) => y < 1 || y > 9999) || !validIntent) {
        throw invalid("explicit yearly identity/years/intent");
      }
    }
    input.items.sort((a, b) => a.symbol < b.symbol ? -1 : a.symbol > b.symbol ? 1 : 0);
    // BTreeSet dedup on years already applied; item dedup = full equality.
    input.items = input.items.filter((it, i) =>
      i === 0 || !itemEq(it, input.items[i - 1]));
    if (input.items.length === 0 || input.items.length > 100) {
      throw invalid("explicit yearly budget 1..100");
    }
    for (let i = 1; i < input.items.length; i++) {
      if (input.items[i - 1].symbol === input.items[i].symbol) {
        throw invalid("conflicting yearly bindings for same entity");
      }
    }
    // serde_json::to_value round-trip: years arrays already sorted/deduped.
    return { items: input.items.map(itemToValue) };
  }
  resolve(raw) {
    const input = parseExplicitYears(raw);
    const todayYear = Number(this.today().slice(0, 4));
    const future = input.items
      .filter((i) => i.years.some((y) => y > todayYear))
      .map((i) => i.symbol);
    if (future.length) {
      throw invalid(`future yearly write rejected: [${future.map((s) => `"${s}"`).join(", ")}]`);
    }
    const scopes = new Set();
    const items = [];
    for (const item of input.items) {
      const comparisonId = intentVariant(item.intent) === "CurrentHead"
        ? intentBody(item.intent).expected_yearly_head
        : item.comparison_version_id;
      const first = `${String(item.years[0]).padStart(4, "0")}-01-01`;
      const last = `${String(item.years[item.years.length - 1]).padStart(4, "0")}-12-31`;
      let base;
      try {
        base = this.repository.fixed_facts(item.base_daily_version_id);
      } catch (e) { throw repositoryError(e); }
      let available;
      try {
        available = this.repository.fixed_window_years(
          item.base_daily_version_id, first, last);
      } catch (e) { throw repositoryError(e); }
      let comparison;
      try {
        comparison = comparisonId === null ? null
          : this.repository.fixed_facts(comparisonId);
      } catch (e) { throw repositoryError(e); }
      if (comparison !== null &&
          (comparison.symbol !== item.symbol || comparison.period !== "Year")) {
        throw invalid("comparison yearly identity/period mismatch");
      }
      if (base.symbol !== item.symbol || base.period !== "Day" ||
          base.version_id !== item.base_daily_version_id) {
        throw invalid("fixed daily identity/period mismatch");
      }
      if (!item.years.every((y) => available.includes(y))) {
        throw tcNeedsVerification("selected year absent from fixed daily");
      }
      const scope = scopeId(item.symbol, "year");
      scopes.add(scope);
      const fi = frozen(item.base_daily_version_id, base.integrity_sha256,
        item.years, item.intent);
      fi.comparison = comparison === null ? null : {
        version_id: comparison.version_id,
        integrity_sha256: comparison.integrity_sha256,
      };
      items.push({
        item_id: scope, scope_keys: [scope],
        target: { symbol: item.symbol, period: "year" },
        progress_unit: "security_period", frozen_inputs: fi,
      });
    }
    return { items, scopes: [...scopes].sort(), shared_evidence: null };
  }
}
const itemEq = (a, b) =>
  a.symbol === b.symbol &&
  a.base_daily_version_id === b.base_daily_version_id &&
  a.comparison_version_id === b.comparison_version_id &&
  a.years.length === b.years.length &&
  a.years.every((y, i) => y === b.years[i]) &&
  valEq(a.intent, b.intent);
const itemToValue = (it) => ({
  symbol: it.symbol, base_daily_version_id: it.base_daily_version_id,
  years: it.years, intent: it.intent,
  comparison_version_id: it.comparison_version_id,
});

/** tasks.rs::validate_binding — verbatim frozen-inputs compare. */
export const validateBinding = (binding, command, candidate) => {
  const op = binding.head.key.operation;
  if (op !== "update_data" && op !== "backfill_data") return;
  const years = candidate.raw.local_yearly_derivation_v1?.evidence?.years;
  if (!Array.isArray(years)) {
    throw mIntegrity("missing field `years`");
  }
  const expected = frozen(
    candidate.base_daily_version_id, candidate.base_integrity_sha256,
    [...new Set(years)].sort((a, b) => a - b), command.intent);
  expected.comparison = binding.item.spec.frozen_inputs.comparison ?? null;
  // Generation mode stamps source_generation_id at accept; legacy tasks
  // leave the key absent — carry it through only when present.
  const stamped = binding.item.spec.frozen_inputs.source_generation_id;
  if (stamped !== undefined) expected.source_generation_id = stamped;
  if (binding.head.key.contract_version !== CONTRACT_VERSION ||
      binding.head.key.dataset !== DATASET ||
      binding.item.spec.item_id !== command.item_id ||
      !valEq(binding.item.spec.frozen_inputs, expected)) {
    throw mConflict("yearly publication differs from accepted frozen inputs");
  }
};
