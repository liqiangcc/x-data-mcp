// NM-076 — gap_verification app-layer probe (S10a).
// Golden-vector verification mirroring src/gap_verification/tests.rs
// 1:1 — the Rust suite is the semantic oracle (no binary exists for
// this layer; `cargo test gap_verification` must stay green). Every
// check asserts the same end-state the Rust test asserts: stored
// rows, task/item fields, error class + message prefix.
//
// Usage:
//   node gap_verification_probe.mjs [--selftest]
//   --selftest injects an intentional FAIL -> must exit 1.
import { copyFileSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import { openDatabase, StorageError } from "../../src_node/storage/sqlite_adapter.mjs";
import { MarketError } from "../../src_node/market/record_codec.mjs";
import * as GAPS from "../../src_node/market/gaps.mjs";
import * as M from "../../src_node/gap_verification/mod.mjs";
import * as C from "../../src_node/gap_verification/contract.mjs";
import * as R from "../../src_node/gap_verification/runner.mjs";
import { GapRepository } from "../../src_node/gap_verification/store.mjs";
import { stage } from "../../src_node/market/market_write.mjs";
import { buildMarketFixture, acceptGapTask, ensureSkeleton, NOW } from "./gap_fixture.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const REPO = join(here, "..", "..");

const SELFTEST = process.argv.includes("--selftest");
const OUT = process.argv.includes("--out")
  ? process.argv[process.argv.indexOf("--out") + 1] : null;
const FIX = mkdtempSync(join(tmpdir(), "nm076-"));
const SKELETON = ensureSkeleton(FIX);
process.on("exit", () => rmSync(FIX, { recursive: true, force: true }));

const results = [];
const emit = (id, status, detail) => {
  results.push({ family: "gap_verification", id, status, detail });
  console.log(JSON.stringify(results[results.length - 1]));
};
// Canonical equality: BTreeMap key-order-insensitive deep compare
// (frozen_inputs reads back key-sorted — assert content, not order).
const canon = (v) => v === null || typeof v !== "object" ? v :
  Array.isArray(v) ? v.map(canon) :
  Object.fromEntries(Object.keys(v).sort().map((k) => [k, canon(v[k])]));
const eq = (a, b) => JSON.stringify(canon(a)) === JSON.stringify(canon(b));
const run = (id, fn) => {
  try { fn(); emit(id, "PASS", ""); }
  catch (e) { emit(id, "FAIL", `${e.message}`); }
};
const sha256 = (s) => createHash("sha256").update(s).digest("hex");

// ---- fixture verifier (tests.rs FixtureVerifier verbatim) ----------
class FixtureVerifier {
  constructor(o = {}) {
    this.id = o.id ?? "fixture-verifier-1";
    this.conclusion = o.conclusion ?? "confirmed_missing_range";
    this.claimId = o.claimId ?? null;
    this.badDigest = o.badDigest ?? false;
    this.wrongSubject = o.wrongSubject ?? false;
    this.wrongTarget = o.wrongTarget ?? false;
  }
  verifierId() { return this.id; }
  verify(target, subject) {
    let end = target.end_date;
    if (this.wrongTarget) {
      const d = new Date(Date.UTC(+end.slice(0, 4), +end.slice(5, 7) - 1, +end.slice(8, 10) + 1));
      end = d.toISOString().slice(0, 10);
    }
    const basis = {
      source_identity: "fixture-source",
      source_contract: "fixture-source-v1",
      symbol: this.wrongTarget ? "sz.000001" : target.symbol,
      start_date: target.start_date,
      end_date: end,
      subject_version_id: this.wrongSubject ? "other-version" : subject.version_id,
      subject_version_digest: subject.integrity_sha256,
      artifact_identity: "fixture:source-facts",
      artifact_sha256: "a".repeat(64),
      conclusion: this.conclusion,
      observed_at: "2026-09-15T00:00:00Z",
      verifier_id: this.claimId ?? this.id,
      basis_sha256: "",
    };
    basis.basis_sha256 = M.basisContentDigest(basis);
    if (this.badDigest) basis.basis_sha256 = "0".repeat(64);
    return basis;
  }
}
const fixture = () => new FixtureVerifier();
const TARGET = { symbol: "sh.600006", start_date: "2025-01-01", end_date: "2025-01-05" };
const GAP_PAYLOAD = { target: { symbol: "sh.600006", period: "day",
  start_date: "2025-01-01", end_date: "2025-01-05" } };
const DECIDED_AT = "2026-09-15T01:00:00Z";

const fixtureRepo = (name) => {
  const path = join(FIX, `${name}.db`);
  const f = buildMarketFixture(path, SKELETON);
  return { ...f, repo: new GapRepository(f.db, f.adm) };
};
const counts = (db) => [
  Number(db.get("SELECT COUNT(*) c FROM gap_verification_decisions").c),
  Number(db.get("SELECT COUNT(*) c FROM proven_gaps").c),
];
const errClass = (e) =>
  e instanceof MarketError ? `market:${e.kind}` :
  e instanceof StorageError ? `storage:${e.code}` : `other:${e.name}`;
const expectErr = (fn) => {
  try { fn(); return null; } catch (e) { return e; }
};

// ---- A. decide_gap unit cases (tests.rs:87-271) --------------------

// official_stub_fails_closed_with_basis_not_verified
run("A1_stub_basis_not_verified", () => {
  const { db, repo } = fixtureRepo("a1");
  const subject = repo.freezeGapSubject(TARGET);
  const e = expectErr(() =>
    M.decideGap(repo, new M.UnverifiedGapVerifier(), TARGET, subject, "d1", DECIDED_AT));
  if (!(e instanceof MarketError) || e.kind !== "basis_not_verified")
    throw new Error(`want basis_not_verified, got ${errClass(e)} ${e?.message}`);
  if (e.message !== "GAP_BASIS_NOT_VERIFIED: no installed trusted source-gap verifier (#54/#46/#71)")
    throw new Error(`text: ${e.message}`);
  if (!eq(counts(db), [0, 0])) throw new Error(`counts ${counts(db)}`);
});

// fixture_verifier_commits_decision_and_gap_atomically
run("A2_fixture_commit_atomic", () => {
  const { db, repo } = fixtureRepo("a2");
  const subject = repo.freezeGapSubject(TARGET);
  if (subject.version_id !== "legacy-daily") throw new Error("subject version");
  if (!/^[0-9a-f]{64}$/.test(subject.integrity_sha256)) throw new Error("digest shape");
  if (!M.decideGap(repo, fixture(), TARGET, subject, "d1", DECIDED_AT)) throw new Error("not written");
  const d = GAPS.readDecision(db, "d1");
  if (!d) throw new Error("decision row missing");
  if (d.symbol !== "sh.600006" || d.period !== "Day" ||
      d.subject_version_id !== "legacy-daily" ||
      d.subject_version_digest !== subject.integrity_sha256 ||
      d.conclusion !== "confirmed_missing_range" ||
      d.basis_kind !== M.GAP_BASIS_KIND ||
      d.verifier_id !== "fixture-verifier-1") throw new Error("decision fields");
  if (d.decision_sha256 !== GAPS.decisionContentDigest(d)) throw new Error("decision digest");
  const open = GAPS.openProvenGaps(db, "sh.600006", "Day");
  if (open.length !== 1) throw new Error("open gaps");
  if (open[0].gap_id !== GAPS.deriveGapId(d) ||
      open[0].evidence_kind !== GAPS.GAP_DECISION_EVIDENCE_KIND ||
      open[0].evidence_id !== "d1" ||
      open[0].evidence_digest !== d.decision_sha256 ||
      open[0].start_date !== "2025-01-01" || open[0].end_date !== "2025-01-05")
    throw new Error("gap row fields");
  // Identical replay is a no-op.
  if (M.decideGap(repo, fixture(), TARGET, subject, "d1", DECIDED_AT) !== false)
    throw new Error("replay not a no-op");
  if (!eq(counts(db), [1, 1])) throw new Error(`counts ${counts(db)}`);
});

// missing_or_damaged_subject_fails_closed (three arms)
run("A3a_missing_subject_notfound", () => {
  // Bare skeleton: no head at all.
  const path = join(FIX, "a3a.db");
  copyFileSync(SKELETON, path);
  const db = openDatabase(path, { mode: "readwrite" });
  const e = expectErr(() => M.freezeSubjectOn(db, TARGET));
  if (!(e instanceof MarketError) || e.kind !== "not_found")
    throw new Error(`want not_found, got ${errClass(e)} ${e?.message}`);
  db.close();
});
run("A3b_staged_unpublished_notfound", () => {
  // Staged but not published: head row absent. Build fixture but skip
  // the publish commit — stage + task rows exist, no effective head.
  const path = join(FIX, "a3b.db");
  copyFileSync(SKELETON, path);
  const db = openDatabase(path, { mode: "readwrite" });
  const samples = JSON.parse(readFileSync(
    join(REPO, "tests/fixtures/issue79/legacy-records.json"), "utf8"));
  stage(db, samples[0].record);
  const e = expectErr(() => M.freezeSubjectOn(db, TARGET));
  if (!(e instanceof MarketError) || e.kind !== "not_found")
    throw new Error(`want not_found, got ${errClass(e)} ${e?.message}`);
  db.close();
});
run("A3c_damaged_record_fails_closed", () => {
  const { db } = fixtureRepo("a3c");
  // Corrupt stored bar bytes: full read_record recheck must fail.
  db.run("UPDATE segment_bars SET bar_json=json_set(bar_json,'$.close','9.99')");
  const e = expectErr(() => M.freezeSubjectOn(db, TARGET));
  if (e === null) throw new Error("damaged subject froze");
});

// forged_subject_binding_fails_closed
run("A4_forged_subject_binding", () => {
  const { db, repo } = fixtureRepo("a4");
  const forged = { version_id: "legacy-daily", integrity_sha256: "f".repeat(64) };
  const e1 = expectErr(() => M.decideGap(repo, fixture(), TARGET, forged, "d1", DECIDED_AT));
  if (!(e1 instanceof MarketError) || e1.kind !== "integrity")
    throw new Error(`forged: want integrity, got ${errClass(e1)}`);
  const absent = { version_id: "ghost-version", integrity_sha256: "f".repeat(64) };
  const e2 = expectErr(() => M.decideGap(repo, fixture(), TARGET, absent, "d1", DECIDED_AT));
  if (e2 === null) throw new Error("absent subject passed");
  if (!eq(counts(db), [0, 0])) throw new Error(`counts ${counts(db)}`);
});

// moved_subject_head_aborts_commit_without_rows
run("A5_moved_head_aborts", () => {
  const { db, repo } = fixtureRepo("a5");
  const subject = repo.freezeGapSubject(TARGET);
  db.exec("PRAGMA foreign_keys=OFF; UPDATE effective_heads SET version_id='moved-head' WHERE symbol='sh.600006' AND period='day';");
  const e = expectErr(() => M.decideGap(repo, fixture(), TARGET, subject, "d1", DECIDED_AT));
  if (!(e instanceof MarketError) || e.kind !== "integrity")
    throw new Error(`want integrity, got ${errClass(e)} ${e?.message}`);
  if (e.message !== "MARKET_INTEGRITY: gap decision subject head moved")
    throw new Error(`text: ${e.message}`);
  if (!eq(counts(db), [0, 0])) throw new Error(`counts ${counts(db)}`);
  if (GAPS.openProvenGaps(db, "sh.600006", "Day").length !== 0) throw new Error("gaps");
});

// non_gap_conclusion_stores_decision_without_gap
run("A6_non_gap_conclusion", () => {
  const { db, repo } = fixtureRepo("a6");
  const subject = repo.freezeGapSubject(TARGET);
  const v = new FixtureVerifier({ conclusion: "source_limited" });
  if (!M.decideGap(repo, v, TARGET, subject, "d1", DECIDED_AT)) throw new Error("not written");
  if (!eq(counts(db), [1, 0])) throw new Error(`counts ${counts(db)}`);
  if (GAPS.openProvenGaps(db, "sh.600006", "Day").length !== 0) throw new Error("gaps");
  if (M.decideGap(repo, v, TARGET, subject, "d1", DECIDED_AT) !== false)
    throw new Error("replay not a no-op");
});

// basis_digest_or_binding_mismatch_is_rejected
run("A7_basis_mismatches", () => {
  const { db, repo } = fixtureRepo("a7");
  const subject = repo.freezeGapSubject(TARGET);
  for (const [name, v] of [
    ["bad_digest", new FixtureVerifier({ badDigest: true })],
    ["wrong_subject", new FixtureVerifier({ wrongSubject: true })],
    ["wrong_target", new FixtureVerifier({ wrongTarget: true })],
  ]) {
    const e = expectErr(() => M.decideGap(repo, v, TARGET, subject, "d1", DECIDED_AT));
    if (!(e instanceof MarketError) || e.kind !== "integrity")
      throw new Error(`${name}: want integrity, got ${errClass(e)} ${e?.message}`);
  }
  if (!eq(counts(db), [0, 0])) throw new Error(`counts ${counts(db)}`);
});

// basis_verifier_identity_mismatch_is_rejected
run("A8_verifier_identity_mismatch", () => {
  const { db, repo } = fixtureRepo("a8");
  const subject = repo.freezeGapSubject(TARGET);
  const v = new FixtureVerifier({ claimId: "fixture-verifier-2" });
  const e = expectErr(() => M.decideGap(repo, v, TARGET, subject, "d1", DECIDED_AT));
  if (!(e instanceof MarketError) || e.kind !== "integrity")
    throw new Error(`want integrity, got ${errClass(e)} ${e?.message}`);
  if (e.message !== "MARKET_INTEGRITY: gap basis verifier identity mismatch")
    throw new Error(`text: ${e.message}`);
  if (!eq(counts(db), [0, 0])) throw new Error(`counts ${counts(db)}`);
});

// ---- B. acceptance contract (tests.rs:305-422) --------------------

// acceptance_rejects_every_caller_pass_through_field (12 x {top,nested})
const PASS_THROUGH = [
  "conclusion", "basis_kind", "basis_ref", "basis_sha256",
  "decision_sha256", "decided_by", "decided_at", "verifier_id",
  "subject_version_id", "subject_version_digest", "source_identity",
  "approved",
];
run("B1_accept_rejects_passthrough_fields", () => {
  const { db, repo } = fixtureRepo("b1");
  const contract = new C.GapVerificationContract(repo);
  for (const [i, field] of PASS_THROUGH.entries()) {
    for (const [loc, payload] of [
      ["top", { ...GAP_PAYLOAD, [field]: "x" }],
      ["nested", { target: { ...GAP_PAYLOAD.target, [field]: "x" } }],
    ]) {
      const e = expectErr(() => contract.normalize(payload));
      if (!(e instanceof MarketError) || e.kind !== "task" ||
          !e.message.startsWith("INVALID_COMMAND: "))
        throw new Error(`${field}@${loc}: want INVALID_COMMAND, got ${errClass(e)} ${e?.message}`);
    }
  }
  const n = db.get("SELECT COUNT(*) c FROM requests").c;
  if (Number(n) !== 1) throw new Error(`requests rows ${n} (fixture's "r" only)`);
  if (!eq(counts(db), [0, 0])) throw new Error(`counts ${counts(db)}`);
});

// acceptance_rejects_non_day_period_inverted_range_and_bad_symbols
run("B2_accept_rejects_bad_targets", () => {
  const { db, repo } = fixtureRepo("b2");
  const contract = new C.GapVerificationContract(repo);
  const cases = [
    { target: { ...GAP_PAYLOAD.target, period: "year" } },
    { target: { ...GAP_PAYLOAD.target, start_date: "2025-01-05", end_date: "2025-01-01" } },
    ...["sh.60006", "SH.600006", "sh.6000067", "bj.600006", "600006", "sh.60000a"]
      .map((s) => ({ target: { ...GAP_PAYLOAD.target, symbol: s } })),
  ];
  for (const [i, payload] of cases.entries()) {
    const e = expectErr(() => acceptGapTask(repo, contract, `bad-${i}`, payload));
    if (!(e instanceof MarketError) || e.kind !== "task" ||
        !e.message.startsWith("INVALID_COMMAND: "))
      throw new Error(`case ${i}: want INVALID_COMMAND, got ${errClass(e)} ${e?.message}`);
    if (db.get("SELECT COUNT(*) c FROM requests WHERE request_id=?", `bad-${i}`).c !== 0n)
      throw new Error(`case ${i}: request persisted`);
  }
});

// first_acceptance_freezes_subject_in_item_frozen_inputs
run("B3_freeze_at_acceptance", () => {
  const { db, repo } = fixtureRepo("b3");
  const contract = new C.GapVerificationContract(repo);
  const subject = repo.freezeGapSubject(TARGET);
  const { taskId } = acceptGapTask(repo, contract, "freeze-1", GAP_PAYLOAD);
  const snap = repo.task(taskId);
  const k = snap.task.key;
  const ck = M.commandKey();
  if (k.operation !== ck.operation || k.dataset !== ck.dataset ||
      k.contract_version !== ck.contract_version) throw new Error("task key");
  if (snap.items.length !== 1) throw new Error("items");
  const frozen = snap.items[0].spec.frozen_inputs;
  if (!eq(frozen, {
    target: { symbol: "sh.600006", period: "day",
      start_date: "2025-01-01", end_date: "2025-01-05" },
    subject_version_id: "legacy-daily",
    subject_version_digest: subject.integrity_sha256,
  })) throw new Error(`frozen_inputs: ${JSON.stringify(frozen)}`);
  const scopeId = JSON.stringify(["market", "sh.600006", "daily"]);
  if (!snap.items[0].spec.scope_keys.has(scopeId))
    throw new Error(`scope_keys ${[...snap.items[0].spec.scope_keys]}`);
});

// official_stub_execution_fails_closed_without_rows
run("C1_runner_stub_fails_closed", () => {
  const { db, repo } = fixtureRepo("c1");
  const contract = new C.GapVerificationContract(repo);
  const { taskId } = acceptGapTask(repo, contract, "official-1", GAP_PAYLOAD);
  const runner = new R.GapVerifyRunner(repo, "run");
  const out = runner.run(taskId, new M.UnverifiedGapVerifier(), DECIDED_AT);
  if (out.notDecided === undefined) throw new Error("stub must not decide");
  const item = out.notDecided.items[0];
  if (item.stage !== "finished") throw new Error("stage");
  if (item.outcome === null || item.outcome.success !== false)
    throw new Error("outcome");
  if (!item.outcome.error.startsWith("GAP_BASIS_NOT_VERIFIED"))
    throw new Error(`error text: ${item.outcome.error}`);
  if (!eq(counts(db), [0, 0])) throw new Error(`counts ${counts(db)}`);
});

// fixture_verifier_end_to_end_persists_decision_and_gap
run("C2_runner_e2e", () => {
  const { db, repo } = fixtureRepo("c2");
  const contract = new C.GapVerificationContract(repo);
  const { taskId, itemId } = acceptGapTask(repo, contract, "fixture-1", GAP_PAYLOAD);
  const runner = new R.GapVerifyRunner(repo, "run");
  const out = runner.run(taskId, fixture(), DECIDED_AT);
  if (out.decided === undefined) throw new Error("should decide");
  if (out.decided.task.status !== "succeeded") throw new Error(`status ${out.decided.task.status}`);
  if (out.decided.items[0].outcome?.success !== true) throw new Error("item outcome");
  const decisionId = R.gapDecisionId(taskId, itemId);
  const d = GAPS.readDecision(db, decisionId);
  if (!d || d.conclusion !== "confirmed_missing_range" ||
      d.subject_version_id !== "legacy-daily") throw new Error("decision row");
  if (!eq(counts(db), [1, 1])) throw new Error(`counts ${counts(db)}`);
  const open = GAPS.openProvenGaps(db, "sh.600006", "Day");
  if (open.length !== 1 || open[0].start_date !== "2025-01-01" ||
      open[0].evidence_id !== decisionId) throw new Error("open gap");
  // Runner replay neither re-verifies nor duplicates rows.
  const out2 = runner.run(taskId, fixture(), DECIDED_AT);
  if (out2.decided === undefined) throw new Error("replay should return Decided");
  if (!eq(counts(db), [1, 1])) throw new Error(`replay counts ${counts(db)}`);
});

// head_drift_after_acceptance_fails_closed_without_rows
run("C3_runner_head_drift", () => {
  const { db, repo } = fixtureRepo("c3");
  const contract = new C.GapVerificationContract(repo);
  const { taskId } = acceptGapTask(repo, contract, "drift-1", GAP_PAYLOAD);
  db.exec("PRAGMA foreign_keys=OFF; UPDATE effective_heads SET version_id='moved-head' WHERE symbol='sh.600006' AND period='day';");
  const runner = new R.GapVerifyRunner(repo, "run");
  const out = runner.run(taskId, fixture(), DECIDED_AT);
  if (out.notDecided === undefined) throw new Error("stale subject must not decide");
  if (out.notDecided.items[0].outcome?.success !== false) throw new Error("outcome");
  if (!eq(counts(db), [0, 0])) throw new Error(`counts ${counts(db)}`);
  if (GAPS.openProvenGaps(db, "sh.600006", "Day").length !== 0) throw new Error("gaps");
});

// same_request_id_replays_acceptance_without_recomputing_subject
run("B4_replay_acceptance", () => {
  const { db, repo } = fixtureRepo("b4");
  const contract = new C.GapVerificationContract(repo);
  const first = acceptGapTask(repo, contract, "replay-1", GAP_PAYLOAD);
  // Move the head: a fresh freeze would now fail or freeze differently.
  db.exec("PRAGMA foreign_keys=OFF; UPDATE effective_heads SET version_id='moved-head' WHERE symbol='sh.600006' AND period='day';");
  const replay = acceptGapTask(repo, contract, "replay-1", GAP_PAYLOAD);
  if (!replay.reused) throw new Error("not reused");
  if (replay.taskId !== first.taskId) throw new Error("task id");
  const s1 = repo.task(first.taskId), s2 = repo.task(replay.taskId);
  if (!eq(s2.items[0].spec.frozen_inputs, s1.items[0].spec.frozen_inputs))
    throw new Error("frozen_inputs diverged");
  const changed = { target: { ...GAP_PAYLOAD.target, end_date: "2025-01-06" } };
  const e = expectErr(() => acceptGapTask(repo, contract, "replay-1", changed));
  if (!(e instanceof StorageError) || e.code !== "conflict" ||
      e.message !== "REQUEST_CONFLICT")
    throw new Error(`want REQUEST_CONFLICT, got ${errClass(e)} ${e?.message}`);
});

// cancelled_task_closes_without_decision
run("C4_runner_cancelled_closes", () => {
  const { db, repo } = fixtureRepo("c4");
  const contract = new C.GapVerificationContract(repo);
  const { taskId } = acceptGapTask(repo, contract, "cancel-1", GAP_PAYLOAD);
  repo.transitionTask(taskId, 0n, "run", { kind: "requestCancel" }, NOW);
  const snap = repo.task(taskId);
  if (snap.task.status !== "cancel_requested") throw new Error(`status ${snap.task.status}`);
  const runner = new R.GapVerifyRunner(repo, "run");
  const out = runner.run(taskId, fixture(), DECIDED_AT);
  if (out.notDecided === undefined) throw new Error("cancelled must not decide");
  if (out.notDecided.task.status !== "cancelled") throw new Error(`final ${out.notDecided.task.status}`);
  if (!eq(counts(db), [0, 0])) throw new Error(`counts ${counts(db)}`);
});

// ---- summary -------------------------------------------------------
if (SELFTEST) emit("selftest_forced", "FAIL", "intentional failure for negative selftest");
const fails = results.filter((r) => r.status === "FAIL");
emit("summary", fails.length === 0 ? "PASS" : "FAIL",
  `${results.length} checks, ${fails.length} failed`);
if (OUT) {
  mkdirSync(OUT, { recursive: true });
  writeFileSync(join(OUT, "probe-results.jsonl"),
    results.map((r) => JSON.stringify(r)).join("\n") + "\n");
  writeFileSync(join(OUT, "results.json"), JSON.stringify({
    probe: "gap_verification", total: results.length,
    pass: results.filter((r) => r.status === "PASS").length,
    fail: fails.length,
  }, null, 2) + "\n");
}
process.exit(fails.length ? 1 : 0);
