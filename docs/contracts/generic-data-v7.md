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
  end_date?: "YYYY-MM-DD"}`；`end_date` 缺省为受理时钟的
  Asia/Shanghai 当日，只允许传**更早**日期，大于当日即拒绝。
- 行为（只读，不建任务、不写库）：在单个读快照内对当前 catalog 覆盖
  ∩ 已首采实体逐实体解析，分为 `eligible`（可执行）、`already_current`、
  `absent`、`ineligible:<reason>` 四类。
- 输出（小）：`{plan_id, dataset, contract_version, generation_ref,
  target_day, counts{eligible, already_current, absent, ineligible},
  ineligible_reasons[{reason, count}], snapshot_time}`。不枚举实体。
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
- 取消/失败排查：`unfinished` 恒含失败项（`success=false` + `error`），
  未开始项 `success=null`。

## 6. 与 #294 的关系

显式 `update_data` 的「整批原子受理、>100 拒绝」语义不变（见
[generic-data-tools](../generic-data-tools.md) 的原子受理裁决）。plan/run 工作流
内部不存在整批拒绝模式：单个 item 失败只影响该 item。这**仅在**新工作流
内消除 #294 的失败模式；#294 本身是否收口由 issue 裁定，不由本契约隐含。
