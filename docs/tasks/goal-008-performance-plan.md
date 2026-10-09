# GOAL-008: Daily And Yearly Performance Closure

Status: implementation in progress; not a production certification.
Decision owner: xm. Execution: AWS tmux Lead/Worker.
Requirement entry: Issue #262 and Owner GOAL-008.
Current implementation review: PR #323.

## 1. Objective And Boundaries

Daily and yearly full-market publication must each finish their agreed
end-to-end write loop in less than 600 seconds on aws-node.
All current catalog entities must be considered, with at most 100 items
per write task and serial write-task submission.

The clock starts immediately before the first write RPC and ends when
the driver receives the final terminal result. Include acceptance, task
execution, polling, result reads and all intervening probes. Record catalog/
availability preparation before that boundary and readback after it
separately, and also report the complete operator wall time.
Never use server timestamps alone to claim end-to-end completion.

This is the current Owner-approved GOAL-008 target, not a retrospective
restatement of old #259 targets. NoChange, changed daily and yearly
aggregation are distinct workloads with separate results.

Constraints:

- Keep fail-closed behavior, evidence/source bindings, immutable versions,
  fixed-version reads, task/request idempotency and item-level outcomes.
- No production writes, direct SQLite in the operator/Skill driver, relaxed
  durability, deleted history, unbounded tasks or additional writer concurrency.
- Preserve existing work, retained inputs, private baseline binaries and logs.
- Production preview.30 remains unchanged until separately approved.

## 2. Facts, Not Hypotheses

Code baseline: 4ff45d7. First read-path change: 299f668, PR #323.
Private measurement environment: aws-node, two vCPU, about 1.9 GiB RAM;
actual swap, disk, load and competing processes must be captured per run.
Build/test/package execution is in GitHub Actions, not on aws-node.

| Evidence | Workload | Recorded result | Interpretation |
| --- | --- | --- | --- |
| XP-007 / update-b.json | Synthetic truncated generation to full generation, daily Append | About 1,485s | Private baseline, not a natural production generation pair |
| XP-008 / yearly-run.json | Full yearly driver | About 4,021s | Year-presence probes and progress queries dominate call time |
| XP-010 / result-xp009-update-nc.json | Same-generation daily NoChange, 5,224 items, 53 tasks | 433.805212s write loop | Timing passes ONLY this workload; correctness hold remains |
| Same XP-010 artifact | Availability-before scan | 72.86025309562683s | Outside write loop; report separately |
| XP-010 / result-xp009-progress-idle.json | Completed old 100-item tasks | About 10-13s per call | Old-task observations remain expensive |

These results are frozen inputs. Read their artifacts; do not rerun them
to regenerate summaries. Different workloads cannot establish causal speedup.
Task last_progress_at denotes item progress, not a commit/finalize event.

## 3. Root-Cause Model

Confirmed call path at 299f668:

    Observer::progress
      -> SqliteSupply::task / task_facts
      -> range_result / update::read_result
      -> compact_results::read_update_audit
      -> read_market_update / require_published_facts
      -> compact_versions::fixed

The outer slim projection does not remove the inner full reconstruction.
Base and result are both verified, even when NoChange makes them identical.
Fresh progress responses also revalidate results in another read transaction.

Availability still constructs task_list over task history. Lighter row
decoding does not remove history-dependent scope. The yearly driver resolves
present years with one availability call per natural year; date bounds alone
do not prove that a year has records.

These paths are established in source. Their individual percentages,
lock waits and serialization costs require nested measurement; do not
infer them from a long RPC alone. Swap occupancy alone is not proof of
active memory thrashing.

## 4. Implementation Direction

Use transaction-scoped verified-facts reuse, not cross-request trusted caches.
Create the context within the SQLite read transaction; discard it before
commit. First use performs the full required verification. Later use of the
same immutable version in that snapshot reuses its verified result.
Retain only bounded facts and single-result continuity data, not all
100 full histories.

Preserve Full/Append continuity checks, source/evidence/digest linkages,
request/task consistency and cursor revalidation. A stored digest is not
proof that unexamined bars or task rows have remained intact.

Yearly orchestration must derive actual present years from the pinned daily
version through a supported MCP interface. Define that interface before
implementation. Do not infer missing years, switch to latest heads after
pinning, or add undocumented tool behavior.

## 5. Task Graph And Ownership

| Task | Owner | Dependency | Deliverable |
| --- | --- | --- | --- |
| XP-010 | Worker; Lead reviews | Already running | Finish the existing run once, preserve raw evidence |
| XP-011 | Worker; Lead reviews | XP-010 finishes | Nested counts/timings, shared-read rework, corruption regressions, CI |
| XP-012 | Worker; Lead reviews | XP-011 correctness accepted | CI-package bounded hot-path measurements |
| XP-013 | Lead | May proceed while Worker implements XP-011 | Year-presence MCP/driver contract proposal; no implementation |
| XP-014 | Worker; Lead reviews | XP-012 passes and Owner approves XP-013 contract | Yearly orchestration/aggregation optimization |
| XP-015 | Worker measures; Lead reviews; Owner decides | Earlier correctness and small gates pass | Daily and yearly full-market final evidence |

TASK-XP-011.md already exists on AWS. Adopt it; do not create a duplicate
implementation task. OWNER-TASK-XP-012..015 are authoritative dependency/
acceptance supplements for Lead to turn into TASK files.
Only one Worker implementation task may be active.

## 6. Correctness Gate

PR #323 is on hold because healthy-output equivalence does not resolve
the disclosed reduction in corruption detection. Require original and
optimized public observation classifications to agree for:

1. Modified segment bar with the stored digest unchanged.
2. Missing segment/ancestor, broken clip linkage or projection digest.
3. Modified task stage/outcome/counters with valid task/request digests.
4. Missing or inconsistent task scopes/occupancy for the task state.
5. Corruption committed between cursor pages.
6. Unauthorized access, including absence of leaked private diagnostics.
7. Legacy, unpublished and missing-version fallback behavior.

CI green alone does not lift the hold. Review all affected read consumers.

## 7. Measurement Gates

Small gate before another full run:

- Same completed 100-item Append and NoChange task IDs, same private data.
- CI-built release binary with recorded commit and artifact checksums.
- Ten serial get_task_progress samples per task, no warm-up discarded.
- Median below 1s and every sample below 2s, plus the correctness gate.
- This is an internal iteration gate, not a new public service SLO.
- If it fails, profile the bounded case; do not launch another full market run.

Final workload matrix:

- Daily NoChange.
- Changed cross-generation daily Append; identify synthetic vs real inputs.
- Real mixed append/rewrite/fail-closed coverage, recorded separately.
- Yearly initial aggregation and changed-daily aggregation.

Each row reports considered/attempted/published/no-change/failed/
needs-verification counts, all task/request IDs, preflight/write-loop/
readback/total wall time, correctness checks and PASS/FAIL/NOT_RUN.
NoChange or replay alone cannot certify changed publication.
Do not add overlapping call-time sums to server windows as disjoint costs.

## 8. Execution And Coordination

AWS tmux Lead plans/reviews; AWS tmux Worker owns source and private runtime.
CI runs fmt/clippy/tests/build and release packaging.
Use preview-release workflow_dispatch for the exact branch SHA; verify
outer archive checksum, inner SHA256SUMS and smoke report.

Reuse surviving services and in-flight commands. Do not stop unrelated
desktop/user processes for a cleaner benchmark without explicit approval.
Capture actual load rather than hard-coding machine specifications.

File delivery is not task acceptance. Inspect exact pane state, submit
only to idle empty input, and confirm ACK or real tool execution.
display-message is a visual hint only. Lead maintains current task/role
state and continues through Worker completion, review and next dispatch.

## 9. Completion And Repository Record

Place this plan in docs/tasks/goal-008-performance-plan.md on the Worker
branch, preserving its text. Add implementation decisions and result links
as reviewed follow-up sections; do not rewrite historical measurements.
Do not update production facts in AGENTS/README from private benchmark results.
Final acceptance does not authorize merge or production deployment.
