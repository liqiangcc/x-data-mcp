# NM-098 — publication_verification domain service (S7b coverage-sweep gap)

Port of `src/publication_verification/` (admission / decision / existing /
runner / wire) onto NM-059 commit writes + NM-056 read paths.

## Surface

`src_node/publication_verification/`:

- `wire.mjs` — `VerifyPublication{contract_version,request_id,target_task_id}`
  deny_unknown_fields; DESCRIPTION byte-identical to `wire.rs`.
- `mod.mjs` — command key re-export, Evidence wire shape, verbatim
  `task_control::Error` / `market_storage::Error` display helpers +
  `toTaskError` (sqlite_market task_error mapping).
- `admission.mjs` — `PublicationVerification` contract: empty-run gate,
  `Input{target_task_id}` decode, eligibility (needs_verification + running
  item + supported contract + valid uncertainty marker), FrozenWork with
  `verification:task:<target>` scope and admission snapshot digest frozen
  inputs.
- `decision.mjs` — `Conclusion`/`Receipt` accessors; `prepare`/`validateAudit`
  delegate to NM-056 `prepareCheck`/`validateCheckAudit`.
- `runner.mjs` — `run/cancel/preserve_unresolved/facts/prepare` verbatim:
  durable-receipt short-circuit, needs_verification/running -> OutcomeUnknown,
  cancel arms, non-pending -> OutcomeUnknown, claim+startItem, commit with
  exactly one fresh-facts retry on Conflict, receipt fallback after ambiguous
  commit errors.
- `existing.mjs` — `known()` (target+contract gate, all_recorded-only binding)
  + `accept()` (request replay -> known binding -> accept -> InvalidCommand
  re-check).
- `store.mjs` — the seam adapter: TaskAdmission bound reads + transitions as
  `TaskRepository`; NM:059 `commitCheckTx`/`checkReceipt`/`knownPublication`/
  `validateTerminal`/`readRequestRecord` + NM-056 `readFacts` under explicit
  `BEGIN [IMMEDIATE]` (write_transaction parity; adapter transaction() is
  deferred).

## Evidence

- `probe-results.jsonl` — 46 checks, 0 fail. Covers:
  - A*: admission refusals (empty run, decode edges, not-found,
    unsupported contract, terminal/same-run targets, frozen-work shape,
    uncertainty markers).
  - B*: decision outcomes — insufficient_evidence, all_recorded,
    not_applicable, unknown/dup/incomplete fact sets, missing-on-success,
    wrong-run conflict.
  - C*: runner arms — identity gate, durable-receipt short-circuit,
    unsupported key, visible-needs_verification, running item, cancel
    (pending + ordering), terminal check, end-to-end commit (target
    unchanged + audit row + receipt read-back), commit-conflict retry,
    conflict-with-drift -> needs_verification, facts-error preserve,
    commit-error receipt fallback.
  - D*: existing::accept — empty request/run, missing/unsupported target,
    fall-through accept, known-receipt terminal binding (requests row
    verified), replay reuse, insufficient receipt does not bind, request
    replay.
  - E*: tamper — audit digest, association, projection, completion drift,
    evidence-changed post-commit.
  - F1 — `ser(audit,"checkAudit")` byte-identical to the Rust-written
    `audit_json` on the oracle-built DIFF_CHECK fixture (4518 bytes).
- `results.json` — summary. Selftest (`--selftest`) exits 1.

Differential scenario: `scripts/node_migration_p3/scenarios/
pubcheck_tail.jsonl` (NM-059 harness ops) — 25 ops, 0 diffs vs the Rust
`diff_harness` oracle on the DIFF_CHECK committed-receipt fixture:
committed receipt read-back, known_publication bind/miss, read_facts,
check_plan capture error on succeeded check, admit+claim+startItem of a
fresh check task, plan capture, commit fresh + replay + tweak-conflict,
bind_known_publication bind/replay/divergent(REQUEST_CONFLICT), audit-sha
tamper -> integrity error, audit-row delete -> missing-audit error,
post-delete known_publication error, bound-read checkpoint.

Rust oracle: `cargo test --lib publication_checks` 4/4 green;
`publication_verification` inspection test 1/1 green.

## Known lane divergences (flagged, not in this port)

- `task_repository.mjs` bound-read marker validation collapses the serde
  decode failure and the does-not-bind failure into
  `"invalid execution uncertainty"` (Rust texts: `invalid execution
  uncertainty: <serde>` / `execution uncertainty does not bind unresolved
  running item`, both under `ACCEPTANCE_NEEDS_VERIFICATION:`). The
  domain-layer `executionUncertainty` here emits the verbatim texts when
  reached; markers caught earlier by the bound read surface the w1 text.
  Filed for the storage-lane owner.
