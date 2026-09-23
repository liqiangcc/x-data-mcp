# Node 迁移 P0 契约与表面冻结清单

任务：NM-001（agent-coordination/node-migration）。日期：2026-09-23。
基线：`02d4884b76035094e450e86ecede22887c8756fe`（Worker 分支 `migration/node-devin`，worktree `/home/box/x-data-mcp-node`）。
作者：devin-node（swe-2-high）。机器可读清单：`evidence/node-migration/p0/inventory.json`。

本文件是迁移方案 `docs/tasks/nodejs-full-migration-plan.md` §8 阶段 P0 的首个可审查产物：冻结当前 CLI/MCP 表面、schema/存储边界、数值/摘要/安全验证入口、保留输入与证据引用，并显式列出 P1 待决项。它不是实施声明；`#262` 保持 OPEN 且**不授权迁移范围**，迁移实施需要独立 Epic/Issue。

状态约定：`measured`=本会话在基线 SHA 上读源码/只读命令核实；`historical`=Issue 评论/已提交文档的历史证据，本轮未重跑；`proposal`=设计建议，未批准；`not_run`=入口存在但本轮未执行。

## 1. CLI 入口清单（12 个，全部 measured）

源码 `src/bin/`；除标注外退出语义均为 `Ok`→0、错误经 `eprintln`+`exit(1)`。

| 二进制 | 用法（源码证据） | 退出码 | 平台 | 备注 |
|---|---|---|---|---|
| `data_service` | `data_service <initialized-db> <private-socket> [--poll-ms <10..60000>]`；`--version` 打印 `x-data-mcp <pkg> <build-sha> NOT_PRODUCTION_READY` | 0/1 | Linux-only（非 Linux `run()` 直接报错） | DatabaseLease flock + Endpoint + WorkerGate + SIGTERM/SIGINT；不建库不迁移；schema-19 库复用本进程租约 |
| `acquire_service` | `acquire_service <generation-root> <private-socket> [--poll-ms <10..60000>] [--mirror <dir>]`；`--version` 打印 `x-data-acquire …` | 0/1 | Linux-only | `AcquireService::open` 建私有 store 布局并对账 durable 状态；`Source::Remote` 默认，`--mirror` 为本地镜像源 |
| `mcp_connect` | `mcp_connect <private-socket>` | 0(EOF)/1 | Linux-only | 纯传输 stdio↔unix socket 代理；服务死亡产生 EOF 而不等客户端 stdin |
| `mcp_data` | `mcp_data <initialized-database-path>`；`--version` | 0/1 | 任意（`inspect_verification_task` 工具非 Linux 不注册） | 开发 stdio 入口，挂全量 data_supply registry；不建库不导 fixture |
| `mcp_server` | 无参数；env `X_DATA_MCP_STORE`（默认 `.x-data-mcp`） | 0(EOF)/1 | 任意 | 旧八工具 JSON-store 原型；`OfficialFilePipeline` fail-closed stub |
| `mcp_sqlite_dev` | `mcp_sqlite_dev <initialized-database-path>` | 0(EOF)/1 | 任意 | 旧六任务控制工具，SQLite 任务存储 |
| `task_sqlite` | `task_sqlite <init\|init19\|upgrade> <database-path>` | 0 打印 `task schema <N> ready`/1 | 任意 | `init`→legacy schema 18；`init19`→全新 compact schema 19（#259 fresh-rebuild 生产入口）；`upgrade`→legacy 升级；普通读取永不触发 |
| `inspect_day` | `inspect_day PATH_TO_DAY_FILE`（恰好一个参数） | 0 打印 `x-data-mcp.day-structural-inspection.v1` JSON/1 | 任意 | 仅结构：sha256/bytes/record_count/首末日期；`business_semantics=NOT_VERIFIED` |
| `inspect_snapshot` | `inspect_snapshot <manifest.json> <trusted-input-directory>` | 0/1 | 任意 | manifest 读取上限 1MiB+1 |
| `consistency_check` | `consistency_check <input.json>` | 0 打印 `file_consistency::validate` verbatim 决定/1 | 任意 | 本地 Input schema `deny_unknown_fields` |
| `create_demo` | `create_demo <NEW fixture-database-path>` | 0 打印 `FIXTURE_ONLY` JSON/1 | 任意 | 经真实 task/publish 路径发布合成数据；不会被正常服务调用 |
| `seed_gap_subject` | `seed_gap_subject <database> --symbol <ex.code> [--drop-date <YYYY-MM-DD>] [--source official\|fixture] [--request-id <id>] [--catalog-version <id>]` | 0/1 | 任意（需 env 固定官方快照 `X_DATA_MCP_OFFICIAL_MANIFEST`+`X_DATA_MCP_OFFICIAL_DIR`） | #187 验收支持；无已检查官方源即拒绝；coverage 标注 scratch subject |

参数/退出码均来自各 `src/bin/*.rs` 的 usage 字符串与 `main` 结构（measured）；实际运行行为本轮 `not_run`。

## 2. MCP 工具表面（registry 实测，非 README 静态数字）

### 2.1 data_service / mcp_data —— 20 工具（Linux）
- 注册表：`src/data_supply/registry.rs` `entry<I,O>` 单点注册；schema 经 schemars 从 `src/data_supply/wire.rs` typed wire 同源生成；golden example 同文件内联。
- 工具（注册顺序）：`inspect_verification_task`（**`#[cfg(target_os="linux")]`**，非 Linux 为 19）、`verify_publication_result`、`verify_stopped_writes`、`verify_data_gap`、`update_data`、`backfill_data`、`republish_data`、`run_worker_once`、`compute_metrics`、`start_collection`、`retry_task`、`get_task`、`get_request`、`cancel_task`、`query_data`、`list_data_versions`、`get_data_availability`、`list_tasks`、`get_task_progress`、`get_data_capabilities`。
- descriptor 字段：`name/handler/description/input/output/example/operator_only`；`get_data_capabilities` 输出 `implemented/operator_only/runtime_readiness/governance_readiness` + `blocking_reasons{code,detail,evidence_ref?}` + `declared_scope` + `active_generation_ref`（#262 Phase A 契约，historical）。

### 2.2 acquire_service —— 10 工具
- 注册表：`src/acquire/mcp.rs`；store `src/acquire/store.rs`；wire `src/acquire/wire.rs`。
- 工具：`check_official_updates`、`acquire_generation`、`get_acquisition_task`、`get_acquisition_request`、`verify_generation`、`activate_generation`、`retire_generation`、`sync_status`、`run_worker_once`、`get_acquisition_capabilities`。
- capability_revision `x-data-acquire-v1`；二进制 Linux-only。

### 2.3 legacy mcp_server —— 8 工具
`src/mcp.rs` `public_tools()`：`start_collection`、`get_task`、`run_worker_once`、`cancel_task`、`retry_task`、`get_market_data`、`list_market_data_versions`、`get_request`。untyped 旧 wire（`symbols`/`periods`），JSON TaskStore。

### 2.4 legacy mcp_sqlite_dev —— 6 工具
`src/sqlite_task_mcp.rs`（对 `mcp::public_tools()` 过滤）：`start_collection`、`get_task`、`get_request`、`cancel_task`、`retry_task`、`run_worker_once`。Stage A 开发用；worker 只记录来源拒绝。

README 中的 8/13/15/16/18 等历史数字为过期快照；冻结以本节实测为准。

## 3. Schema / 存储边界

- 常量：`src/sqlite_tasks.rs` `SCHEMA_VERSION=18`（legacy）、`COMPACT_SCHEMA_VERSION=19`；`src/sqlite_market/backend.rs:31,49` 按库 schema 分派 `Legacy`/`Compact`。
- 迁移：`migrations/tasks/001.sql`–`019_compact_market.sql` 共 19 个显式迁移；`init`/`upgrade`/`init19` 语义见 §1 `task_sqlite`；非空升级拒绝、显式触发语义不随语言改变。
- legacy JSON：仅 `mcp_server` 原型的 `TaskStore`（JSON 文件）；与 SQLite 无跨库原子发布。
- compact 后端：`src/sqlite_market/{compact_repo,compact_results,compact_runtime,compact_versions,gaps,yearly_results}/` 与 `compact_v19.rs`。
- **文档滞后事实**：README `SCHEMA_VERSION=18` 指 legacy 常量；#262 2026-09-22 评论（historical）记录 ECS `preview.18` 已用 fresh schema-19 完成 MCP-only E2E。AGENTS.md「当前 schema 禁止 ECS 全市场 collection」截至 2026-09-21 成立；schema-19 已修 audit payload/`bar_json`，剩余阻塞为 >100 实体 `FULL_MARKET_NOT_SUPPORTED` 与 `production_ready=false`。本文件不改 README/AGENTS。

## 4. P0 验证地图（冻结入口；执行均 not_run）

| 领域 | 关键源码 | 现有测试/验证入口 |
|---|---|---|
| 数值/定点/BigInt | `src/qfq.rs`、`src/yearly.rs`、`src/gbbq.rs`、`src/market_data.rs`、`src/metrics/consecutive.rs`（num-bigint 唯一使用点，half-even 12dp）、`src/compact_market.rs`、`src/official_market.rs`、`src/data_supply/observe.rs`、`src/sqlite_market.rs`、`src/sqlite_market/compact_v19.rs` | `tests/qfq.rs`、`tests/yearly.rs`、`tests/consecutive.rs`、`tests/gbbq_semantic.rs`、`tests/gbbq.rs`、`tests/day_file.rs`、`tests/dbf.rs`、`tests/metrics_storage.rs`、`tests/metrics_stdio.rs` |
| canonical JSON/摘要 | `src/compact_market.rs`、`src/snapshot.rs`、`src/official_market.rs`、`src/verification_runtime.rs`、`src/sqlite_market/compact_results.rs`、`src/hash.rs`；crates `sha2`、`serde_json_canonicalizer` | `tests/fixtures/issue79/legacy-records.json` + `legacy_record_digest.rs.txt`、`tests/storage_roundtrip.rs`、`tests/sqlite_market.rs`；三种摘要风格：legacy-published-record-v1 / RFC8785 / compact envelope |
| MCP framing/协议 | `src/mcp.rs`（`run_stdio` JSON-RPC）、`src/local_runtime/frame.rs`、`src/local_runtime/server.rs` | `tests/mcp.rs`、`tests/mcp_stdio.rs`、`tests/data_stdio.rs`、`tests/metrics_stdio.rs` |
| SQLite 原子性/恢复 | `src/sqlite_tasks.rs`+`sqlite_tasks/`、`src/sqlite_market/`、`src/task_control/`、`src/market_update/`、`src/execution.rs`、`src/worker.rs`、`src/data_supply/worker.rs` | `tests/sqlite_tasks.rs`、`tests/sqlite_market.rs`、`tests/sqlite_binaries.rs`、`tests/sqlite_catalog.rs`、`tests/sqlite_range_update.rs`、`tests/publish.rs`、`tests/market_update.rs`、`tests/occupancy.rs`、`tests/execution.rs`、`tests/task.rs`、`tests/retry.rs`、`tests/cancel.rs`、`tests/stopped_verification.rs`、`tests/publication_verification.rs` |
| Linux 文件/UID/租约原语 | `src/local_filesystem.rs`（`supported_file` fstatfs 白名单）、`src/local_runtime/paths.rs`（`O_NOFOLLOW` 目录解析、flock 租约、`SO_PEERCRED` 同 UID）、`src/local_runtime/server.rs`、`src/verification_runtime.rs`、`src/sqlite_verification_runtime/guarded.rs` | `tests/local_runtime.rs`、`tests/local_runtime/lifecycle.rs`、`src/sqlite_verification_runtime/tests.rs`、`scripts/test_runtime_peer.py`（跨 UID，CI 中 sudo） |
| generation/采集 | `src/generation/mod.rs`、`src/acquire/{fetch,service,store,wire,mcp}.rs` | `tests/acquire_service.rs`、`tests/data_supply_generation.rs` |
| 文件一致性/快照/目录 | `src/file_consistency.rs`、`src/snapshot.rs`、`src/official_files.rs`、`src/official_market.rs`、`src/catalog_tdx/` | `tests/file_consistency.rs`、`tests/snapshot.rs`、`tests/official_files.rs`、`tests/official_market.rs`、`tests/catalog_tdx.rs`、`tests/catalog.rs`、`tests/sqlite_catalog.rs` |

相关 env：`X_DATA_MCP_STORE`（mcp_server）、`X_DATA_MCP_OFFICIAL_MANIFEST`+`X_DATA_MCP_OFFICIAL_DIR`（官方快照源）、`X_DATA_MCP_UNSAFE_NONLOCAL_FS=1`（#213 开发 opt-out，生产禁用）、`SOAK_DAYS/SOAK_SYMBOLS/SOAK_DUMP`（容量 soak）。

## 5. 保留输入与证据引用

- `/home/box/retained-inputs/MANIFEST.md`（measured）：gen-2 固定 sha256（gbbq.zip、hsjday.zip 549MB、base.zip、member、gbbq_reader.py）；gen-3（2026-09-20 #217 有界采集，gen3/ 三 zip）；`official-tq-docs/` 快照；客户端 deb。**gen-1 永久不可得**，跨代一致性走已提交派生证据。命中+hash 匹配才可用；本任务不下载。
- `evidence/` 59 个目录（measured）：issue-259-s0…s5f 系列、issue-262-mcp-daily-sync、issue-217-gen3-acquisition、issue-54-gbbq-decimal、issue-90-community-qfq 等。
- 性能交接（historical）：`docs/tasks/issue-262-local-performance-handoff.md`（Rust 基线 p50 聚合 -21% 等）、`docs/tasks/issue-262-performance-analysis.md`。

## 6. 事实 / Issue 证据 / 提案 / 待决分离

### 已确认事实（measured）
- 12 CLI、data 20（Linux）+acquire 10+legacy 8+6 工具表面、schema 18/19 双常量与 backend 分派、19 个迁移、§4 验证入口全部以源码路径在案。
- 依赖面（Cargo.toml）：rust_decimal、serde/serde_json、thiserror、tempfile、sha2、rusqlite 0.40.2(bundled)、schemars、num-bigint/num-traits、encoding_rs、zip 2.4.2、serde_json_canonicalizer=0.3.2、libc=0.2.189(linux)、jsonschema(dev)。
- 环境：Box `node v20.19.2`/`npm 9.2.0`；`node:sqlite` **不存在**（实测 `ERR_UNKNOWN_BUILTIN_MODULE`）；cargo 1.85.1 不可用需 rustup 1.98.1；ECS 本轮未访问。

### Issue 证据（historical）
- #262 OPEN：D1–D6 用户批准；preview.18 schema-19 ECS E2E 功能 PASS（2026-09-22）；>100 实体 default collection 拒（`FULL_MARKET_NOT_SUPPORTED`）；`production_ready=false`。**#262 不授权迁移范围**。
- #259：v14 物理审计 17.9GiB 基线；schema 16→19 演进链；审计时点容量 AC 未过。

### 提案（proposal，未批准）
- 迁移方案的 Node 24 LTS、单 npm package 目录划分、50%/1.2x 性能预算均为设计建议。
- P0 后续切片建议：①冻结 tools/list+capabilities+错误分类+golden 落盘 JSON；②wire/schema/摘要固定向量；③数值 golden（含 f32 投影、BigInt half-even、None 优先）；④故障/恢复向量（crash replay、CAS、needs_verification、guard 拒绝、TOCTOU）；⑤同机 Rust dev-loop 与运行时探针基线报告。

### 待决/阻塞（不阻塞本文件，阻塞对应阶段）
- NM-P0-D1 Node 运行时版本（Box v20 无 node:sqlite；ECS 未确认）→ P1。
- NM-P0-D2 SQLite 驱动选型（node:sqlite vs better-sqlite3 等）→ P1。
- NM-P0-D3 Linux 原语绑定（fstatfs/flock/SO_PEERCRED/O_NOFOLLOW）→ P1，不得降级 PID 文件。
- NM-P0-D4 迁移 Epic 未建 → P0–P6 正式授权。
- NM-P0-D5 性能预算未冻结 → P5。
- NM-P0-D6 `acquire_mcp.py` 已 DEPRECATED：迁移或归档裁决 → P4 范围。
- NM-P0-D7 README/AGENTS schema 措辞滞后 preview.18 → P6 文档更新。

## 7. 范围声明

本任务仅产生文档与机器可读证据；未修改既有源码/测试/迁移/README/AGENTS，未编译、未运行服务、未装依赖、未访问 ECS、未下载输入、未创建 Issue、未 push/merge/deploy。所有测试入口标注 `not_run`；Issue 结论标注 `historical`，均需在其所属阶段重新取证。
