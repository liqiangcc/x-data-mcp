# Node 迁移 P3 — 重试准入（accept_tx RetryGuard）

任务：NM-047（agent-coordination/node-migration）。日期：2026-09-24。
基线：`3c8bb86edeac499f88322ea3f4c3e838ec340f15`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；`accept_tx` 重试分支（sqlite_tasks.rs:1246-1287）+ `RetryGuard`。
证据：`evidence/node-migration/p3/retry_admission/`（probe-results.jsonl 14 行全 PASS、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果：14/14 PASS / 0 FAIL（R1）。**

## 1. 原语（`task_repository.mjs::TaskAdmission.admit`）

`plan.retry={originalTaskId,expectedRevision,selectedItems}` 在 admission 事务内、digest 门之后、scope 冲突之前执行：

1. 原任务经 validated bound read 存在 → `task_not_found`
2. 存储 `state_revision==expected` → `revision_conflict`
3. `command.intent 全等 {original_task_id,selected_items}（整值比较，多余键拒绝）` 且 `command.key=={operation:'retry_task',dataset,contract_version}` 沿用原 key → `invalid_retry_selection`
4. 原任务绑定 request_id != 新 id → `conflict` REQUEST_CONFLICT
5. 每个 work item 在 selected 内且 spec 逐字段与原 ItemSpec 相等且 outcome 缺失或 `success!==true` → `invalid_retry_selection`
6. `items.length==selected.length` → `invalid_retry_selection`

存储 `command_key_json` 为**原任务** key；`retry_of` 绑定；items/scopes/occupancy/request 与普通准入相同。重试 request_id 重放时按原 key 校验持久绑定。

## 2. 验证矩阵（14 例）

| 组 | 例数 | 结果 |
|---|---|---|
| 失败 item 重试成功（key=原、retry_of、occupancy 写入） | 1 | PASS |
| 守卫拒绝（成功 item/spec 分歧/陈旧 rev/缺原任务/intent 不符/intent 多键/key 非 retry_task/request_id 复用→conflict/绑定分歧→inconsistent/数量不符） | 9+1 | PASS |
| scope 冲突仍生效 | 1 | PASS |
| 重放 | 1 | PASS |
| 边界（隐式 mode） | 1 | PASS |
| 自测 `retry` | exit 1 | |

## 3. 范围边界

无重试策略/选择 UX/verification inspection/market 仓储/worker/服务。

## 4. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p3/retry_probe.mjs .` | 0（14 PASS） |
| `node ... retry`（自测） | 1 |
| NM-046..NM-035 回归 | 0（17/15/19/13/11/16/19/16/24/12/35/23） |
| `sha256sum -c` / `git diff --check` | 0 |
