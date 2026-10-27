# generic-data-v8 契约（入口拆分、列式 v2、任务面 v2）

**状态**：已部署（2026-10-27，v8 最小切换）。当前部署为 `generic-data-v8` /
15 data tools（`mcp.sock` 的 `ops` 入口）/ compact schema 24（见 [STATUS](../STATUS.md)）。
research/maint 入口、隧道切换与清单 §2.1/§2.2/§2.6 推迟到第二阶段，
见 [v8-switch-checklist](v8-switch-checklist.md) §9。

设计依据：v8 设计 [#387](../tasks/issue-387-v8-ai-tool-surface.md)。凡本文标注「实现已核」的
论断都附 `文件:行号`（行号取自 main `84b6657`）；未标注的是契约目标或未验证项。

## 1. 适用面与不变量

| 项 | v7（已部署） | v8（代码，未部署） | 兼容性 |
|---|---|---|---|
| `capability_revision` | `generic-data-v7` | `generic-data-v8`（2026-10-27 部署日翻） | 客户端按 revision 握手，不匹配即停 |
| data 工具数 | 23 | 代码 24（+ 只读 `list_catalog_facets`）；最小切换部署的 `ops` 入口 15 | 旧工具名全部保留，无重命名、无删除 |
| 入口 | 1 个 data socket（23 工具） | **3 个入口**：research 5 / ops 15 / maint 9（`src/data_supply/registry.rs:43,51,70`） | `mcp.sock` 变成编排入口；旧单元不带 `--entry` 时默认 `ops`（`src/bin/data_service.rs:44`） |
| `contract_version` | `v1` | `v1` + **`v2`**（读取面与任务面，逐工具独立分支） | 缺省仍是 `v1`；v1 字节不变 |
| `tools/list` | 含 `outputSchema` | **不含** `outputSchema`，描述 ≤240 B | 客户端失去内联校验；schema 从 capabilities 按需取 |
| 入参 | 结构体实体、严格日期、必填契约版本 | 别名归一化 + 缺省值（§6） | 旧写法继续可用 |
| 时点参数 | 无 | `as_of`/`known_at` 是**保留名**，v8 明确拒绝 | 新增错误，不是新语义 |
| schema | 21（部署） | v8 自身**不改 schema**（§10） | 部署不需要因 v8 迁移 |
| acquire server | 10 工具 | 不变 | — |

不变量（v8 只改形状，不改语义）：

- `version_id` 仍是内容摘要；`latest_per_entity` 仍是服务端快照内解析。
- 精确小数仍是十进制字符串；空值仍不伪造；列式布局的每个单元格与 v1 行里的值逐位相同
  （`src/data_supply/columnar.rs:1-9`，`tests/mcp/v8_read_v2.rs:423`）。
- 写受理契约（`start_collection`/`update_data`/`backfill_data`/`republish_data`/`retry_task`/
  `cancel_task`）与核实工具仍是 `v1`、`WriteAcceptance` 形状不变（`src/data_supply/wire.rs:56-65`）。
- 入口不是权限：研究入口没有写/核实/采集工具，边界靠 allowlist + 交集为空的测试守住
  （`tests/mcp/v8_tool_surface.rs:37-102`）。

## 2. 入口与 socket

| 入口 | `--entry` | socket | 工具数 | 隧道暴露 |
|---|---|---|---|---|
| 研究 | `research` | `run/research.sock`（新增） | 5 | **是（唯一）** |
| 编排 | `ops` | `run/mcp.sock`（沿用） | 15 | 否 |
| 运维/取证 | `maint` | `run/maint.sock`（新增） | 9 | 否 |
| 来源接入 | — | `run/acquire.sock`（不变） | 10 | 否 |

allowlist 是静态常量（`src/data_supply/registry.rs:43-80`）：

- **research（5）**：`get_data_capabilities`、`query_data`、`get_data_availability`、
  `list_data_versions`、`list_catalog_facets`。
- **ops（15）**：`get_data_capabilities`、`plan_market_update`、`run_market_update`、
  `start_collection`、`update_data`、`get_task`、`get_task_progress`、`list_tasks`、
  `get_request`、`get_data_availability`、`query_data`、`list_data_versions`、
  `get_yearly_base_years`、`verify_publication_result`、`verify_stopped_writes`
  （== 日更 Skill 的 data 侧 allowlist）。
- **maint（9）**：`get_data_capabilities`、`backfill_data`、`republish_data`、`retry_task`、
  `cancel_task`、`inspect_verification_task`、`verify_data_gap`、`run_worker_once`、
  `compute_metrics`。

行为（实现已核）：

- 入口外的工具名 → `unknown_tool`（`src/data_supply/mcp.rs:715-725`），**不是** `forbidden`：
  入口不可见，也不泄露存在性。这与设计文档写的 `tool_not_found` 不同，见 §12。
- 启动闭合检查：allowlist 里出现未注册工具或重复项时进程拒绝启动
  （`src/data_supply/registry.rs:114-145`，`src/bin/data_service.rs:81`）。
- 只有 `ops` 入口跑后台 worker 与启动时的 plan-run `interrupted` 收尾
  （`src/bin/data_service.rs:90,115-116`）；research/maint 启动不写任务状态。
- 同一份注册表同时驱动 `tools/list`、`get_data_capabilities` 与 dispatch
  （`src/data_supply/mcp.rs:701-713`）；三个入口共享的工具（`query_data` 等）逐字节相同
  （`tests/mcp/v8_tool_surface.rs:232-252`）。
- stdio/dev 面（`EntryKind::All`）不是部署面：`data_service` 不接受 `--entry all`
  （`src/data_supply/registry.rs:82-90`，`tests/mcp/v8_tool_surface.rs:254-263`）。

`tools/list` 预算（CI 断言，`tests/mcp/v8_tool_surface.rs:104-142`）：研究 5 工具
≤11,000 B、编排 15 ≤27,500 B、运维 9 ≤14,500 B；每条描述 ≤240 B；无 `outputSchema`；
`inputSchema.type` 为 `object`。

## 3. 发现面

### 3.1 `get_data_capabilities`

输入只有 `dataset` 与 `operation`（`src/data_supply/registry.rs:204-208`）；带
`contract_version` 会因 `deny_unknown_fields` 报 `invalid_arguments`。

- **紧凑目录**（不带 `operation`）：一行一个工具，字段为 `tool_name`、`contract_version`
  （工具的默认 wire 版本，恒 `v1`）、`implemented`、`operator_only`、`entry`；不含
  `description`/`input_schema`/`output_schema`/`example`/`schema_digest`/`runtime_readiness`
  （`src/data_supply/registry.rs:215-247`，`src/data_supply/mcp.rs:368-380`）。
  研究入口的紧凑目录 ≤1,500 B（`tests/mcp/v8_tool_surface.rs:165-171`）。
- **详版**（带 `operation`）：在紧凑字段之上补 `description`、`dataset`、
  `runtime_readiness`、`governance_readiness`、`blocking_reasons`、`declared_scope`、
  `active_generation_ref`、`input_schema`、`output_schema`、`example`、`schema_digest`；
  `input_schema`/`output_schema` 与 registry 逐字节相等（`tests/mcp/v8_tool_surface.rs:176-195`）。
  一个工具按 dataset 展开成多行。
- `operation` 不是注册工具名 → `unsupported_operation`（`src/data_supply/mcp.rs:333-341`）。
- **语义变化（相对 v7）**：无过滤的 capabilities 不再返回 schema 详情；schema 必须按
  `operation`（可再加 `dataset`）取。这是 v8 的主要发现成本控制手段。
- `items[].entry` 是 `research`/`ops`/`maint`（stdio/dev 面为 `all`）。

### 3.2 `list_catalog_facets`（新工具，只读）

输入 `{dataset?: "securities.catalog", catalog_version?: "<id>"|"latest"}`；`dataset` 只接受
`securities.catalog`，其它值 → `invalid_arguments`（`src/data_supply/mcp.rs:411-420`）。

输出 `{dataset, catalog_version, facets:{industries:[{code, name, members}]}}`
（`src/data_supply/registry.rs:250-275`）。只有 `industry.status=assigned` 的成员计数；
目录没有 head → `catalog_not_found`。它是**数据事实**，不进 capabilities（capabilities 是契约目录）。

## 4. 读取面 v2

### 4.1 请求

`query_data`/`get_data_availability` 的输入（`src/data_supply/wire.rs:232-251`）：

```json
{"dataset":"market.daily","contract_version":"v2",
 "selection":{"kind":"single","entity":"sh.600000"},
 "filter":{"kind":"daily","start_date":"2025-01-02","end_date":"2025-12-31"},
 "version":{"kind":"latest_per_entity"},
 "fields":["close"],"page_size":100,"cursor":null}
```

`list_data_versions` 用同一 `dataset`/`selection`/`contract_version`/`page_size`/`cursor`，无
`filter`/`version`/`fields`（`src/data_supply/wire.rs:352-363`）。

dataset 矩阵（实现已核）：

| dataset | selection.kind | filter.kind | version.kind | v2 layout | 备注 |
|---|---|---|---|---|---|
| `securities.catalog` | `catalog`（`scope:"current_sh_sz_a"`） | `catalog`（`code`/`name_query`/`industry_code` 可选） | `latest_per_entity` 或 `catalog_version` | `table` | 无实体行键 |
| `market.daily` | `single`/`batch`/`universe` | `daily`/`cross_section` | `latest_per_entity`/`explicit_version_map` | `panel`/`cross_section` | 只读**前复权**序列（§12） |
| `market.yearly` | `single`/`batch`（`universe` 被归一化层拒绝） | `yearly` | `latest_per_entity`/`explicit_version_map` | `panel` | 本地自然年派生 |
| `market.index` | `single`/`batch`/`universe` | `daily`/`cross_section` | `latest_per_entity`/`explicit_version_map` | `panel`/`cross_section` | 指数基准；`universe` 来自静态登记（9 条），不回显 `catalog_version` |
| `market.adjustment_events` | `single`/`batch`/`universe` | `daily`（生效日区间） | `latest_per_entity`/`explicit_version_map` | `table` | `known_at` 不输出 |
| `market.calendar` | `market`（无实体） | `daily`（区间） | `latest_per_entity` | `table` | market 级，无 `entities` |

`market.index`/`market.adjustment_events`/`market.calendar` 是 v2-only：`contract_version:"v1"`
请求 → `invalid_arguments`（`<dataset> requires contract_version v2`，`src/data_supply/query.rs:34-38,527-533`）。
v1 请求带 `fields`、`selection.kind=universe|market` 或 `filter.kind=cross_section` 同样拒绝
（`src/data_supply/query.rs:30-51`）——不静默忽略。

`selection.kind=universe`：服务端解析目录版本（缺省 `latest`），把**具体版本 id** 回显到
`catalog_version`；`market.index` 的成员来自 `src/index_registry.rs:131` 的 9 条登记。
`selection.kind=market` 只用于 `market.calendar`（`src/data_supply/wire.rs:166-192`）。

`cross_section` 窗口按交易日历解析，**最多 5 个交易日**；超出 → `invalid_arguments`，消息给出
实际交易日数并指向 `filter.kind=daily` + `selection.kind=batch`（`src/data_supply/query.rs:126-138`）。

分页与预算：`page_size` 1..1000（缺省 100）；单快照 ≤100,000 行、≤16 MiB
（`src/data_supply/query.rs:10-11`）；cursor 是快照内位置，跨请求复用会报
`cursor conditions changed: …`（`src/data_supply/query.rs:353-360,604-614`）。

### 4.2 响应信封

所有 v2 布局共享同一信封（`src/data_supply/columnar.rs:249-278`）：

| 字段 | 含义 |
|---|---|
| `dataset` | 数据集名 |
| `contract_version` | 恒 `"v2"` |
| `layout` | `panel` / `cross_section` / `table` |
| `snapshot_id`、`snapshot_time` | 本次读快照 |
| `next_cursor`、`page_complete` | 分页 |
| `limitations` | 非空才出现 |
| `row_count` | 本页行数（`versions_table` 是本页版本数） |
| `entities` | 实体顺序；`market.calendar` 没有 |
| `trade_days` | 仅 `cross_section` |
| `dates` | 仅 `panel`：每个实体一列日期 |
| `columns` | 见 §4.3 |
| `versions` | 版本清单，每个 manifest 条目一行（§4.4） |
| `metadata` | 共享元数据（§4.4） |
| `catalog_version` | `universe` 解析出的目录版本（仅查询类工具） |
| `tasks`、`limitations`（顶层） | 仅 `get_data_availability`：相关任务对象共享一次 |

### 4.3 三种布局

- `panel`：`dates[entity_index][row_index]`，`columns[field][entity_index][row_index]`
  （`src/data_supply/columnar.rs:390-469`）。
- `cross_section`：`trade_days` 一次、`entities` 一次，
  `columns[field][day_index][entity_index]`（`src/data_supply/columnar.rs:704-769`）。
- `table`：`columns[field][row_index]`。目录、版本清单、因子事件、日历、availability 用它
  （`src/data_supply/columnar.rs:472-560,561-575,638-702`）。

空值规则：整列都是 `null`/空数组时该列**不出现**；部分为 `null` 时保留 `null` 单元格，行不位移
（`src/data_supply/columnar.rs:283-295`）。

`fields` 投影（v2 only）：缺省为各数据集的核心列；未知或重复列 → `invalid_arguments` 并列出可选值
（`src/data_supply/columnar.rs:94-118`）。允许/缺省集合：

| dataset | 允许字段 | 缺省 |
|---|---|---|
| `market.daily`/`market.index` | `open, high, low, close, volume, amount` | 全部 |
| `market.yearly` | 上述 + `first_trading_date, last_trading_date, year_unfinished` | 全部 |
| `securities.catalog` | `name, industry_code, industry_name, industry_status, industry_reason` | 前三个 |
| `market.adjustment_events` | `category, c1, c2, c3, c4` | 全部（`effective_day` 是行键，不重复） |
| `market.calendar` | `trade_day` | 同 |
| `get_data_availability` | 见 `src/data_supply/columnar.rs:55-74` | 全部 |

数值：价格、量、额都是十进制字符串；`null` 表示源未提供，不用 0 代替。

### 4.4 `versions` 与 `metadata`

- `versions` 每个 manifest 条目一行：`entity`（无实体时省略）、`status`
  （`present`/`absent`）、`version_id`（present 时）、`published_at`、`stale`/`as_of_day`/`reason`
  （存在时）（`src/data_supply/columnar.rs:207-236`）。`integrity_sha256` 不再输出——它与
  `version_id` 相同。
- `metadata` 只写**所有实体相同**的值，键为 `source`、`coverage_status`、`coverage_reason`、
  `units`、`adjustment`、`lineage`、`index`（`src/data_supply/columnar.rs:129-205`）。逐实体不同的
  值下沉到该实体的 `versions` 行；全为 null 的键不出现。生产里 `coverage_status` 会因
  fail-closed 实体而逐实体不同，所以它经常出现在 `versions` 行而不是共享 `metadata`。
- `metadata.adjustment` 是既有的 `Evidence` 对象
  `{field:"price_adjustment", status, value, reason}`（`src/data_supply/wire.rs:379-386`，
  `src/sqlite_supply.rs:1142-1149`），`value` 为 `"raw"`/`"qfq"`。它**不含** `price_scale` 或
  `algorithm_version`——设计文档写的是 `{algorithm_version, price_scale, basis}`，实现未提供，见 §12。
- `metadata.index` 只在 `market.index` 出现：`aggregate_scope`、`member_count`、`price_unit`、
  `volume_unit`、`amount_unit`、`aliases`、`excluded_bar_count`（指数坏 bar 剔除数量，
  #383 owner 2026-10-27 裁决；`src/data_supply/wire.rs:410-421`，
  `src/sqlite_supply.rs:2556-2596`）。
- `metadata.lineage` 只在派生年线出现：`base_daily_version_id`、`algorithm_version`、
  `first_trading_date`、`last_trading_date`（`src/data_supply/wire.rs:387-392`）。

`market.adjustment_events` 只有 `effective_day`（行键）与 `category`/`c1..c4`；`known_at` 恒 NULL，
因此不输出（`src/data_supply/wire.rs:320-333`，`src/sqlite_supply.rs:555-566`）。

### 4.5 `get_data_availability` 的 v2

一行一个实体（`layout:"table"`）；`selection.kind=universe` 折叠成一行 `universe`
汇总（`entities/present/absent/stale/unavailable`），`related_tasks` 只留 id、任务对象共享在顶层
`tasks`，逐实体相同的 `limitations` 提到顶层（`src/data_supply/columnar.rs:583-702`）。
`related_tasks_total`/`related_tasks_truncated` 在 universe 行不输出（逐实体相同的零没有信息量）。

### 4.6 `list_data_versions` 的 v2

`layout:"table"`，`versions` 每实体一行（`entity`、`version_id`、`published_at`），
无 `integrity_sha256`；`universe` 时回显 `catalog_version`
（`src/data_supply/columnar.rs:561-575`，`src/data_supply/query.rs:497-525`）。

## 5. 任务面 v2

`get_task`/`get_task_progress`/`list_tasks` 支持 `contract_version:"v2"`（缺省 v1）
（`src/data_supply/wire.rs:478-497,527-556`）。v2 只改四件事（`src/data_supply/task_v2.rs:1-20`）：

1. **`item_id` 结构化**：`{dataset, entity?, period?}`。证券 item 的存储形式是
   `["market","sh.600000","day"]`；服务器内部派生 item（`derived:calendar`、
   `derived:index:sh.000001`）整串放进 `dataset`，不发明 entity/period
   （`src/data_supply/wire.rs:656-679`，`src/data_supply/task_v2.rs:75-85`）。
   `ItemId::text()` 仍产出 `retry_task` 接受的字符串（`src/data_supply/wire.rs:668-679`）。
2. **`failed_by_reason` 顶层直出**：按 `reason_code` 计数，不再只藏在 `plan` 里
   （`src/data_supply/task_v2.rs:120-124,137-168`）。
3. **`get_task` 一次给全**：`total/pending/running/finished/succeeded/failed` 计数 + 最多
   `ERROR_SAMPLE = 20` 条失败样本 `errors[{item_id, reason_code, error}]`
   （`src/data_supply/task_v2.rs:34,137-168`）。`error` 截断到 240 字符并附原始长度
   （`src/data_supply/task_v2.rs:31,56-64`）。
4. **两个版本名分开**：任务自身的契约版本是 `task_contract_version`，wire 版本是
   `wire_contract_version`（`src/data_supply/task_v2.rs:126-135`）。

`reason_code` 取值封闭：`REVISION_UNVERIFIED`、`PUBLISH_PRECONDITION`、`NO_BARS_IN_RANGE`、`OTHER`
（`src/data_supply/task_v2.rs:39-46`）。`list_tasks` 的 `reason_code` 过滤是 v2 only：v1 携带 →
`invalid_arguments`；未知值 → `invalid_arguments` 并列出四个类
（`src/data_supply/observe.rs:742-753`）。

`get_task_progress` v2：`unfinished[].item_id` 结构化、失败项恒带 `reason_code` 与有界 `error`；
`frozen_*`/`*_results` 的 `item_id` 也结构化（实体不再重复出现）；`plan` 只留
`plan_id`/`generation_id`/`target_day`，`failed_by_reason` 提到顶层
（`src/data_supply/task_v2.rs:171-300`）。大任务（`item_count > 100`）的 keyset 分页语义与 v1 相同：
只看 `status`/计数/`next_cursor`，`unfinished` 是翻页时刻的最新项状态。

`list_tasks` v2：`items[]` 是 v1 summary 的改名版（`task_contract_version`），加顶层
`wire_contract_version`（`src/data_supply/observe.rs:686-703`）。

`get_request` 保持 v1（`WriteAcceptance`/`RequestView` 形状不变）。

## 6. 入参归一化（解码层，不改语义）

归一化在 typed decode 之前执行（`src/data_supply/wire/normalize.rs:69-83`），只放宽**写法**；
归一化后的值参与同一套契约 `normalize` 与幂等摘要，所以同请求的 digest 与 v7 相同。

| 输入 | 友好写法 | 契约写法 |
|---|---|---|
| 实体 | `"sh.600000"` | `{"exchange":"sh","code":"600000"}` |
| `verify_data_gap.symbol` | `{"exchange","code"}` 或 `"sh.600000"` | 规范文本 `"sh.600000"` |
| 日期 | `YYYYMMDD` | `YYYY-MM-DD`（`start_date`/`end_date`/`target_day`） |
| `contract_version` | 省略（读取/任务面 → `v1`） | `"v1"` |
| `version` | 省略 / `{"kind":"latest"}` | `{"kind":"latest_per_entity"}` |
| `filter`（catalog） | 省略 | `{"kind":"catalog"}` |
| `cancel_task.expected_revision` | 省略（读当前 revision） | 显式 u64（严格前置） |

仍然拒绝（每条都有测试，`src/data_supply/wire/normalize/tests.rs`）：

- `as_of`/`known_at` → `reserved_for_m2`，消息指向 M2（`src/data_supply/wire/normalize.rs:62,86-100`）。
- 非六位代码、未知 exchange、无法解析的日期 → `invalid_arguments`，消息给出可接受写法。
- `selection.kind` 与 dataset 不匹配 → `invalid_arguments`，消息列出该 dataset 接受的 kind
  （`src/data_supply/wire/normalize.rs:132-156`）。
- 非 catalog 数据集缺 `filter` → `invalid_arguments`，消息给出 `filter.kind` 与必填字段
  （`src/data_supply/wire/normalize.rs:170-224`）。
- 多余字段 → `invalid_arguments`（`deny_unknown_fields`；`market`/`latest_per_entity`/`default`
  都是 struct variant，unit variant 会漏检，已改）。

## 7. text 分级

`content[0].text` 按入口分级（`src/mcp.rs:690-724`）：

| 入口 | 缺省 | 行为 |
|---|---|---|
| research / stdio-dev | `full` | `text` 与 `structuredContent` 是同一份紧凑 JSON |
| ops / maint | `summary` | 载荷 ≤2,048 B 且非错误时给完整 JSON；否则给 ≤300 B 的有界摘要：`ok`、`tool`、`status`、`counts`（计数 + `page_items`）、`next_cursor`、`refs`（`task_id`/`request_id`/`generation_id`/`plan_id`/`snapshot_id`） |
| 任意 | `off` | `content` 为空数组（本机吞吐脚本用） |

`X_DATA_MCP_TEXT_MODE=full|summary|off` 覆盖入口缺省；未知值让 `data_service` 拒绝启动
（`src/bin/data_service.rs:83-86`）。`structuredContent` 始终是完整载荷，不受分级影响。

## 8. 错误码

沿用 v7 的错误码，新增/明确：

| code | 条件 |
|---|---|
| `unknown_tool` | 工具不在本入口 allowlist，或根本未注册（`src/data_supply/mcp.rs:662,721`） |
| `reserved_for_m2` | 请求带 `as_of`/`known_at` |
| `unsupported_operation` | capabilities 的 `operation` 不是注册工具名 |
| `catalog_not_found` | `list_catalog_facets` 没有已发布目录 |
| `invalid_arguments` | 解码/归一化/投影/窗口超限/cursor 条件变化 |
| `version_not_found`、`stale_version`、`precondition_conflict`、`integrity_error`、`forbidden` | 同 v7（`src/data_supply/mcp.rs:686-698`） |
| 任务面 | `task_not_found`、`unsupported_contract`、`needs_verification`、`request_conflict`、`range_conflict`、`retry_request_id_conflict`、`empty_request_id`、`invalid_retry_selection`、`outcome_unknown`（`src/data_supply/mcp.rs:1210-1232`） |

## 9. 升版、并行期与下线

- 升版理由是**入口与 wire 形状**，不是精度（D-32：6dp 不升版）。
- `v1` 分支保留：读取面与任务面的 v1 字节不变（`tests/mcp/v8_read_v2.rs:423-465,538-546`，
  `tests/mcp/v8_task_v2.rs:70-140`）。
- 并行期从**部署日**起算，覆盖两个验收点，且不短于部署后 10 个交易日：
  1. 日更 Skill 在 `generic-data-v8` 下跑通一次真实日更；
  2. 下游 `x` 仓库的 `KlineReader` 在 v8 上核对通过（只读核对；若要改连 `research.sock`
     需另行批准，D-40）。
  任一未完成 → 并行期无限延长，不以“时间到了”为由下线。
- 下线 v1 分支与 [generic-data-v7](generic-data-v7.md) 走独立兼容评估，不夹在 v8 切换里。
  并行期长度与下线条件的执行清单见 [v8-switch-checklist](v8-switch-checklist.md) §1。

## 10. 部署与回滚

- **v8 自身不改 schema、不加表**：PR-1…PR-6 没有 migration，也没有读写新的行情表。
- 但 v8 与 M1（schema 22）、M2 PR-1（schema 23）在**同一个二进制**里：`COMPACT_SCHEMA_VERSION = 23`
  （`src/sqlite_tasks.rs:43`），`check_schema`（服务启动时 `current=true`）只接受 legacy 18 或当前 compact 版本
  （`src/sqlite_tasks.rs:12,43,450-465`）。所以**部署 v8 的发布二进制不能服务 schema 21 库**，
  必须按 M1 的 D-08 归档旧库 + 全新 init（[M1 部署清单](../tasks/issue-383-m1-deploy-checklist.md)）。
- 回滚必须**二进制与数据库一起退**：上一版 release + 归档的 schema 21 库。只切回二进制会因
  schema 不匹配拒绝启动。v8 自己的入口/socket 回滚只需把隧道指回 `mcp.sock`。
- 详细的部署日步骤、单元与隧道改动、常量翻转清单见 [v8-switch-checklist](v8-switch-checklist.md)。

## 11. 明确不做

- 不实现 `as_of`/`known_at`（M2）；不实现证券主表、ST、停牌、涨跌停。
- 不做服务端筛选/排序/聚合（谓词、`order_by`、`limit`、top-N）——方向等 XDM-14 实测（D-19）。
- 不新增 MCP 文件传输：`export_data` 只设计不做（D-39），导出仍是 M1 的 `export_research` bin。
- 不删除任何现有工具：`compute_metrics` 只移到运维入口（D-38），下线在 v9 评估。
- 不改 acquire server；不改写受理语义（D-18 的 fail-closed 不变）。
- 不暴露涨跌停价；规则数据到位后作为独立 dataset 挂到 `query_data`（名字与形状在 M2 设计
  [#391](../tasks/issue-391-m2-security-master.md) 里定，分离点 6）。

## 12. 与设计文档的差异（实现记录，需在部署前裁决）

以下是 main 上的实现与 v8 设计不一致、而本 PR 只改文档无法修正的点。逐条给出事实与代价，
不静默按设计写。

1. **`capability_revision` 已在 2026-10-27 部署日提交翻为 `generic-data-v8`**（`src/data_supply/mcp.rs`）。
   设计 §9.3 写“与 capability bump 同提交（代码常量）”，但 PR-1…PR-5 都没有 bump；AC-V8-13
   要求“v8 实现合并后”是 v8。**处理**：bump 与 `EXPECTED_DATA_CAPABILITY_REVISION`、
   `EXPECTED_SKILL_CAPABILITY_REVISION` 及当前状态区是同一组事实，提前 bump 会让
   `check_current_facts.py` 要求把“已部署 v8”写进当前区（与事实相反）。因此把 bump 放进
   **部署日同一提交**（清单 §5.1），AC-V8-13 的 revision 子句在部署日满足，合并日不满足。
2. **读面没有 `adjustment` 选项**：`Query` 没有该字段（`src/data_supply/wire.rs:232-251`），
   `market.daily`/`market.yearly` 固定读前复权序列
   （`src/sqlite_supply.rs:411-430`）。设计 §4.2/§10.2 说“扩成 `raw`/`qfq`，缺省 `raw`”。
   **处理**：v8 契约按实现写（`market.daily` = 前复权 6dp）；不复权只在研究导出读模型
   （`src/research/read.rs:163-173,262`）里可见。这不是分离点 2 的破坏（事实层仍在库里、
   派生层可重算），但“缺省 raw”的设计意图未实现；补 `adjustment` 需要一次 wire 变更，
   不在文档 PR 范围。**部署前需 owner 决定**：接受 v8 只暴露前复权，或补一个实现 PR。
3. **`metadata.adjustment` 形状**：实现是 `Evidence{field,status,value,reason}`
   （`src/sqlite_supply.rs:1142-1149`），设计 §5/§11 要求
   `{algorithm_version, price_scale, basis}`。`price_scale`/`algorithm_version` 在库里
   （`src/sqlite_market/series.rs:54-59`）和导出读模型里都有，MCP 读面没带出来。
   **处理**：契约按实现写；补齐是响应形状变更（需 v2 增字段 + golden 更新），列为后续 PR。
4. **入口外的工具名报 `unknown_tool`**（`src/data_supply/mcp.rs:715-725`），设计/Issue 写的是
   `tool_not_found`。**处理**：契约按实现写。`unknown_tool` 与既有“未注册工具”同码，
   不泄露入口存在性；改名要动错误码契约，收益低。
5. **`market.yearly` 不接受 `selection.kind=universe`**：归一化层拒绝
   （`src/data_supply/wire/normalize.rs:132-156`），设计 §4.3 的矩阵未区分。**处理**：契约按实现写
   （yearly 用 `single`/`batch`；全市场年线用 `plan_market_update`）。

## 13. 未验证（发布前如实保留）

| # | 项 | 为什么未验证 | 取证方法 |
|---|---|---|---|
| U-1 | 各客户端是否把 `structuredContent` 注入模型上下文 | 需要真实客户端会话 | 隧道 + ChatGPT/Claude/Devin 各跑一次同一 `query_data` |
| U-2 | Devin 在 `text=summary` 下是否被截断、摘要字段是否够用 | 官方未规定；只有 Skill 的经验 | 部署前用 Devin 跑一次 v8 编排入口（Q3） |
| U-3 | 全市场截面（T2）的真实字节 | CI 只有夹具 | 私有栈实测一次并贴 Issue |
| U-4 | 三个单元的内存与冷启动影响 | 需要编译产物 | 私有栈实测（单库多进程只读） |
| U-5 | 隧道侧实际看到的研究入口工具数 | 隧道配置不在仓库 | 改指 `research.sock` 后 `tools/list` 复核 |
| U-6 | `x` 仓库 `KlineReader` 在 v8 上的行为 | 另一个仓库 | 只读核对（v1 路径不变）；改连入口需另行批准 |
