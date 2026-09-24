// src/data_supply/mod.rs Access — principal + dataset + optional
// per-security allowlist. securities === null grants all local
// securities; an empty set grants none.

import { DATASETS } from "./wire.mjs";

export class StorageError extends Error {
  // Mirrors market_storage::Error Display + the mapped() code surface.
  constructor(kind, text) {
    super(text);
    this.kind = kind; // storage|integrity|base|conflict|notfound|invalid|outcome|task|...
  }
  toString() {
    const tag = {
      storage: "MARKET_STORAGE", integrity: "MARKET_INTEGRITY",
      base: "BASE_UNAVAILABLE", conflict: "PUBLISH_PRECONDITION",
      notfound: "MARKET_NOT_FOUND", invalid: "INVALID_MARKET_COMMAND",
      outcome: "PUBLISH_OUTCOME_UNKNOWN",
      basis: "GAP_BASIS_NOT_VERIFIED",
    }[this.kind];
    if (this.kind === "notfound") return tag;
    if (this.kind === "outcome")
      return `${tag}: ${this.message}; inspect the same publication_id`;
    return `${tag}: ${this.message}`;
  }
}
export const invalid = (s) => new StorageError("invalid", s);

export class Access {
  constructor({ principal, datasets, securities = null }) {
    this.principal = principal;
    this.datasets = datasets; // Set<dataset>
    this.securities = securities; // Set<"exchange.code"> key form, or null
  }
  static local() {
    return new Access({
      principal: "local-owner",
      datasets: new Set(DATASETS),
      securities: null,
    });
  }
  // BTreeSet<Security> membership — compare via canonical symbol form.
  #securityKey(e) { return `${e.exchange}:${e.code}`; }
  allows(dataset, entity) {
    if (!this.datasets.has(dataset)) return false;
    if (entity == null || this.securities === null) return true;
    return [...this.securities].some(
      (e) => this.#securityKey(e) === this.#securityKey(entity));
  }
  require(dataset, entity) {
    if (!this.allows(dataset, entity)) throw invalid("forbidden");
  }
  equalsLocal() {
    return this.principal === "local-owner" &&
      this.securities === null &&
      this.datasets.size === DATASETS.length &&
      DATASETS.every((d) => this.datasets.has(d));
  }
}
