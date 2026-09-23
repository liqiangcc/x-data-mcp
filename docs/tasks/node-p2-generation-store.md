# Node 迁移 P2 — 不可变代际 staging/install

任务：NM-033（agent-coordination/node-migration）。日期：2026-09-23。
基线：`fedac79105516e06219ba3be2bb0810b976412e9`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；#260 代际生命周期；`src/generation/mod.rs::{create_stage,staged_manifest,install_staged,installed_ids}`。
证据：`evidence/node-migration/p2/generation_store/`（probe-results.jsonl 30 行全 PASS、rust-oracle.jsonl、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果（R1）：30/30 PASS / 0 FAIL —— 16 例 live Rust oracle 全类别一致（含 reopen 持久性行）。**

## 1. 移植：`src_node/governance/generation_store.mjs`

- `createStage(rid)`：已存在 → `staging` 逐字拒收；`stagedManifest(rid)`：读失败 → `unverified_stage`，结构/解析失败 → `json`
- `installStaged(rid, now)`：gid=sha256(manifest)；目标已存在 → `already_installed` 逐字；`inspect` 失败 → `not_verified("snapshot inspection failed: ...")`；production_ready 护栏；mkdir generations + **renameat2(RENAME_NOREPLACE)**（N-API，EEXIST→already_installed，不替换既有空目录）+ fsync_dir
- `installedIds()`：**UTF-8 字节序**排序目录名（U+E000 < U+10000 差分验证）；目录缺失 → []；staging 幽灵不列入
- **严于 Rust（已记录）**：request_id 边界（非空 wellformed；禁 `/ \ NUL . ..`）在变更前拒收

## 2. 持久性

同文件系统 rename + generations 目录 fsync；reopen 行展示 rename 后可观测状态。进程内无法注入 rename↔fsync 崩溃——不宣称崩溃持久性，已记录。

## 3. 附带修复

按 REVIEW-NM-032-FINAL 修正 `generation_usable.mjs` 过期注释（post-fstat 增长）——仅注释，不改行为。

## 4. 验证矩阵（25 例）

| 组 | 例数 | 结果 |
|---|---|---|
| oracle（create/staged/install/重复/已安装/孤儿/reopen） | 16 | PASS |
| request_id 边界（8 种逃逸形状） | 8 | PASS |
| 持久性观测 | 1 | PASS |
| 自测 `store` | exit 1 | |

## 5. 范围边界

仅本地 fs；无官方下载/复制，无 active 指针/判定写入/钉住/退役/服务/DB/部署。

## 6. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/generation_store_probe.mjs .` | 0（30 PASS） |
| `node ... store`（自测） | 1 |
| NM-032 / NM-026 / NM-028 回归 | 0（28/72/14 PASS） |
| `rustc` store_oracle | 0 |
| `sha256sum -c` / `git diff --check` | 0 |

> R1（REVIEW-NM-033）：① `installedIds` 改 UTF-8 字节比较（JS UTF-16 序相反）。② rename 改 `renameat2(RENAME_NOREPLACE)` 原子不覆盖（`src_node/native/rename_noreplace.c`，gcc 直编 `.node` 不入库，加载失败即拒）。③ `exists` 仅 ENOENT/ENOTDIR 视为缺席；`createStage` 用独占 mkdir 杜绝双成功。
