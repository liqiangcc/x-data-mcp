# NM-082 — market_update decision layer (pure half)

Differential evidence for `src_node/market_update/` against a Rust
oracle (`scripts/node_migration_p4/market_update_oracle.rs`) compiled
against the baseline `libx_data_mcp` rlib at 2666e45. The oracle runs
the **real Rust decision logic** (`tasks::RangeContract`,
`Publisher::prepare_range`, `task_control::decide_head`,
`market_update::prepare`, `commit_acquired_range`/`run_item` decision
paths) behind a stub `RangeRepository`/`RangeProvider` that logs every
call; the Node probe replays the same scenarios through the ported
modules and diffs status, error code, exact error message, decision
detail and the repository call order.

## Lane

- `src_node/market_update/serde.mjs` — serde-compatible primitives:
  struct map+seq decode, `deny_unknown_fields` (with deferred-unknown
  semantics — a mid-loop value error on a later known key still wins),
  `Option<T>`/`#[serde(default)]` in both forms, `invalid type:`/
  `missing field`/`unknown variant`/`invalid length` error text,
  chrono `NaiveDate`/`DateTime<Utc>` parse+canon, `rust_decimal` parse
  + canonicalization (scale preserved, exponent/.5/5./-0 forms, half-up
  rounding at 28 fractional digits, mantissa cap, f64-shortest for
  numeric JSON), `serde_json::to_vec` struct-order serializers for
  `Bar`/`MarketDataBatch`/`Evidence` (candidate_digest parity) and
  BTreeMap-sorted `Value` emitters.
- `src_node/market_update/mod.mjs` — market_update.rs: DateRange,
  Evidence/ContinuityEvidence, Reason, Decision/PreparedRange,
  UpdateBaseFacts/UpdateBatchItem, `candidate_digest` (struct-order
  bytes), `unit_drift`, `prepare`, typed market_storage::Error and
  task_control::Error surfaces with verbatim Display.
- `src_node/market_update/tasks.mjs` — tasks.rs: contract versions,
  Operation::key, `RangeContract::normalize/resolve` verbatim
  (deny-unknown structs, symbol/budget/same-symbol-conflict rules,
  future-date rejection, repository_error mapping), DefaultFrozen
  strict decode + required_ranges/union + parse_default_frozen,
  frozen() shape.
- `src_node/market_update/publication.mjs` — publication.rs pure half:
  `Publisher::prepare_range` verbatim (intent binding -> fixed ->
  check_frozen_inputs -> prepare -> per-decision gates -> envelope
  audit pack), `check_frozen_inputs` (explicit frozen-equality +
  default-contract union-equality + generation stamp), plan_record/
  check_task_binding/finish_task via the ported task_control
  `decide_head`, `envelope::pack_batch`.
- `src_node/market_update/runner.mjs` — runner.rs pure halves:
  `update_candidate` verbatim (typed ops-first acquisition path),
  `binding`, `run_items` (batch commit + per-item fallback),
  `ensure_default_coverings`, `RangeRepositorySeam`/`RangeProviderSeam`
  — every unwired repository/provider call throws a typed
  `MARKET_STORAGE`/`SourceError`; nothing silently succeeds.

## NOT ported in this lane

`run_item`, `acquire_and_commit*` and `commit_acquired_range` are the
storage-commit orchestration — owned by the S4/S5 lane. The probe
contains a faithful replica used only to drive `run_items` fallback
vectors; the real Rust runs in the oracle, so any semantic drift in the
ported seam functions surfaces as a diff.

## Run

    cargo build --locked            # baseline lib
    rustc --edition 2024 -L target/debug/deps \
      --extern x_data_mcp=target/debug/libx_data_mcp.rlib \
      --extern serde_json=...rlib --extern chrono=...rlib \
      --extern rust_decimal=...rlib \
      -o /tmp/mu_oracle scripts/node_migration_p4/market_update_oracle.rs
    node scripts/node_migration_p4/market_update_cases.mjs > scenarios.jsonl
    /tmp/mu_oracle scenarios.jsonl > oracle-golden.jsonl
    TMPDIR=/dev/shm RESULTS_PATH=probe-results.jsonl \
      node scripts/node_migration_p4/market_update_probe.mjs \
      scenarios.jsonl oracle-golden.jsonl

## Results

232 scenario vectors: **232 match, 0 diffs, 0 oracle panics** (exit 0).
Byte-level canonical equality (deep key-sorted parse) also verified.

Covered: decimal/date/datetime scalars (37), batch serde incl. seq
forms + deny-unknown ordering (7), command keys (3), task
normalize/resolve incl. budget/sort/dedup/conflict/future/repository-
error mapping (30), `prepare` decision matrix — no_change, candidate,
needs_verification (coverage_unknown, basis_unknown, units_incompatible,
missing_old_dates, known_value_lost, revision_unverified), invalid
records/batches (40), `prepare_range` gates — intent binding, frozen
equality, combined-evidence digests, candidate-vs-merge, unit-loss,
PublishPlan audit/payload Debug parity (39), `run_items` batch +
fallback paths (16), `run_item` visible-status/ownership/stage/outcome
matrix + frozen-shape integrity + typed seams (23).
