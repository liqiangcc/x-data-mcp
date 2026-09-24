# Node 迁移 P3 — SQLite schema 门与能力读取

任务：NM-039（agent-coordination/node-migration）。日期：2026-09-24。
基线：`93cc9eb7f521fd67bb3b94b4b504c7e740ee9411`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；NM-006 SQLite 契约；NM-038 适配器。
证据：`evidence/node-migration/p3/schema_gate/`（probe-results.jsonl 15 行全 PASS、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果：15/15 PASS / 0 FAIL。**

## 1. 门（`src_node/storage/schema_gate.mjs`）

`gateDatabase(path)` 只读三级判定，返回 `{schemaVersion, kind, migrationsVerified, tables, indexes, bytesUnchanged}`：

1. `user_version`：0 或非 {18,19} → `unavailable`（`unsupported/uninitialized schema {v}; explicit initialization/upgrade required`，与 Rust `check_schema` 逐字）
2. `schema_migrations` 校验链：行数==版本、version 有序 1..v、checksum==sha256(已提交 `migrations/tasks/*.sql` 原始字节) → 否则 `schema migration identity mismatch` 逐字
3. 结构检查：从迁移 SQL 解析的 op 链（CREATE/DROP TABLE、RENAME、ADD COLUMN、INDEX，含 DROP 级联与 v17 post-migration `DROP TABLE version_segments`）推导应有表/索引/列 → 缺失即 `unavailable`

schema 19 报告为 `kind:"compact"`（#259 opt-in）；门不声明 legacy 兼容。

## 2. 验证矩阵（15 例）

| 组 | 例数 | 结果 |
|---|---|---|
| 正向（18→legacy、19→compact、字节不变） | 3 | PASS |
| 版本门（0/10/20/缺文件/垃圾文件） | 4 | PASS |
| 链完整性（checksum 篡改、行缺失） | 2 | PASS |
| 结构（删表/删列/删索引） | 3 | PASS |
| 副作用（WAL 副产物非变异、readonly 拒绝写） | 2 | PASS |
| 自测 `gate` | exit 1 | |

## 3. 范围边界

只读能力读取；无仓储 CRUD、无迁移执行、无崩溃恢复、无服务接入。

## 4. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p3/schema_gate_probe.mjs .` | 0（15 PASS） |
| `node ... gate`（自测） | 1 |
| NM-038/037/036/035 回归 | 0（24/12/35/23） |
| `sha256sum -c` / `git diff --check` | 0 |
