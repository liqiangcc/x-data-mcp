---
name: x-data-daily-sync
description: 每日官方行情同步 — 仅通过 x-data-acquire / x-data-mcp 两个 MCP server 完成 generation 闭环 + plan/run 日更（含日线/年线两步补漏）；无 shell/SSH/SQLite/文件旁路
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
  - mcp__x-data-mcp__plan_market_update
  - mcp__x-data-mcp__run_market_update
  - mcp__x-data-mcp__update_data
  - mcp__x-data-mcp__get_task
  - mcp__x-data-mcp__get_request
  - mcp__x-data-mcp__get_task_progress
  - mcp__x-data-mcp__list_tasks
  - mcp__x-data-mcp__get_data_availability
  - mcp__x-data-mcp__get_yearly_base_years
  - mcp__x-data-mcp__query_data
  - mcp__x-data-mcp__list_data_versions
  - mcp__x-data-mcp__verify_publication_result
  - mcp__x-data-mcp__verify_stopped_writes
---

# x-data 每日同步编排

本 Skill 只编排结构化事实。禁止：shell/exec/SSH/curl/SQLite/读写本地文件、`run_worker_once`、自行签发任何验证结论、解析自由文本判断状态。所有调用只使用上方 allowlist 内的 MCP 工具；工具在本会话不可用（名称不符或 server 未注册）→ 立即停止并报告 `SKILL_CONTRACT_MISMATCH`。

参数一律按 `tools/list` 中该工具的 `inputSchema` 传：schema 声明 `contract_version` 时必须为 `"v1"`；schema 未声明的字段一律不传（输入对象 `deny_unknown_fields`，多余字段即拒绝）。`update_data`/`start_collection`/`acquire_generation`/`verify_generation`/`activate_generation` 各自使用独立且唯一的 `request_id`；`plan_market_update` 只读、不建任务，`run_market_update` 每次受理需要一个独立且唯一的 `request_id`。

每个 MCP server 首次使用前按客户端要求调用一次 `mcp_list_tools`。该结果只用于确认 server
可发现；registry 很大导致输出截断时，**不得读取 overflow 文件、不得使用 shell/exec，
也不要求枚举全部工具**。直接按本 Skill 已命名的工具和参数调用；若 named tool 不存在或
调用层返回 unavailable，立即报告 `SKILL_CONTRACT_MISMATCH`。步骤 0 的固定调用为
`get_acquisition_capabilities({"contract_version":"v1"})` 与
`get_data_capabilities({})`，无需从 overflow 重读 schema。

当前能力边界：data capability 的 `max_batch_entities=100` 是**每次调用**上限——
单次 `start_collection`/`update_data` 超过 100 个证券或 item 会整体拒绝，服务端不会自动分页。
v7 起全市场日更由 `plan_market_update`/`run_market_update` 在服务器侧解析与执行，编排方
不再自行把全市场切成 ≤100 item 调用；只有**补漏**（§3.3/§3.6）与首采（§3.7）仍按
≤100 item 的显式契约提交，每个 chunk 使用独立唯一 `request_id`、**串行提交**（服务端单 worker）。
plan/run 大任务的进度按 `get_task_progress` 的 `unfinished` keyset 分页读取（调用参数规则见 §3.2）。

本 Skill 的默认意图是“日行情同步”，不是“只要任一官方文件变化就获取 generation”：

- 只有远端 `day` role 明确不同于当前 active generation（`pending_roles` 包含 `day`），才允许创建 acquisition task。
- `base_industry` / `gbbq` pending 但 `day` 未 pending 时，只报告 pending source changes 并结束；
  不下载、不验证、不激活 generation，也不创建数据任务。
- 部署后空库重建是独立、必须由用户明确提出的操作；不得从 `list_data_versions`
  返回空列表自动推断
  重建授权，也不得借“每日同步”隐式执行。用户明确要求空库重建时，必须报告拟使用的
  verified active generation、其数据截止日与远端 role 状态，再按单独确认的重建范围执行。

## 0. Revision 握手（每次执行第一步，不可跳过）

调用并核对：

- `mcp__x-data-acquire__get_acquisition_capabilities`：
  - `implementation.capability_revision` 必须等于 `x-data-acquire-v1`
  - `implementation.generation_contract_version` 必须等于 `official-generation-v1`
- `mcp__x-data-mcp__get_data_capabilities`（按当前 schema 传 `{}`，不得附加
  `contract_version`）：
  - `capability_revision` 必须等于 `generic-data-v7`
    （v6 起 market 数据为单版本序列：`version_id` 为内容 hex，`list_data_versions` 每实体至多一项；显式 pin 读取被取代时报 `stale_version`，写路径 stale base 为 item 级 `needs_verification`（`STALE_BASE:` 前缀），均须重取当前 head 重试，不得静默适配；v5 的 `related_tasks` 口径沿用，只含在途任务与最近 5 个已结束任务，另有 `related_tasks_total` / `related_tasks_truncated`；v6 的 `generic-data-v6` 断言遇到 v7 即按下一条停止，不得静默适配。v7 新增 `plan_market_update`/`run_market_update`，日更编排见 §3。）

任一不匹配或缺失 → 停止，报告 `SKILL_CONTRACT_MISMATCH` 与双方实际值。记录 `schema_digest`、`active_generation_ref`、`runtime_readiness`、`blocking_reasons` 作为事实。

握手后先调用 `sync_status`。若存在 inflight acquisition：

- 本次会话先前在 `day` pending 证据下创建的 task，可用原 `task_id` / `request_id`
  恢复轮询；
- 其他 task（包括缺少同次 `day` pending 证据、用户已撤回意图或仅由非 day role
  变化触发的 task）只报告并停止；不得新建第二个 acquisition task，终态后也不得由本
  Skill 验证或激活；
- acquisition contract 当前没有 cancel 工具；禁止通过停服务、删文件或新 request_id
  模拟取消。

acquisition server 在全新环境可因 `no_active_generation` 报告 blocked；该原因不阻止
`check_official_updates` / `acquire_generation` / `verify_generation` / 首次
`activate_generation`。其他 acquisition blocker 仍停止。data server 的写工具必须在
激活 generation 后重新读取 capability；目标工具若仍非 ready，则停止并原样转述
`blocking_reasons`。

## 1. 远端探测（以 day role 是否 pending 为日更门禁）

调用 `mcp__x-data-acquire__check_official_updates`。响应中的 `changed` 是相对上一次
persisted `seen` 的远端 drift；`pending_roles` 才是相对当前 active generation
manifest 的待采集差异，二者不能混用。若 `pending_roles`/`files[].active_change`
字段缺失 → `SKILL_CONTRACT_MISMATCH`。

- 任一 role 为 `unknown` → 停止并报告（unknown 是停止信号，不是行动提示）。
- `day` 不在 `pending_roles` → 报告 active generation ref、检查时间、全部 `changed`
  roles 与 `pending_roles`，立即结束；不得再做 catalog/market 只读探测。固定报告的
  “最新可用行情日期”在本分支填 `not_queried_by_no_update_branch`，除非此前步骤已获得
  结构化 `last_record`。即使 `base_industry` 或 `gbbq` pending，也**不调用
  `acquire_generation`，不创建 generation/data task**。
- `day` 在 `pending_roles` → 记录 day 的 remote ETag / Last-Modified /
  Content-Length、active manifest 对照值、checked_at、seen `changed` 结果与全部
  `pending_roles`；只有该结构化证据存在时才继续步骤 2。

不得用当前日期、市场是否已经收盘、GBBQ 时间、旧数据最新日期、上次 probe 的
`changed` 记忆或自由文本推测 `day` pending。远端 probe 没有明确给出 `day`
pending 就必须停止。

## 2. 获取并激活新 generation

进入本节前必须同时满足：本次会话存在步骤 1 的 `day` pending 结构化证据，且
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

## 3. 数据更新（v7：日线 plan/run → 日线补漏 → 年线 plan → 年线 run → 年线补漏 → readback）

完整有序步骤见 `docs/contracts/generic-data-v7.md` §7.1；本节是 Skill 侧的可执行版本。
**两步补漏（§3.3、§3.6）不可省略**。

`market.daily` / `market.yearly` 的 `query_data`、`list_data_versions` 与
`get_data_availability` **只接受** `selection:{"kind":"single"}` 或
`selection:{"kind":"batch","entities":[…]}`；不接受 `catalog` 或 `default` selection。
`catalog` selection 只能用于 `dataset:"securities.catalog"`。全市场更新由
`plan_market_update` 在服务器侧解析，编排方不再自行切 ≤100 显式批次刷全市场。

### 3.1 日线 plan

`plan_market_update`（`{"dataset":"market.daily","contract_version":"v1"}`；默认**不传**
`end_date`，服务端取 active generation 内最新交易日作为 `target_day`，不晚于今天）。复核
返回的 `target_day` 与 `target_day_source`、`counts{eligible,already_current,absent,ineligible}`、
`ineligible_reasons` 与 `generation_ref`。**若 `target_day_source == "today_fallback"`：
停止并报告**（generation 不可读，无法确定目标交易日），不要继续 run。`eligible` 即本次
run 要处理的实体；`absent` 实体无 daily head（走 §3.7 首采，不进 run）。

### 3.2 日线 run + 轮询到终态

`run_market_update`（`{"dataset":"market.daily","contract_version":"v1",
"request_id":<本次唯一>,"target_day":<plan 的 target_day>,"plan_id":<plan 的 plan_id>}`）。

- `plan_stale` → 状态已变（head/catalog/gap/generation 变化），重新 plan 后重提。
- `plan_in_progress` → 已有同 dataset 非终态更新任务，等待其终态或报告。
- 受理失败均无落库副作用；不得通过换 `request_id` 绕过 `plan_stale`。

轮询 `get_task_progress(task_id)`：每次调用带 `wait_seconds: 45`，并带 `since_revision`
（取上一次响应里的 `task.state_revision`；首次调用不传 `since_revision`）。**任何两次
连续调用不得使用完全相同的参数**——客户端会把参数相同的重复调用当作死循环拦停。大任务
（item_count>100）按 `unfinished` keyset 分页（`page_size`≤100），只看
`status`/计数/`next_cursor`，**不期待逐 item 结果数组**。终态为
`succeeded`/`partially_failed`/`failed`；全市场日更 `partially_failed` 是正常收尾。

### 3.3 日线失败路由（按 `failed_by_reason` 补漏，不可省略）

读顶层 `plan.failed_by_reason`（`reason_code → 计数`）；逐项 `reason_code` 的动作：

- `REVISION_UNVERIFIED`：上游对已发布历史重算（除权/复权），plan/run 的整段历史刷新
  窗口下按设计 fail-closed。对每个此类实体用 `update_data` 显式**前向窗口**
  `[该实体 last_record+1, target_day]`，每任务 ≤100 项，串行至终态。**不要**
  `retry_task`（同一窗口、同一证据，确定性复败）。
- `PUBLISH_PRECONDITION`：不应出现；出现即缺陷，保留现场并报告，不重试。
- `NO_BARS_IN_RANGE`：预期的 fail-closed（停牌/退市尾部），无需动作。
- `OTHER`：读 `error`，必要时报告。

清单取法：`unfinished` 分页里 `reason_code=REVISION_UNVERIFIED` 的项即实体（`entity` 字段）。
显式 `update_data`（market.daily）请求每项形状：
`{"entity":{"exchange","code"},"base_version_id":<当前 head version_id>,
"start_date":<last_record+1>,"end_date":<target_day>}`；`base_version_id` 取提交时当前 head，
被并发取代会 item 级 `STALE_BASE:` `needs_verification`，重取 head 后重提该 item。
该显式批次若被**整批拒绝**（`invalid_arguments`/`needs_verification`），按 §3.8 二分归因，
不要当成全局停止。

### 3.4 年线 plan

日线任务（含 §3.3 的补漏任务）**全部终态**后，调
`plan_market_update`（`{"dataset":"market.yearly","contract_version":"v1"}`；同 §3.1 默认
**不传** `end_date`）。若 `market.daily` 存在非终态更新任务 → `daily_update_inflight`，
等待或报告，不提交。复核 `target_day`/`target_day_source`（`today_fallback` → 停止并报告）、
`counts`/`ineligible_reasons`。

### 3.5 年线 run + 轮询

`run_market_update`（`{"dataset":"market.yearly","contract_version":"v1",
"request_id":<本次唯一>,"target_day":<plan 的 target_day>,"plan_id":<plan 的 plan_id>}`）。
只处理 plan 的 `eligible`；`market.yearly` 的 collection 被 worker 设计性拒绝，年线一律走
plan/run 或 §3.6 显式分支。轮询与 fail-closed 判定同 §3.2/§3.3。

**年线 run 必须排在 §3.6 年线补漏之前**：`run_market_update` 的 `plan_id` 绑定 plan 时刻的
状态，而 §3.6 的显式年线重推会推进这些实体的年线 head、改变 `ineligible` 集合；若把补漏
排在 run 之前，本步的 run 必然 `plan_stale`。

### 3.6 年线不可处理路由（按 `ineligible_reasons` 补漏，不可省略）

年线 run（§3.5）终态后处理 `ineligible_reasons`（全量原因表见
`docs/contracts/generic-data-v7.md` §7.3）：

- `value_semantics_changed` / `daily_replaced_since_derivation`：旧年线已失效
  （日线 replace 或 {source,unit_currency,adjustment_evidence} 变化），**必须显式重推**。
- `already_current`：正常状态，无需动作。
- `head_unreadable` / `integrity_failure`：fail-closed，不自动修复，报告。
- `no_daily_base`：该实体无已发布 daily head，先走 §3.7，本步不做年线。
- `no_derivable_years`：无内容可推，无需动作。

显式年线重推步骤（每批 ≤100 实体）：

1. 需重推集**优先**用年线 plan 返回的 `ineligible_entities`（`reason ∈
   {daily_replaced_since_derivation, value_semantics_changed}`）作实体清单；仅当
   `ineligible_entities_truncated=true`（实体数超过 500）时回退到枚举：
   `get_data_availability`（`dataset:"market.yearly"`，batch）取 `version_ref`
   为空 ∪ `stale=="behind"` ∪ `reason ∈ {daily_replaced_since_derivation,
   value_semantics_changed}`。
2. `list_data_versions`（`dataset:"market.yearly"`，batch）取每实体已存 head
   `version_ref.version_id` 作 `expected_yearly_head`。**失效年线在 availability 上报
   `version_ref=null`，但已存 head 行仍在；必须 pin 步骤 2 的 id，否则 CAS 以
   `effective head already exists` 拒绝**。只有完全没有已存年线的实体才取 null。
3. `get_data_availability`（`dataset:"market.daily"`，batch）取该实体当前 daily head
   `version_id` 与 `first_record`/`last_record`。
4. `get_yearly_base_years`（`{"contract_version":"v1","items":[{"entity",
   "base_daily_version_id":<daily head>}]}`，**不传 `start_year`/`end_year`**，用默认
   1..9999 以免隐藏未来年）取 `present_years`，与 `[first_record 年, last_record 年]`
   求交得 `years`；未来年（> 上海当前年）记异常入报告。`reason_code:"base_superseded"`
   → 重取当前 daily head 重试该实体；`base_unavailable`/`integrity_failure` 属 fail-closed。
5. `update_data`（`dataset:"market.yearly"`），每项
   `{"entity","base_daily_version_id":<daily head>,"years":[<交集>],
   "publication":{"mode":"current_head","expected_yearly_head":<步骤 2 的 id 或 null>}}`；
   按 ≤100 分 chunk 串行至终态。

补漏完成后可选再 `plan_market_update`（`dataset:"market.yearly"`）核对一次：此时 `eligible`
应为 0，`ineligible` 只剩 `already_current` 及预期的无需动作原因。

本步的显式批次被整批拒绝时按 §3.8 二分归因（年线 items 与日线 items 形状不同，但拆批与
request_id 规则相同）。

### 3.7 无 published daily head → `start_collection` 显式 securities（首采路径，保留）

本路径只在步骤 1 已确认 `day` pending、步骤 2 的新 generation 已 verified+active，
且机器事实确认目标实体没有 published daily head（即 §3.1 plan 的 `absent` 集合）时进入。
若 `day` 未 pending，daily-sync 停止并报告 `FRESH_DB_REBUILD_REQUIRES_EXPLICIT_REQUEST`，
不得仅因空库复用旧 active generation 开始首采。

1. 若 catalog version 尚不存在，先 `start_collection`：
   `{"dataset":"securities.catalog","contract_version":"v1","request_id":<唯一>,"selection":{"scope":"current_sh_sz_a"}}` → 轮询终态 → `list_data_versions` 取 catalog `version_id`。
2. `query_data` 对该 catalog `version_id` 分页取全部成员（exchange+code），再按
   market.daily `batch` availability 复核；只对 `version_ref=null` 的实体建立首采。
3. 无 head 实体按 ≤100 分 chunk，逐 chunk `start_collection`：
   `{"dataset":"market.daily","contract_version":"v1","request_id":<唯一>,"selection":{"kind":"explicit","securities":[{exchange,code},…]}}`，串行至终态。
- `start_collection` 不接受 `{"kind":"default"}`；`selection` 顶层为 `{"scope":"current_sh_sz_a"}`（catalog）或 `{"kind":"explicit","securities":[…]}`（daily）。

轮询 `get_task_progress(task_id)`：调用参数规则同 §3.2（带 `wait_seconds: 45` 与递增的
`since_revision`，连续两次调用参数不得完全相同）；100-item 任务实测 8-120s。单任务预算
10 分钟，耗尽 → 停止并返回 refs。

任务终态判定：task `status` 为 `succeeded`/`partially_failed`/`failed` 均到终态——
`partially_failed` 在全市场是**正常**收尾。逐 item 检查 outcome：

- `published` / `no_change` → 正常。
- per-item `needs_verification`（含 `NEEDS_VERIFICATION`/`NOT_VERIFIED`/`SOURCE_NOT_VERIFIED`）或窗口内无源 bar → **预期 fail-closed 类别**：除权实体与停牌/退市尾部在全市场日更中常规出现。**不重试、不绕过**，继续后续 chunk；`REVISION_UNVERIFIED` 类按 §3.3 走显式 `update_data` 前向窗口补齐（这是唯一需要动作的失败类）。
- 任务级失败、受理拒绝（整个调用被拒）、大面积同类 per-item 失败（系统性迹象）→ **停止**并报告，不继续提交剩余 chunk。

### 3.8 显式 items 整批被拒：分裂重试（兜底，不是主路径）

`update_data` 的显式 items 受理是**调用级 fail-closed**：一批里任一项不合格，整个调用被拒，
拿不到 per-item 结果（#294 选方向 C，见 `docs/decisions.md` D-18）。所以 §3.3/§3.6 的显式
批次被整批拒绝时，按下面的规则二分归因，不要直接判死。

**适用（三条同时成立）**：提交的是显式 items 批次（§3.3 日线前向窗口 / §3.6 年线重推）；
整批被拒且错误码是 `invalid_arguments` 或 `needs_verification`；该批已过形状自查——1..100 项、
`exchange` + 6 位 `code`、`start_date ≤ end_date`、`end_date ≤ target_day`、同一实体只有一段区间。

**不适用（按 §6 停止或按原规则处理，不二分）**：`request_conflict`、`range_conflict`、
`storage_error`、`outcome_unknown`、`unsupported_contract`、`empty_request_id`；形状自查不过
（先改参数，不是分裂）。`range_conflict` 表示这些实体已被在途任务占用，等它终态。

**怎么拆**：按 entity 排序把该批对半切成两个子批，各自提交；被受理的一半照常执行，只对仍被拒
的一半继续二分，直到单项。深度上限 7（100→50→25→13→7→4→2→1），整棵树受理次数上限 2n−1
（n≤100 时 ≤199）。子批**串行**提交（服务端单 worker）。

**request_id**：被拒的受理**不消费** request_id——请求行只在受理事务的最后一步写入
（`src/task_control/service.rs:61-93`：先读请求、normalize、resolve、validate，再进受理事务；
`INSERT INTO requests` 在同一事务末尾，`src/sqlite_tasks.rs:1780`），所以子批可以复用原
request_id。**推荐每个子批用新的唯一 request_id**，事后才能按 `get_request` 定位是哪一半被拒。
两条硬规则：已受理的 request_id 不能换内容重提（`request_conflict`，
`src/task_control/service.rs:78-87`）；已受理的项不能出现在后续子批里（实体范围已被占用，
`range_conflict`，`src/sqlite_tasks.rs:1717-1731`）——分裂后只提交尚未受理的那一半。

**终止条件**：单项仍被拒 → 该 item 归因完成，记入报告（entity + 错误码）并跳过，不用同一输入
重试；归因过程中出现上面「不适用」的错误码 → 立即停掉整棵分裂树；深度或受理次数到上限 →
停止并报告剩余项。

**归因结果怎么用**：分裂只回答「哪一项让整批被拒」，不修该项本身。归因出的项若是 base 已被
并发推进（受理期报 `needs_verification` + `STALE_BASE:`，来源 `src/sqlite_market/series.rs:283-292`
的 superseded 判定与 `src/market_update/tasks.rs:277` 的前缀映射；执行期报 `REVISION_UNVERIFIED`），
重取当前 head 后重提；其余 per-item 归因项按预期 fail-closed 聚合上报，不绕过。同一批反复触发
分裂说明 plan 的可执行集合与受理实际可接受的集合不一致（典型：plan 之后 head 前进），报告这一
事实，不要自行改成逐实体提交。

## 4. 验证与汇报

- 复用 §3.0/§3.1 的 catalog `version_id` 与实体枚举；对 market.daily 继续按 ≤100 实体分批调用 `get_data_availability`
  （`selection:{"kind":"batch","entities":[…]}`），确认 coverage/freshness/gap 事实。
- `query_data` 抽样若干实体最新交易日数据（`selection:{"kind":"single"|"batch"}`，`filter:{"kind":"daily",...}`，`version:{"kind":"latest_per_entity"}`）。
- 汇报：active generation、plan/run 的 `plan_id` 与 `counts`、`failed_by_reason`、published/no_change 计数、最新数据日期、全部 request/task refs。

可选：派生 `market.yearly` — 走 §3.4/§3.5/§3.6（plan/run + 显式重推），
`selected year absent from fixed data` 属预期 fail-closed（源内无该年 bar），聚合上报即可。

## 5. 超时与结果不明的恢复（绝不盲发新 ID）

- `acquire_generation`/`verify_generation`/`activate_generation` 调用超时或连接断 → 用**同一 `request_id`** 调 `get_acquisition_request` 恢复绑定与终态；只有确认原请求无绑定时才允许换新 request_id。
- `update_data`/`start_collection` 超时 → 同一 `request_id` 调 `get_request`。
- `run_market_update` 超时或连接断 → 同一 `request_id` 调 `get_request` 恢复绑定与终态；只有确认原请求无绑定时才允许换新 request_id。
- 发布 outcome unknown → `verify_publication_result`（`target_task_id`）核实，不把超时当失败。
- 已开始的 task 停止状态不明 → `verify_stopped_writes` 核实证据，不强制释放占用。

## 6. 全局停止条件（任一命中即停止并报告 refs）

`unknown` 观测、`day` 未明确 pending、缺少同次 day-pending 证据的 inflight acquisition、
`not_verified`/`conflict`/`corrupt` generation、revision 不匹配、任务级失败/受理拒绝
（`update_data` 显式批次先按 §3.8 归因，per-item 类别走分裂重试，其余才停）、
大面积同类 per-item 失败、轮询预算耗尽、工具不可用。per-item
`needs_verification` 不属全局停止（见 §3.3/§3.7：`REVISION_UNVERIFIED` 走显式补漏，
其余聚合上报为 fail-closed 集合）。停止时旧
heads 与旧 active generation 保持不变；报告已完成的步骤、最后结构化状态和恢复用 refs。
