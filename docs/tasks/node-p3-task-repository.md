# Node 迁移 P3 — 只读 task/request 仓储切片

任务：NM-040（agent-coordination/node-migration）。日期：2026-09-24。
基线：`1225b3a2cbdbf756b1b5ec7825de54127d021952`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；NM-038 适配器；NM-039 schema 门。
证据：`evidence/node-migration/p3/task_repository/`（probe-results.jsonl 19 行全 PASS、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果：19/19 PASS / 0 FAIL。**

## 1. 仓储（`src_node/storage/task_repository.mjs`）

- `TaskRepository.open(path, {owner})` → `gateDatabase`（schema 门）+ readonly adapter；不支持/损坏库在查询前 fail closed
- `getRequest(id)`：`sha256(command_json)==command_digest` 逐字校验 → `command digest mismatch` → `inconsistent`
- `getTask`/`getTaskForRequest`/`getRequestForTask`：17 列/6 列完整映射，i64 计数器 bigint，null 保留
- `listTasks({limit,cursor})`：keyset `(created_at,task_id)`（Rust claim 序），limit 必填 1..10000；`owner` 显式输入，null=全 owner（显式 opt-out，不静默放宽）
- 全参数绑定，无 SQL 插值

## 2. 验证矩阵（19 例）

| 组 | 例数 | 结果 |
|---|---|---|
| 开门 + 行映射（gate 能力、typed 行、null、digest、终态、双向绑定） | 6 | PASS |
| 排序/分页 + owner 域 | 2 | PASS |
| digest 篡改 → inconsistent | 1 | PASS |
| 边界（limit/cursor/id/owner） | 5 | PASS |
| fail-closed（garbage/empty/missing） | 3 | PASS |
| 字节不变 + 自测 `repo` | 1 + exit 1 | |

## 3. 范围边界

无写入/CAS/revision/occupancy/worker 执行/崩溃恢复/服务接入。

## 4. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p3/task_repository_probe.mjs .` | 0（19 PASS） |
| `node ... repo`（自测） | 1 |
| NM-039/038/037/036/035 回归 | 0（16/24/12/35/23） |
| `sha256sum -c` / `git diff --check` | 0 |
