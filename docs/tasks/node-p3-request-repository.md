# Node 迁移 P3 — 请求幂等准入原语

任务：NM-041（agent-coordination/node-migration）。日期：2026-09-24。
基线：`366382d16b429a4d9185791f297b2bd5e89a54c2`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；Rust `task_control/service.rs::accept` 准入路径。
证据：`evidence/node-migration/p3/request_repository/`（probe-results.jsonl 12 行全 PASS、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果：12/12 PASS / 0 FAIL。**

## 1. 原语（`src_node/storage/request_repository.mjs`）

- `RequestRepository.open(path,{mode})` —— 显式 `readwrite|create`（无静默默认），先过 schema 门
- `admit(requestId, commandJson, outcome)` 单事务：
  - 空/空白 id → `invalid_input`（`empty request_id` 逐字）
  - 非法 JSON / 非法 outcome 形状 → `invalid_input`（先于 SQL）
  - 已存在：重验存储 digest（篡改 → `inconsistent`），字节相等 → `reused:true`，不同 → `conflict`（`REQUEST_CONFLICT` 逐字），库字节不变
  - 不存在：`INSERT`（task→task_id / terminal→terminal_json 绑定），CHECK 约束兜底——违规回滚无残留

## 2. 验证矩阵（12 例）

| 组 | 例数 | 结果 |
|---|---|---|
| 准入（首插/重放/冲突不变/终态绑定） | 4 | PASS |
| CHECK 违规回滚 | 1 | PASS |
| 边界（id/JSON/outcome/显式 mode/garbage） | 5 | PASS |
| 存储 digest 篡改 | 1 | PASS |
| 自测 `req` | exit 1 | |

## 3. 范围边界

无 task 创建/CAS/revision/occupancy/worker 执行/崩溃恢复/服务接入。

## 4. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p3/request_repository_probe.mjs .` | 0（12 PASS） |
| `node ... req`（自测） | 1 |
| NM-040/039/038/037/036/035 回归 | 0（19/16/24/12/35/23） |
| `sha256sum -c` / `git diff --check` | 0 |
