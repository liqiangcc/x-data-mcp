# Node 迁移 P3 — SQLite 驱动中立适配层

任务：NM-038（agent-coordination/node-migration）。日期：2026-09-24。
基线：`20a4ccd509d55af8de4f005366b78d2156a6b651`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；NM-006 驱动选型验收（`evidence/node-migration/p1/platform/`）。
证据：`evidence/node-migration/p3/sqlite_adapter/`（probe-results.jsonl 21 行全 PASS、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果：21/21 PASS / 0 FAIL。**

## 1. 适配器（`src_node/storage/sqlite_adapter.mjs`）

- **显式打开模式**：`readonly` / `readwrite`（均 fileMustExist，不创建不写）/ `create`（唯一可创建模式）——readonly/readwrite 打开缺失库 → `unavailable` 且不产生文件
- **safeIntegers 默认 ON**——NM-006 C8 记录默认精度丢失；`safeIntegers:false` 为显式 opt-out
- **类型化错误** `StorageError{code,driverCode}`：SQLITE_BUSY→`busy`、READONLY→`readonly`、NOTADB→`corrupt`、CANTOPEN→`unavailable`、CONSTRAINT→`constraint`、closed/invalid_input/error
- `transaction(fn)`：driver BEGIN/COMMIT/ROLLBACK 边界，抛错即回滚不留部分状态
- `checkpoint(mode)`：WAL TRUNCATE 等模式透传
- **加载期门禁**：better-sqlite3 必须精确 11.10.0 + Node ≥20，否则拒绝加载
- 无隐式 schema 升级、无 fallback 库创建

## 2. 验证矩阵（21 例）

| 组 | 例数 | 结果 |
|---|---|---|
| 模式与 fail-closed（缺库不建、schema18/19 readonly 字节不变、empty 零 schema、garbage→corrupt、readonly 写拒、create 显式） | 9 | PASS |
| 精度与事务（BigInt 往返、opt-out 边界、commit/rollback 原子） | 4 | PASS |
| WAL/busy（journal_mode、TRUNCATE checkpoint、EXCLUSIVE 并发→busy、busy_timeout） | 3 | PASS |
| 边界（mode/path/closed handle/driver pin） | 5 | PASS |
| 自测 `sql` | exit 1 | |

## 3. 范围边界

无业务仓储、无 schema-18/19 迁移逻辑、无崩溃恢复、无服务；node:sqlite 需 Node≥22.5 未启用。

## 4. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p3/sqlite_adapter_probe.mjs .` | 0（21 PASS） |
| `node ... sql`（自测） | 1 |
| P1 sqlite_probe（NM-006 回归） | 0（12 PASS/1 INFO/1 NOT_RUN） |
| NM-037/036/035 回归 | 0 |
| `sha256sum -c` / `git diff --check` | 0 |
