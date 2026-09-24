// NM-056 — sqlite_market/collection.rs (+ compact_repo impl): the
// DailyCollectionRepository read surface. transition_task delegates to
// the coordinator-bound task handle at the call site.
import { MarketError } from "./record_codec.mjs";
import { head, isCompactSchema } from "./market_read.mjs";
import { readPublication } from "./compact_write.mjs";

const integrity = (m) => new MarketError("integrity", `MARKET_INTEGRITY: ${m}`);

const parseReceipt = (s) => {
  let c;
  try { c = JSON.parse(s); } catch (e) { throw integrity(e.message); }
  if (c === null || typeof c !== "object" || Array.isArray(c)) {
    throw integrity("missing field `publication_id`");
  }
  for (const k of ["publication_id", "version_id", "task_id"]) {
    if (typeof c[k] !== "string") throw integrity(`missing field \`${k}\``);
  }
  if (typeof c.task_revision !== "number" || !Number.isInteger(c.task_revision)) {
    throw integrity("missing field `task_revision`");
  }
  return c;
};

/** collection_receipt — legacy reads publication_batches.receipt_json;
 *  compact resolves publication_results kind=1 -> stored publication. */
export const collectionReceipt = (db, task, item) => {
  if (isCompactSchema(db)) {
    const row = db.get(
      "SELECT publication_id FROM publication_results WHERE task_id=? AND item_id=? AND publication_kind=1",
      task, item);
    if (!row) return null;
    const stored = readPublication(db, row.publication_id);
    return {
      publication_id: stored.identity.publication_id,
      version_id: stored.identity.result_version_id,
      task_id: stored.identity.task_id,
      task_revision: Number(stored.identity.task_revision),
    };
  }
  const row = db.get(
    "SELECT receipt_json FROM publication_batches WHERE task_id=? AND item_id=?",
    task, item);
  if (!row) return null;
  return parseReceipt(row.receipt_json);
};

export const effectiveHead = (db, scope) => head(db, scope);
