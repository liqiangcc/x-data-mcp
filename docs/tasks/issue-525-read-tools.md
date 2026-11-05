# 读取工具拆分[设计](https://github.com/liqiangcc/x-data-mcp/issues/525)（owner 2026-11-05 已批准）

日期：2026-11-05。代码版本：main `44b9320`，线上 preview.45 / `generic-data-v10` / schema 27（vm2-126）。
验证原型在仓库外：工具定义和到 `query_data` 的映射，在 vm2 上跑过 19 个示例调用。

## 1. 结论

- `query_data` 保留。新增 **10 个语义明确的只读工具**，放在 `research` 入口。其中 9 个是薄封装，不新增 SQL；1 个（`get_security_history`）需要新的读取模式。
- 拆分规则：**一个工具回答一类问题；参数不同就拆；同一参数在任何情况下含义相同。**
- 日线和年线分成两个工具（`get_daily_bars`、`get_yearly_bars`），指数单独一个（`get_index_bars`）。
- 全市场筛选工具 `screen_market` 不在本批，等 XDM-14 的测量（#295）。

## 2. 现状（实测与代码核对）

| 事实 | 依据 |
|---|---|
| `query_data` 定义 4,417 B，5 个必填字段；`selection` 5 种、`filter` 4 种、`version` 3 种 | 线上 `tools/list` |
| 13 个数据集；合法组合只写在契约矩阵和归一化代码里，schema 表达不出来 | v8 §4.1，`wire/normalize.rs` |
| 省略 `contract_version` 时按 **v1** 处理，`fields` 等功能随之报错 | `normalize.rs` `contract_version()` |
| `market.daily` 查指数不报错，只返回 `absent` 和 0 行，要用 `market.index` | 实测 `sh.000300` |
| 批量上限 1..100 只；截面最多 5 个交易日 | `query.rs`；实测 |
| 事实类数据的 `as_of` 默认最近交易日，只返回**当天生效**的记录，且区间结束日当天已不生效 | `m2.rs` `interval_hits`；实测 `sz.002731` |
| **查不到区间历史**：比如"这只股票什么时候被 ST 过"，现在只能逐日试 `as_of` | 同上 |
| 名称历史只有深市（7,346 行）；沪市查询返回 0 行且版本 `present`，看起来像没有名称 | 实测 |
| ETF 查不到名称：`securities.catalog` 只有现行 A 股，名称历史里 ETF 为 0 行 | 实测 |
| `research` 工具列表 12,021 B，CI 预算 12,200 B；工具描述上限 240 B；不输出 `outputSchema` | `tests/mcp/v8_tool_surface.rs` |

pi 查 2 只标的的最新两天行情用了 5 轮。提示里要求它先弄清参数，所以这个数字偏高，只作现象，不作基线。

## 3. 设计规则

1. **工具名说明问题：** 动词加对象。
2. **参数不同就拆：** 不用 `kind` 区分不同用法。允许枚举（`fact`、`event`），前提是每个取值的参数完全相同。
3. **三种时间分开：**
   - `start_date`/`end_date`：查询的日期范围。
   - `as_of`：查这一天生效的记录。
   - `known_at`：只用这一天已知的信息，防止前视。
4. **去掉内部字段：** 不暴露 `contract_version`、`version`、`selection`、`filter`。新工具内部固定用 v2 和 `latest_per_entity`。
5. **一次调用只对应一次查询：** 响应继续带 `snapshot_id`，分页语义不变；一次调用跨多个数据集时拿不到同一快照，所以 `fact` 是单选。
6. **不放松现有规则：** ETF 必须显式 `adjustment=raw`（D-70 Q3）等报错照旧。
7. **错误给下一步：** 比如指数传给 `get_daily_bars` 时，报错并指向 `get_index_bars`，不再静默返回 `absent`。

## 4. 工具定义

通用：标的写 `sh.600000`；日期 `YYYY-MM-DD`；`page_size` 1..1000，默认 100；`cursor` 翻页。响应沿用 v2 信封（`columns`、`entities`、`versions`、`metadata`、`next_cursor`、`temporal`）。

### 4.1 `find_securities`
```json
{"query?":"名称片段","code?":"600519","industry_code?":"X2102","page_size?":100,"cursor?":""}
```
映射：`securities.catalog` + `catalog/current_sh_sz_a` + `filter.kind=catalog`。
范围：现行 A 股。ETF 和退市标的不在内（见 §6）。

### 4.2 `get_daily_bars`
```json
{"securities":["sh.600000"],"start_date":"2026-11-04","end_date":"2026-11-05",
 "adjustment?":"qfq|raw","fields?":["open","high","low","close","volume","amount","no_trade"],
 "page_size?":100,"cursor?":""}
```
映射：`market.daily` + `single/batch` + `filter.kind=daily`。
语义：
- 1..100 只。A 股默认 `qfq`。
- ETF 不传 `adjustment` 报 `adjustment_required_for_etf`；A 股和 ETF 混查时传 `raw`，全部返回不复权。
- `no_trade` 只有 ETF 有。
- **新增：** 传入指数时报 `invalid_arguments` 并指向 `get_index_bars`。

### 4.3 `get_yearly_bars`
```json
{"securities":["sh.600000"],"start_year":2024,"end_year":2026,
 "fields?":["open","high","low","close","volume","amount","first_trading_date","last_trading_date","year_unfinished"],
 "page_size?":100,"cursor?":""}
```
映射：`market.yearly` + `single/batch` + `filter.kind=yearly`。
语义：
- 只有 A 股，由前复权日线按自然年派生，没有 `adjustment` 参数。
- 当年 `year_unfinished=true`，收盘价是最新交易日的。
- ETF 和指数返回 `versions[].status=absent`、0 行（D-70 Q6），不影响同批 A 股。

### 4.4 `get_index_bars`
```json
{"indices":["sh.000300"],"start_date":"2026-11-04","end_date":"2026-11-05",
 "fields?":["open","high","low","close","volume","amount"],"page_size?":100,"cursor?":""}
```
映射：`market.index` + `single/batch` + `filter.kind=daily`。
`indices` 是枚举，只能选登记的 9 条：`sh.000001`、`sh.000016`、`sh.000300`、`sh.000688`、`sh.000852`、`sh.000905`、`sh.000906`、`sz.399001`、`sz.399006`。

### 4.5 `get_market_snapshot`
```json
{"start_date":"2026-11-05","end_date":"2026-11-05",
 "universe?":"a_current|a_listed_ever|etf_current|etf_listed_ever",
 "adjustment?":"qfq|raw","fields?":["close"],"page_size?":100,"cursor?":""}
```
映射：`market.daily` + `selection.kind=universe`（`a_current` 不带 `scope`，其余映射到 `sh_sz_*` scope）+ `filter.kind=cross_section`。
语义：最多 5 个交易日；ETF 范围要传 `adjustment=raw`。

### 4.6 `get_security_state`：当时是什么状态
```json
{"securities":["sz.002731"],"fact":"security_master|security_names|risk_warnings|suspensions|industry",
 "as_of?":"2026-07-07","known_at?":"2026-10-01","page_size?":100,"cursor?":""}
```
映射：`fact` 对应的数据集 + `single/batch` + `filter={kind:daily,start_date:as_of,end_date:as_of,as_of}` + 顶层 `known_at`。
语义：
- 只返回 `as_of` 当天生效的记录。不传 `as_of` 时取最近交易日。
- 不传 `known_at` 表示用现在已知的全部信息。回测应该传。
- **新增：** 沪市查 `security_names` 时在 `limitations` 写明"名称历史只覆盖深市"，不让 0 行看起来像没有名称。

### 4.7 `get_security_history`：这段时间有过哪些状态（新读取模式）
```json
{"securities":["sz.002731"],"fact":"security_names|risk_warnings|suspensions|industry",
 "start_date":"2020-01-01","end_date":"2026-11-05","known_at?":"","page_size?":100,"cursor?":""}
```
语义：返回与日期范围有重叠的全部区间。
实现：不是薄封装。读取路径要增加一种不应用 `as_of` 的模式（`m2.rs` 的行准入，`TemporalPlan.as_of` 变为可选），`known_at` 照旧先应用。单独一个 PR。

### 4.8 `get_security_events`：这段时间发生了什么
```json
{"securities":["sh.600000"],"event":"capital|adjustment",
 "start_date":"2025-01-01","end_date":"2026-11-05","page_size?":100,"cursor?":""}
```
映射：`market.capital_events` / `market.adjustment_events` + `single/batch` + `filter.kind=daily`。
语义：这两个数据集没有可用的知晓时间，所以**没有 `known_at` 参数**。

### 4.9 `get_trading_calendar`
```json
{"start_date":"2026-11-02","end_date":"2026-11-05"}
```
映射：`market.calendar` + `selection.kind=market`。

### 4.10 `get_price_limit_rules`
```json
{"as_of?":"2026-11-05","page_size?":100,"cursor?":""}
```
映射：`market.price_limit_rules` + `selection.kind=market` + `start_date=end_date=as_of`。

### 4.11 保留的工具

`list_catalog_facets`、`get_data_availability`、`get_data_capabilities`、`list_data_versions`、`query_data`。需要钉版本复现时用 `query_data` 或研究导出。

## 5. 为什么这样拆

| 决定 | 理由 |
|---|---|
| 日线、年线、指数分开 | 参数不同（日期 / 年份；有无 `adjustment`），标的范围也不同 |
| `state`、`history`、`events` 分开 | 问题不同，时间参数也不同：`as_of` / 日期范围 + `known_at` / 只有日期范围 |
| `fact` 用枚举，不拆成 5 个工具 | 参数完全相同，拆开只增加工具数 |
| 日历和涨跌停规则分开 | 参数不同，schema 都很小 |

## 6. 已知缺口

| 缺口 | 影响 | 建议 |
|---|---|---|
| ETF 名称不可查 | "沪深 300 ETF 的代码是什么"查不到，只能已知代码 | 名单来源里有简称，但没有进读模型。另开数据任务，不放进本次 |
| 沪市名称历史 | D-61：沪市和北交所 `NOT_AVAILABLE`，只有深市有 | 用 `limitations` 写明，不改数据范围 |
| `as_of` 默认值用 `Local::now()` | vm2 时区是上海，host 是 UTC，默认值可能差一天 | 已有待办：`query.rs:248` 改成 `shanghai_today`，放进 PR-1 |
| `versions[]` 重复长文本 | 混查 2 行响应 4.5 KB，其中多为 `adjustment.reason` | 不在本次，基线测完再定 |
| 指数走 `query_data` 仍静默 `absent` | 老接口行为不变 | 只在新工具里报错 |

## 7. 实现方式

- `wire.rs` 为每个工具定义窄输入类型；`registry.rs` 注册，并加入 `RESEARCH_TOOLS`。
- 处理函数把窄输入转成现有 `Query`（`contract_version=V2`），调用 `queries.query_v2`。**不复制查询、校验和完整性逻辑。**
- `ops`、`maint` 入口不变，日更不用新工具。
- 字段白名单继续用 `columnar.rs`。

### schema 大小（原型实测）

| 工具 | 字节 |
|---|---|
| `find_securities` | 485 |
| `get_daily_bars` | 759 |
| `get_yearly_bars` | 762 |
| `get_index_bars` | 663 |
| `get_market_snapshot` | 720 |
| `get_security_state` | 767 |
| `get_security_history` | 779 |
| `get_security_events` | 669 |
| `get_trading_calendar` | 282 |
| `get_price_limit_rules` | 359 |
| **合计** | **6,245** |

`research` 工具列表从 12,021 B 增加到约 **18,300 B**，超过 12,200 B 的预算，**需要 owner 批准上调**。去掉 `query_data` 约 13,900 B，但要等评测通过（见问题 2）。

示例调用的入参从 198–285 B 降到 15–128 B。

## 8. 契约与部署

- 新增契约 `generic-data-v11`，加工具不改 schema（仍是 27），也没有迁移。
- 部署：发 preview.46，用 `switch_binary.sh` 只换二进制。vm2 有短暂中断，要避开日更。
- `capability_revision` 升到 v11 时，日更 Skill 的钉值要在部署同一提交里一起改，否则日更报 `SKILL_CONTRACT_MISMATCH`。
- host 的 preview.43 不变；tunnel 迁到 vm2 后客户端才能用到新工具。

## 9. 验收：同一组问题前后对比

先在**现有接口**上跑基线，再在新工具上跑，用同一模型、同一提示。

问题集（10 题）：
1. 贵州茅台的代码和行业是什么？
2. `sh.600000` 最近 5 个交易日的日线
3. `sh.510300`（ETF）11 月的日线
4. `sh.600000` 2024–2026 年的年线
5. 沪深 300 指数 11 月的日线
6. 2026-11-05 全市场收盘价的前 10 行
7. `sz.002731` 2026-07-07 是否处于风险警示？
8. 只用 2026-10-01 已知的信息，上一题怎么回答？
9. `sz.002731` 历史上有过哪些风险警示？（基线上预期答不好）
10. `sh.600000` 2024 年以来的股本变动

记录：每题的轮数、总 token、错误调用次数、答案是否正确。

通过标准（建议）：
- 全部答案正确，错误调用数不高于基线。
- 单一问题平均轮数不超过 2。
- 总 token 比基线至少降 30%。

没达到就调整工具定义，不直接上线。

## 10. 分 PR

| PR | 内容 | 依赖 |
|---|---|---|
| 准备 | 基线评测（不改代码）+ XDM-14 耗时分解 | 11-09 日更连续 3 次正常 |
| PR-1 | 9 个薄封装、错误提示、`limitations`、`shanghai_today`、CI 预算、v11 契约草案 | owner 批准本设计 |
| PR-2 | `get_security_history` 的读取模式 | PR-1 |
| PR-3 | 发布 preview.46、部署、Skill 钉值、复跑评测 | PR-1、PR-2 |
| 后续 | `screen_market` | XDM-14 的结论（#295） |

## 11. Owner 裁决（2026-11-05，"按建议，提前"，D-75）

1. 批准这 10 个工具的定义和命名。
2. `research` 的 `tools/list` 预算上调到 18,600 B（随 PR-1 改 CI）。评测通过后，再决定是否把 `query_data` 移出 `research`（可回到约 14,000 B）。x 接入前要确认它用的入口。
3. `get_security_history` 放进第一批。
4. ETF 名称搜索另开数据任务，不放进本次。
5. 本工作排在 XDM-8（存储增长）之前。基线评测和 XDM-14 在 2026-11-09 日更连续 3 次正常后开始。

## 12. 未验证

- 只验证了 9 个薄封装的映射，用的是 Python 原型调现有 `query_data`，尚未写 Rust。
- 新增的错误提示和 `limitations`、`get_security_history` 都还没有实现。
- schema 字节含 `additionalProperties:false`，服务端会去掉，实际会略小。
- 工具名和合并方式能否让 agent 少走弯路，要等 §9 的评测。
