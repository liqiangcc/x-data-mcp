# Node 迁移 P3 — 验证存储面（审计回执 + 任务链接 + 检查谓词 + 运行时守卫）

任务：NM-048（agent-coordination/node-migration）。日期：2026-09-24。
基线：`9aa2321f75ea6f255cf5d7c9f6a9332a3193f8b8`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；`sqlite_verification.rs` / `sqlite_verification_inspection.rs` / `sqlite_verification_runtime.rs` / `verification_runtime.rs`。
证据：`evidence/node-migration/p3/verification/`（probe-results.jsonl 10 行全 PASS、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果：10/10 PASS / 0 FAIL。**

## 1. 模块

- `src_node/storage/verification_storage.mjs`：serde 规范序列化器（结构体声明序 + Value 位排序）、快照规范化/相等、readAudit（digest+关联+validate_audit 重放 prepare）、readReceipt（递归 AlreadyVerified 链）、验证侧 + 检查侧 `validate_task_link`（接入 `_readBoundTaskValidated`，检查链接先行）、`validateTerminalStop`/`validateTerminalInspection`/`validateAbsentRequest`/`validateTaskRequest`、`readRegistration`/`runtimeRegistration`/`claimWithRuntimeGuard`（三重 verify_binding 次序）、`writeTaskSnapshot`
- `src_node/storage/verification_runtime.mjs`：`ExecutionGuard` 移植（Context：canonicalize db、statfs 本地文件系统门、FileIdentity dev+ino+nlink==1、boot_id、锁目录；create：临时文件+flock+descriptor+persist_noclobber+目录 fsync；tryAcquire：非阻塞；verifyBinding：三重绑定重验）
- `src_node/native/rename_noreplace.c`：新增 `flockfd`、`statfsmagic`

## 2. 验证矩阵（10 例）

| 组 | 例数 | 结果 |
|---|---|---|
| 回执（not_applicable 读/interrupted 链接/absent→null） | 3 | PASS |
| 链接篡改 fail-closed（needs_verification） | 1 | PASS |
| 检查谓词（task_request 拒绝/absent 负例/terminal 重放与分歧/异 key） | 1 | PASS |
| 运行时（create+claim+registration/busy/overlayfs 拒绝/orphan） | 4 | PASS |
| 边界（隐式 mode） | 1 | PASS |
| 自测 `ver` | exit 1 | |

## 3. 范围边界

无 commit_stop_check/bind_existing_stop/commit_inspection 写路径（本切片只覆盖读+claim）、market 仓储、publication 注册表、worker、服务。

## 4. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p3/verification_probe.mjs .` | 0（10 PASS） |
| `node ... ver`（自测） | 1 |
| NM-047..NM-035 回归 | 0（14/17/15/19/13/11/16/19/16/24/12/35/23） |
| `sha256sum -c` / `git diff --check` | 0 |
