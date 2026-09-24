# NM-076 — gap_verification app layer (S10a)

Port of `src/gap_verification*` (wire + contract + runner + decision
producer) into `src_node/gap_verification/`, over the w1-landed gaps
store (`market/gaps.mjs`, NM-056) and task machinery
(`storage/task_repository.mjs`, NM-040s).

## Files

- `src_node/gap_verification/mod.mjs` — command key constants,
  `GapTarget`/`validateTarget`, `SubjectBinding`, `GapBasis`
  (`gap-basis-v1` content digest + `validate`), `SourceGapVerifier`
  port surface, `UnverifiedGapVerifier` (fails closed),
  `freezeSubjectOn`/`checkSubjectOn` (market_read head + full
  readRecord recheck), `decideGap` (fail-closed binding chain +
  guarded atomic commit).
- `src_node/gap_verification/wire.mjs` — `VerifyDataGap`/
  `VerifyGapTarget`/`VerifyGapPeriod`/`ContractVersion` decoders
  (`deny_unknown_fields`) + `gapPayload`.
- `src_node/gap_verification/contract.mjs` — `GapIntent` decode on the
  shared serde_value machinery, `validSymbol`, `GapFrozen` +
  `parseGapFrozen` + `gapFrozenParts`, `repositoryError` (Rust
  `market_update::tasks::repository_error` verbatim mapping),
  `GapVerificationContract` (key/normalize/resolve -> FrozenWork).
- `src_node/gap_verification/runner.mjs` — `gapDecisionId`,
  `visibleStatus`, `GapVerifyRunner.run/decide/close` decision tree
  verbatim (durable-decision short-circuit, NeedsVerification,
  running-item refuse, terminal/NotDecided, cancel close-out,
  claim -> start -> decide -> close).
- `src_node/gap_verification/store.mjs` — `GapRepository`: in-lane
  adapter binding one readwrite db to the GapTaskStore +
  GapDecisionStore surface via `TaskAdmission` + `gaps.mjs` +
  `market_read.mjs` (read-only consumption; snapshot read uses
  explicit BEGIN/COMMIT so domain errors propagate un-wrapped —
  the adapter's `transaction()` re-maps non-StorageError throws).

## Probe — `scripts/node_migration_p4/gap_verification_probe.mjs`

Mirrors `src/gap_verification/tests.rs` 1:1 (all 16 cases, including
the three-arm missing/damaged subject case). Oracle: the Rust suite
itself (`cargo test gap_verification` — 16/16 green at the baseline
commit); assertions match the same end-state the Rust tests assert
(stored rows, item outcomes, error class + verbatim message prefixes).

Fixture (`gap_fixture.mjs`): schema-18 skeleton `/dev/shm/nm006/
db18.sqlite` copied then populated entirely through landed ports —
`market_write.stage` (issue79 record), `TaskAdmission.admit` +
claim/startItem, `commitPublication` — reproducing
`sqlite_market::tests::setup()`.

Results: **18 checks, 18 PASS, 0 FAIL**; `--selftest` exits 1.

## Boundaries

- No `TaskControl` service wrapper (S2 non-goal): `acceptGapTask`
  composes contract.normalize -> resolve -> `admit` verbatim,
  including the replay-before-resolve ordering.
- `transition_task` maps onto w1's per-change tx methods (claim /
  startItem / finishItem / finishExecution / finishCancellation /
  requireVerification / requestCancel) — identical decide gates, CAS
  and post-conditions, then a validated snapshot re-read.
- No pushes/merges/deploys; Rust untouched.
