# Node 迁移 P2 — 已安装代际可用性门（usable）

任务：NM-032（agent-coordination/node-migration）。日期：2026-09-23。
基线：`614a8c48c89cb1dda58a288b779363a64ee67e2a`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；#260 代际绑定/可用不变式；`src/generation/mod.rs::{verify_installed,decision_artifact,usable}`、`src/snapshot.rs`、`src/file_consistency.rs`。
证据：`evidence/node-migration/p2/generation_usable/`（probe-results.jsonl 26 行全 PASS、rust-oracle.jsonl、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果：26/26 PASS / 0 FAIL —— 21 例 live Rust oracle 全类别一致（含真实 gen-2 无判定负例）。**

## 1. 移植：`src_node/governance/generation_usable.mjs`

- `usable(root, gid, now)`：目录缺失 → `not_installed`；manifest 读取失败/越界 → `corrupt(io)`；`sha256(bytes) != gid` → `corrupt` 逐字 "manifest digest does not match directory identity"；manifest serde 结构级（字段/类型/未知键/时间戳）→ `json`；`inspect` 语义失败 → `corrupt`；decision 缺失 → `not_consistency_verified`；decision serde/形状错 → `corrupt`；schema/gid 不绑定 → `corrupt` 逐字 "decision does not bind this generation"；manifest_sha 不一致或 status≠VERIFIED_COMPATIBLE → `not_consistency_verified`
- DecisionArtifact 类型边界：精确 10 字段、status/role/reason 枚举白名单、evaluated_at RFC3339、evidence_sha256 null|string、probes JSON 值、孤立代理拒收
- 读取有界：manifest/decision ≤1MiB
- 共享变更：导出 `snapshot.mjs` 的 `strictJson`（NM-026 回归 72/72 PASS）

## 2. 比较策略

确定性内文逐字比对（not_installed、not_consistency_verified、digest/绑定 corrupt）；io/serde 内文按 kind+稳定前缀（serde 文本差异，同 NM-026 已记录）。

## 3. 真实 gen-2

已提交 manifest `b8a0d7b9…` + retained 字节复制入 fixture → `not_consistency_verified`（不写伪判定、不推断 #131 适用）。

## 4. 范围边界

只读；不创建/修复/激活/钉住/退役代际；该门只读已记录判定，不认证签发者/来源。

## 5. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/generation_usable_probe.mjs .` | 0（26 PASS） |
| `node ... usable`（自测负证） | 1 |
| `node scripts/node_migration_p2/snapshot_probe.mjs .`（NM-026 回归） | 0（72 PASS） |
| `node scripts/node_migration_p2/generation_binding_probe.mjs .`（NM-031 回归） | 0（49 PASS） |
| `rustc` usable_oracle | 0 |
| `sha256sum -c` / `git diff --check` | 0 |
