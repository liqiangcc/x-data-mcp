---
name: x-data-daily-sync
description: 每日官方行情同步 — 仅通过 x-data-acquire / x-data-mcp 两个 MCP server 完成 generation 闭环 + update_data；无 shell/SSH/SQLite/文件旁路
triggers:
  - user
allowed-tools:
  - mcp__x-data-acquire__get_acquisition_capabilities
  - mcp__x-data-acquire__check_official_updates
  - mcp__x-data-acquire__acquire_generation
  - mcp__x-data-acquire__get_acquisition_task
  - mcp__x-data-acquire__get_acquisition_request
  - mcp__x-data-acquire__verify_generation
  - mcp__x-data-acquire__activate_generation
  - mcp__x-data-acquire__sync_status
  - mcp__x-data-mcp__get_data_capabilities
  - mcp__x-data-mcp__start_collection
  - mcp__x-data-mcp__update_data
  - mcp__x-data-mcp__get_task
  - mcp__x-data-mcp__get_request
  - mcp__x-data-mcp__get_task_progress
  - mcp__x-data-mcp__list_tasks
  - mcp__x-data-mcp__get_data_availability
  - mcp__x-data-mcp__query_data
  - mcp__x-data-mcp__list_data_versions
  - mcp__x-data-mcp__verify_publication_result
  - mcp__x-data-mcp__verify_stopped_writes
---

# x-data 每日同步编排

本 Skill 只编排结构化事实。禁止：shell/exec/SSH/curl/SQLite/读写本地文件、`run_worker_once`、自行签发任何验证结论、解析自由文本判断状态。所有调用只使用上方 allowlist 内的 MCP 工具；工具在本会话不可用（名称不符或 server 未注册）→ 立即停止并报告 `SKILL_CONTRACT_MISMATCH`。

参数一律按 `tools/list` 中该工具的 `inputSchema` 传：schema 声明 `contract_version` 时必须为 `"v1"`；schema 未声明的字段一律不传（输入对象 `deny_unknown_fields`，多余字段即拒绝）。`update_data`/`start_collection`/`acquire_generation`/`verify_generation`/`activate_generation` 各自使用独立且唯一的 `request_id`。

当前能力边界：data capability 的 `max_batch_entities=100`，default daily 和
catalog-resolved collection 超过 100 个证券会整体拒绝，不会自动分页成多个写任务。
#259 容量 AC 也尚未通过。因此本 Skill 当前只可执行不超过 100 个证券的有界功能
流程；发现目标 scope 超过 100 或无法证明不超过 100 时，必须停止并报告
`FULL_MARKET_NOT_SUPPORTED`，不得自行分片、绕过预算或在 ECS 上启动全市场 collection。

## 0. Revision 握手（每次执行第一步，不可跳过）

调用并核对：

- `mcp__x-data-acquire__get_acquisition_capabilities`：
  - `implementation.capability_revision` 必须等于 `x-data-acquire-v1`
  - `implementation.generation_contract_version` 必须等于 `official-generation-v1`
- `mcp__x-data-mcp__get_data_capabilities`（`contract_version: "v1"`）：
  - `capability_revision` 必须等于 `generic-data-v2`

任一不匹配或缺失 → 停止，报告 `SKILL_CONTRACT_MISMATCH` 与双方实际值。记录 `schema_digest`、`active_generation_ref`、`runtime_readiness`、`blocking_reasons` 作为事实。

acquisition server 在全新环境可因 `no_active_generation` 报告 blocked；该原因不阻止
`check_official_updates` / `acquire_generation` / `verify_generation` / 首次
`activate_generation`。其他 acquisition blocker 仍停止。data server 的写工具必须在
激活 generation 后重新读取 capability；目标工具若仍非 ready，则停止并原样转述
`blocking_reasons`。

## 1. 远端探测（无变化不建任务）

调用 `mcp__x-data-acquire__check_official_updates`：

- 任一 role 为 `unknown` → 停止并报告（unknown 是停止信号，不是行动提示）。
- 全部 `unchanged` → 报告 active generation ref 与检查时间，结束。**不调用 acquire_generation，不创建任何任务。**
- 存在 `changed` → 继续步骤 2。

## 2. 获取并激活新 generation

1. `mcp__x-data-acquire__acquire_generation`，`request_id` 取本次唯一值（如 `daily-<YYYYMMDD>-<短随机>`）。返回 `task_id` / `generation_id`。
2. 有界轮询 `get_acquisition_task(task_id)`：每次一轮，最多 20 轮。task 未到终态且预算耗尽 → 停止，返回 `request_id`/`task_id`/`generation_id` 并说明仍在进行，不无限等待。
3. task `failed` → 停止，转述 `error`/`detail` 与 refs。
4. `verify_generation(request_id=<新值>, generation_id)`：结果必须是 `consistency_verified`；`not_verified` / `conflict` / 其他 → 停止并报告。
5. 先 `sync_status` 取当前 `active_generation_ref`；`activate_generation(request_id=<新值>, generation_id, expected_previous=<当前 active 或省略>`) 。拒绝/conflict → 停止，旧 active 不变。
6. 再次 `sync_status` 确认 active 已切换到目标 generation，否则停止。

## 3. 数据更新

- 写入前先通过 catalog `query_data` 分页计数目标 scope；一旦发现第 101 个证券即
  停止并报告 `FULL_MARKET_NOT_SUPPORTED`。不要调用 default write 试错。
- 已有 published daily heads且目标不超过 100（`get_data_availability` 有实体覆盖）→ `mcp__x-data-mcp__update_data`，`{"dataset":"market.daily","contract_version":"v1","request_id":<唯一>,"selection":{"kind":"default"}}`。
- 全新数据库（无任何 daily head）→ 两步 `start_collection`：
  1. `{"dataset":"securities.catalog","contract_version":"v1","request_id":<唯一>,"selection":{"scope":"current_sh_sz_a"}}` → 轮询终态 → 由 `list_data_versions`（`dataset:"securities.catalog"`）取 catalog `version_id`。
  2. 对 catalog 分页计数；超过 100 时停止并保留 catalog 事实，不创建 daily collection。
  3. 仅当 catalog 不超过 100 时，`{"dataset":"market.daily","contract_version":"v1","request_id":<唯一>,"selection":{"kind":"catalog","version_id":<上一步 catalog 版本>}}` → 轮询至终态。
  - `start_collection` 不接受 `{"kind":"default"}`；default selection 只属于 `update_data`/`backfill_data`。

轮询 `get_task_progress(task_id)`：每次一轮，最多 60 轮；耗尽 → 停止并返回 refs。

任务终态判定：全部 item `outcome` 落终态。逐 item 检查：

- `published` / `no_change` → 正常。
- `needs_verification` 或 error 含 `NEEDS_VERIFICATION` / `NOT_VERIFIED` / `CONFLICT` / `SOURCE_NOT_VERIFIED` → **停止**，报告 item_id 与 error；不重试、不绕过、不改动数据。
- 其他失败 → 停止并报告。

## 4. 验证与汇报

- `get_data_availability`：`{"dataset":"market.daily","contract_version":"v1","selection":{"kind":"catalog","scope":"current_sh_sz_a"},"filter":{"kind":"daily","start_date":"1990-01-01","end_date":<今日>},"version":{"kind":"latest_per_entity"}}`，分页确认 coverage/freshness/gap 事实。
- `query_data` 抽样若干实体最新交易日数据（`selection:{"kind":"single"|"batch"}`，`filter:{"kind":"daily",...}`，`version:{"kind":"latest_per_entity"}`）。
- 汇报：active generation、published/no_change 计数、最新数据日期、全部 request/task refs。

可选：派生 `market.yearly` — yearly 只接受显式 items（无 default selection）：先经 `get_data_availability`/`list_data_versions` 取得每个实体的当前 head `version_id`，再 `update_data` `dataset:"market.yearly"`，每项 `{entity, base_daily_version_id:<head>, years:[<当前年>], publication:{"CurrentHead":{"expected_yearly_head":<当前 yearly head 或 null>}}}`；同样走轮询与 fail-closed 判定。

## 5. 超时与结果不明的恢复（绝不盲发新 ID）

- `acquire_generation`/`verify_generation`/`activate_generation` 调用超时或连接断 → 用**同一 `request_id`** 调 `get_acquisition_request` 恢复绑定与终态；只有确认原请求无绑定时才允许换新 request_id。
- `update_data`/`start_collection` 超时 → 同一 `request_id` 调 `get_request`。
- 发布 outcome unknown → `verify_publication_result`（`target_task_id`）核实，不把超时当失败。
- 已开始的 task 停止状态不明 → `verify_stopped_writes` 核实证据，不强制释放占用。

## 6. 全局停止条件（任一命中即停止并报告 refs）

`unknown` 观测、`not_verified`/`conflict`/`corrupt` generation、item `needs_verification`、revision 不匹配、轮询预算耗尽、工具不可用。停止时旧 heads 与旧 active generation 保持不变；报告已完成的步骤、最后结构化状态和恢复用 refs。
