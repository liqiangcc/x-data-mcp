# Node 迁移 P3 — 原子任务 claim CAS

任务：NM-043（agent-coordination/node-migration）。日期：2026-09-24。
基线：`83a4becf064685a492195662612674dc9ba3fd09`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；`task_control::decide(Change::Claim)` + `transition_tx`/`guarded_task_update`。
证据：`evidence/node-migration/p3/task_claim/`（probe-results.jsonl 11 行全 PASS、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果：11/11 PASS / 0 FAIL。**

## 1. 原语（`task_repository.mjs::TaskAdmission.claim`）

`claim(taskId, expectedRevision, runId, now)` 单事务：

- 边界：task_id/runId/now 校验（`empty run identity` 逐字）；expectedRevision 必须 bigint ≥0
- 任务缺失 → `task_not_found`（`TASK_NOT_FOUND`）
- decide 门：status='pending' 且 owner_run_id IS NULL，否则 `revision_conflict`（`STATE_REVISION_CONFLICT`）
- 守护 UPDATE：`SET status='running',state_revision=rev+1,owner_run_id=run,last_progress_at=now,RECOUNT_COUNTERS WHERE task_id AND state_revision=expected`；`changes!=1` → `revision_conflict`（库内 CAS 兜底竞态/陈旧）
- item 行与 scope_occupancy 行不动（Rust 在 read_task 时按活 revision 标注）

## 2. 验证矩阵（11 例）

| 组 | 例数 | 结果 |
|---|---|---|
| 成功 claim（恰一行、rev+1、owner、计数/占用不变） | 1 | PASS |
| 拒绝（二次 claim/陈旧 rev/缺任务/已属他人/终态） | 5 | PASS |
| 边界（空 run/非 bigint rev/空 id/隐式 mode） | 4 | PASS |
| 重开读回持久态 | 1 | PASS |
| 自测 `claim` | exit 1 | |

## 3. 范围边界

仅 claim；无 item 转换/取消/重试/worker/服务。

## 4. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p3/task_claim_probe.mjs .` | 0（11 PASS） |
| `node ... claim`（自测） | 1 |
| NM-042/041/040/039/038/037/036/035 回归 | 0（11/16/19/16/24/12/35/23） |
| `sha256sum -c` / `git diff --check` | 0 |
