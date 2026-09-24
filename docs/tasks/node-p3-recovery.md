# Node 迁移 P3 — 恢复面：require_verification + reconcile + candidates

任务：NM-046（agent-coordination/node-migration）。日期：2026-09-24。
基线：`88634588453c130396bbb74ff399c5dc9e51b1be`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；`decide` RequireVerification/Reconcile + `reconcile_tx`/`candidates_tx`。
证据：`evidence/node-migration/p3/recovery/`（probe-results.jsonl 12 行全 PASS、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果：12/12 PASS / 0 FAIL。**

## 1. 原语（`task_repository.mjs::TaskAdmission`）

- `requireVerification(taskId, rev, run, itemId, reason, now)`：共享门序 + arm 守卫（空 reason / owner!=run / status 非 running|cancel_requested / item 非 running → `revision_conflict`）；写 `needs_verification` + `result_json={"execution_uncertainty":{item_id,previous_result,previous_status,reason}}`（serde 键序，previous_* 逐字）；item 行不动；rev+1 RECOUNT CAS
- `reconcile(run)`：选非终态 `accepted<>run OR owner 外属`；`visible!=status` → `needs_verification` rev+1 **不盖 progress 戳**；一致行零写；返回计数
- `candidates(run)`：`accepted=run AND owner NULL AND pending|cancel_requested ORDER BY created_at,task_id`；claims 按活 revision 读时标注（不写）

## 2. 验证矩阵（12 例）

| 组 | 例数 | 结果 |
|---|---|---|
| require_verification（成功/cancel_requested 下/三守卫分流） | 3 | PASS |
| reconcile（标外属 running/一致行零写/progress 不变） | 2 | PASS |
| candidates（排序/scope/claims 活戳/nv 排除） | 2 | PASS |
| 边界（空 run/空 item/隐式 mode） | 4 | PASS |
| 自测 `rec` | exit 1 | |

## 3. 范围边界

无 retry/verification inspection/market 仓储/worker/服务。

## 4. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p3/recovery_probe.mjs .` | 0（12 PASS） |
| `node ... rec`（自测） | 1 |
| NM-045..NM-035 回归 | 0（15/19/13/11/16/19/16/24/12/35/23） |
| `sha256sum -c` / `git diff --check` | 0 |
