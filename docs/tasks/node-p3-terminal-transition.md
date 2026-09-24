# Node 迁移 P3 — 终态转换：cancel + finish_execution + finish_cancellation

任务：NM-045（agent-coordination/node-migration）。日期：2026-09-24。
基线：`4ed3a2fe6003239daade6466bbded83e42db36f8`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；`decide` RequestCancel/FinishExecution/FinishCancellation + `write_task_delta` 终态路径。
证据：`evidence/node-migration/p3/terminal_transition/`（probe-results.jsonl 14 行全 PASS、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果：14/14 PASS / 0 FAIL。**

## 1. 原语（`task_repository.mjs::TaskAdmission`）

共享门序逐字：存在 → 存储 rev==expected → 终态（RequestCancel 特例：终态/已取消 → Ok 无写，先于 run 门）→ run 身份 → arm → CAS。

- `requestCancel`：非终态非已取消 → `cancel_requested`，rev+1，progress 戳；不改 result/occupancy
- `finishExecution`/`finishCancellation`：后者须 `cancel_requested` 否则 `revision_conflict`；`owned || never_started` 且无 running item，否则 `needs_verification` "execution has not safely ended" 逐字
- 派生：cancel_finish 且 success<len → `cancelled`；finished==len → succeeded/failed/partially_failed；else cancel_requested → `cancelled`；else `revision_conflict`
- 终态写：`result_json` serde 键序（acknowledged 仅经 finish_cancellation→cancelled）；`DELETE scope_occupancy`；RECOUNT_COUNTERS；rev+1

## 2. 验证矩阵（14 例）

| 组 | 例数 | 结果 |
|---|---|---|
| cancel（pending/幂等无写/终态任意 run/跨 run 拒绝） | 4 | PASS |
| finish_execution 派生（succeeded/partially_failed/failed） | 3 | PASS |
| 守卫（running item/未启动 pending/非 owner） | 3 | PASS |
| finish_cancellation（acknowledged/须 cancel_requested/finish_execution→cancelled 无 ack） | 3 | PASS |
| 重开读回 | 1 | PASS |
| 自测 `term` | exit 1 | |

## 3. 范围边界

无 require_verification/reconcile/candidates/retry/verification inspection/worker/服务。

## 4. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p3/terminal_transition_probe.mjs .` | 0（14 PASS） |
| `node ... term`（自测） | 1 |
| NM-044..NM-035 回归 | 0（19/13/11/16/19/16/24/12/35/23） |
| `sha256sum -c` / `git diff --check` | 0 |
