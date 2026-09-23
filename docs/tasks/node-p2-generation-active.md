# Node 迁移 P2 — active 指针读取与条件交换

任务：NM-035（agent-coordination/node-migration）。日期：2026-09-24。
基线：`d24570f71093af2d8f091e09ca0e48419bd9b3f4`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；#260 代际激活；`src/generation/mod.rs::{active,activate,ActivePointer}`。
证据：`evidence/node-migration/p2/generation_active/`（probe-results.jsonl 16 行全 PASS、rust-oracle.jsonl、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果：16/16 PASS / 0 FAIL —— 11 例 live Rust oracle 含逐字冲突文本与逐字节 to_vec_pretty 输出。**

## 1. 移植（`generation_store.mjs` 扩展）

- `active()`：ENOENT → null；strictJson + serde 字段校验 → `json`；**oracle 验证**：`schema` 为普通 String（错误值照样接受）、`previous_generation_id` 缺席 → null
- `activate()`：CAS `expected_previous` vs 当前 gid（null=缺席）→ 冲突 `active pointer conflict: expected {expected:?}, found {actual:?}` 逐字（Debug Option fmt）且指针字节不动；成功写 `.active-generation.json.tmp` → fsync → rename → fsync_dir(root)
- 指针字节 = `to_vec_pretty` 逐字节一致（337 字节逐字节比对）
- **严于 Rust（已记录）**：request_id/gid 边界、expected_previous 类型化、指针读取 ≤1MiB

## 2. 调用方排他

activate 自身不加锁（与 Rust 一致）。探针通过 `linux_guards.node` 的 flockfd 在 lease 文件上持锁，独立进程 EWOULDBLOCK 拒绝——真实内核竞争，非进程内标志。

## 3. 验证矩阵（16 例）

| 组 | 例数 | 结果 |
|---|---|---|
| oracle（缺席/坏JSON/未知字段/坏时间戳/错误schema/缺字段/首次/冲突×2/二次/reopen） | 11 | PASS |
| 租约+边界（flock 持有、跨进程冲突、租约下激活、坏 now 不变更） | 4 | PASS |
| 指针冲突不变更证明 | 1 | PASS |
| 自测 `act` | exit 1 | |

## 4. 范围边界

合成 root；无真实激活、无 DB/服务、无已验证推断。reopen 证明可读性，非崩溃持久性。

## 5. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/generation_active_probe.mjs .` | 0（16 PASS） |
| `node ... act`（自测） | 1 |
| NM-034 / NM-033 / NM-032 / NM-026 回归 | 0（45/38/28/72） |
| `rustc` active_oracle | 0 |
| `sha256sum -c` / `git diff --check` | 0 |
