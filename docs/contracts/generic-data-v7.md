# generic-data-v7 契约（plan/run 行情更新工作流）

本文是面向调用方的 `generic-data-v7` 契约增量：在 v6（latest-only 单版本存储，
见 [generic-data-v6](generic-data-v6.md)）之上新增 `plan_market_update` 与
`run_market_update` 两个工具，供 AI 以「服务器侧计划 + 单任务执行」完成全市场
日线/年线更新，不再用 ≤100 item 的显式批次编排刷全市场。

**状态**：代码已宣告 `generic-data-v7`（`src/data_supply/mcp.rs`
`capabilities()`），但**尚未部署**；当前部署仍为 `generic-data-v6` /
compact schema 21（见 [STATUS](../STATUS.md)）。schema 不变（仍 21），无
迁移；部署仅需替换二进制（见 [v7-switch-checklist](v7-switch-checklist.md)）。
凡下文标注「实现已核」的论断附 文件:行号；未标者为契约目标。

## 1. 适用面与不变量

- `capability_revision`：`generic-data-v6` → `generic-data-v7`；data 工具数
  21 → 23；schema 与存量工具契约不变。
- 显式 `update_data`/`start_collection` 的 ≤100 item 受理契约不变；
  `max_batch_entities` 仍报 100。plan/run 是**独立语义**的新工作流，
  不削弱既有契约的原子受理语义。
- 计划与执行都无服务端持久化（无状态摘要）：plan 不落表、不过期；
  run 通过重算比对 digest 绑定状态。

## 2. `plan_market_update`

- 输入：`{dataset: "market.daily" | "market.yearly", contract_version: "v1",
  end_date?: "YYYY-MM-DD"}`；显式 `end_date` 只允许不晚于受理时钟的
  Asia/Shanghai 当日，大于当日即拒绝。**缺省**时目标日取
  `min(当日, active generation 内最新交易日)`：官方当日行情要到下午晚些
  时候才发布，缺省取当日会让每个实体永远显示为待更新。"generation 内最新
  交易日"是对排序后 plan universe 的前 32 个实体读取其日线最后一根 K 线
  日期取最大值（只读，不钉住 generation，结果只取决于 generation 字节与
  库状态）；没有 active verified generation 或样本全部读不到时回退为当日。
- 行为（只读，不建任务、不写库）：在单个读快照内对当前 catalog 覆盖
  ∩ 已首采实体逐实体解析，分为 `eligible`（可执行）、`already_current`、
  `absent`、`ineligible:<reason>` 四类。
- 输出（小）：`{plan_id, dataset, contract_version, generation_ref,
  target_day, target_day_source?, counts{eligible, already_current, absent,
  ineligible}, ineligible_reasons[{reason, count}], snapshot_time}`。不枚举
  实体。`target_day_source ∈ {explicit, generation_latest, today_fallback}`
  说明目标日的来源；它只是说明字段，**不进入** canonical plan，不影响
  `plan_id`。
- `plan_id = sha256(canonical_json)`；canonical =
  `{dataset, contract_version:"market-update-plan-v1", generation_id,
  target_day, items:[…按 entity 排序], ineligible:[{entity,reason} 排序]}`，
  **不含时间戳**：同状态同参数必得同 plan_id，任何 catalog/head/gap/
  generation 变化必得不同 plan_id。
- `generation_ref`：受理时的 active verified generation；无则为 null。

## 3. `run_market_update`

- 输入：`{dataset, contract_version:"v1", request_id, target_day, plan_id}`
  （`target_day` 必填，即 plan 返回的 `target_day`）。
- 受理序列（全部失败均无落库副作用）：
  1. `plan_id` 非 64 位小写 hex → `invalid_plan`；
  2. 同一读快照内重算 canonical；digest ≠ 入参 plan_id → `plan_stale`
     （「状态已变，请重新 plan」）——generation 换代、head 前进、catalog
     变化、gap 变化均归入此类；
  3. 同 dataset 存在非终态更新任务 → `plan_in_progress`；market.yearly
     还要求 market.daily 无非终态更新任务 → `daily_update_inflight`；
  4. 相等则受理**一个** task（contract `market-plan-run-task-v1`，
     `plan_id` 记入任务证据与 `get_task_progress` 摘要），worker 串行执行。
- 执行：逐 item 沿用现有 append/derive 语义与 per-item CAS；单项失败
  （needs_verification 等）不拒绝整批。
- 输出：`{request_id, task_id, dataset, contract_version, plan_id, reused,
  status, item_count, plan_counts, runtime_readiness, blocking_reasons}`；
  不返回 item 列表，进度与逐 item 结果经 `get_task_progress` 分页读。
- 续跑：不跟踪 consumed。中途取消/部分完成后 head 已前进，旧 plan_id 重算
  必不等 → `plan_stale` → 重新 plan 即得剩余实体的新 plan。

## 4. 错误码

| code | 条件 |
|---|---|
| `invalid_plan` | plan_id 非 64 位小写 hex |
| `plan_stale` | 重算 digest ≠ plan_id（含换代、head/catalog/gap 变化） |
| `plan_in_progress` | 同 dataset 已有非终态更新任务 |
| `daily_update_inflight` | yearly plan/run 时 market.daily 有非终态更新任务 |
| 沿用 | `invalid_arguments`、`unsupported_contract`、`request_conflict`、`empty_request_id`、`unknown_dataset` |

## 5. 大任务的读形状（item_count > 100）

`get_task`/`list_tasks`/`get_task_progress` 对超过
`observe::LARGE_TASK_ITEMS`（=100，与显式契约受理上限同值）的任务切
换到有界读：内存与响应体积与任务项数无关。≤100 项的任务（全部 v6
显式任务）形状逐字节不变。

- `get_task`/`list_tasks`：summary 字段不变（含 plan-run 任务的
  `plan_id`/`plan_counts`），服务端只读任务头列与
  `shared_evidence`，不反序列化 `frozen_work_json` 的 item 列表。
- `get_task_progress`：响应为任务状态、聚合计数
  （`total/pending/running/finished/succeeded/failed`）、`task.plan_id`/
  `plan_counts`、新增顶层 `plan{plan_id, generation_id, target_day}`；
  `unfinished` 仅分页返回**未完成或失败**的 item（按 `item_id` 升序
  keyset 翻页，`page_size` 决定本页条数），成功项不逐条列出；
  `frozen_daily_ranges`/`range_results`/`frozen_yearly_inputs`/
  `yearly_results`/`publication_versions` 恒为空数组（大任务的成功项
  结果经 `market_update_results` 审计面或 item receipt 单独查）。
  `page_size`≤100 时每页响应 <50KB。
- 游标语义：大任务的 `next_cursor` 为 keyset 位置（末项
  `item_id`），翻页读到的是翻页时刻的最新项状态（非创建游标时的
  快照）；小任务保持原有快照分页语义不变。
- 取消/失败排查：`unfinished` 恒含失败项（`success=false`），未开始项
  `success=null`。大任务页里失败项的 `error` 截断到
  `observe::BOUNDED_ERROR_CHARS`（240 字符，超出处追加 `… (N chars
  total)`；完整文本留在任务记录里——一个除权实体的 `RevisionUnverified`
  错误曾长达 ~94KB），并带稳定的 `reason_code`。
- 失败原因归类：顶层 `plan.failed_by_reason`（`reason_code → 计数`，
  首页快照时统计，无失败时省略）加上每个失败项的 `reason_code`，让调用
  方按类别路由而不必翻完所有失败项。`reason_code` 取值（映射在
  `observe::reason_code`，改动需同步本表）：

  | reason_code | 含义 | 调用方动作 |
  |---|---|---|
  | `REVISION_UNVERIFIED` | 窗口内已发布历史被上游重算（典型：除权），无修订证据，按设计 fail-closed | 用 `update_data` 显式前向窗口 `[last_day+1, target_day]` 补（见 `v7-switch-checklist.md` §2），每任务 ≤100 项 |
  | `PUBLISH_PRECONDITION` | 执行值与受理时冻结的输入不一致 | 不应出现；出现即缺陷，保留现场并报告，不要重试 |
  | `NO_BARS_IN_RANGE` | 受理区间内无行情（停牌/退市） | 预期的 fail-closed，无需动作 |
  | `OTHER` | 其它 | 读 `error`，必要时报告 |

### 等待参数（长轮询）

`get_task_progress` 额外接受两个**可选**入参，让一次调用在服务端等待任务推进，
免去调用方（AI 代理没有 `sleep`/shell）反复空轮询——每次调用参数不同，也不会被
客户端的重复调用检测判为死循环：

| 字段 | 类型 | 语义 |
|---|---|---|
| `wait_seconds` | 整数，可选 | 0 或缺省 = 立即返回（原行为）；>0 时服务端最多等待这么久，**上限 50 秒**（超过按 50 处理，不报错）。仅当任务**非终态**时等待；任务不存在/无权限等错误照旧立即返回。 |
| `since_revision` | 整数，可选 | 调用方上次看到的 `task.state_revision`。等待在 `state_revision > since_revision` 时提前结束；缺省时以进入等待时读到的 revision 为基准。 |

等待循环每 300~500ms 只重读任务头（`tasks` 行的 `state_revision`/`status`，不物化
item），每次读完立即释放读作用域再休眠：不跨越 sleep 持有读事务，不阻塞后台 worker。
返回仍是原来的正常进度响应（含大任务的有界形状），只是可能更晚返回；`wait_seconds`/
`since_revision` 只出现在该工具的 inputSchema 里，不出现在响应里。等待只作用于**新
调用**；携带 `cursor` 的翻页调用始终立即返回。单次调用最长占用其连接约 50 秒（每个连接
独立线程，不影响其它连接或后台 worker）。

给 Skill 的推荐用法：轮询时用 `wait_seconds: 45`，`since_revision` 取上一响应的
`task.state_revision`；收到响应后按需翻页 `unfinished`（见上），再以新的
`state_revision` 发起下一次等待。

## 6. 与 #294 的关系

显式 `update_data` 的「整批原子受理、>100 拒绝」语义不变（见
[generic-data-tools](../generic-data-tools.md) 的原子受理裁决）。plan/run 工作流
内部不存在整批拒绝模式：单个 item 失败只影响该 item。这**仅在**新工作流
内消除 #294 的失败模式；#294 本身是否收口由 issue 裁定，不由本契约隐含。

## 7. 日更完整顺序与补漏路由

本节把「acquire→verify→activate 之后，一次全市场日更按什么顺序做完，以及
plan/run 有意不处理的那一小部分实体如何补」写成可机械执行的顺序。**范围**：
本节只描述 v7 的 `plan_market_update`/`run_market_update`（§2/§3）与既有显式
`update_data`（[generic-data-tools](../generic-data-tools.md)）的组合；
acquire→verify→activate 仍按 [daily-sync](../daily-sync.md) 执行，不在此重复。

### 7.1 有序步骤

| # | 步骤 | 调用 | 前置条件 | 终态/产出 |
|---|---|---|---|---|
| 1 | 日线 plan | `plan_market_update{dataset:"market.daily", contract_version:"v1"[, end_date]}` | active verified generation 存在；无在途 acquisition；无同 dataset 非终态更新任务 | `plan_id`、`target_day`、`counts`、`ineligible_reasons` |
| 2 | 日线 run | `run_market_update{dataset:"market.daily", contract_version:"v1", request_id, target_day, plan_id}` | 上一步 plan 刚返回，其间无 catalog/head/gap/generation 变化（否则 `plan_stale`，§3）；同 dataset 无非终态更新任务 | `task_id`；worker 串行执行 |
| 3 | 轮询日线到终态 | `get_task_progress(task_id)` | — | `status ∈ {succeeded, partially_failed, failed}`；大任务按 `unfinished` keyset 分页（§5） |
| 4 | 日线失败路由 | 读顶层 `plan.failed_by_reason`；对 `REVISION_UNVERIFIED` 走显式 `update_data`（§7.2） | 日线任务已到终态 | 每个显式任务终态后 head 前进 |
| 5 | 年线 plan | `plan_market_update{dataset:"market.yearly", contract_version:"v1"[, end_date]}` | **`market.daily` 无非终态更新任务**（含步骤 4 的显式补漏任务；否则 `daily_update_inflight`，§3/§4） | 年线 `plan_id`、`counts`、`ineligible_reasons` |
| 6 | 年线 run（只处理 `eligible`） | `run_market_update{dataset:"market.yearly", contract_version:"v1", request_id, target_day, plan_id}` | 年线 plan 刚返回，其间无 catalog/head/gap/generation 变化（否则 `plan_stale`）；`market.daily` 无非终态任务 | `task_id`；worker 串行执行 |
| 7 | 年线不可处理路由 | 对 `ineligible_reasons` 中 `value_semantics_changed` / `daily_replaced_since_derivation` 走显式年线重推（§7.4） | 年线 run 已到终态 | 每个显式任务终态后 yearly head 前进 |
| 8 | readback | `get_data_availability` / `query_data`（`latest_per_entity`） | 步骤 3/6/7 任务均终态 | 各实体最新交易日、coverage/freshness 事实 |

顺序约束：服务端单 worker 串行执行。日线为 run→补漏（步骤 2→4）；年线为
plan→run→补漏（步骤 5→6→7）。**年线补漏必须排在年线 run 之后**：
`run_market_update` 的 `plan_id` 绑定 plan 时刻的状态（§3 步骤 2），而显式年线重推
（§7.4）会推进这些实体的年线 head、改变 ineligible 集合；若把补漏排在 run 之前，
步骤 6 的 run 必然 `plan_stale`。补漏完成后可选再 `plan_market_update`
（`dataset:"market.yearly"`）核对一次：此时 `eligible` 应为 0，`ineligible` 只剩
`already_current` 及预期的无需动作原因。

### 7.2 日线失败路由

`reason_code → 动作` 的分类表见 §5，本节不重复；此处只补 `REVISION_UNVERIFIED`
的显式调用示例。

成因：plan/run 的日线刷新窗口是**整段历史**（`refresh_range=[first_day,
target_day]`，`src/data_supply/plan.rs:519-522`），窗口内已发布 bar 被上游重算
（除权/复权）且无修订证据 → 按设计 fail-closed。显式 `update_data` 改用**前向
窗口** `[last_record+1, target_day]`：窗口内无历史 bar，走重写分支完成。每任务
≤100 项，按实体当前 head 分批。

请求示例（字段逐一对齐 `prod_append_v6.py` 的 `build_append_items`:207-219 与
`submit_and_wait`:186-188）：

```json
{
  "dataset": "market.daily",
  "contract_version": "v1",
  "request_id": "daily-<YYYYMMDD>-residual-001",
  "items": [
    {
      "entity": {"exchange": "<sh|sz>", "code": "<6位代码>"},
      "base_version_id": "<该实体当前 daily head 的 version_id>",
      "start_date": "<该实体 last_record 的次一日>",
      "end_date": "<target_day>"
    }
  ],
  "selection": null
}
```

- `entity`/`base_version_id`/`start_date`/`end_date` 即 `RangeWriteItem`
  （`wire.rs:761-766`）；显式 items 形式下 `selection` 传 `null`（prod 驱动显式
  传 `None`；该字段为 `Option`，不传等价）。
- `base_version_id` 必须取**提交时**该实体当前 head；被并发取代会 item 级
  `STALE_BASE:` `needs_verification`，重取 head 后重提该 item。
- 不要对这些实体 `retry_task`：同一窗口、同一证据，确定性复败。

### 7.3 年线不可处理原因

`plan_market_update(dataset:"market.yearly")` 逐实体解析后落入 `eligible`（进
run）或 `ineligible:<reason>`。`resolve_yearly_on`
（`src/data_supply/plan.rs:597-715`）里**全部**出现的 reason：

| reason | 含义（出处） | 调用方动作 |
|---|---|---|
| `already_current` | 年线 base 仍等于当前 daily 状态（`YearlyState::Fresh`，`series.rs:680-681,731-735`） | 正常状态，无需动作 |
| `head_unreadable` | daily series 行存在但读不出/不一致（Integrity，`plan.rs:612-619`） | fail-closed；不自动修复，报告（损坏或缺陷） |
| `no_daily_base` | 该实体无已发布 daily head（`plan.rs:622-628`） | 与日线 `absent` 同类；先走日线首采，本步不做年线 |
| `no_derivable_years` | daily 区间内没有 ≤`target_day.year()` 的可派生年份（`plan.rs:639-644,673-679`） | 无需动作；无内容可推 |
| `integrity_failure` | `yearly_state` 判定 lineage 断裂/伪造（`plan.rs:656-664`；`series.rs:713-754`） | fail-closed；不自动修复，报告 |
| `daily_replaced_since_derivation` | base 之后日线发生 `replace`，旧年线内容基于已被替换的数据（`plan.rs:692-703`；`series.rs:695-696,745-747`） | **需显式年线重推**（§7.4） |
| `value_semantics_changed` | base 之后日线 commit 改变 `{source, unit_currency, adjustment_evidence}`（`plan.rs:698-700`；`series.rs:697-698,748-750`） | **需显式年线重推**（§7.4） |

注：`YearlyState::Behind`（`series.rs:682-684`）产生的是 **item**（进 run），不是
ineligible；只有 `Invalid`（上表最后两行）才进 ineligible 并需显式重推。

### 7.4 显式年线重推调用形状

对 §7.3 需重推的实体（每批 ≤100 项；**排在年线 run 之后，见 §7.1**），按
`prod_append_v6.py` 的 `phase_yearly_rederive`:319-401 顺序：

1. `get_data_availability{dataset:"market.yearly", contract_version:"v1",
   selection:{"kind":"batch","entities":[…chunk…]}, filter:{"kind":"yearly", …},
   version:{"kind":"latest_per_entity"}, page_size:100}` → 取 `items`；需重推集 =
   `version_ref` 为空 ∪ `stale=="behind"` ∪ `reason` 为
   `daily_replaced_since_derivation` 或 `value_semantics_changed`
   （`prod_append_v6.py:306-316,328-337`）。
2. `list_data_versions{dataset:"market.yearly", …, selection:{"kind":"batch",
   "entities":[…chunk…]}}` → 取每实体 `version_ref.version_id` 作为
   `expected_yearly_head`（`prod_append_v6.py:347-354`）。**坑**：失效年线在
   availability 上报 `version_ref=null`，但 `market_series` 仍存 head 行；
   `expected_yearly_head` 必须 pin `list_data_versions` 里那个已存版本，否则 CAS
   网关以 `effective head already exists` 拒绝。只有完全没有已存年线的实体才取
   null（`prod_append_v6.py:341-346`）。
3. `get_data_availability{dataset:"market.daily", …}` → 取该实体 daily
   `version_ref.version_id` 与 `first_record`/`last_record`
   （`prod_append_v6.py:355-365`）。
4. `get_yearly_base_years{contract_version:"v1", items:[{entity,
   base_daily_version_id:<daily head>}]}` → 取 `present_years`（**不传
   `start_year`/`end_year`**，用默认 1..9999，`wire.rs:273-280`）；`resolve_years`
   把 `present_years ∩ [first_record 年, last_record 年]` 作为 `years`，未来年
   （> 上海当前年）记异常入报告（`s5_run.py:357-388`）。
5. `update_data{dataset:"market.yearly", contract_version:"v1", request_id,
   items:[…]}` 提交（`prod_append_v6.py:374-386`）。

请求示例（字段对齐 `YearlyWriteItem`，`wire.rs:832-838`；item 构造见
`prod_append_v6.py` `phase_yearly_rederive`:374-379）：

```json
{
  "dataset": "market.yearly",
  "contract_version": "v1",
  "request_id": "daily-<YYYYMMDD>-y-001",
  "items": [
    {
      "entity": {"exchange": "<sh|sz>", "code": "<6位代码>"},
      "base_daily_version_id": "<该实体当前 daily head 的 version_id>",
      "years": [<present_years ∩ [first_record 年, last_record 年]>],
      "publication": {
        "mode": "current_head",
        "expected_yearly_head": "<list_data_versions 里已存 yearly head version_id，无则 null>"
      }
    }
  ]
}
```

- `publication` 为 `YearlyPublication::CurrentHead`（`wire.rs:826-828`），`mode`
  取 `current_head`。
- `get_yearly_base_years` 的 `base_daily_version_id` 已被新 head 取代时返回
  `reason_code:"base_superseded"`（`wire.rs:296-302`）——重取当前 daily head
  重试该实体；`base_unavailable`/`integrity_failure` 属 fail-closed。

### 7.5 预期规模

这两类占比随追赶跨度变化：单日追赶通常是**几十个**量级，跨多个交易日的追赶
会到**数百个**。每个显式任务 ≤100 项，所以按当前 head 分批串行提交。规模随
当日事件数与追赶跨度变化，不构成固定阈值。
