# Node 迁移 P2 — 代际判定/证据 artifact 一次性写入

任务：NM-034（agent-coordination/node-migration）。日期：2026-09-23。
基线：`99c8fe93380a0378b06eb942713d19eaaedf1de8`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；#260；`src/generation/mod.rs::{write_decision,decision,write_artifact,artifact}`。
证据：`evidence/node-migration/p2/generation_artifacts/`（probe-results.jsonl 27 行全 PASS、rust-oracle.jsonl、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果：27/27 PASS / 0 FAIL —— 19 例 live Rust oracle 类别与消息逐字一致。**

## 1. 移植（`generation_store.mjs` 扩展）

- `writeDecision` / `writeArtifact`：目录不存在 → `not_installed`；`O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW` 独占创建 → EEXIST → `decision_exists`/`artifact_exists` 逐字；完整写 + fsync 文件 + fsync 目录；返回 sha256
- `decision`/`artifact`：ENOENT → null，其余 → bytes/io；读限 ≤1MiB（decision 与 NM-032 对齐；artifact 严于 Rust，已记录）
- 名称检查：Rust 逐字（空/`/`/`\`/NUL）+ wellformed（代理拒绝，防 WTF-8 别名）；`..` 解析到目录 → `artifact_exists`（与 Rust 结果一致）
- `generation_id` 边界：禁 `.`/`..`/分隔符/NUL/代理（严于 Rust，已记录）

## 2. 冲突证明

普通文件与符号链接冲突均拒；外部符号链接目标字节未动；`O_NOFOLLOW` 保证绝不打开既有目标写。

## 3. 验证矩阵（27 例）

| 组 | 例数 | 结果 |
|---|---|---|
| oracle（缺失/写/复读/重拒/名称/链接/reopen） | 19 | PASS |
| 边界（gid 逃逸、代理名、非 Buffer、超界读、决策名符号链接） | 8 | PASS |
| 自测 `art` | exit 1 | |

## 4. 范围边界

合成代际目录；不改 `usable` 语义、不写真实 gen-2 判定、无 DB/服务/激活/部署。

## 5. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/generation_artifact_probe.mjs .` | 0（27 PASS） |
| `node ... art`（自测） | 1 |
| NM-033 / NM-032 / NM-026 / P1-native 回归 | 0（38/28/72/10） |
| `rustc` art_oracle | 0 |
| `sha256sum -c` / `git diff --check` | 0 |
