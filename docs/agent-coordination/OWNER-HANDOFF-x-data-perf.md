# Owner Handoff — xm Coordination of x-data-perf (GOAL-008)

Written by: xm (ecs-node), 2026-10-10. Audience: whoever takes over the
Owner/coordinator role (user, successor xm session, or escalated Lead).
Committed to git so AWS can pull; the live copy is the mailbox file of the
same name (single source of truth for day-to-day state = session.json +
this file + TASK/RESULT/REVIEW chain on aws-node).

## 1. What this operation is

GOAL-008: make full-market daily update AND yearly aggregation each run
<10 minutes end-to-end on the pinned ~5,224-entity scope, without weakening
integrity verification, and without touching production `/opt` until every
gate passes. Roles:

- **Owner (xm, ecs-node)**: analysis, spec/decision authority, independent
  gate computation from raw evidence, dispatch. Does NOT write feature
  code, does NOT merge/release/deploy.
- **AWS Lead (tmux a-leadN, aws-node)**: task splitting, full-diff review,
  execution verification. Report-only until Owner approves.
- **AWS Worker (tmux a-workerN, aws-node)**: mechanical implementation in
  `/root/x-data-mcp-worker` on branch `perf/g8-daily-update-perf`.
  Only `cargo check`/`cargo fmt` locally — real verification = GitHub CI.
- **GitHub Actions**: fmt/clippy/test/build (`ci.yml` on PR) +
  `preview-release.yml` (workflow_dispatch → release tarball + smoke).

Coordination = file mailbox at
`/root/x-data-mcp/.git/agent-coordination/x-data-perf/` on aws-node,
mirrored selectively to ecs-node. NOTE: that path lives INSIDE `.git/` —
it is NOT versioned; the committed copy of THIS file is the handoff.

## 2. Dispatch discipline (hard-won, keep)

- **Real dispatch = `tmux send-keys -t <pane> -l "<msg>"` + a later bare
  `Enter`.** `display-message` is visual-only and caused repeated stalls
  where a task sat in the mailbox for ~an hour with nobody awake.
- Queued-message trap: if a pane is still processing its prompt file, a
  typed message lands in the queue ("1 queued" in status bar) — press
  bare `Enter` to actually send. Verify the pane shows Thinking/Running
  before walking away.
- ACK convention: worker writes ACK-*.md before starting; treat "no ACK"
  as "not started", never as "in progress".
- Rotation: at ~75-78% ctx the session writes HANDOFF-* and a fresh
  numbered session takes over (names in session.json are often stale —
  check `tmux ls`). Any side may be rotated; work state lives in the
  mailbox, not the session.
- Uncommitted worker state: `git diff` (plus `--no-index` for untracked
  files) → `WIP-XP-*-checkpoint.patch` in mailbox BEFORE restart.
- Single-writer: only the Owner writes OWNER-* verdicts; only Lead writes
  REVIEW/TASK; only Worker writes ACK/RESULT/WIP. Reports never
  self-judge gates — Owner recomputes from raw.
- 3-round limit per review cycle → BLOCKED upward, no silent workaround.

## 3. Evidence and gate rules (verbatim, do not relax)

XP-012 progress-read gate (the one now PASSED): approved commit only;
artifact run's headSha == approved SHA; package sha256sums + manifest
git_sha + smoke PASS; private `/proc/<pid>/exe` digest == packaged
binary; unchanged collector `owner_xp012_samples.py`
(sha256 4d4b4028ab5224371f56baa7181dd11ee8544dd3cb2ff20cdbf98e56ab1913a8);
pinned completed 100-item tasks Append `task-d6086989cedf22a5b30ef53bec67f343`
and NoChange `task-5e9087fd80b4e6623859d313b4b12ccb`; 10 serial samples
each; no warmup/filter/retry/exclusion; cold seq0 counts; Owner computes
median<1s AND every-sample<2s from raw NDJSON. Raw evidence frozen under
`/root/x-perf-g8/probe-xp012r*/` with sha256 recorded.

Full-market gates (still open): daily `update_data` same-generation run
<10min E2E; yearly aggregation run <10min E2E — measured on the private
stack by the existing drivers, Owner verdicts only.

## 4. State as of this handoff

PASSED:
- XP-009/011/016/018/019/020 rounds culminating in **XP-012 R4 PASS**
  (OWNER-XP012-R4-VERDICT.md): completed-100-item progress query median
  0.108s append / 0.091s nochange, max 0.894s — ~18-20x over R3 FAIL.
  Mechanism = XP-020 committed-state verification tier: progress/metadata
  reads verify committed rows (integrity_digest recompute + all
  binding/digest/structure checks + delta clip header-level checks)
  instead of re-reading ~640k bars per RPC. Content reads
  (fixed/range/commit/integrity-verifier) keep FULL verification —
  honest tier split, regression-tested.
- CI green on `ef4fea4` (run 38013106980); PR #323 open, NOT mergeable
  yet (gates below). Issue #324 open (stopped-write canonical-outcome
  defect — promotion blocker, separate fix).
- XP-013 `get_yearly_base_years` contract APPROVED by user
  (OWNER-APPROVAL-XP-013.md): read-only, v1, ≤100 items, resolved|
  unavailable + reason_code, single read tx, MAX_BYTES, capability
  generic-data-v4 + Skill revision bumped TOGETHER.

IN FLIGHT:
- **XP-014** (TASK-XP-014.md): implement get_yearly_base_years + rewrite
  yearly driver to replace per-year availability probes (was 59% of
  yearly E2E). worker10 produced ~23-path WIP (+440/-59, presence.rs +
  yearly_presence.rs + driver/Skill/docs/tests) before the box rebooted;
  WIP-XP-014-checkpoint.patch sha256 835032ad… preserves it; worktree
  retains the uncommitted state. a-lead5 + a-worker11 are continuing:
  verify-vs-WIP → cargo check → contract tests → commit/push → RESULT
  → Lead diff review → STOP for Owner diff review → CI → package.
- Next after XP-014: XP-015 full-market acceptance (daily + yearly
  <10min), then #324 fix, then merge/release decision — all gated.

FROZEN/UNTOUCHED:
- Production `/opt/x-data-mcp-preview/` preview.30 — never modified by
  this work; systemd x-data-mcp-data/acquire/tunnel auto-recovered after
  reboot and are the OLD preview.30 binaries.
- Private stack (manual, not systemd — dies on reboot, restart recipe in
  §6): data_service + acquire_service on
  `/root/x-perf-g8/dist-xp012r4/.../bin/` (approved ef4fea4), sockets
  `/root/x-perf-g8/svc/run/{mcp,acquire}.sock`, DB
  `/root/x-perf-g8/svc/db/preview.sqlite` (884,469,760B) + gen + mirror
  unchanged. As of handoff: pids 7342/7354, verified digest==package,
  NO trace flag.
- All frozen dist/probe dirs under /root/x-perf-g8/ (dist-xp009/012/
  012r2/012r3/012r4/019, probe-xp011/012r2/012r3/017/019/012r4).

## 5. Key files (mailbox, aws-node unless noted)

- PROTOCOL.md, EXEC-MODEL-CI.md — operating rules.
- session.json — current session names/state (often stale; `tmux ls` is
  truth).
- TASK-XP-*.md / RESULT-*/REVIEW-*/ACK-* — task chain (latest in flight:
  XP-014).
- OWNER-* — my verdicts/approvals (R3 FAIL, R4 PASS, XP-019 diagnostic,
  XP-013 approval, XP-020 acceptance).
- HANDOFF-lead-*.md, HANDOFF-worker-*.md, WORKER-PROMPT-AWS-*.md —
  generation-rotation artifacts.
- owner_xp012_samples.py — THE frozen acceptance collector (do not edit).
- WIP-*.patch — uncommitted-state checkpoints.
- evidence/perf-g8/ (repo) — drivers incl. yearly_g8.py (XP-014 modifies).
- GitHub: PR #323 = perf branch; watch `gh run list -R liqiangcc/
  x-data-mcp --branch perf/g8-daily-update-perf`.

## 6. Recovery recipes (used post-reboot, keep)

If aws-node reboots: (a) systemd prod services self-recover — verify
`systemctl is-active x-data-mcp-*` + sockets, never redeploy them;
(b) private stack does NOT auto-start — relaunch from the frozen dist:

    DIST=/root/x-perf-g8/dist-xp012r4/x-data-mcp-0.2.0-preview.30-linux-x86_64/bin
    setsid env X_DATA_MCP_GENERATION_ROOT=/root/x-perf-g8/svc/gen \
      "$DIST/data_service" /root/x-perf-g8/svc/db/preview.sqlite \
      /root/x-perf-g8/svc/run/mcp.sock >> svc/data-*.log 2>&1 &
    setsid "$DIST/acquire_service" /root/x-perf-g8/svc/gen \
      /root/x-perf-g8/svc/run/acquire.sock --mirror \
      /root/x-perf-g8/svc/mirror >> svc/acquire-*.log 2>&1 &

then verify `sha256sum /proc/<pid>/exe` == package digests
(data ce08479d…, acquire b12a426e… for ef4fea4) and no
X_DATA_MCP_PROGRESS_STAGE_TRACE in environ (acceptance = clean env).
(c) WIP: check worktree `git status` vs latest WIP-*.patch before any
checkout/reset — never `git checkout -- .` on a worker's tree.
(d) sessions: spawn replacements with the newest HANDOFF; they rehydrate
from the mailbox.
(e) resource discipline on the 2vCPU/1.9GB box: ONE cargo check at a
time, MemAvailable<300MB or df<5GB ⇒ stop; the reboot was caused by
sessions+cargo+desktop stacking into deep swap.

## 7. Do not do

- No merge/tag/release/deploy/systemctl on the perf work; production
  `/opt` and ecs-node storage untouched until Owner says so.
- No gate relaxation, no dropping samples, no "it usually passes".
- No cross-RPC verification caching (each RPC/page = fresh tx checks).
- No weakening shared full-tier readers — committed tier is separate
  functions routed only from SqliteProgressRead.
- No ssh writes to ecs-node, no edits to the frozen collector, no resets
  of private DB/gen/mirror.
- Devin CLI flag gotcha: `--dangerously-skip-permissions` is unknown on
  devin 3000.x — use `--permission-mode dangerous`.

## 8. Open items for the successor

1. XP-014 in flight (§4) — review its diff when RESULT+REVIEW land.
2. XP-015 full-market acceptance once XP-014's artifact passes CI+review.
3. Issue #324 stopped-write canonical-outcome defect — promotion blocker.
4. Final merge/release/deploy — requires explicit user decision.
5. Optional hardening: PROTOCOL already encodes "confirm action after
   notify" — consider adding the queued-message Enter nuance explicitly.
