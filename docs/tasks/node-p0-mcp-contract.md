# Node 迁移 P0 — MCP 契约 descriptor/golden 冻结

任务：NM-002（agent-coordination/node-migration）。日期：2026-09-23。
基线：`ea14ede82af049c9d71485b9d66f4a6c90a8ee16`（`migration/node-devin`，二进制 `--version` 报告 `0.2.0-preview.24 ea14ede… NOT_PRODUCTION_READY`）。
关联：Epic #271（迁移）、#262（OPEN，不授权迁移范围）。上游 P0 清单：`docs/tasks/node-p0-inventory.md`。
证据：`evidence/node-migration/p0/mcp/`（probes、captures、summary.json、SHA256SUMS、README 复现步骤）。

**本文件记录的是真实 wire 输出**，非源码推断：全部捕获来自本机隔离合成库上的真实二进制（debug build，rustup cargo 1.98.1，`/dev/shm` tmpfs）。工具执行（mutating tools/call 成功路径）、非 Linux 平台、官方输入与 ECS 均未覆盖，见 §6 NOT_RUN。

## 1. 协议与 framing（实测）

- framing：换行分隔 JSON-RPC 2.0，stdio 与 unix socket 同构（`src/local_runtime/frame.rs` 逐 `\n` 成帧；4MiB 帧上限、30s 不完整帧预算来自 `server.rs` Config 默认值）。
- `initialize` 应答：`protocolVersion: "2025-06-18"`、`serverInfo{name:"x-data-mcp",version:"0.2.0-preview.24"}`、`capabilities:{tools:{}}`。
- `notifications/*` 无 id → 静默忽略（7 请求 7 应答中 notification 无响应）。
- 成功 `tools/call`：`result{content:[{type:"text",text:<json-string>}],structuredContent:<value>,isError:false}`。
- 工具级错误（ApiError）：同 result 结构，`isError:true`，`structuredContent={code,message[,scopes]}`。实测样本：`{code:"unknown_tool",message:"unsupported tool: definitely_not_a_tool"}`；`{code:"invalid_arguments",message:"missing field `contract_version`"}`（`get_task` 同型为 `missing field \`task_id\``）。
- RPC 级错误：`error{code:<jsonrpc-int>,message,data:{code,message}}`。实测：未知 method → `-32601 method_not_found`；`tools/call` 缺 name → `-32602 invalid_tool_call`。

## 2. 冻结表面（实测工具数与名单）

### data_service / mcp_data —— 20 工具
注册表 `src/data_supply/registry.rs`；descriptor 字段 `name/description/inputSchema/outputSchema`；schema 方言 JSON Schema 2020-12 + `$defs`。
`backfill_data, cancel_task, compute_metrics, get_data_availability, get_data_capabilities, get_request, get_task, get_task_progress, inspect_verification_task, list_data_versions, list_tasks, query_data, republish_data, retry_task, run_worker_once, start_collection, update_data, verify_data_gap, verify_publication_result, verify_stopped_writes`
平台限定：`inspect_verification_task` 仅 `cfg(target_os="linux")` 注册 → 非 Linux 为 19（非 Linux 构建未验证，source-derived）。

### acquire_service —— 10 工具
注册表 `src/acquire/mcp.rs`；descriptor 字段 `name/description/inputSchema`。
`acquire_generation, activate_generation, check_official_updates, get_acquisition_capabilities, get_acquisition_request, get_acquisition_task, retire_generation, run_worker_once, sync_status, verify_generation`（tools/list 顺序，非字母序）。

### mcp_server（legacy）—— 8 工具
`src/mcp.rs public_tools()`：`cancel_task, get_market_data, get_request, get_task, list_market_data_versions, retry_task, run_worker_once, start_collection`。unknown tool → `unsupported tool: …`。

### mcp_sqlite_dev（legacy）—— 6 工具
`src/sqlite_task_mcp.rs`（过滤 public_tools）：`cancel_task, get_request, get_task, retry_task, run_worker_once, start_collection`。unknown tool → `unsupported development task tool: …`（与主表不同文）。

## 3. 能力面（实测）

### get_data_capabilities（`capability_revision: "generic-data-v2"`）
- `production_ready: false`；`production_blocking_reasons`: `declared_scope_non_production`、`generation_root_unconfigured`（本次 env 未设 `X_DATA_MCP_GENERATION_ROOT`）。
- `items`: 30 条 per-operation 能力项（>20 工具数：同一工具按 dataset/operation 拆项），每项 `tool_name/contract_version/dataset/implemented/operator_only/runtime_readiness/governance_readiness/blocking_reasons/declared_scope/active_generation_ref/schema_digest/input_schema/output_schema/example`。
- 限额：`max_batch_entities=100`、`max_page_size=1000`、`max_snapshot_bytes=16777216`、`max_snapshot_rows=100000`、`snapshots_per_category=32`。`max_batch_entities=100` 即 `FULL_MARKET_NOT_SUPPORTED` 的静态上限来源。

### get_acquisition_capabilities（`x-data-acquire-v1`）
- `implementation{server:"x-data-acquire",version:"0.2.0-preview.24",generation_contract_version:"official-generation-v1",consistency_contract_version:"official-file-consistency-v1+official-generation-probe-v1"}`。
- `runtime_readiness:"blocked"`，`blocking_reasons:[no_active_generation]`（空 genroot 实测）；`governance_readiness:"ready"`；含 `schema_digest` 与 `tools` 清单。
- `sync_status`（`{contract_version:"v1"}`）：`{active:null,generations:[],inflight_tasks:[]}` + 两个 contract_version 字段。
- acquire 工具入参需要 `contract_version`；空参 → `invalid_arguments missing field contract_version`（实测）。

## 4. 关键等价/差异事实（实测）

- **三个 data 捕获逐字节相同**（sha256 `1387946c…`）：`mcp_data`@schema18、`mcp_data`@schema19、`data_service`@schema19。在当前冻结面上，tools/list、capabilities 与错误包络对传输（stdio/socket）与 schema-18/19 后端分派透明。
- acquire 与 data 的 `serverInfo.name` 同为 `"x-data-mcp"`（wire 不区分服务；capabilities.implementation.server 才区分）。
- guard 实证（本次捕获附带验证）：socket 父目录非 0700 UID-owned → 拒绝（`runtime path must be owned…`）；tmpfs 过 `supported_file` 白名单无需 opt-out；SIGTERM 后 socket 经 `Endpoint::drop` 清理、`*.sock.owner.json` 保留。

## 5. 复现与哈希

逐命令与退出语义见 `evidence/node-migration/p0/mcp/README.md`；全部文件 sha256 见 `SHA256SUMS`；归一化清单见 `summary.json`（含 task_id、baseline、环境、逐文件哈希、NOT_RUN 列表）。探针文件本身入库，重跑即可 diff。

## 6. 剩余缺口与 NOT_RUN（下一切片输入）

- mutating `tools/call` 成功路径（受理/执行/取消/重放）未冻结 —— 属 P0 故障/恢复向量切片。
- 非 Linux 工具的 19 项表（cfg 差异）仅 source-derived。
- `acquire_generation`/`verify_generation`/`activate_generation` 执行路径、generation 绑定会话（`active_generation_ref` 非 null 形态）未冻结。
- `X_DATA_MCP_GENERATION_ROOT` 装配下的能力面差异未测。
- 旧 JSON `mcp_server` 的 dataset 动态工具参数矩阵已冻结（8 工具），但其 JSON TaskStore 行为契约未冻结。
- 官方输入、ECS、生产 DB：一律 NOT_RUN/未访问。

## 7. 范围声明

#271 为迁移 Epic（OPEN）；#262 OPEN 且不授权迁移范围；性能预算（50%/1.2x）仍为未批准提案。本任务只新增文档与 `evidence/node-migration/p0/mcp/` 证据；未改运行时代码、测试、迁移、AGENTS/README、会话文件；无 push/merge/deploy/ECS/官方输入/真实 DB。
