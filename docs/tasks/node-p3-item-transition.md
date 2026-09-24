# Node 迁移 P3 — 原子 item start/finish 转换

任务：NM-044（agent-coordination/node-migration）。日期：2026-09-24。
基线：`0c0445f9e11e4b0c72901dc78988b3c4ca3d0058`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；`decide(StartItem/FinishItem)` + `write_item_row`/`counter_delta`/`task_item_execution_starts`。
证据：`evidence/node-migration/p3/item_transition/`（probe-results.jsonl 19 行全 PASS、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果：19/19 PASS / 0 FAIL（R1）。**

## 1. 原语（`task_repository.mjs::TaskAdmission`）

共享门序（与 NM-043 claim 一致）：存在 → 存储 rev==expected → 终态 → run 身份（accepted/owner != run → `needs_verification` 逐字）→ decide arm → 守护 CAS。

- `startItem(taskId, rev, run, itemId, now)`：`status='running'` 且 `owner==run`；其他 item 已 running 或目标非 pending → `revision_conflict`；item → `running`；**增量** counter_delta（pending-1, running+1）；同事务插 `task_item_execution_starts(task,item,run,new_rev,now)`
- `finishItem(taskId, rev, run, itemId, outcome, now)`：仅 owner 绑定（无 status arm —— cancel_requested 下也允许）；`success&&error` → `invalid_input` 逐字 `success with error`；item 须 running；`stage='finished'`+`outcome_json`（serde 规范形状：键序 success→error→evidence，error 恒在场 null，evidence 缺省时省略，封闭键集；error 须 string|null）；counters running-1, finished+1（成功再 +1 succeeded）

## 2. 验证矩阵（19 例）

| 组 | 例数 | 结果 |
|---|---|---|
| 成功 start+finish（success/failure、marker 行、计数逐步验证） | 4 | PASS |
| 拒绝不变（并发 running/非 owner/已 finish item/未 running item/缺任务/陈旧 rev/pending 任务 start） | 7 | PASS |
| 边界（success+error/空 run/坏 outcome/未知键/非字符串 error/隐式 mode） | 6 | PASS |
| cancel_requested 下 finish（无 status arm） | 1 | PASS |
| 重开读回 | 1 | PASS |
| 自测 `item` | exit 1 | |

## 3. 范围边界

无 finish_execution/finish_cancellation/cancel/reconcile/require_verification/candidates/retry/worker/服务。

## 4. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p3/item_transition_probe.mjs .` | 0（19 PASS） |
| `node ... item`（自测） | 1 |
| NM-043..NM-035 回归 | 0（13/11/16/19/16/24/12/35/23） |
| `sha256sum -c` / `git diff --check` | 0 |
