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

当前能力边界：data capability 的 `max_batch_entities=100` 是**每次调用**上限——
单次 `start_collection`/`update_data` 超过 100 个证券或 item 会整体拒绝，服务端不会自动分页。
全市场（约 5,200 实体）已通过生产实测：编排方必须自行把目标切成 ≤100 item 的调用，
每个 chunk 使用独立唯一 `request_id`，**串行提交**（服务端单 worker，并发提交只会排队且
占 scope）。跨代日更全量约 30-40 分钟、同代无变更扫描约 10 分钟、年线全量约 15 分钟、
首次全市场 collection 约 6 分钟——这些是预期时长，不是挂起信号；轮询预算按此校准。

本 Skill 的默认意图是“日行情同步”，不是“只要任一官方文件变化就获取 generation”：

- 只有 `day` role 明确为 `changed`，才允许创建 acquisition task。
- `base_industry` / `gbbq` 变化但 `day` 未变化时，只报告 pending source changes 并结束；
  不下载、不验证、不激活 generation，也不创建数据任务。
- 部署后空库重建是独立、必须由用户明确提出的操作；不得从 `data_versions=0` 自动推断
  重建授权，也不得借“每日同步”隐式执行。用户明确要求空库重建时，必须报告拟使用的
  verified active generation、其数据截止日与远端 role 状态，再按单独确认的重建范围执行。

## 0. Revision 握手（每次执行第一步，不可跳过）

调用并核对：

- `mcp__x-data-acquire__get_acquisition_capabilities`：
  - `implementation.capability_revision` 必须等于 `x-data-acquire-v1`
  - `implementation.generation_contract_version` 必须等于 `official-generation-v1`
- `mcp__x-data-mcp__get_data_capabilities`（按当前 schema 传 `{}`，不得附加
  `contract_version`）：
  - `capability_revision` 必须等于 `generic-data-v3`

任一不匹配或缺失 → 停止，报告 `SKILL_CONTRACT_MISMATCH` 与双方实际值。记录 `schema_digest`、`active_generation_ref`、`runtime_readiness`、`blocking_reasons` 作为事实。

握手后先调用 `sync_status`。若存在 inflight acquisition：

- 本次会话先前在 `day=changed` 证据下创建的 task，可用原 `task_id` / `request_id`
  恢复轮询；
- 其他 task（包括缺少同次 `day=changed` 证据、用户已撤回意图或仅由非 day role
  变化触发的 task）只报告并停止；不得新建第二个 acquisition task，终态后也不得由本
  Skill 验证或激活；
- acquisition contract 当前没有 cancel 工具；禁止通过停服务、删文件或新 request_id
  模拟取消。

acquisition server 在全新环境可因 `no_active_generation` 报告 blocked；该原因不阻止
`check_official_updates` / `acquire_generation` / `verify_generation` / 首次
`activate_generation`。其他 acquisition blocker 仍停止。data server 的写工具必须在
激活 generation 后重新读取 capability；目标工具若仍非 ready，则停止并原样转述
`blocking_reasons`。

## 1. 远端探测（以 day role 为日更门禁）

调用 `mcp__x-data-acquire__check_official_updates`：

- 任一 role 为 `unknown` → 停止并报告（unknown 是停止信号，不是行动提示）。
- `day=unchanged` → 报告 active generation ref、检查时间及全部 changed roles，结束。
  即使 `base_industry` 或 `gbbq` 为 changed，也**不调用 `acquire_generation`，不创建
  generation/data task**。
- `day=changed` → 记录 day 的 ETag / Last-Modified / Content-Length 与 checked_at；
  只有该结构化证据存在时才继续步骤 2。

不得用当前日期、市场是否已经收盘、GBBQ 时间、旧数据最新日期或自由文本推测
`day=changed`。远端 probe 没有明确给出 changed 就必须停止。

## 2. 获取并激活新 generation

进入本节前必须同时满足：本次会话存在步骤 1 的 `day=changed` 结构化证据，且
`sync_status.inflight_tasks` 为空。否则报告 `DAY_UPDATE_NOT_CONFIRMED` 或
`ACQUISITION_ALREADY_INFLIGHT` 并停止。

1. `mcp__x-data-acquire__acquire_generation`，`request_id` 取本次唯一值（如 `daily-<YYYYMMDD>-<短随机>`）。返回 `task_id` / `generation_id`。
2. 有界轮询 `get_acquisition_task(task_id)`：约 60s 一轮，总预算约 45 分钟（官方快照约 570MB，
   实测下载 20-40 分钟）。预算耗尽仍未到终态 → 停止，返回 `request_id`/`task_id`/`generation_id`
   并说明仍在进行，不无限等待。
3. task `failed` → 停止，转述 `error`/`detail` 与 refs。
4. `verify_generation(request_id=<新值>, generation_id)`：结果必须是 `consistency_verified`；`not_verified` / `conflict` / 其他 → 停止并报告。
5. 先 `sync_status` 取当前 `active_generation_ref`；`activate_generation(request_id=<新值>, generation_id, expected_previous=<当前 active 或省略>`) 。拒绝/conflict → 停止，旧 active 不变。
6. 再次 `sync_status` 确认 active 已切换到目标 generation，否则停止。

## 3. 数据更新

### 3a. 已有 published daily heads → `update_data`（常规日更路径）

全市场时必须用**显式 items**；`selection:{"kind":"default"}` 只在解析结果 ≤100 实体时可用：

1. `get_data_availability`（`{"dataset":"market.daily","contract_version":"v1","selection":{"kind":"catalog","scope":"current_sh_sz_a"},"filter":{"kind":"daily","start_date":"1990-01-01","end_date":<今日>},"version":{"kind":"latest_per_entity"}}`）分页拉全量——每实体取 `entity`、`version_ref.version_id`、`last_record`。
2. 构造 item：`{"entity":{"exchange","code"},"base_version_id":<head version_id>,"start_date":<last_record+1 日>,"end_date":<目标交易日>}`。`end_date` 取本次 generation 快照的实际最后交易日；取不到时用当前日期——窗口内无源 bar 只会该 item fail-closed，不影响其他 item。
3. items 按 ≤100 分 chunk，逐 chunk 调 `update_data`（`{"dataset":"market.daily","contract_version":"v1","request_id":<唯一>,"items":[…]}`），串行：一个任务终态后再提下一个。
4. 已有 head 的实体**不得**改用 `start_collection`（会返回 `existing daily head requires the update_data range path`）。

### 3b. 全新数据库（无 daily head）→ `start_collection`

本路径只在步骤 1 已确认 `day=changed`、步骤 2 的新 generation 已 verified+active，
且机器事实确认没有 published daily head 时自动进入。若 `day` 未变化，则 daily-sync
停止并报告 `FRESH_DB_REBUILD_REQUIRES_EXPLICIT_REQUEST`，不得仅因空库复用旧 active
generation 开始首采。

1. `{"dataset":"securities.catalog","contract_version":"v1","request_id":<唯一>,"selection":{"scope":"current_sh_sz_a"}}` → 轮询终态 → `list_data_versions`（`dataset:"securities.catalog"`）取 catalog `version_id`。
2. `query_data` 对该 catalog `version_id` 分页取全部成员（exchange+code）。
3. 成员按 ≤100 分 chunk，逐 chunk `start_collection`：`{"dataset":"market.daily","contract_version":"v1","request_id":<唯一>,"selection":{"kind":"explicit","securities":[{exchange,code},…]}}`，串行至终态。
- `start_collection` 不接受 `{"kind":"default"}`；`selection` 顶层为 `{"scope":"current_sh_sz_a"}`（catalog）或 `{"kind":"explicit","securities":[…]}`（daily）。

轮询 `get_task_progress(task_id)`：每 ~10-15s 一轮；100-item 任务实测 8-120s（采集约 6-8s、跨代更新约 40-120s、同代无变更约 6-12s）。单任务预算 10 分钟，耗尽 → 停止并返回 refs。

任务终态判定：task `status` 为 `succeeded`/`partially_failed`/`failed` 均到终态——`partially_failed` 在全市场是**正常**收尾。逐 item 检查 outcome：

- `published` / `no_change` → 正常。
- per-item `needs_verification`（含 `NEEDS_VERIFICATION`/`NOT_VERIFIED`/`SOURCE_NOT_VERIFIED`）或窗口内无源 bar → **预期 fail-closed 类别**：除权实体（`.day` member 非前缀追加，连续性不可证）与停牌/退市尾部在全市场日更中常规出现（实测约 3%）。**不重试、不绕过**，继续后续 chunk，聚合全部此类 item 的 `item_id`+error 上报为 fail-closed 集合。
- 任务级失败、受理拒绝（整个调用被拒）、大面积同类 per-item 失败（系统性迹象）→ **停止**并报告，不继续提交剩余 chunk。

## 4. 验证与汇报

- `get_data_availability`：`{"dataset":"market.daily","contract_version":"v1","selection":{"kind":"catalog","scope":"current_sh_sz_a"},"filter":{"kind":"daily","start_date":"1990-01-01","end_date":<今日>},"version":{"kind":"latest_per_entity"}}`，分页确认 coverage/freshness/gap 事实。
- `query_data` 抽样若干实体最新交易日数据（`selection:{"kind":"single"|"batch"}`，`filter:{"kind":"daily",...}`，`version:{"kind":"latest_per_entity"}`）。
- 汇报：active generation、published/no_change 计数、最新数据日期、全部 request/task refs。

可选：派生 `market.yearly` — yearly 只接受显式 items（无 default selection，collection 亦被 worker 设计性拒绝）：先经 `get_data_availability`/`list_data_versions` 取得每个实体的当前 daily head `version_id` 与当前 yearly head，再按 ≤100 分 chunk 调 `update_data` `dataset:"market.yearly"`，每项 `{entity, base_daily_version_id:<daily head>, years:[<当前年>], publication:{"mode":"current_head","expected_yearly_head":<当前 yearly head version_id 或 null>}}`；串行轮询与 fail-closed 判定同上。`selected year absent from fixed data` 属预期 fail-closed（源内无该年 bar），聚合上报即可。

## 5. 超时与结果不明的恢复（绝不盲发新 ID）

- `acquire_generation`/`verify_generation`/`activate_generation` 调用超时或连接断 → 用**同一 `request_id`** 调 `get_acquisition_request` 恢复绑定与终态；只有确认原请求无绑定时才允许换新 request_id。
- `update_data`/`start_collection` 超时 → 同一 `request_id` 调 `get_request`。
- 发布 outcome unknown → `verify_publication_result`（`target_task_id`）核实，不把超时当失败。
- 已开始的 task 停止状态不明 → `verify_stopped_writes` 核实证据，不强制释放占用。

## 6. 全局停止条件（任一命中即停止并报告 refs）

`unknown` 观测、`day` 未明确 changed、缺少同次 day-change 证据的 inflight acquisition、
`not_verified`/`conflict`/`corrupt` generation、revision 不匹配、任务级失败/受理拒绝、
大面积同类 per-item 失败、轮询预算耗尽、工具不可用。per-item
`needs_verification` 不属全局停止（见 3 节：聚合上报为 fail-closed 集合）。停止时旧
heads 与旧 active generation 保持不变；报告已完成的步骤、最后结构化状态和恢复用 refs。
