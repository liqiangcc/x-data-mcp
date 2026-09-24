// NM-056 — sqlite_market/gaps.rs verbatim port: append-only proven-gap
// facts. Stores opaque evidence identity; never decides which gaps
// exist. record_* run inside ONE BEGIN IMMEDIATE transaction
// (write_transaction parity — SqliteMarket::write / compact `tx`).
import { createHash } from "node:crypto";
import { MarketError } from "./record_codec.mjs";
import { head as headVersionId } from "./market_read.mjs";
import { parseRfc3339Utc } from "./compact_market.mjs";

const sha256 = (s) => createHash("sha256").update(s).digest("hex");
const isSha256 = (s) =>
  typeof s === "string" && s.length === 64 && /^[0-9a-f]+$/.test(s);
const integrity = (m) => new MarketError("integrity", `MARKET_INTEGRITY: ${m}`);
const invalid = (m) => new MarketError("invalid", `INVALID_MARKET_COMMAND: ${m}`);
const storage = (m) => new MarketError("storage", `MARKET_STORAGE: ${m}`);
const outcomeUnknown = (m) => new MarketError("outcome_unknown", `PUBLISH_OUTCOME_UNKNOWN: ${m}; inspect the same publication_id`);

export const CONFIRMED_MISSING_RANGE = "confirmed_missing_range";
export const GAP_DECISION_EVIDENCE_KIND = "gap_verification_decision";

// ---- value types: validate() verbatim ----
const blank = (s) => typeof s !== "string" || s.trim() === "";
export const validateDecision = (d) => {
  if (blank(d.decision_id) || blank(d.symbol) || d.period !== "Day" ||
      blank(d.subject_version_id) || !isSha256(d.subject_version_digest) ||
      blank(d.conclusion) || blank(d.basis_kind) || blank(d.basis_ref) ||
      !isSha256(d.basis_sha256) || blank(d.verifier_id) ||
      !isSha256(d.decision_sha256) || d.start_date > d.end_date) {
    throw invalid("invalid gap verification decision");
  }
};
export const validateGap = (g) => {
  if (blank(g.gap_id) || blank(g.symbol) || blank(g.evidence_kind) ||
      blank(g.evidence_id) || !isSha256(g.evidence_digest) ||
      g.start_date > g.end_date) {
    throw invalid("invalid proven gap fact");
  }
};
export const validateCovering = (cv) => {
  if (blank(cv.gap_id) || blank(cv.version_id)) {
    throw invalid("invalid gap covering");
  }
};
// gap-decision-v1 domain-separated canonical digest (serde_json::json!
// array to_string -> sha256 hex)
export const decisionContentDigest = (d) =>
  sha256(JSON.stringify([
    "gap-decision-v1", d.decision_id, d.symbol,
    d.period === "Day" ? "day" : "year",
    d.start_date, d.end_date, d.subject_version_id,
    d.subject_version_digest, d.conclusion, d.basis_kind, d.basis_ref,
    d.basis_sha256, d.verifier_id, canonTs(d.decided_at)]));
// derive_gap_id:
export const deriveGapId = (d) =>
  sha256(JSON.stringify([
    "gap-v1", d.symbol, d.period === "Day" ? "day" : "year",
    d.start_date, d.end_date, d.subject_version_id,
    d.subject_version_digest]));
const periodText = (p) => p === "Day" ? "day" : "year";

// ---- row decoders: stored rows are mechanically re-validated ----
const parseDate = (s, what) => {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(s)) throw integrity(`unreadable ${what}`);
  return s;
};
const parseTs = (s, what) => {
  try { return parseRfc3339Utc(s); }
  catch { throw integrity(`unreadable ${what}`); }
};
const decisionEq = (a, b) =>
  a.decision_id === b.decision_id && a.symbol === b.symbol &&
  a.period === b.period && a.start_date === b.start_date &&
  a.end_date === b.end_date && a.subject_version_id === b.subject_version_id &&
  a.subject_version_digest === b.subject_version_digest &&
  a.conclusion === b.conclusion && a.basis_kind === b.basis_kind &&
  a.basis_ref === b.basis_ref && a.basis_sha256 === b.basis_sha256 &&
  a.verifier_id === b.verifier_id && a.decided_at === b.decided_at &&
  a.decision_sha256 === b.decision_sha256;
const gapEq = (a, b) =>
  a.gap_id === b.gap_id && a.symbol === b.symbol && a.period === b.period &&
  a.start_date === b.start_date && a.end_date === b.end_date &&
  a.evidence_kind === b.evidence_kind && a.evidence_id === b.evidence_id &&
  a.evidence_digest === b.evidence_digest && a.observed_at === b.observed_at;

const decisionRow = (r) => {
  const period = r.period === "day" ? "Day"
    : (() => { throw integrity(`unreadable decision period ${JSON.stringify(r.period)}`); })();
  const decision = {
    decision_id: r.decision_id, symbol: r.symbol, period,
    start_date: parseDate(r.start_date, "decision start_date"),
    end_date: parseDate(r.end_date, "decision end_date"),
    subject_version_id: r.subject_version_id,
    subject_version_digest: r.subject_version_digest,
    conclusion: r.conclusion, basis_kind: r.basis_kind,
    basis_ref: r.basis_ref, basis_sha256: r.basis_sha256,
    verifier_id: r.verifier_id,
    decided_at: parseTs(r.decided_at, "decision decided_at"),
    decision_sha256: r.decision_sha256,
  };
  try { validateDecision(decision); }
  catch { throw integrity("stored gap decision violates invariants"); }
  return decision;
};
const gapRow = (r) => {
  const period = r.period === "day" ? "Day" : r.period === "year" ? "Year"
    : (() => { throw integrity(`unreadable gap period ${JSON.stringify(r.period)}`); })();
  const gap = {
    gap_id: r.gap_id, symbol: r.symbol, period,
    start_date: parseDate(r.start_date, "gap start_date"),
    end_date: parseDate(r.end_date, "gap end_date"),
    evidence_kind: r.evidence_kind, evidence_id: r.evidence_id,
    evidence_digest: r.evidence_digest,
    observed_at: parseTs(r.observed_at, "gap observed_at"),
  };
  try { validateGap(gap); }
  catch { throw integrity("stored proven gap violates invariants"); }
  return gap;
};
const coveringRow = (r) => {
  const covering = {
    gap_id: r.gap_id, version_id: r.version_id,
    covered_at: parseTs(r.covered_at, "covering covered_at"),
  };
  try { validateCovering(covering); }
  catch { throw integrity("stored gap covering violates invariants"); }
  return covering;
};

const DECISION_COLS =
  "decision_id,symbol,period,start_date,end_date,subject_version_id," +
  "subject_version_digest,conclusion,basis_kind,basis_ref,basis_sha256," +
  "verifier_id,decided_at,decision_sha256";
const GAP_COLS =
  "gap_id,symbol,period,start_date,end_date,evidence_kind,evidence_id," +
  "evidence_digest,observed_at";

export const readDecision = (db, decisionId) => {
  const r = db.get(
    `SELECT ${DECISION_COLS} FROM gap_verification_decisions WHERE decision_id=?`,
    decisionId);
  return r ? decisionRow(r) : null;
};
export const readGap = (db, gapId) => {
  const r = db.get(
    `SELECT ${GAP_COLS} FROM proven_gaps WHERE gap_id=?`, gapId);
  return r ? gapRow(r) : null;
};
const readCovering = (db, gapId, versionId) => {
  const r = db.get(
    "SELECT gap_id,version_id,covered_at FROM gap_coverings WHERE gap_id=? AND version_id=?",
    gapId, versionId);
  return r ? coveringRow(r) : null;
};
export const readOpenGaps = (db, symbol, period) =>
  db.query(
    `SELECT ${GAP_COLS} FROM proven_gaps g WHERE g.symbol=? AND g.period=? AND NOT EXISTS(SELECT 1 FROM gap_coverings c WHERE c.gap_id=g.gap_id) ORDER BY g.start_date,g.end_date,g.gap_id`,
    symbol, periodText(period)).map(gapRow);
export const gapCoverings = (db, gapId) =>
  db.query(
    "SELECT gap_id,version_id,covered_at FROM gap_coverings WHERE gap_id=? ORDER BY covered_at,version_id",
    gapId).map(coveringRow);

const sqlRun = (db, sql, ...p) => {
  try { return db.run(sql, ...p); } catch (e) { throw storage(e.message); }
};
const insertDecision = (db, d) => {
  sqlRun(db,
    "INSERT INTO gap_verification_decisions VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
    d.decision_id, d.symbol, periodText(d.period), d.start_date, d.end_date,
    d.subject_version_id, d.subject_version_digest, d.conclusion,
    d.basis_kind, d.basis_ref, d.basis_sha256, d.verifier_id,
    d.decided_at, d.decision_sha256);
};
const insertGap = (db, g) => {
  sqlRun(db,
    "INSERT INTO proven_gaps VALUES(?,?,?,?,?,?,?,?,?)",
    g.gap_id, g.symbol, periodText(g.period), g.start_date, g.end_date,
    g.evidence_kind, g.evidence_id, g.evidence_digest, g.observed_at);
};

// record_gap_decision_tx — shared by both backends; the subject-head
// guard resolves through the schema-aware `head`.
const canonTs = (v) => parseRfc3339Utc(v);

export const recordGapDecisionTx = (db, decision, guardSubjectHead) => {
  validateDecision(decision);
  decision = { ...decision, decided_at: canonTs(decision.decided_at) };
  const gap = {
    gap_id: deriveGapId(decision), symbol: decision.symbol,
    period: decision.period, start_date: decision.start_date,
    end_date: decision.end_date,
    evidence_kind: GAP_DECISION_EVIDENCE_KIND,
    evidence_id: decision.decision_id,
    evidence_digest: decision.decision_sha256,
    observed_at: decision.decided_at,
  };
  validateGap(gap);
  let inserted;
  try {
    insertDecision(db, decision);
    inserted = true;
  } catch (insert) {
    const existing = readDecision(db, decision.decision_id);
    if (existing !== null && decisionEq(existing, decision)) {
      inserted = false;
    } else if (existing !== null) {
      throw integrity("gap decision identity conflict");
    } else {
      throw insert;
    }
  }
  // The stale-head guard gates only a NEW commit; an identical
  // already-committed decision replays as a no-op even if the head has
  // since moved.
  if (guardSubjectHead && inserted) {
    if (headVersionId(db, { symbol: decision.symbol, period: decision.period }) !==
        decision.subject_version_id) {
      throw integrity("gap decision subject head moved");
    }
  }
  if (decision.conclusion === CONFIRMED_MISSING_RANGE) {
    try {
      insertGap(db, gap);
      return true;
    } catch (insert) {
      const existing = readGap(db, gap.gap_id);
      if (existing !== null && gapEq(existing, gap)) return inserted;
      if (existing !== null) throw integrity("proven gap identity conflict");
      throw insert;
    }
  }
  return inserted;
};

// write_transaction parity: BEGIN IMMEDIATE + COMMIT (commit failure ->
// OutcomeUnknown) + ROLLBACK on error; inner MarketError propagates.
const writeTx = (db, f) => {
  db.exec("BEGIN IMMEDIATE");
  try {
    const r = f();
    try { db.exec("COMMIT"); } catch (e) { throw outcomeUnknown(e.message); }
    return r;
  } catch (e) {
    try { db.exec("ROLLBACK"); } catch { /* rolled back */ }
    throw e;
  }
};

export const recordGapDecision = (db, d) =>
  writeTx(db, () => recordGapDecisionTx(db, d, false));
export const recordGapDecisionGuarded = (db, d) =>
  writeTx(db, () => recordGapDecisionTx(db, d, true));
export const gapVerificationDecision = (db, id) => readDecision(db, id);

export const recordProvenGap = (db, gap) => {
  validateGap(gap);
  gap = { ...gap, observed_at: canonTs(gap.observed_at) };
  return writeTx(db, () => {
    try {
      insertGap(db, gap);
      return true;
    } catch (insert) {
      const existing = readGap(db, gap.gap_id);
      if (existing !== null && gapEq(existing, gap)) return false;
      if (existing !== null) throw integrity("proven gap identity conflict");
      throw insert;
    }
  });
};

// cover_gap — legacy arm (schema-18 rows: state='published' OR
// publication_batches link); compact arm (state=3 OR task_publications).
export const coverGap = (db, covering, compact = false) => {
  validateCovering(covering);
  covering = { ...covering, covered_at: canonTs(covering.covered_at) };
  return writeTx(db, () => {
    const r = db.get(
      `SELECT ${GAP_COLS} FROM proven_gaps WHERE gap_id=?`, covering.gap_id);
    const gap = r === undefined ? null : compact
      // compact_repo::cover_gap decodes inline — parse failures surface
      // as rusqlite InvalidQuery -> storage; no stored-row re-validation.
      ? (() => {
          const dt = (v) => { try { return parseRfc3339Utc(v); }
            catch { throw storage("InvalidQuery"); } };
          const dy = (v) => { if (!/^\d{4}-\d{2}-\d{2}$/.test(v)) throw storage("InvalidQuery"); return v; };
          return {
            gap_id: r.gap_id, symbol: r.symbol,
            period: r.period === "day" ? "Day" : "Year",
            start_date: dy(r.start_date), end_date: dy(r.end_date),
            evidence_kind: r.evidence_kind, evidence_id: r.evidence_id,
            evidence_digest: r.evidence_digest, observed_at: dt(r.observed_at),
          };
        })()
      : gapRow(r);
    if (gap === null) throw invalid("covering references unknown gap");
    const scope = compact
      ? db.get(
          "SELECT v.entity AS symbol, v.period AS pcode FROM data_versions v WHERE v.version_id=? AND (v.state=3 OR EXISTS(SELECT 1 FROM task_publications b WHERE b.version_key=v.version_key))",
          covering.version_id)
      : db.get(
          "SELECT v.symbol, v.period FROM data_versions v WHERE v.version_id=? AND (v.state='published' OR EXISTS(SELECT 1 FROM publication_batches b WHERE b.version_id=v.version_id))",
          covering.version_id);
    if (!scope) {
      throw invalid("covering references unknown or unpublished version");
    }
    const symbolOk = scope.symbol === gap.symbol;
    const periodOk = compact
      ? scope.pcode === (gap.period === "Day" ? 1n : 2n)
      : scope.period === periodText(gap.period);
    if (!symbolOk || !periodOk) throw invalid("covering version scope mismatch");
    if (compact) {
      const at = db.get(
        "SELECT covered_at FROM gap_coverings WHERE gap_id=? AND version_id=?",
        covering.gap_id, covering.version_id);
      if (at !== undefined) {
        if (at.covered_at === covering.covered_at) return false;
        throw integrity("gap covering identity conflict");
      }
    } else {
      const existing = readCovering(db, covering.gap_id, covering.version_id);
      if (existing !== null) {
        if (coveringEq(existing, covering)) return false;
        throw integrity("gap covering identity conflict");
      }
    }
    sqlRun(db, "INSERT INTO gap_coverings VALUES(?,?,?)",
      covering.gap_id, covering.version_id, covering.covered_at);
    return true;
  });
};
const coveringEq = (a, b) =>
  a.gap_id === b.gap_id && a.version_id === b.version_id &&
  a.covered_at === b.covered_at;

export const provenGap = (db, gapId) => readGap(db, gapId);
export const openProvenGaps = (db, symbol, period) =>
  readOpenGaps(db, symbol, period);
