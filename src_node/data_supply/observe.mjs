// src/data_supply/observe.rs — Observer seam: the pre-repository
// validation + in-memory cursor cache are ported verbatim; every
// repository read terminates at RepositorySeam (S12c).
//
// Cursor semantics: Saved entries live in a per-instance deque capped at
// 32; any token unknown to this process -> "invalid/expired cursor"; a
// key/access mismatch -> "cursor conditions/authorization changed".

import { invalid } from "./access.mjs";
import { pageSize, QueryPlan } from "./query.mjs";
import { serValue } from "../mcp/serde_json.mjs";

export class Observer {
  constructor(repository, runId) {
    this.repository = repository;
    this.run_id = runId;
    this.cache = []; // VecDeque<Saved>
    this.sequence = 0;
    this.instance = `obs-${process.pid}-${performance.now() * 1e6 | 0}`;
  }
  id() {
    return `${this.instance}-${++this.sequence}`;
  }
  resume(token, key, access) {
    const s = this.cache.find((x) => x.token === token);
    if (!s) throw invalid("invalid/expired cursor");
    if (s.key !== key || s.access !== access)
      throw invalid("cursor conditions/authorization changed");
    return s;
  }
  save(s) {
    const token = this.id();
    s.token = token;
    if (this.cache.length === 32) this.cache.shift();
    this.cache.push(s);
    return token;
  }
  // observe.rs:258 — page_size -> inverted range -> dataset require ->
  // cursor/repo seam.
  tasks(args, access) {
    pageSize(args.page_size);
    if (args.created_from !== null && args.created_to !== null &&
        args.created_from > args.created_to)
      throw invalid("inverted task time range");
    if (args.dataset !== null) access.require(args.dataset, null);
    const cursor = args.cursor;
    if (cursor !== null)
      this.resume(cursor, `tasks:${serValue({ ...args, cursor: null })}`, access);
    this.repository.tasks(access); // seam — throws typed error
  }
  // observe.rs:318 — page_size -> cursor/repo seam.
  progress(args, access) {
    pageSize(args.page_size);
    if (args.cursor !== null)
      this.resume(args.cursor, `progress:${serValue({ ...args, cursor: null })}`, access);
    this.repository.task(args.task_id, access); // seam
  }
  // observe.rs:440 — cursor take, then QueryPlan::new, then seam.
  availability(args, access) {
    const cursor = args.cursor;
    args.cursor = null;
    const plan = new QueryPlan(args, access);
    if (cursor !== null)
      this.resume(cursor, `availability:${serValue(plan.args)}`, access);
    this.repository.availability(plan, access); // seam
  }
}
