# v8 工具面（入口拆分 / 列式 v2 / 任务面 v2）：发布说明草稿

关联 Issue：[#387](https://github.com/liqiangcc/x-data-mcp/issues/387)。
**状态：草稿，未发布。** 部署时用作 GitHub Release 正文与 [STATUS](../STATUS.md) 当前状态区的
更新依据；部署前不要当已发布事实引用。设计与拆分见 [v8 设计](issue-387-v8-ai-tool-surface.md)，
契约见 [generic-data-v8](../contracts/generic-data-v8.md)，部署步骤见
[v8 切换清单](../contracts/v8-switch-checklist.md)。

## 一句话

工具面按**使用者**拆成三个入口（研究 5 / 编排 15 / 运维 9，共 24 个 data 工具），
读取面与任务面新增 **`contract_version: v2`**（列式面板、单日全市场截面、`fields` 投影、
结构化 `item_id`），`v1` 与既有工具名全部保留；`capability_revision` 升
**`generic-data-v8`**。隧道只暴露研究入口。

## 变了什么

1. **三个入口，一个二进制。** `data_service --entry research|ops|maint`；`mcp.sock` 保留为
   编排入口，新增 `research.sock`（隧道改指它）与 `maint.sock`。入口是 allowlist 过滤：
   入口外工具报 `unknown_tool`，不泄露存在性；同一工具在不同入口输出逐字节相同。
2. **`tools/list` 不再内联 `outputSchema`**，描述 ≤240 B。记录 schema 按
   `get_data_capabilities({dataset, operation})` 取，与注册表同源。这是发现成本的主要降幅：
   设计基线 data 侧 `tools/list` 115,496 B → 研究入口 ≤11,000 B（CI 断言）。
3. **`query_data` 的 v2：列式。** `panel`（单实体/批量面板，`dates` + `columns` 分轴）、
   `cross_section`（某日全市场，`trade_days` + `entities` 各出现一次，窗口 ≤5 个交易日）、
   `table`（目录/版本/事件/日历/availability）。实体、版本清单、共享 `metadata` 只出现一次；
   `fields` 按 dataset 投影。单元格取值与 v1 行式逐位相同。
4. **三个新 dataset（v2 only）**：`market.index`（9 条指数基准）、
   `market.adjustment_events`（因子事件，`known_at` 恒 NULL 不输出）、
   `market.calendar`（交易日历）。不新增工具，只加 dataset 分支。
5. **新只读工具 `list_catalog_facets`**：行业码/名称/成员数，省掉"先翻 5,228 条目录猜行业码"。
6. **任务面 v2**：`item_id` 结构化（`{dataset, entity?, period?}`，仍可回传给 `retry_task`）、
   `failed_by_reason` 顶层直出、`get_task` 一次给计数 + 最多 20 条失败样本（错误截断 240 字符）、
   `list_tasks` 支持 `reason_code` 过滤。`get_task`/`get_task_progress`/`list_tasks` 三处。
7. **入参归一化**：实体可写 `"sh.600000"`、日期可写 `YYYYMMDD`、`version`/`filter`/
   `contract_version` 有缺省值；`as_of`/`known_at` 明确报 `reserved_for_m2`（不是裸
   `unknown field`）。归一化不改语义，幂等摘要不变。
8. **`text` 分级**：研究入口 `text` 与 `structuredContent` 同值；编排/运维入口大响应给
   ≤300 B 的有界摘要（`status`/计数/`next_cursor`/`refs`），`structuredContent` 仍是完整载荷；
   `X_DATA_MCP_TEXT_MODE` 可覆盖。
9. **`compute_metrics` 移到运维入口**（不删除，v9 再评估，D-38）。

## 为什么

- **发现成本按入口计价**：研究客户端不必为 9 个取证/采集工具付工具定义字节，运维能力留在本机。
- **重复字节是主要浪费**：XDM-18 实测单股一年日线 130,126 B 中 49.3% 是逐行重复的
  `version_ref`；10 实体单日截面 78% 是逐实体重复的 manifest。列式 + 单次版本清单直接去掉这部分。
- **想做的事没有入口**："某日全市场涨幅前 10"过去没有截面接口（≈82.8 万 token / 54 次调用）。
- **schema 不该在每次发现时全量下发**：`tools/list` 只保证"不查帮助也能传对参数"。

## 使用者要做什么

- **研究者（隧道）**：重新发现一次（工具集变了）。`get_data_capabilities({})` 只用于
  revision 握手；要 schema 用 `{dataset, operation}`。大结果用 `fields` 收窄，不要靠翻页。
- **日更 Skill（编排入口）**：allowlist 收缩到 15 个工具；`tools/list` 不再带 `outputSchema`。
  编排流程本身不变（plan → run → 进度 → 补漏）。
- **operator（本机）**：取证/重试/取消改在 `maint.sock`；`compute_metrics` 只在这里可见。
- **下游 `x` 仓库**：**不需要改**。它只调 `query_data`（v1，字节不变）连 `mcp.sock`；
  `mcp.sock` 的工具从 23 变 15，但 `query_data` 仍在。要改连 `research.sock` 需另行批准（D-40）。
  部署前仍要核对两件事（见下）。

## 部署前置（都要 owner 批准）

1. **Devin 实测（#387 的 Q3）**：编排入口 `text=summary` 偏离 MCP 规范的 SHOULD，Devin 侧
   行为**未验证**。部署前必须跑一次真实日更，确认摘要字段够用、客户端不重复注入、
   大响应不被静默截断。未实测不得部署。
2. **`x` 仓库核对（P-02 已撤销，剩两项）**：`P-02`（版本 pin）已于 2026-10-26 撤销——
   只读核对确认 x 侧不 pin、不缓存、不持久化 `version_id`。部署前仍要确认：
   ① x 能处理 6 位小数（D-32）；② x 实际调用的工具名都在 `mcp.sock` 的 15 个之内。
3. **schema 23 与 M1 重建**：v8 的发布二进制是 `COMPACT_SCHEMA_VERSION = 23`，读不了 schema 21
   库。v8 部署必须与 M1 的归档 + 全新 init 同一窗口（[M1 部署清单](issue-383-m1-deploy-checklist.md)）。
4. **三个单元的租约裁决**：`DatabaseLease` 是独占 flock，三个进程不能同时持有同一个库；
   maint 的写工具与"只有 ops 受理写"也尚未裁决。见 [切换清单](../contracts/v8-switch-checklist.md) §2。
5. **单元与隧道改动**：新增 `x-data-mcp-research`/`x-data-mcp-maint`、隧道改指 `research.sock`，
   逐条批准后执行（[切换清单](../contracts/v8-switch-checklist.md) §3–§4）。
6. 合并、打 tag、发布、部署（D-10 的同类边界）；保留上一版 release + 归档库作回滚点。

## 回滚

- **隧道**：drop-in 指回 `mcp.sock`，重启。客户端会看到 15 个工具（能工作，但比研究入口宽）。
- **单元**：停 `x-data-mcp-research`/`x-data-mcp-maint`，`x-data-mcp-data` 不动。
- **text 分级**：`X_DATA_MCP_TEXT_MODE=full` 重启 `x-data-mcp-data`。
- **二进制 + 库**：上一版 release + 归档的 schema 21 库一起退。只退二进制会因 schema 不匹配
  拒绝启动。

## 明确不变

- `v1` 读取面与任务面的字节、既有 23 个工具名、写受理语义（fail-closed 不变）。
- 数值语义：精确小数仍是十进制字符串，空值仍不伪造，列式只改形状不改取值。
- 幂等/取消/版本/核实语义；acquire server 仍是 10 工具。
- 不做服务端筛选/排序/聚合；不做 MCP 文件传输（`export_data` 只设计不做，D-39）；
  不实现 `as_of`/`known_at`（M2）；不改 schema（v8 自身无 migration）。

## 未验证（发布时如实保留）

- 真实全市场截面的字节与耗时：CI 只有夹具。夹具实测（`scripts/v8_token_budget.py`，2026-10-27
  在 main 上运行）：T1 单股 242 根面板 4,127 token（预算 5,000）、单日截面 2 天 341 token
  （预算 400）、目录行业 244 token（预算 300）、任务诊断 951 token（预算 2,000）、
  availability 10 实体 1,157 token（预算 1,500）。这些是合成快照，不是生产数据。
- 三个单元的内存与冷启动影响（单库多进程只读未实测）。
- Devin 在 `text=summary` 下的行为（Q3）。
- 隧道侧实际看到的研究入口工具数（隧道配置不在仓库）。
- 各客户端是否把 `structuredContent` 注入上下文（需要真实客户端会话）。
