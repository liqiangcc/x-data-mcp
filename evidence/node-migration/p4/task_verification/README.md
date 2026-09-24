# NM-106 — task_verification decision submodules (pure half)

Differential evidence for `src_node/task_verification/` against a Rust
oracle (`scripts/node_migration_p4/task_verification_oracle.rs`) linked
to the baseline `libx_data_mcp` rlib at 16df795. The oracle runs the
real Rust code (`admission::StopVerification`, `decision::prepare`,
`never_started::NeverStarted`, `determinate::Determinate`/`Audited`,
`existing::accept` through a real `TaskControl`, `wire::VerifyStopped`)
behind a stub `TaskRepository`/`StopRepository`/`ExistingStopRepository`/
`EvidenceRepository` that logs every call; the Node probe replays the
same scenarios and diffs status, error code, exact Display text,
decision detail and call order.

## Lane

- `src_node/task_verification/mod.mjs` — DATASET/CONTRACT_VERSION/
  commandKey/verificationScope, task_control::Error surface (full
  Display text + wire `code`), visibleStatus, `snapshotDigest`
  (sha256 of serde_json::to_vec(&snapshot)), FrozenWork::validate,
  TaskRepositorySeam/TaskControlSeam.
- `src_node/task_verification/wire.mjs` — VerifyStopped decoder
  (deny_unknown_fields, seq+map forms), DESCRIPTION verbatim.
- `src_node/task_verification/admission.mjs` — StopVerification::new/
  normalize/resolve verbatim (Input deny-decode, dataset + eligibility
  gates, FrozenWork incl. admission_snapshot_sha256).
- `src_node/task_verification/decision.mjs` — Observation, Conclusion,
  prepare() verbatim (verification-shape gates -> inspector ->
  conclusion/audit assembly), validateAudit, StopInspector/
  StopRepository seams, UnverifiedStopInspector.
- `src_node/task_verification/never_started.mjs` — supported() +
  NeverStarted.inspect verbatim (revision-zero gates, snapshot
  equality, durable-effects refusal).
- `src_node/task_verification/determinate.mjs` — Determinate.inspect
  verbatim (started/committed item accounting, evidence_ref counters)
  + Audited chain (never_started first, determinate second).
- `src_node/task_verification/existing.mjs` — TerminalResult/
  ExistingPlan + existing()/accept() verbatim (interruption-link,
  receipt binding, bounded recheck on InvalidCommand).

## serde note

`serSnap` in `mod.mjs` serializes canonical TaskSnapshot objects with
declaration-order struct fields AND propagates the child shape into
array elements — byte-equal to serde_json::to_vec. NM-048's
`verification_storage.ser` drops child shapes at array positions
(items emit sorted) — latent digest-vs-Rust divergence flagged to lead;
not in this lane, not touched.

## Run

    cargo build --locked
    rustc --edition 2024 -L target/debug/deps \
      --extern x_data_mcp=target/debug/libx_data_mcp.rlib \
      --extern serde_json=... --extern chrono=... --extern schemars=... \
      -o /tmp/tv_oracle scripts/node_migration_p4/task_verification_oracle.rs
    node scripts/node_migration_p4/task_verification_cases.mjs > scenarios.jsonl
    /tmp/tv_oracle < scenarios.jsonl > oracle-golden.jsonl
    TMPDIR=/dev/shm RESULTS_PATH=probe-results.jsonl \
      node scripts/node_migration_p4/task_verification_probe.mjs \
      scenarios.jsonl oracle-golden.jsonl

## Results

137 scenario vectors: **137 match, 0 diffs, 0 oracle panics**;
canonical byte equality verified (deep key-sorted parse). Negative
selftest: tampered golden -> probe exit 1. Ops: verify_decode 13,
admission_new 4, admission_normalize 9, admission_resolve 10,
inspect 39 (never_started 22 / determinate 14 / audited 3),
prepare 33 (all gate arms + scripted + real inspector chains),
validate_audit 9 (valid, receipt tamper, evidence_ref flip,
target_before tamper, stopped variant), existing_accept 20 (replay,
interrupted+receipt bind, link/receipt missing, conclusion/target/
revision mismatch, bind seam error, accept InvalidCommand recheck,
non-invalid propagation, empty/whitespace ids).

## Coverage boundary

`decision::Audit`/`StopPlan` internals are `pub(crate)` — the oracle
emits `plan.receipt()` + inspector/repository call order; full audit
equality is additionally exercised through NM-048's storage-bound
`readAudit` (same `canonEq` + same field order on write). Probe's
`serviceAccept` is a verbatim service.rs `accept` replica that calls
the real ported normalize/resolve/validateWork — scaffolding only.
