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

// Access PartialEq — principal + dataset set + security set (Ord over
// the canonical exchange:code key).
const accessEq = (a, b) => {
  if (a.principal !== b.principal) return false;
  const sKey = (s) =>
    s === null ? null : [...s].map((e) => `${e.exchange}:${e.code}`).sort();
  const setEq = (x, y) =>
    x.size === y.size && [...x].every((v) => y.has(v));
  const as = sKey(a.securities), bs = sKey(b.securities);
  return setEq(a.datasets, b.datasets) &&
    (as === null ? bs === null
      : bs !== null && as.length === bs.length && as.every((v, i) => v === bs[i]));
};

// serde DateTime<Utc> to_rfc3339_opts(AutoSi, use_z=true): ".000Z" drops
// the sub-second part.
const serTs = (d) => {
  const iso = d.toISOString();
  return iso.endsWith(".000Z") ? iso.slice(0, -5) + "Z" : iso;
};
const byteLen = (v) => Buffer.byteLength(serValue(v), "utf8");

// Queries<R: ReadRepository> — query.rs:122. Own Saved cache (separate
// from Observer's): session-bound, LRU-capped at 32, keyed on the
// request shape + access.
export class Queries {
  constructor(repository) {
    this.repository = repository;
    this.cache = []; // VecDeque<Saved>
    this.sequence = 0;
    this.instance = `${process.pid}-${Date.now() * 1e6 | 0}`;
  }
  id() {
    return `${this.instance}-${++this.sequence}`;
  }
  save(s) {
    const token = this.id();
    s.token = token;
    if (this.cache.length === 32) this.cache.shift();
    this.cache.push(s);
    return token;
  }
  resume(token, key, access) {
    const s = this.cache.find((x) => x.token === token);
    if (!s) throw invalid("invalid/expired cursor");
    if (s.key !== key || !accessEq(s.access, access))
      throw invalid("cursor conditions/authorization changed");
    return s;
  }
  // query.rs:163 — plan -> cursor key -> resume-or-read -> verify ->
  // page window.
  query(args, access) {
    const plan = new QueryPlan(args, access);
    const cursor = plan.args.cursor;
    const key = `query:${serValue({ ...plan.args, cursor: null })}`;
    let saved;
    if (cursor !== null) {
      saved = this.resume(cursor, key, access);
    } else {
      const data = this.repository.query(plan, access);
      if (data.rows.length > MAX_ROWS || byteLen(data) > MAX_BYTES)
        throw invalid("snapshot budget exceeded");
      saved = { token: "", key, access, time: new Date(),
        id: this.id(), offset: 0, content: { kind: "data", data } };
    }
    if (saved.content.kind !== "data")
      throw invalid("cursor category mismatch");
    const data = saved.content.data;
    this.repository.verify(
      data.manifest.filter((m) => m.version_ref !== null)
        .map((m) => m.version_ref));
    const end = Math.min(saved.offset + plan.args.page_size, data.rows.length);
    const records = data.rows.slice(saved.offset, end);
    const manifest = data.manifest;
    const more = end < data.rows.length;
    saved.offset = end;
    const result = {
      dataset: plan.args.dataset,
      contract_version: "v1",
      snapshot_id: saved.id,
      query_time: serTs(saved.time),
      version_manifest: manifest,
      records,
      next_cursor: null,
      page_complete: !more,
      selection_complete: true,
      limitations: [
        "fixed version selection is not a shared source acquisition time",
        "declared scope: owner personal research, local-only (#71 comment-5749934485); coverage is not inferred from dates",
      ],
    };
    if (more) result.next_cursor = this.save(saved);
    return result;
  }
  // query.rs:217 — same flow over the versions list.
  versions(args, access) {
    access.require(args.dataset, null);
    pageSize(args.page_size);
    entities(args.dataset, args.selection, access);
    const cursor = args.cursor;
    const key = `versions:${serValue({ ...args, cursor: null })}`;
    let saved;
    if (cursor !== null) {
      saved = this.resume(cursor, key, access);
    } else {
      // cursor already taken — the repository sees the cursor-less args.
      const data = this.repository.versions({ ...args, cursor: null }, access);
      if (data.length > MAX_ROWS || byteLen(data) > MAX_BYTES)
        throw invalid("snapshot budget exceeded");
      saved = { token: "", key, access, time: new Date(),
        id: this.id(), offset: 0, content: { kind: "versions", data } };
    }
    if (saved.content.kind !== "versions")
      throw invalid("cursor category mismatch");
    const data = saved.content.data;
    this.repository.verify(data.map((v) => v.version_ref));
    const end = Math.min(saved.offset + args.page_size, data.length);
    const items = data.slice(saved.offset, end);
    const more = end < data.length;
    saved.offset = end;
    const result = {
      snapshot_id: saved.id,
      snapshot_time: serTs(saved.time),
      items,
      next_cursor: null,
      page_complete: !more,
    };
    if (more) result.next_cursor = this.save(saved);
    return result;
  }
}

// query.rs:275 unique_refs — BTreeSet dedup by (dataset, version_id).
export const uniqueRefs = (refs) => {
  const seen = new Set();
  const out = [];
  for (const r of refs) {
    const k = `${r.dataset}${r.version_id}`;
    if (!seen.has(k)) { seen.add(k); out.push(r); }
  }
  return out;
};
