---
name: x-data-status
description: 只读查看 x-data 同步状态 — generation 生命周期、任务进度与已发布数据可用性；不创建或改变任何任务
triggers:
  - user
allowed-tools:
  - mcp__x-data-acquire__get_acquisition_capabilities
  - mcp__x-data-acquire__sync_status
  - mcp__x-data-mcp__get_data_capabilities
  - mcp__x-data-mcp__get_task
  - mcp__x-data-mcp__get_task_progress
  - mcp__x-data-mcp__list_tasks
  - mcp__x-data-mcp__get_data_availability
  - mcp__x-data-mcp__get_yearly_base_years
  - mcp__x-data-mcp__query_data
  - mcp__x-data-mcp__list_data_versions
---

# x-data 只读状态检查

只允许上方 allowlist 内的只读 MCP 工具；禁止 shell/exec/SQLite/文件读写，禁止任何写工具（`acquire_generation`、`verify_generation`、`activate_generation`、`update_data`、`start_collection`、`backfill_data`、`republish_data`、`run_worker_once`、`cancel_task`、`retry_task`）。工具不可用 → 报告 `SKILL_CONTRACT_MISMATCH` 并停止。

1. `get_acquisition_capabilities` 使用 `contract_version:"v1"`；
   `get_data_capabilities` 按当前 schema 使用空对象 `{}`。期望
   `x-data-acquire-v1` / `official-generation-v1` / `generic-data-v4`；不匹配 →
   停止并报告 `SKILL_CONTRACT_MISMATCH`。
2. `sync_status`：报告 active generation、各 generation 生命周期状态、inflight acquisition。
3. `get_data_availability`：报告 market.daily（可选 market.yearly）coverage/freshness/gap 事实。
4. 可选 `list_tasks` / `get_task_progress`：报告近期任务状态。

汇总输出：readiness、active generation、最新数据日期、阻塞原因（原样转述，不解释、不修复）。
