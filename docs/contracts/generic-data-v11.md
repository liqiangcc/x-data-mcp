# generic-data-v11 契约（读取工具，D-75）

**状态**：**代码面（未部署）**。当前部署仍是 [generic-data-v10](generic-data-v10.md) / compact schema 27（preview.45，vm2-126）。
本文件记录 D-75（[#525](https://github.com/liqiangcc/x-data-mcp/issues/525)）在 `research` 入口新增的 10 个只读工具，以及 `tools/list` 的 schema 变化。
设计与裁决见 [issue-525-read-tools](../tasks/issue-525-read-tools.md)；工具表见 [mcp-tools](../mcp-tools.md)「读取工具」。
部署时才翻转 Skill 钉值和 `scripts/check_current_facts.py` 的 `EXPECTED_DEPLOYED_CAPABILITY_REVISION`、`EXPECTED_SKILL_CAPABILITY_REVISION`。

## 1. 相对 v10 的增量

| 项 | v10 | v11 |
|---|---|---|
| `capability_revision` | `generic-data-v10` | `generic-data-v11` |
| data 工具数 | ops 16 / research 5 / maint 9 | ops 16 / **research 15** / maint 9 |
| compact schema | 27 | **不变**（27，没有迁移） |
| `query_data` 的输入与输出 | | **不变**，同一请求逐字节相同（见 §4） |
| `tools/list` 输入 schema | 根上有 `$schema`、`title` | 读 schema 去掉这两项（见 §3） |

加工具不改 schema，也不改 `query_data` 的 wire 形状。`TemporalBlock.as_of` 在内部变为可选，但 `query_data` 永远写它，已发布的 schema 保持 `{"type":"string","format":"date"}`。

## 2. 10 个新工具

均在 `research` 入口。每个工具把窄输入转成一个现有的 v2 `Query`（`latest_per_entity`），由同一条读路径做校验、完整性检查和列式投影。响应是 v2 信封（`layout`、`columns`、`versions`、`metadata`、`limitations`、`page_complete`、`next_cursor`，事实类数据集带 `temporal`）。输入类型拒绝多余字段（`invalid_arguments`），不暴露 `contract_version`、`version`、`selection`、`filter`。标的写作 `sh.600000`，日期 `YYYY-MM-DD`（也接受 `YYYYMMDD`）。分页参数 `page_size` 1..1000（默认 100）和 `cursor` 对所有带分页的工具相同。

| 工具 | 必填 | 可选 | 对应的 `query_data` |
|---|---|---|---|
| `find_securities` | 无 | `query`、`code`、`industry_code`、分页 | `securities.catalog` |
| `get_daily_bars` | `securities`（1..100）、`start_date`、`end_date` | `adjustment`、`fields`、分页 | `market.daily` |
| `get_yearly_bars` | `securities`、`start_year`、`end_year` | `fields`、分页 | `market.yearly` |
| `get_index_bars` | `indices`（登记的 9 条）、`start_date`、`end_date` | `fields`、分页 | `market.index` |
| `get_market_snapshot` | `start_date`、`end_date` | `universe`、`adjustment`、`fields`、分页 | `market.daily`，`universe` + `cross_section` |
| `get_trading_calendar` | `start_date`、`end_date` | 分页 | `market.calendar` |
| `get_price_limit_rules` | 无 | `as_of`、分页 | `market.price_limit_rules` |
| `get_security_state` | `securities`、`fact` | `as_of`、`known_at`、分页 | `fact` 对应的数据集，`filter.as_of` |
| `get_security_history` | `securities`、`fact`、`start_date`、`end_date` | `known_at`、分页 | 见 §2.1，**没有对应的 `query_data` 请求** |
| `get_security_events` | `securities`、`event`、`start_date`、`end_date` | 分页 | `market.capital_events` / `market.adjustment_events` |

语义与 [mcp-tools](../mcp-tools.md) 一致，此处只记两个容易被问到的差别：

- `get_security_state` 只返回 `as_of` 当天生效的记录（区间结束日不含），不传 `as_of` 取最近交易日；`get_security_history` 返回窗口内的全部区间，没有 `as_of`。
- `get_security_events` 没有 `known_at`，也没有 `as_of`：这两个数据集没有可用的知晓时间。

### 2.1 `get_security_history`

```json
{"securities":["sz.002731"],"fact":"security_names|risk_warnings|suspensions|industry",
 "start_date":"2020-01-01","end_date":"2026-11-05","known_at?":"2026-10-01","page_size?":100,"cursor?":""}
```

- `fact` 比 `get_security_state` 少 `security_master`：主表每个证券只有生命周期记录，不是状态序列。
- 返回与 `[start_date, end_date]` 有重叠的全部区间，按底层读取顺序（实体、起始日）排列。重叠测试与 `query_data` 的窗口测试相同：区间 `[from, to)`，`to` 为空表示未结束；`from <= end_date` 且（`to` 为空或 `to > start_date`）。`suspensions` 的区间两端都含（`start_day <= end_date` 且 `end_day >= start_date`），与 `query_data` 一致。
- 不应用 `as_of`：一个窗口内的多段区间（如多段 ST）全部返回，窗口外的不返回。
- `known_at` 照旧先应用（信息集先于记录集）：晚于 `known_at` 才知道的行被排除，`known_at` 为空的行也被排除；被排除的行数在 `temporal.unknown_known_at_rows` 里。`suspensions` 里推导出的 `unresolved_gap` 行 `known_at` 为空，传 `known_at` 时全部被排除，行为与 `query_data` 一致。
- 响应的 `temporal` 没有 `as_of` 键（没有记录集日期）；`known_at` 照常给出。`datasets_without_known_at` 的规则不变。
- `industry`：返回所选标的在窗口内重叠的全部已存区间，只有 `assigned` 行。不会生成 `unknown` 行，因为没有被问到的单一日期；窗口早于首个快照时返回 0 行，`limitations` 说明区间从首个快照起，0 行不等于没有行业。
- 沪市标的查 `security_names` 时，`limitations` 追加“name history covers SZ only (D-61)…”，与 `get_security_state` 相同。
- 游标与具体请求绑定：游标是对序列化请求的键，换窗口、换 `fact`、换成 `get_security_state` 的请求都返回 `invalid_arguments`（cursor conditions changed）。

错误：`invalid_arguments`（`fact` 不在枚举内、缺少日期、`start_date > end_date`、`securities` 为空或超过 100、带 `as_of` 或其他多余字段、游标不匹配或过期）。`forbidden`、`integrity_error`、`storage_error` 与其他读取工具相同。

### 2.2 读取路径的改动

只有 `get_security_history` 走新模式：内部 `Filter::History { start_date, end_date }`。它不可反序列化、不进 schema，`query_data` 不能构造它（`filter.kind=history` 返回 `invalid_arguments`）。

- `TemporalPlan.as_of`、`TemporalReport.as_of`、`TemporalBlock.as_of` 变为 `Option<NaiveDate>`；`None` 只在 `Filter::History` 下出现。
- `sqlite_supply/m2.rs`：`interval_hits` 与 `suspension_hits` 的 `as_of` 参数改为 `Option`；`None` 时只做窗口重叠测试。`query_data` 总是传 `Some`，计算与改动前相同。没有历史模式的三个数据集（`security_master`、`capital_events`、`price_limit_rules`）通过 `point_in_time()` 取 `as_of`，没有值时返回错误，不猜。
- `research/industry.rs`：新增 `history()`，行业区间有自己的读取路径；`sqlite_supply/industry.rs` 在 `as_of` 为 `None` 时走它，`Some` 时的代码路径不变。
- `QueryPlan::new` 只让 `Filter::History` 配 `security_names`、`risk_warnings`、`suspensions`、`industry_assignments` 和非 `universe` 的选择；其余组合返回 `dataset/filter range mismatch`。

## 3. `tools/list` 的 schema 变化

读工具的输入 schema 去掉根上的 `$schema` 和 `title`（只去根；`$defs` 与字段里的 `title` 不动）。MCP 规定没有 `$schema` 时按 JSON Schema 2020-12 解释，schemars 写的正是 2020-12，所以语义不变。实现在 `registry.rs` 的 `entry()`，与去掉 `additionalProperties` 同一个分支，因此写工具（`start_collection` 等保留 `additionalProperties:false` 的那组）不受影响。每个读工具约省 80 B。

| 入口 | 工具数 | 改动前 | 改动后 | 预算 |
|---|---|---|---|---|
| `research` | 14 → 15 | 18,437 B | **18,094 B** | 18,600 B |
| `ops` | 16 | 30,748 B | **29,872 B** | 30,800 B |
| `maint` | 9 | 13,523 B | **13,195 B** | 14,500 B |

测量方式：`tests/mcp/v8_tool_surface.rs` 的 `tools_list_stays_within_entry_budgets_without_output_schema`，`serde_json::to_vec(tools)`，代码版本为本 PR。预算上限没有改。`ops` 的 `plan_market_update`、`run_market_update`、`start_collection`、`refresh_exchange_sources`、`update_data`，和 `maint` 的 `backfill_data` 等写工具除去根上的两项外与改动前相同。

`research` 紧凑目录（`get_data_capabilities` 不带参数）为 2,559 B（15 个工具），测试上限由 2,500 B 改为 2,600 B；这个上限不是 SLO 门禁。

10 个新工具的 `tools/list` 条目（名称 + 描述 + 输入 schema，字节）：

| 工具 | 字节 |
|---|---|
| `find_securities` | 494 |
| `get_daily_bars` | 785 |
| `get_yearly_bars` | 807 |
| `get_index_bars` | 674 |
| `get_market_snapshot` | 739 |
| `get_security_state` | 755 |
| `get_security_history` | 824 |
| `get_security_events` | 656 |
| `get_trading_calendar` | 377 |
| `get_price_limit_rules` | 350 |
| **合计** | **6,461** |

每个工具描述不超过 240 B。

## 4. `query_data` 不变的证据

- `src/data_supply/mcp/query_data_golden.json`：在改动前的代码上记录 23 个 `query_data` 请求（`security_names`、`risk_warnings`、`suspensions`，长窗口内 `as_of` 在区间内、在两段之间、在开区间内、在窗口外、恰在区间结束日，单日窗口，不传 `as_of`，以及两个 `known_at`），改动后逐项比较，完全相同（去掉每次调用都不同的 `snapshot_id`、`snapshot_time`、`next_cursor` 和写入时刻 `published_at`）。
- `src/research/industry/industry_query_golden.txt`：`market.industry_assignments` 在 4 个标的 × 5 个 `as_of` 上的行与 `temporal`，同样在改动前记录。
- `m2.rs` 的单元测试保留原有的 `as_of` 半开区间断言，增加 `as_of: None` 的窗口重叠断言。

## 5. 未验证

线上真实数据未测；本代码面未部署，`tools/list` 字节为本地实测，线上以部署后的读回为准。
