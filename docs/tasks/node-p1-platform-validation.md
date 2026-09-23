# Node 迁移 P1 — 运行时 / SQLite 驱动验证与选型

任务：NM-006（agent-coordination/node-migration）。日期：2026-09-23。
基线：`8a02e30218d0fc48f3f5f208ec57eab6ea91687b`（`migration/node-devin`）。
关联：Epic #271、#262。上游：P0 全套冻结（清单/契约/数值向量/恢复向量/Rust 基线）。
证据：`evidence/node-migration/p1/platform/`（probe-results.jsonl 14 行原始记录、results.json 决策记录、SHA256SUMS、README 复现）。

## 0. 结论（证据驱动，非待用户选择）

**选定候选：`better-sqlite3@11.10.0`**（精确版本固定于 `package.json` + `package-lock.json`）。

理由（实测）：系统 Node v20.19.2 上唯一无需工具链即可工作的候选——官方预编译二进制（prebuild-install，零本地编译）、同步 API、`defaultSafeIntegers` 提供 i64 精确读回、WAL/checkpoint pragma、`busy_timeout`、`readonly`/`fileMustExist`。npm audit 0 漏洞。

## 1. 候选与实测状态

| 候选 | 状态 | 证据 |
|---|---|---|
| `node:sqlite`（内建） | NOT_RUN | `ERR_UNKNOWN_BUILTIN_MODULE` on v20.19.2（需 ≥22.5）；Node 24 LTS 供给是独立运行时决策 |
| `better-sqlite3@13.0.3` | REJECTED | engines ≥22；回退 node-gyp 重建，`make` 缺失失败（npm debug log 2026-09-23T11:07:08Z） |
| `better-sqlite3@11.10.0` | SELECTED | 预编译装包成功（38 pkgs, 0 vulns）；bundled SQLite 3.49.2 |
| 其他（sqlite3 回调式、libsql、WASM） | 未深入 | 接口模型或维护/同步语义不满足同步 fail-closed 需求；记录为未选，非阻塞 |

工具链记录：gcc 14.2.0 在，**make 缺失**（node-gyp 不可运行，系统包安装被禁）。`npm ping` 182ms。

## 2. 适配层验证（对 Rust 初始化的真实 schema-18/19 合成库）

`scripts/node_migration_p1/sqlite_probe.mjs`：驱动无关 `Db` 适配器（open/close/exec/query/run/tx/pragma），调用方只见 bigint 不见驱动类型。14 行 case，全 PASS：

| case | 观察 |
|---|---|
| open schema-19 readonly | `user_version=19`, `journal_mode=wal`, 31 tables（tasks/requests/scope_occupancy 在） |
| open schema-18 readonly | `user_version=18`, 30 tables |
| empty.db 探测 | 0 tables, uv=0 → 可检测"无已知 schema"（对应 Rust STORAGE_UNAVAILABLE） |
| garbage.bin | `SQLITE_NOTADB file is not a database`（与 Rust 同一拒绝文案） |
| tx commit | request+task 对原子落库可见 |
| tx rollback | PK 冲突中断 → 无部分状态（t 计数仍 1） |
| bigint_precision | safeIntegers 下 9007199254740993、i64 min/max 精确往返为 BigInt |
| default_int64_boundary | 未开 safeIntegers 时 >2^53 静默丢精度（适配层必须全局开启，已记录为边界规则） |
| wal_checkpoint | WAL 写期 -wal 8272B → TRUNCATE checkpoint 后 0B |
| busy_locking | EXCLUSIVE 写者持锁时第二连接写 `SQLITE_BUSY`（busy_timeout=0）；读者读到已提交快照（WAL 语义正确） |
| readonly_write_rejected | `SQLITE_READONLY` |
| no_implicit_migration | readonly 打开 0 字节文件不建表、不写文件（init/upgrade 必须显式，镜像 Rust 边界） |

## 3. 兼容性缺口与停止条件（诚实声明）

- **Linux 原语不在驱动覆盖内**：fstatfs 白名单、flock 单例租约、SO_PEERCRED、private_metadata/0700 需要独立适配层（见 NM-004 gap 清单）；本探针不主张存储兼容。
- **本探针不覆盖**：schema 升级迁移执行、多表 join 性能、并发写吞吐、崩溃恢复语义 —— 均标注为未验证。
- **safeIntegers 是硬要求**：适配层边界必须开启，否则 i64 精度静默降级（有实测证据）。
- Rust 基线（NM-005）仅作比较参照；此处不对比、不宣称。

## 4. 验证记录

| 命令 | 退出码 |
|---|---|
| `npm install better-sqlite3@13.0.3` | 1（gyp: not found: make — 记录为拒绝证据） |
| `npm install better-sqlite3@11.10.0 --save-exact` | 0（prebuild，无本地编译） |
| `node -e require("better-sqlite3")` smoke | 0 |
| `node scripts/node_migration_p1/sqlite_probe.mjs /dev/shm/nm006` | 0（12 PASS + INFO + NOT_RUN） |
| `json.tool` / `sha256sum -c` / `git diff --check` | 0 |

## 5. 范围声明

仅新增/修改：`package.json`、`package-lock.json`、`scripts/node_migration_p1/sqlite_probe.mjs`、`docs/tasks/node-p1-platform-validation.md`、`evidence/node-migration/p1/platform/`。未改 Rust 源码/测试/迁移；无系统包安装、ECS、真实 DB、push/merge/deploy/publish。
