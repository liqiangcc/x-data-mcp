// src/data_supply/query.rs — the pure, pre-repository validation surface
// (QueryPlan::new / entities / page_size) plus the Queries seam: every
// repository read terminates at RepositorySeam (S12c lands SqliteSupply).
//
// Validation order is load-bearing for error parity:
//   QueryPlan::new = access.require(dataset) -> page_size -> entities()
//     -> filter/dataset range match -> version choice -> batch normalize
//   versions()     = access.require -> page_size -> entities() -> seam
//   query()        = QueryPlan::new -> cursor resume -> repository.query
//   (both cursor and repository paths hit the seam in the skeleton)

import { invalid } from "./access.mjs";
import {
  DATASET_CATALOG, DATASET_DAILY, DATASET_YEARLY,
  securityCmp, securityValidate,
} from "./wire.mjs";
import { serValue } from "../mcp/serde_json.mjs";

export const MAX_ROWS = 100_000;
export const MAX_BYTES = 16 * 1024 * 1024;

export function pageSize(n) {
  if (!(n >= 1 && n <= 1000)) throw invalid("page_size must be 1..1000");
}

// entities(dataset, selection, access) — query.rs:75
export function entities(dataset, s, access) {
  let e;
  if (dataset === DATASET_CATALOG && s.kind === "catalog") e = [];
  else if ((dataset === DATASET_DAILY || dataset === DATASET_YEARLY) &&
           s.kind === "single") e = [s.entity];
  else if ((dataset === DATASET_DAILY || dataset === DATASET_YEARLY) &&
           s.kind === "batch") e = [...s.entities];
  else throw invalid("invalid selection for dataset");
  if (dataset !== DATASET_CATALOG && (e.length === 0 || e.length > 100))
    throw invalid("selection must contain 1..100 entities");
  e.sort(securityCmp);
  for (let i = 1; i < e.length; i++)
    if (securityCmp(e[i - 1], e[i]) === 0) throw invalid("duplicate query entity");
  for (const x of e) {
    const bad = securityValidate(x);
    if (bad) throw invalid(bad);
    access.require(dataset, x);
  }
  return e;
}

// QueryPlan::new — query.rs:16 (decoded Query uses the wire tag name as
// the variant key: filter.kind, selection.kind, version.kind).
export class QueryPlan {
  constructor(args, access) {
    access.require(args.dataset, null);
    pageSize(args.page_size);
    const es = entities(args.dataset, args.selection, access);
    const f = args.filter, d = args.dataset;
    const ok =
      (f.kind === "daily" && d === DATASET_DAILY &&
        f.start_date <= f.end_date &&
        f.start_date >= "0001-01-01" && f.end_date <= "9999-12-31") ||
      (f.kind === "yearly" && d === DATASET_YEARLY &&
        f.start_year >= 1 && f.end_year >= f.start_year && f.end_year <= 9999) ||
      (f.kind === "catalog" && d === DATASET_CATALOG);
    if (!ok) throw invalid("dataset/filter range mismatch");
    const v = args.version;
    if (v.kind === "latest_per_entity") {
      // always valid
    } else if (v.kind === "catalog_version") {
      if (!(d === DATASET_CATALOG && v.version_id !== ""))
        throw invalid("invalid version selection");
    } else if (v.kind === "explicit_version_map") {
      if (d === DATASET_CATALOG) throw invalid("invalid version selection");
      v.versions.sort((a, b) => securityCmp(a.entity, b.entity));
      if (v.versions.map((x) => x.entity).length !== es.length ||
          v.versions.some((x, i) => securityCmp(x.entity, es[i]) !== 0) ||
          v.versions.some((x) => x.version_id === ""))
        throw invalid("explicit version map must match selection exactly");
    } else {
      throw invalid("invalid version selection");
    }
    const mutArgs = args;
    if (mutArgs.selection.kind === "batch") mutArgs.selection.entities = es;
    this.args = mutArgs;
    this.entities = es;
  }
}

// Queries::query/versions — the repository half terminates at the seam.
export class Queries {
  constructor(repository, observer) {
    this.repository = repository;
    this.observer = observer; // shared cursor cache lives on Observer
  }
  query(args, access) {
    const plan = new QueryPlan(args, access);
    if (plan.args.cursor !== null)
      this.observer.resume(plan.args.cursor, `query:${serValue({ ...plan.args, cursor: null })}`, access);
    this.repository.query(plan, access); // seam — throws typed error
  }
  versions(args, access) {
    access.require(args.dataset, null);
    pageSize(args.page_size);
    entities(args.dataset, args.selection, access);
    if (args.cursor !== null)
      this.observer.resume(args.cursor, `versions:${serValue({ ...args, cursor: null })}`, access);
    this.repository.versions(args, access); // seam
  }
}
