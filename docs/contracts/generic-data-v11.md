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
| `query_data` 的输入与输出 | | 工具和 `get_security_history` 的读取模式不改变它（见 §4）；但同一修订里有别的 v2 变化，见 §1.1 |
| `tools/list` 输入 schema | 根上有 `$schema`、`title` | 读 schema 去掉这两项（见 §3） |
| `market.adjustment_events` 的 v2 列 | `category, c1..c4`（没有日期） | **追加 `effective_day`**（见 §6） |
| `market.security_master` 的 v2 缺省列 | 不含 `effective_from` | **追加 `effective_from`**（见 §6） |
| `find_securities` 的输入 | `query`、`code`、`industry_code`、分页 | **追加可选 `kind`**（`stock` 缺省，`etf`），缺省响应不变（见 §7） |

加工具不改 schema，也不改 `query_data` 的 wire 形状。`TemporalBlock.as_of` 在内部变为可选，但 `query_data` 永远写它，已发布的 schema 保持 `{"type":"string","format":"date"}`。

### 1.1 同一修订里的其他变化（相对已部署的 v10）

下面这些不是读取工具带来的，但会随 v11 一起上线。客户端升级时要看这一节。

| 变化 | 影响 | 出处 |
|---|---|---|
| v2 `universe` 读的分页形状：`entities`、`versions`、`columns` 只含本页行涉及的实体，不再每页带全量 | 大响应变小；按页合并时要按实体拼。v1 不变 | #541，[v8 §4](generic-data-v8.md) |
| 新错误码 `memory_budget_exceeded`（`isError`）：服务接近自身 cgroup 的 `memory.high` 时拒绝读取 | 可以重试或缩小查询；不是“没有数据”。游标不被消耗 | #547，[v8 §4](generic-data-v8.md) |
| 游标池变成进程级：总 48 MiB、单连接 24 MiB 且 32 条，LRU 淘汰 | 约 5 个以上全市场读者同时翻页时，别的连接可能收到 `invalid/expired cursor`，要重新查询 | #547 |
| `market.risk_warnings` 的 `coverage_reason` 文案，不再有过时的 `pending`/`#308` 文字 | 只是文本 | #539、#548 |
| ETF 的 `security_master.effective_from`（D-80）：有 `listing_date` 的 class=2 行取上市日，`known_at` 仍是首次观察日；没有上市日的行不变。A 股行不变。schema 与列不变；主表 `algorithm_version` 升到 `security-master-assembly-v2`，导出 `producer` 随之变化 | `market.security_master`、`universe_intervals` 里 ETF 行的 `effective_from`/`member_from` 变早；`snapshot_id` 随之变；`known_at` 早于首次观察日的查询仍排除并计数。v10 是已部署快照，不改：v10 §1.1「这两个字段是身份观测/退市上界」按旧取值写 | [#509](https://github.com/liqiangcc/x-data-mcp/issues/509)，[M3-2 设计](../tasks/issue-509-m3-2-etf.md) §3.3 |

x 的 `KlineReader` 固定 `contract_version=v1` + `selection.kind=single`，不受前两行分页形状影响；错误以 `isError` 到达（2026-11-06 读 `liqiangcc/x` 确认）。

## 2. 10 个新工具

均在 `research` 入口。每个工具把窄输入转成一个现有的 v2 `Query`（`latest_per_entity`），由同一条读路径做校验、完整性检查和列式投影。响应是 v2 信封（`layout`、`columns`、`versions`、`metadata`、`limitations`、`page_complete`、`next_cursor`，事实类数据集带 `temporal`）。输入类型拒绝多余字段（`invalid_arguments`），不暴露 `contract_version`、`version`、`selection`、`filter`。标的写作 `sh.600000`，日期 `YYYY-MM-DD`（也接受 `YYYYMMDD`）。分页参数 `page_size` 1..1000（默认 100）和 `cursor` 对所有带分页的工具相同。

| 工具 | 必填 | 可选 | 对应的 `query_data` |
|---|---|---|---|
| `find_securities` | 无 | `query`、`code`、`industry_code`、`kind`（`stock`/`etf`）、分页 | `kind=stock`（缺省）：`securities.catalog`；`kind=etf`：见 §7，**没有对应的 `query_data` 请求** |
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
| `research` | 14 → 15 | 18,437 B | **18,094 B**（PR-2）→ **18,197 B**（加 `find_securities.kind`） | 18,600 B |
| `ops` | 16 | 30,748 B | **29,872 B** | 30,800 B |
| `maint` | 9 | 13,523 B | **13,195 B** | 14,500 B |

测量方式：`tests/mcp/v8_tool_surface.rs` 的 `tools_list_stays_within_entry_budgets_without_output_schema`，`serde_json::to_vec(tools)`，代码版本为本 PR。预算上限没有改。`ops` 的 `plan_market_update`、`run_market_update`、`start_collection`、`refresh_exchange_sources`、`update_data`，和 `maint` 的 `backfill_data` 等写工具除去根上的两项外与改动前相同。

`research` 紧凑目录（`get_data_capabilities` 不带参数）为 2,559 B（15 个工具），测试上限由 2,500 B 改为 2,600 B；这个上限不是 SLO 门禁。加 `kind` 后紧凑目录仍是 2,559 B。

10 个新工具的 `tools/list` 条目（名称 + 描述 + 输入 schema，字节）：

| 工具 | 字节 |
|---|---|
| `find_securities` | 494（加 `kind` 前）→ **597**（加 `kind` 后） |
| `get_daily_bars` | 785 |
| `get_yearly_bars` | 807 |
| `get_index_bars` | 674 |
| `get_market_snapshot` | 739 |
| `get_security_state` | 755 |
| `get_security_history` | 824 |
| `get_security_events` | 656 |
| `get_trading_calendar` | 377 |
| `get_price_limit_rules` | 350 |
| **合计** | **6,461**（加 `kind` 前）→ **6,564** |

每个工具描述不超过 240 B。`find_securities` 的描述为 193 B（测试按 240 B 检查）。

## 4. `query_data` 不变的证据

- `src/data_supply/mcp/query_data_golden.json`：在改动前的代码上记录 23 个 `query_data` 请求（`security_names`、`risk_warnings`、`suspensions`，长窗口内 `as_of` 在区间内、在两段之间、在开区间内、在窗口外、恰在区间结束日，单日窗口，不传 `as_of`，以及两个 `known_at`），改动后逐项比较，完全相同（去掉每次调用都不同的 `snapshot_id`、`snapshot_time`、`next_cursor` 和写入时刻 `published_at`）。
- `src/research/industry/industry_query_golden.txt`：`market.industry_assignments` 在 4 个标的 × 5 个 `as_of` 上的行与 `temporal`，同样在改动前记录。
- `m2.rs` 的单元测试保留原有的 `as_of` 半开区间断言，增加 `as_of: None` 的窗口重叠断言。

## 5. 风险警示的时点规则（D-81 R1、R4、R5）

适用于 `market.risk_warnings`（`query_data`、`get_security_state`、`get_security_history` 读到的 `risk_warning_intervals`）。**compact schema 不变**，`tools/list` 与响应形状不变。`ALGORITHM_VERSION` 在代码面升级，影响见下。

### 5.1 R1：`known_at` 是日期，`known_at = D` 表示 D 日收盘后才保证已知

过滤用 `known_at ≤ cutoff`（`temporal.rs`），两端都含。因此：

| 决策时点 | 该用的 `known_at` 截止 | 理由 |
|---|---|---|
| 收盘后（盘后选股、复盘） | `D` | D 日公告在 D 日收盘后已知 |
| 开盘或盘中（按 D 日开盘价成交、盘中信号） | `≤ D-1`（上一交易日） | 公告可能在 D 日盘中才发布，D 日的 `known_at` 不保证开盘前已知 |

`known_at` 只有日期，没有盘中时刻（D-81 R3 留给二期），所以同一天内无法再细分。这是引擎调用方的约定，不是过滤规则的改动：同一行在截止 `D-1` 时被排除，在截止 `D` 时可见。测试：`research::read_model_tests::a_risk_interval_known_on_d_is_visible_at_d_and_hidden_at_the_previous_day`。

### 5.2 R4：一期不支持按更正重放

库里是更正后的最新值，`known_at` 是首个包含该行的快照日；更正前的值取不回来，所以“更正发生之前的世界是什么样”不可重放。

- **沪市**：同一 `(公告 id, 事件)`（事件 = 动作 + 状态）在日更实际读取的页里出现两次、生效日或公告日不同，`derived:risk_warnings` 以 `source_invalid` 失败，错误文本带公告 id、代码、两组日期和 `D-81 R4`。以前保留第一次的日期、丢弃更正后的日期。完全相同的重复（两个标题过滤器都命中同一公告）照旧只读一次。
- **重放**：`replay --scope risk_warnings` 与日更使用同一条规则，但只对日更会读的页生效。同一窗口、同一过滤器下，日更只读最新 `END_DATE` 的那一页集；重放还持有更早 `END_DATE` 的旧页（`superseded_bulletins`），旧页只补充新页没有的事件，与日期不一致不报错。否则重放会在日更成功的数据上失败。
- **深市**：简称表被更正时仍走 `BrokenChain` fail-closed，不变。

### 5.3 R5（修订 D-68 第 4 项）：同日冲突只影响该代码

以前同一代码同日既有实施又有撤销（沪市 `SameDayActionConflict`），或同日有两条简称变更（深市 `SameDayConflict`），整个数据集失败。现在：

- 冲突代码在冲突日写**一行** `marker_only_unresolved`（`effective_from` = 冲突日，`effective_to` = 次日；表的 CHECK 已含这个状态）。冲突日之前打开的区间在冲突日结束，不跨过冲突日延伸。冲突日之后到下一个事件之前**没有行**，读作 `unknown`。其余代码照常发布。
- `raw_status_code` 为 `unresolved:same_day_action_conflict`（沪）或 `unresolved:same_day_name_changes`（深）；`source_record_id` 是涉事记录 id 按字典序用逗号连接；`source_document_sha256` 取其中字典序最小的一条记录；`known_at` 取涉事行中最晚的一个，任何一行为空则为空。这些值与行的输入顺序无关，所以重放与日更逐行相同。
- 数据集证据（item 的 `source.same_day_conflicts` 与登记证据 `same_day_conflicts`）写 `count`、`codes` 和前 20 个 `(exchange, code, day, kind, source_record_id)` 样本。
- 不变的 fail-closed：`BrokenChain`、同日两个不同状态的实施（`SameDayStatusConflict`）、无状态的实施事件、区间重叠、重复主键。深市同日冲突只在这组变更与链仍能连接时隔离；连接不上的仍是 `BrokenChain`。
- 读路径：`marker_only_unresolved` 不是 `normal`。`member_state` 原样返回该状态；`price_limit` 把它与 `unknown` 同样处理（`StUnknown`，不写 `status_class`，只有所有状态共用一个涨跌幅档时才给出已知的涨跌幅并标 `status_unknown_band_invariant`）；`tradability` 不读风险状态。

### 5.4 版本号与部署后的影响

- `risk_warnings` 的 `ALGORITHM_VERSION`：`risk-warnings-v1` → `risk-warnings-v2`。它写入 `derived_dataset_versions.algorithm_version`、登记证据和导出 `producer.algorithms`（不属于导出快照的身份）。没有代码读取该值做兼容判断。
- 内容摘要（`dataset_digest`，前缀仍是 `risk-warnings-v1`）、`digest_format`、`version_id` 不变。没有同日冲突的数据集重新组装后行逐字节相同，`version_id` 不变，只有 `algorithm_version` 一列在下一次 `derived:risk_warnings` 写入时更新为 `v2`。
- 解析器版本（item 证据的 `parser_version`）：`risk-warnings-v5` → `risk-warnings-v6`，只影响证据标注。
- 预期的数据影响：D-68 之后 preview.42 全量页集上 `SameDay*` 冲突为 0，所以线上现有 884 行沪市记录预计不变；这一点没有在线上核对（本任务不访问线上）。部署后第一次日更若出现冲突，会多出 `marker_only_unresolved` 行并在证据里列出，而不是让 item 失败。
- 无 schema 变化，无 `tools/list` 变化，无需重新 `init`。

## 6. 未验证

线上真实数据未测；本代码面未部署，`tools/list` 字节为本地实测，线上以部署后的读回为准。

## 6. `table` 布局的行键

`table` 布局没有 `dates`，所以行键必须是一列。v10 有两处丢了行键：`market.adjustment_events` 的响应没有日期，查 `sh.600000` 只能看到金额，看不出哪天除权；`market.security_master` 的缺省列不含 `effective_from`，只有显式 `fields` 才有。v11 都已补上。追加列不改行顺序、`version_id`、`snapshot_id`，v1 不变。

| dataset | 行键 | v10 | v11 |
|---|---|---|---|
| `securities.catalog` | 实体 | `entities` | 不变 |
| `market.adjustment_events` | `effective_day` | **丢失** | **第一列，缺省返回，`fields` 可选** |
| `market.calendar` | `trade_day` | 列 | 不变 |
| `market.security_master` | `effective_from` | **缺省丢失** | **缺省含** |
| `market.security_names` | `effective_from` | 列 | 不变 |
| `market.capital_events` | `effective_day` | 列 | 不变 |
| `market.risk_warnings` | `effective_from` | 列 | 不变 |
| `market.suspensions` | `start_day` | 列 | 不变 |
| `market.price_limit_rules` | `rule_id` + `effective_from` | 列 | 不变 |
| `market.industry_assignments` | `effective_from` | 列 | 不变 |

`market.adjustment_events` 的行仍按源（gbbq 解码）顺序，不是日期顺序。`basis_status` 不在读面上。

## 7. `find_securities kind=etf`（ETF 名称 PR-3，D-76 Q6 (a)，Refs #509）

只加一个可选输入 `kind`，不新增工具，所以 `capability_revision` 仍是 `generic-data-v11`（升级放到读取工具系列的最后一个 PR）。`tools/list` 的 `research` 增加 103 B（`find_securities` 494 → 597 B，`kind` 的 schema 是 `{"enum":["stock","etf"],"default":"stock"}`，工具描述改为两种范围），测量方式与 §3 相同。

```json
{"kind":"etf","query?":"沪深300ETF","code?":"510300","page_size?":100,"cursor?":""}
```

- **缺省与 `kind=stock` 不变**：不传 `kind`，或传 `kind=stock`，走原来的 `securities.catalog` 请求，`columns`、`entities`、`versions`、`metadata`、`limitations` 与加 `kind` 之前逐字相同，与 `query_data` 的等价测试照旧通过。
- **范围**：主表（`security_master`）里 `instrument_class=2` 的沪深行，现行和退市。ETF 仍不进 `securities.catalog`（D-70 Q5）。
- **搜索**：`query` 经 NFKC、去全部空白、转小写后，对**最新名称**做子串匹配（D-76 Q8）。不做拼音和模糊。`华夏300`、`广发500` 这类交易所短写法搜不到（短写法不是名称的子串，见 [etf-name-search](../tasks/etf-name-search.md) §8.1）。去空白后为空的 `query` 返回 `invalid_arguments`。`code` 为六位精确匹配，与 `query` 取交集。
- **行内容**（固定列，没有 `fields`）：

| 列 | 含义 |
|---|---|
| `name` | 最新观测到的名称；没有名称时为 `null`，不返回空串 |
| `name_basis` | `observed_from_first_snapshot`，或 `unavailable`（没有观测到名称） |
| `status` | `current`（与 `sh_sz_etf_current` 同一判断）或 `delisted` |
| `instrument_class` | 恒为 `2` |
| `listing_date`、`delisting_date` | 主表值；只知道移出名单、不知道退市日时 `delisting_date` 为 `null` |
| `name_observed_from` | 最新名称首次被名单快照看到的日期（`basis=fetch_time`），不是交易所的改名日；无名称时为 `null` |

- **`limitations`**（每次回答都带，结果为 0 行时也带）：名称历史从首次观测日开始，更早的改名不可得；短写法搜不到；退市 ETF 可能没有名称（`name: null`、`name_basis: unavailable`），用代码找；`delisted` 的含义。主表或名称未登记时另有 `has no registered version`，0 行表示未知，不表示没有。
- **退市 ETF 的名称**：本批没有来源（PR-4）。2026-11-03 之前退市的存量按代码可以找到，`name` 为 `null`；按名称搜不到它们。
- **错误**（`invalid_arguments`）：`kind` 不是 `stock`/`etf`（含大小写不同、`all`、空串、非字符串），`kind=etf` 带 `industry_code`，空白 `query`，多余字段（`as_of`、`fields`、`include_delisted` 等），游标与请求不匹配。
- **读路径**：内部过滤 `Filter::FindEtf { code, name_query }` 与 `Filter::History` 同一种做法，`serde(skip_deserializing)` 且 `schemars(skip)`，`query_data` 构造不了（`filter.kind=find_etf` 返回 `invalid_arguments`），也不改 `query_data` 的 schema。它只与 `market.security_names` + `selection.kind=market` 配对，其余组合在 `QueryPlan::new` 里报 `dataset/filter range mismatch`。搜索归一化是 `security_master::names::search_key`，增加了依赖 `unicode-normalization`（0.1，纯 Rust，带 `tinyvec`）。
- **不变**：`market.security_names` 与 `get_security_state`/`get_security_history` 对 ETF 仍不返回 `exchange_snapshot` 行；`security_names.parquet` 不含 ETF（Q7）。
- **未验证**：线上真实名单未测（代码面，未部署）；约 1.7 千行 ETF 的内存搜索耗时没有单独测量，每次调用读一遍主表和名称表（与 `market.security_master` 的读法相同）。
