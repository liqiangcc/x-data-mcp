# Node 迁移 P3 — 原子任务准入与 scope 占用

任务：NM-042（agent-coordination/node-migration）。日期：2026-09-24。
基线：`2c9da0bffb4d8037daf260a72443e2b503a484b8`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；`sqlite_tasks.rs::accept_tx`；issue-78/issue-92 准入契约。
证据：`evidence/node-migration/p3/task_admission/`（probe-results.jsonl 9 行全 PASS、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果：9/9 PASS / 0 FAIL。**

## 1. 原语（`task_repository.mjs::TaskAdmission`）

`admit(plan)` 单事务，顺序逐字对齐 `accept_tx`：

1. 请求 replay/conflict：存储 digest 重验（`command digest mismatch` → inconsistent）；字节相等 → `reused:true`；不同 → `REQUEST_CONFLICT`
2. 孤儿检查：unbound task → `unbound execution record`
3. 五条注册完整性门（绑定缺失/计数分歧/占用不一致/终态不一致/owner-stage）→ `inconsistent`
4. digest 门：全部存储命令重哈希
5. scope 冲突 → `RANGE_CONFLICT: [plan 序冲突列表]`
6. 插入：task（pending/rev0/owner NULL/counters=items 数/evidence_digest 取自 frozen_work）→ task_items(pending) → task_scopes → scope_occupancy(rev0) → requests 绑定；`task_id` 用 `task-`+`hex(randomblob(16))`

可写打开显式 + 门先行（NM-041 R1 顺序）。

## 2. 验证矩阵（9 例）

| 组 | 例数 | 结果 |
|---|---|---|
| 准入（全行/rev0/occupancy 绑定）+ 重放 | 2 | PASS |
| 冲突不变 + scope overlap → RANGE_CONFLICT | 2 | PASS |
| 畸形 plan 拒绝 + 强制约束回滚 | 2 | PASS |
| 重开读回 | 1 | PASS |
| 边界（隐式 mode / garbage 门拒） | 2 | PASS |
| 自测 `adm` | exit 1 | |

## 3. 范围边界

无 worker 转换/CAS/重试路径/崩溃恢复/服务接入。

## 4. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p3/task_admission_probe.mjs .` | 0（9 PASS） |
| `node ... adm`（自测） | 1 |
| NM-041/040/039/038/037/036/035 回归 | 0（16/19/16/24/12/35/23） |
| `sha256sum -c` / `git diff --check` | 0 |
