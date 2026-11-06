# 可交易性、涨跌停、市值的只读工具（[#433](https://github.com/liqiangcc/x-data-mcp/issues/433) 第 5 项）

日期：2026-11-06。代码版本：main `5e4c4dd`（含读取工具 PR-1、PR-1b）。线上 preview.45 / `generic-data-v10` / schema 27（vm2-126），新工具未部署。
依据：owner 2026-11-05 批准 O6（[D-78](../decisions.md)）：做成 `research` 入口的只读工具，薄封装 `research::tradability` 等，排在 [#525](issue-525-read-tools.md) PR-2 之后，不做 `price_limits.parquet` 导出。

本文只是设计，不改代码，不访问线上。文中「审计 T3」等指 [严谨回测验收审计](rigorous-backtest-audit.md) §6 的建议任务编号。除特别标注，事实来自读代码和文档。标「推断」的是从代码或数据量推出的结论，没有实测。标「临时测试」的是在本分支用现有夹具（`src/research/read_model_tests.rs`）写了几个一次性测试读出的行为，测试没有提交。

## 0. 结论

- **推荐两个工具**，都放进 `research`：`get_tradability`（可交易状态加当日涨跌停价）和 `get_market_cap`（市值，`scope` 必填）。三个工具、两个工具、一个工具的比较见 §2.1。
- **输入**：`securities`（1..100 只 A 股）加 `as_of`（缺省取最近交易日）。**不接受 `known_at`**：这些函数用到的输入里，没有一项对 A 股有可用的知晓时间（§4）。**不分页**，**不支持区间**。
- **一个问题一次调用**：「这只票那天能不能买」用 1 次 `get_tradability`。现在要 5 次以上 `query_data`/读取工具，还要自己重写判定顺序（§2.4）。
- **「薄封装」要先补 research 层**：读代码和临时测试发现 8 处缺口（§1.4）。其中 G2–G4 三处会把「未知」说成确定的「不可交易」（日历之后的日期、ETF、不在主表的代码），G5 会把除权日的涨跌停价当作已知。要在工具前先出一个 research 层的修复 PR。
- **批量要先改成一次加载**：现有函数每个实体都整表读 5 张表，约 9 万行（推断）。逐只调用，100 只会到十秒量级（推断）。修复 PR 里加共享加载（§5）。
- **预算超了**：`research` 现在 18,437 B，预算 18,600 B。两个工具新增约 1,225 B；加上 PR-2 的 `get_security_history`（估约 850 B）后约 20,500 B。选项和字节数在 §6，由 owner 决定。
- **主板涨跌停在读面上大多是 `unknown`**：库里不写 `normal` 状态，没有 ST 区间的日子状态未知，主板的 ST 与非 ST 幅度不同，派生返回 `unknown`（读代码得出，推断，没在线上验证）。这是 fail-closed 的真实结果，不是工具的错。想改善要另做闭世界证据（审计 T3，owner 另决）。
- 待决问题 8 个，在 §8.2。

## 1. 现状

### 1.1 三个库函数

| 函数 | 签名（简） | 输出 | 读的表 | 版本标签 |
|---|---|---|---|---|
| `research::tradability::tradability` | `(&Connection, entity: &str, Temporal) -> Result<TradabilityReport, research::Error>` | `state`（`Tradable` / `NotTradable(reason)` / `Unknown(reason)`）、`member`、`suspension`、`has_bar`、`temporal` | `security_master`、`code_migrations`、`risk_warning_intervals`（为成员判定）、`trading_days`、`suspension_intervals`、`series_bars`（不复权） | **无**（见 G7） |
| `research::price_limit::price_limit` | 同上 | `PriceLimitReport`：`state`（`Known` / `Unbounded` / `Unknown`）、`reason`、`limit_up_units`/`limit_down_units`、`prev_close_*`、`status_class`、`rule_id`、`basis_ref`、`candidates`、`rules_version`、`temporal` | 上面的成员判定表、`price_limit_rules`、`min_tick_rules`、`trading_days`、`series_bars` | `limit-v1` |
| `research::market_cap::market_cap` | `(&Connection, entity, Temporal, ShareScope, &MarketCapOptions) -> Result<MarketCap, research::Error>` | `state`（`Known` / `Unknown`）、`reason`、`value_units`（分×股）、`price_cents`、`shares`、`share_day`、`chain_break_day`、`float_reconciliation`、`known_at_coverage`、`temporal` | `capital_events`（按实体）、`series_bars`（不复权） | `market-cap-v1` |

`Temporal` 是 `{as_of, known_at: Option}`。函数不读时钟，`as_of` 必须由入口给出。三个函数目前除测试外没有调用方（审计与本次 grep 一致）。

### 1.2 能不能从读路径直接调用

能。`DataApi.queries.repository` 是 `SqliteSupply`，它的 `market.read(|c| ...)` 给出一个 `&Connection`，整个闭包在一个 SQLite 读事务里，也就是同一个快照。已有先例：`universe_members`（`src/sqlite_supply.rs`）和 `sqlite_supply/industry.rs` 都在闭包里直接调 research 函数，再把 `research::Error` 转成 `market_storage::Error`。

要注意三点：

1. 这条路径是 `query_data` 的 `QueryPlan` → `QuerySnapshot` → 列式投影。新工具不是数据集查询，没有现成的行类型，见 §2.3 的两种接法。
2. `industry.rs` 把所有 research 错误都转成 `Error::Storage`。新工具要把 `Integrity` 保留为 `integrity_error`，否则数据损坏会显示成存储故障。
3. 函数不检查输入数据集是否已登记（`derived_dataset_versions`）。`query_data` 的读面在未登记时返回 `absent` 和 0 行（`m2.rs` 文件头：未版本化的数据不对外服务）。见 G8。

### 1.3 #433 第 5 项和 AC 的要求，文字要不要改

[#433](https://github.com/liqiangcc/x-data-mcp/issues/433) 第 5 项：涨跌停、可交易性、市值只有库函数，AC-M2-6…10 的 fail-closed 行为在生产上不可观测，需要设计读面。AC 编号用 [#391](https://github.com/liqiangcc/x-data-mcp/issues/391) 正文的那套（`data-catalog.md` §3.1 另有一套同名的 AC-M2-1…5，内容不同）。

| 条目 | 要求（来自 #391 正文和设计） | 新工具怎么兑现 | 文字要改吗 |
|---|---|---|---|
| #433 第 5 项 | 「ops 入口无读面……需要设计读面（v8 数据集或导出文件）」 | D-78 已选第三条路：`research` 入口工具，不导出 | **要改**：写成「`research` 入口只读工具（D-78）」；「v8 数据集或导出文件」两个备选已被否决 |
| AC-M2-6 | 市值：流通市值用流通 A 股；A+H 的「A 股总市值」返回 `unknown`，不用 C4 冒充 | `get_market_cap` 的 `scope` 必填，`a_share_total` 恒 `unknown` + `a_share_total_not_admitted` | 不用改 |
| AC-M2-7 | ST 状态缺失的日期请求涨跌停，返回 `unknown`（或显式 `ambiguous` + 候选集），不按 10% 算 | 主板：`unknown` + `st_unknown` + `limit_candidates`。创业板 2020-08-24 起、科创板：规则行与状态无关，返回 `known`，`status` 仍是 `unknown` | **要改**：现文字对这两类板块不成立。设计 §8.5 已写这个例外（`status_class='all'`），AC 漏了。改为「规则随状态变化的板块/时段返回 `unknown`；规则与状态无关的返回确定值，且状态仍标 `unknown`」 |
| AC-M2-8 | `sz000631` 2006-04-28…2009-06-04 请求可交易性：`tradable=unknown` + `unresolved_gap`，不返回 `suspended` | `state=unknown`，`reason=data_gap`，`suspension` 列写 `unresolved_gap` 和区间 | **建议澄清**：`unresolved_gap` 是停牌区间的 `certainty`，可交易性的原因码是 `data_gap`（设计 §7.4）。另外 `data-catalog.md` §3.3 偏差 6 写「深市全域可交易性是 `unknown`」，而函数对有 bar 的深市日返回 `tradable`（夹具测试 `read_model_tests.rs:1012`）。应改成「深市缺 bar 的日子是 `unknown`」 |
| AC-M2-9 | 6 个板块×时段的规则来自规则表且有 `basis_ref`；`high == limit_up` 的命中率不低于基线 | 工具给 `rule_id`，`basis_ref` 用 `get_price_limit_rules` 按 `rule_id` 查（每行约 400 B，不放进每个实体行）。命中率不是读面的事 | **要拆**：「规则来自规则表」由工具兑现。「命中率」是单独的验证任务，基线在仓库外，且除权日会偏（G5） |
| AC-M2-10 | 规则变更只改数据 | 工具读规则表，响应带 `rules_version` | 不用改（`price_limit/tests.rs:688` 已覆盖） |
| AC-M2-11 | 与导出物化值逐行相等 | D-78 不导出派生值，这条没有对象 | **要改**（#433 第 6 项已列）：「同输入版本和同算法版本给出同结果」 |
| AC-M2-12 | 工具数、dataset、wire 形状不变；`as_of`/`known_at` 被拒 | 已过时：M2 数据集接受这两个参数，工具数在涨 | **要改**（#433 第 6 项已列） |

### 1.4 research 层的缺口（工具前必须先修）

下面 8 处，G1–G3 是用现有夹具写临时测试读出来的，其余读代码得出。涉及的三个函数没有任何非测试调用方，导出不用它们，所以改动不影响已有输出。

| # | 现状 | 证据 | 后果 | 处理 |
|---|---|---|---|---|
| G1 | 主表行因 `known_at` 被排除，返回 `not_tradable` + `not_listed` | `tradability.rs:170` 把所有 `Membership::Out(_)` 当作未上市。临时测试：`sz.000631` + `Temporal::known_by` → `NotTradable(NotListed)`，成员状态是 `Out(ExcludedByKnownAt)` | 带 `known_at` 时，A 股主表行（`known_at` 为空，§4）全部被判「不可交易」 | 工具不暴露 `known_at`，当前走不到。以后加 `known_at` 前必须改成 `unknown` + `known_at_unavailable` |
| G2 | 日历最后一天之后的日期，返回 `not_tradable` + `non_trading_day` | `tradability.rs:197` 用 `calendar.contains`；`TradingCalendar::day_status` 有 `BeyondCalendar`（`trading_calendar.rs:289`）。临时测试：日历外的下一个周一 → `NotTradable(NonTradingDay)` | 日历随日更延伸。日更前问「今天」会被说成非交易日 | 改用 `day_status`；`BeyondCalendar` → `unknown` + `beyond_calendar` |
| G3 | ETF、指数、其它非 A 股，返回 `not_tradable` + `not_listed` | 同 `tradability.rs:170`。临时测试：`sh.000300`（`instrument_class=1`）→ `NotTradable(NotListed)`，成员状态是 `Out(NotAShare)` | 把已上市的 ETF 说成未上市。`price_limit` 已有 `not_a_share`，两个函数不一致 | `Out(NotAShare)` → `unknown` + `not_a_share` |
| G4 | 不在主表的代码，返回 `not_tradable` + `not_listed` | `tradability.rs:150-158` | 主表不是封闭世界：新上市股在日更的 `derived:security_master` 跑之前不在表里，会被说成未上市（推断） | `unknown` + `not_in_master` |
| G5 | 涨跌停基准用除权前的最后一根不复权收盘 | `price_limit.rs:446-447` 只调 `raw_daily_close_before`，文件里没有读 `adjustment_events`。设计（`issue-391` §8.3，第 411 行）写的基准是「除权参考价」，并说前复权价在除权日会错 | 除权除息日的涨跌停价偏离交易所的值，却标 `known` | 见 Q3 |
| G6 | 板块判定的前缀表缺 `302` | `price_limit.rs:69-79` 只有 `300`/`301`。`sz.302132` 是 D-46 的确认迁移目标（创业板） | 返回 `unknown` + `board_unsupported`。结果仍是 fail-closed，但原因错 | 补前缀（板块分类仍是代码常量，不是规则数据，长期要不要进规则表另议） |
| G7 | `tradability` 没有算法版本 | `tradability.rs` 无 `ALGORITHM_VERSION`；另外两个有 | BR-M2-10 要求每个派生带 `algorithm_version` | 加 `tradability-v1` |
| G8 | 输入数据集未登记时，空表被当作「没有事件」 | `tradability` 直读 `suspension_intervals`，不查 `derived_dataset_versions` | 停牌数据集从没写过时，有 bar 的日子返回 `tradable`。设计原则「0 行不是证据」 | 工具层检查登记；缺失 → `unknown` + `input_absent`（§3.1） |

## 2. 工具形态

### 2.1 三个、两个还是一个

沿用 [#525](issue-525-read-tools.md) §3 的拆分规则：一个工具回答一类问题，参数不同就拆，同一参数含义不变。

| 方案 | 参数 | 新增字节（草案） | 「能不能买，涨跌停多少」 | 问题 |
|---|---|---|---|---|
| **A. 三个**：`get_tradability`、`get_price_limits`、`get_market_cap` | 前两个 `securities`+`as_of`，第三个多 `scope` | 554 + 574 + 639 = 1,767 B | 2 次 | 前两个参数完全相同，拆开只多一个工具和约 540 B |
| **B. 两个（推荐）**：`get_tradability`（状态加涨跌停）、`get_market_cap` | 前者 `securities`+`as_of`，后者多 `scope` | 586 + 639 = 1,225 B | 1 次 | 两层的 `unknown` 在一个响应里：状态和涨跌停各有自己的 `state`/`reason` 列，互不影响（可以 `tradable` 而涨跌停 `unknown`） |
| C. 一个：`scope` 可选，缺省不算市值 | 同 B 加可选 `scope` | 约 700 B（估，未手写草案） | 1 次 | `scope` 缺省有特殊含义，违反「同一参数含义不变」；响应混三层，出错时难读 |

选 B 的理由：可交易状态和当日涨跌停是同一类问题（「那天的交易条件」），参数完全相同，规则 2 允许合并；市值多一个必填的口径参数，是另一类问题。

### 2.2 输入输出草案

字节数为手写草案按服务器序列化格式（键排序、含 `$schema` 和 `title`）算出，用同样方法重建 `get_daily_bars` 得到 867 B，与实测一致。

**`get_tradability`**

```json
{"securities":["sz.000631","sh.600519"],"as_of":"2007-06-01"}
```

- `securities`：1..100 只，`^(sh|sz)\.[0-9]{6}$`（与现有工具共用 schema）。登记的指数代码报 `invalid_arguments` 并指向 `get_index_bars`，与 `get_daily_bars` 一致。
- `as_of`：缺省取 `last_trading_day_upto(today)`（上海日，与 `get_price_limit_rules` 一致）。缺省日期是今天且日更还没跑时，结果是 `unknown` + `no_bar` 或 `beyond_calendar`，不是 `not_tradable`。
- 描述（≤240 B）：`Whether each A-share could trade on as_of, plus its price limits that day. state is tradable, not_tradable or unknown; unknown is never tradable. as_of defaults to the latest trading day.`

响应（示意，值是占位，不是实测）：

```json
{
  "tool": "get_tradability", "layout": "table", "snapshot_time": "…",
  "as_of": "2007-06-01",
  "entities": ["sz.000631", "sh.600519"],
  "columns": {
    "state":          ["unknown", "tradable"],
    "reason":         ["data_gap", "tradable"],
    "suspension":     ["unresolved_gap 2006-04-28..2009-06-04", null],
    "flags":          [null, null],
    "limit_state":    ["unknown", "unknown"],
    "limit_reason":   ["st_unknown", "st_unknown"],
    "limit_candidates": ["500bp|1000bp", "500bp|1000bp"],
    "risk_status":    ["unknown", "unknown"],
    "limit_up": [null, null], "limit_down": [null, null],
    "prev_close": [null, "…"], "prev_close_day": [null, "…"], "rule_id": [null, null]
  },
  "inputs": [{"dataset": "security_master", "version_id": "…"}, {"dataset": "suspensions", "version_id": "…"}, …],
  "algorithm_versions": {"tradability": "tradability-v1", "price_limit": "limit-v1"},
  "limitations": ["…"],
  "temporal": {"as_of": "2007-06-01", "known_at": null, "datasets_without_known_at": ["…"]}
}
```

- **每个请求的实体一定有一行**，包括不在主表、不是 A 股的代码。不静默丢行（设计原则「0 行不是证据」）。
- 价格列是十进制字符串，按序列的 `price_scale` 渲染，不假定 2 位小数（设计原则「价格精度是序列属性」）。
- `limit_state` 三值：`known`、`unbounded`（规则明说无涨跌幅限制，例如新股首日，`limit_up`/`limit_down` 为空）、`unknown`。`unbounded` 不是 `unknown`，也不是一个很大的数。
- `limit_state=known` 且 `risk_status=unknown` 是合法组合（创业板 2020-08-24 起、科创板），`risk_status` 不会被写成 `normal`。
- 不放 `basis_ref`（每行约 400 B，100 行就是 40 KB）。需要出处时用 `rule_id` 查 `get_price_limit_rules`。
- `inputs` 列出各输入数据集的登记版本，`algorithm_versions` 列算法版本，共同决定结果（BR-M2-10）。按实体登记的输入（不复权日线、股本事件）不回显版本，用 `list_data_versions` 查。

**`get_market_cap`**

```json
{"securities":["sz.000002","sh.600028"],"scope":"float_a","as_of":"2026-11-05"}
```

- `scope`：必填，`float_a` / `company_total` / `a_share_total`。口径不推断，名字随数字一起返回。`a_share_total` 恒 `unknown`，保留在枚举里是为了让 AC-M2-6 在读面上可观测（约 20 B）。
- 响应列：`state`、`reason`、`market_cap`（元，两位小数的字符串）、`close`（不复权收盘）、`shares`（股）、`share_event_day`、`share_event_category`、`chain_break_day`、`reconciliation`。
- `reconciliation` 恒为 `not_available`：D-50 的 G2 末端对账需要「当前股本参考值」，主表没有存（`market_cap.rs` 文件头，审计 T6）。**不提供** `exclude_low_confidence` 选项，因为在生产里它永远不会触发，提供了只会让人以为做过对账。
- `company_total` 是 A+B+H 总股本，响应 `limitations` 里写明，不能称为「A 股总市值」。

### 2.3 实现路径

| 路径 | 做法 | 代价 |
|---|---|---|
| **1. 新 handler（推荐）** | 新增 `Handler::Derived`（暂名），处理函数在 `market.read` 闭包里调 research 函数，自己拼上面的 `table` 响应；工具名加入 `RESEARCH_TOOLS` 和 `read_tools::NAMES` | 不动 `Dataset` 枚举，所以 `query_data` 等 5 个工具的 schema 不涨；不用造 `QuerySnapshot` 行类型和版本语义 |
| 2. 三个新 `Dataset` | `market.tradability` 等进 `Dataset` 枚举，走 `query_v2` + `fact_table` + capabilities 的 `temporal` 声明 | 枚举值要进 `research` 里 5 个工具的 schema，每个值约 21 B × 5 ≈ 105 B，三个约 315 B（按 `market.industry_assignments` 出现 5 次推出）；派生结果没有自己的版本，`list_data_versions` 对它没意义 |

路径 1 的整次调用在一个 `market.read` 事务里，所有请求的实体读同一个快照。响应没有 `snapshot_id` 和 `next_cursor`：不分页，没有可恢复的游标，一个只能用一次的快照 id 没有用。复现靠 `inputs` 里的版本加算法版本。这与 #525 §3 规则 5 有出入，列在 Q7。

### 2.4 「这只票那天能不能买」最少调几次

| 方式 | 次数 | 说明 |
|---|---|---|
| 新工具 | **1** | `get_tradability(securities=[x], as_of=d)`，得到状态、原因和涨跌停价 |
| 现有工具 | 至少 5 | 日历（`get_trading_calendar`）、主表（`get_security_state` fact=`security_master`）、停牌（`fact=suspensions`）、风险警示（`fact=risk_warnings`）、不复权日线（`get_daily_bars` adjustment=`raw`）。要涨跌停价再加 `get_price_limit_rules` 和前收，还要自己写规则匹配和取整 |

此处的「能不能买」沿用 #391 的含义：可交易。能不能成交（一字涨停买不到、流动性）是回测引擎的事（设计 §7.4 明确不在 M2 做），要判断一字板再调 1 次 `get_daily_bars`。

## 3. fail-closed 状态

总原则：**肯定的证据只有「有 bar」；`not_tradable` 需要一个有名字的事件；其余一律 `unknown`；`unknown` 不写成可交易。**

### 3.1 `get_tradability` 的 `state`

判定顺序固定（设计 §7.4）：不在股票池 → 非交易日 → 确认停牌 → 缺口 → 无 bar → 有 bar。

| state | reason | 触发 | 来源缺口和限制 |
|---|---|---|---|
| `tradable` | `tradable` | 在册、在日历内、没有覆盖该日的停牌区间、有不复权 bar | 「有 bar」不等于能成交：一字板、零成交量的 bar 也算。A 股行没有 `no_trade` 标志（只有 ETF 有）。沪市盘中临时停牌（`stopTime` 为 `AM`/`PM`/`930`）不写 `confirmed`（M3 设计 §14.4），当日有 bar 就是 `tradable` |
| `not_tradable` | `not_listed` | 该日早于上市日；上市日未知且还没有 bar；或区间已结束（代码迁移、代码复用边界） | 上市日来源：交易所名单优先，TDX `SSDATE` 次之（`listing_date` 未知的行为 0，STATUS 11-01 读数） |
| `not_tradable` | `delisted` | 该日不早于已知退市日 | 退市日只有 337 行有值（交易所名单，STATUS 11-01 读数）。退市日未知的成员继续算在册，退市后没有 bar 时落到 `no_bar`（`unknown`），不会说成已退市 |
| `not_tradable` | `non_trading_day` | 该日在日历覆盖范围内，但不在日历里 | 日历由 9 个指数的 bar 日期并集推出，没有官方休市公告核对（`data-catalog.md` §3.3 偏差 3）。只能证明「覆盖范围内不在日历 ⇒ 非交易日」 |
| `not_tradable` | `suspended` | 有 `confirmed` 停牌区间覆盖该日 | **只有沪市 2008 年起有 `confirmed`**（D-60；STATUS 记的 preview.41 读数：`confirmed` 20,420 段）。深市首期没有 |
| `unknown` | `data_gap` | 有 `unresolved_gap` 覆盖该日：没有 bar，也没有公告 | 深市所有缺 bar 的日子；沪市 2008 年前。**缺 bar 不是停牌**（BR-M2-8）。`suspension` 列写区间，让调用方看到证据 |
| `unknown` | `no_bar` | 在册、在日历内、没有覆盖区间、没有 bar | 常见成因：为「今天」查询而日更还没跑；退市日未知的退市股；`derived:suspensions` 的缺口还没派生 |
| `unknown` | `calendar_missing` | 没有派生日历 | 「从没派生过」不是「不是交易日」 |
| `unknown` | `beyond_calendar` | 该日晚于日历最后一天（G2，新增） | 日历不预测 |
| `unknown` | `not_a_share` | 代码是 ETF、指数等非 A 股（G3，新增） | 这个工具只覆盖 A 股；ETF 没有涨跌停和停牌派生 |
| `unknown` | `not_in_master` | 主表里没有这个代码（G4，新增） | 主表不是封闭世界 |
| `unknown` | `input_absent` | `security_master`、日历、`suspensions` 任一输入数据集未登记（G8，新增） | 空表不是证据；`tradable` 需要这三个输入都在 |

`flags` 列带成员判定的提醒，不改变 `state`：`listing_unknown`、`delisting_unknown`、`ambiguous_issuer`（代码复用，`600018` 之类）、`candidate_migration_overlap`。它们让调用方知道结论建立在哪些假设上。

### 3.2 涨跌停：`limit_state` 和 `limit_reason`

| limit_state | limit_reason | 触发 | 来源缺口和限制 |
|---|---|---|---|
| `known` | 空 | 找到唯一一行规则，且前收、最小变动单位都有 | 规则 27 行价格规则加 2 行最小变动单位，全部 `verified`（`a7-d44-v1.json`）。前收日与 `as_of` 之间有 cat1 事件时不是 `known`，见下方 `ex_rights_unverified` |
| `known` | 空，`risk_status=unknown` | 状态未知，但所有状态类别命中同一行规则（`status_class='all'`） | 创业板 2020-08-24 起、科创板。不借用两条同幅度的状态专属规则 |
| `unbounded` | 空 | 规则明确 `band_side='none'` | 新股首日；主板 2023-02-17 起上市前 5 个交易日；创业板、科创板上市前 5 个交易日 |
| `unknown` | `st_unknown` | 状态未知，且各状态类别命中不同规则 | **主板几乎恒为此值**：库里不写 `normal`，无 ST 区间的日子状态就是 `unknown`（`validate_set` 不接受 `Normal`，`status_of` 无区间时返回 `Unknown`）。沪市 2004Q1 前的风险警示状态永久 `unknown`（D-58）。`limit_candidates` 给出候选幅度。2026-07-06 起主板两档同为 10%，候选只剩一个，仍是 `unknown` |
| `unknown` | `rule_missing` | 没有匹配的规则行 | 1996-12-16 前（R7，`NOT_AVAILABLE`）；主板首日 44%/36% 段（沪 2013-12-13 起、深 2014-06-13 起，到 2023-02-16，未编码）。另外，用 `a7-d44-v1.json` 逐日期核对（推断）：**主板上市后第 2–5 个交易日（2023-02-17 前）没有规则行**，创业板 2020-08-24 前的 ST、上市首日和前 5 日、2009-10-30 前整体也没有。这是规则书的覆盖缺口，结果仍是 fail-closed，但每只新股会有几天 `unknown`，见 Q8 |
| `unknown` | `rule_pending` | 只有 `pending` 规则行 | 规则书现在没有 `pending` 行，当前走不到 |
| `unknown` | `rule_ambiguous`、`min_tick_missing`、`min_tick_pending`、`min_tick_ambiguous`、`invalid_rule` | 规则数据有问题 | 配置错误，测试应当拦住；出现说明规则表坏了 |
| `unknown` | `listing_unknown`、`listing_phase_unknown` | 上市日未知，或上市日早于日历起点而无法数交易日 | `listing_date` 现为 0 个未知 |
| `unknown` | `no_series`、`no_prev_close` | 没有不复权序列，或没有更早的 bar | 335 个退市代码没有行情（M3 之前），上市首日无前收（首日规则为 `none` 时不需要前收，返回 `unbounded`） |
| `unknown` | `calendar_missing`、`non_trading_day`、`not_listed`、`not_a_share`、`board_unsupported` | 同 §3.1 的对应情形 | `board_unsupported`：不在 600/601/603/605/688/689/000–003/300/301 的代码，包括 `302`（G6）、北交所、B 股 |
| `unknown` | `ex_rights_unverified`（新增，D-81 已实现） | 前收日与 `as_of` 之间（`(prev_day, as_of]`）有该实体的 category 1 事件，含纯派现 | G5。category 11（ETF 份额折算）不算。判定顺序在规则查找和最小变动单位之后、算价之前：规则是 `band_side=none` 时不读前收也不读事件，仍是 `unbounded`。结果仍带 `prev_close_*` 和规则字段，供核对。读时派生的标签改为 `limit-v1+exr-guard1`；`limit-v1` 继续是已登记规则数据集的标签；`limit-v2` 留给真正算除权参考价的算法。事件按实体走 `adjustment_events_entity_day` 索引，单只与共享上下文走同一路径 |

### 3.3 `get_market_cap` 的 `state` 和 `reason`

| state | reason | 触发 | 来源缺口和限制 |
|---|---|---|---|
| `known` | 空 | 有股本事件、有当日不复权 bar | 股本取 cat 2/3/5/7/8/9/10 的后值（D-50，档位 `OWNER_RISK_ACCEPTED`），cat5 是混合桶。`float_a` 是流通 A 股 |
| `unknown` | `a_share_total_not_admitted` | `scope=a_share_total` | 历史 A 股总股本 `NOT_AVAILABLE`（交易所接口只返回当前一期）。绝不用 C4 冒充 |
| `unknown` | `no_share_event` | `as_of` 当天或之前没有可用股本事件 | ETF、指数没有股本事件，也落在这里（推断） |
| `unknown` | `chain_break` | D-50 G1：相邻事件的「前值」与上一个的「后值」不吻合；`as_of` 不早于断点日 | 容差 `max(110 股, 1e-6 × 值)`。断点是存储链的属性，用全部事件检查；只影响断点日及之后，断点之前的日期照常返回（`shares_at`） |
| `unknown` | `suspicious_only` | 该日前的事件都被 `suspicious` 门禁挡住 | 门禁是 fail-closed 默认 |
| `unknown` | `no_series`、`no_bar` | 没有不复权序列，或 `as_of` 当天没有 bar | 停牌日没有价格，不用前一日收盘代替 |

通用规则：

- 每个请求的实体一行。函数不检查成员资格，所以上市前、退市后的日子靠「没有 bar」落到 `no_bar`，不会返回旧市值。
- `reconciliation` 恒为 `not_available`，不暗示做过对账。

## 4. 时点语义

三种时间沿用 #525 §3 规则 3：日期范围、`as_of`（这一天生效的记录）、`known_at`（只用这一天已知的信息）。

**这两个工具只接受 `as_of`，不接受 `known_at`。** 原因是它们用到的输入里，没有一项对 A 股有可用的知晓时间：

| 输入 | 表 | 知晓时间 | 依据 |
|---|---|---|---|
| A 股主表行（上市日、退市日、区间） | `security_master` | **空**（只有 ETF 行有值） | TDX 行（`official_security.rs:905`）和只由交易所名单产生的行（`security_master/assembly.rs:809`）写 `known_at: None`；ETF 行在 `assembly.rs:314` 才有值。**推断**：没有在线上查证 |
| 风险警示 | `risk_warning_intervals` | 深市：首次采集日（上海日期 2026-11-01）；沪市：公告日 | `data-catalog.md` §3.1；`risk_warnings.rs`。2026-11-01 前的 `known_at` 看不到任何深市行 |
| 停牌 `confirmed` | `suspension_intervals` | **空**：事件表没有公告日，D-49 禁止用事件日代替 | `official_security/a3.rs:931` |
| 停牌 `unresolved_gap` | 同上 | **空**：派生行按定义没有 | `sqlite_market/suspensions.rs:184` |
| 股本事件 | `capital_events` | **空**（BR-M1-9） | `security.rs:48` |
| 复权事件 | `adjustment_events` | **空** | `market_data.rs` |
| 涨跌停规则 | `price_limit_rules`、`min_tick_rules` | 没有该列 | `temporal.rs` 的 `declared`，`known_at_coverage: none` |
| 日历、不复权 bar | `trading_days`、`series_bars` | 没有该列 | 按「T 日收盘后可知」的约定 |

如果硬收一个 `known_at`，A 股主表行会全部被排除：可交易性现状是误判为 `not_listed`（G1，修后是 `unknown`），涨跌停是 `unknown`，市值是 `unknown`（临时测试）。参数形同虚设，还暗示它有用。所以不暴露。这与 #525 §4.8 的 `get_security_events`（无 `known_at`）同理。

- 传了 `known_at` 时，工具报 `invalid_arguments`，消息写明原因（输入缺少知晓时间）并指向 `get_security_state`（风险警示等有 `known_at` 的事实）。不静默忽略，也不靠 `deny_unknown_fields` 的泛泛提示。这符合 design.md「时点参数是契约保留名……没有对应数据时不接受、不静默忽略」。
- 响应的 `temporal` 块照 M2 的形状：`as_of`、`known_at: null`、`datasets_without_known_at`（列出本次实际读到的无知晓时间的数据集）。让调用方看出：这是「按生效日回看」，不是信息集严格的答案。严格的回测不能靠这两个工具。
- 以后给 A 股主表行补知晓时间（审计 T5）后，`get_tradability` 才可能接受 `known_at`。即使那时，停牌行知晓时间仍为空，带 `known_at` 的查询里确认停牌会被排除，结果落到 `no_bar` 的 `unknown`，仍是 fail-closed。到时加参数要走契约版本，不在本设计内。
- `as_of` 的缺省值与 `get_price_limit_rules`、`get_security_state` 一致。

## 5. 批量和性能

**以下全部是从代码和数据量做的推断，没有测量。测量留给实现 PR。**

### 5.1 单日截面，还是单票区间

| 形态 | 回答的问题 | 结论 |
|---|---|---|
| 多票 × 单日（≤100） | 「这几只那天能不能买」 | **采用** |
| 单票 × 区间 | 「这只票这段时间每天的状态」 | 不做。区间长度 N 就是 N 次派生。区间性事实已能读：风险警示和停牌区间用 `get_security_state`/PR-2 的 `get_security_history`，bar 用 `get_daily_bars` |
| 全市场 × 单日 | 筛选、回测截面 | 不做。100 只一次，现行 5,225 只要 53 次调用。服务端筛选等 `screen_market`（D-78：出现不能写脚本的客户端需求再提） |

### 5.2 为什么要先改成一次加载

现有函数每处理一个实体，都会整表读以下表（行数取自 STATUS 在 11-01、11-02 的读数，只用量级）：

| 表 | 行数量级 | 谁读 |
|---|---|---|
| `suspension_intervals` | 70,626 | `tradability`（读全表，再在内存里按实体过滤） |
| `security_master` | 约 7,400（A 股 5,565，ETF 1,846） | 两个函数的成员判定 |
| `trading_days` | 8,798 | `tradability`、`price_limit` |
| `raw_daily_spans` | 约 5,600 个序列 | 成员判定 |
| `risk_warning_intervals` | 约 1,300（D-68 后预计到约 2,200） | 成员判定 |

一个实体约 9 万行要解码。按每行微秒级估算，一个实体百毫秒量级，100 只逐个调用是十秒量级（推断）。

修复 PR 里加共享加载：把这几张表读成一个只读的上下文，然后对每个实体只做内存查找。`select()` 已经接收 `only: Option<&str>`，改造量不大。`market_cap` 只读按实体的股本事件（有索引，约 23 行/实体，共 12.8 万行）加两次按实体的 bar 点查，不需要共享加载。

### 5.3 上限

- `securities` 1..100，与其它读取工具一致，响应不超过 100 行，约 20–30 KB（推断）。
- 共享加载做完且实测过，才用 100。没做共享加载，上限应降到 10 左右（推断）。
- 不设延迟门禁（性能阈值没有确认，AGENTS.md 要求不写成既定要求）。实现 PR 报告 1、10、100 只的实测耗时和数据规模，owner 再决定要不要门禁（Q6）。

## 6. 预算

`research` 入口 `tools/list`（无 `outputSchema`），main `5e4c4dd` 实测 **18,437 B / 14 个工具**，预算 18,600 B（D-75），余 163 B。

| 项 | 字节 | 备注 |
|---|---|---|
| `get_tradability` | 586 | 草案 |
| `get_market_cap` | 639 | 草案 |
| 两个工具合计 | **1,225** | 加在 18,437 上得 19,664 B，**超预算 1,064 B** |
| 三个工具方案（§2.1 A） | 1,767 | 得 20,207 B |
| PR-2 `get_security_history`（估） | 约 850 | 按 PR-1b 的 `get_security_state` 实测 841 B 和 #525 §7 的比例推；尚未实现，推断 |
| 两个工具 + PR-2 | **约 20,520** | 超 1,920 B |
| 三个工具 + PR-2 | 约 21,060 | 超 2,460 B |
| `get_data_capabilities` 紧凑视图 | 约 +240（两个工具），约 +360（含 PR-2） | 现 2,438 B（PR-1b 实测，14 个工具），CI 上限 2,500，所以这个上限也要上调；每个工具约 120 B |

`ops`、`maint` 不受影响（路径 1 不动 `Dataset` 枚举）。

owner 要选一种（或组合）。**下面的推荐是我的建议，不是决定：**

| 选项 | 效果（含 PR-2） | 代价和风险 |
|---|---|---|
| a. 上调预算到实测需要 | 约 20,600 B | 最简单，D-75 已有先例。两个工具加 PR-2 共多约 2,100 B，按 CI 的 4 B/token 估约 520 个 token，每个会话都付 |
| b. 去掉每个读取工具 `inputSchema` 里的 `$schema` 和 `title` | 在 12 个现有工具上实测省 995 B（约 83 B/个），按 17 个工具推约 1,410 B，合计约 19,100 B | 仍超 500 B 左右。**未验证**客户端（kiro、pi、x 的 `mcp_connect`）是否依赖这两个字段 |
| c. 把 `query_data`（4,068 B）移出 `research` | 约 16,450 B | #525 §11 裁决 2 说评测通过后再决定；x 接入前要确认它用哪个入口；pi 的 codemode 是否依赖它，我没有核实 |
| d. 把 `get_data_availability`（4,007 B）或 `list_data_versions`（2,194 B）移出 | 约 16,500 / 18,300 B | 它们是 `query_data` 的配套发现工具，`query_data` 保留时一起移才有意义 |

**推荐**：先用 a 解封，把 b 和 c 留给评测之后（#525 §9 的复跑）。理由是 a 不改任何已有行为，b 和 c 的客户端影响没有数据。

## 7. 扩展场景

按 [design.md](../design.md)「后续数据扩展要求」的做法走六个场景（枚举来自 [M3 设计 §13](issue-446-m3-delisted-industry.md)，与 [ETF 名称设计 §4](etf-name-search.md) 一致，并标出 design.md 的类别）。

| # | 场景（design.md 类别） | 触及吗 | 结论 |
|---|---|---|---|
| 1 | 新增证券类型（新增数据类别） | 部分。工具只覆盖 A 股；ETF 返回 `unknown` + `not_a_share` | 以后做 ETF 可交易性要用 `no_trade` 标志和 ETF 的涨跌停规则（规则书没有 ETF 行），不是只改适配层。北交所：输入 schema 的 `sh\|sz` 模式已挡住，规则、来源都未定 |
| 2 | 新增官方来源（同类数据新增来源） | 不改工具。沪市 2004Q1 前的 ST、A 股总股本等来源准入后，对应 `unknown` 自动变成有值 | `a_share_total` 要在 `market_cap` 里加分支，但 `scope` 枚举已含，工具 schema 不变 |
| 3 | 新增字段或数据集（新增数据类别） | 否。换手率（UC-M2-6 提到）没有库函数，不在 #433 第 5 项，不做 | 若要换手率，是 `get_market_cap` 之外的另一类问题 |
| 4 | 新增频率 | 否 | — |
| 5 | 交易规则变更（涨跌停） | 是 | 规则变更只加规则行，工具不改（AC-M2-10）。代码里仍有一处分类常量：板块前缀表（G6）。它不是幅度常量，但加新板块代码要改代码 |
| 6 | 新增使用方/入口（新增工具面或入口） | 是 | 只进 `research`，静态 allowlist 加两个名字，封闭性测试覆盖；`ops`、`maint` 不变。design.md 第四类那格写的是新增 dataset 不涨 `tools/list`；新增工具会涨，按 D-75 管预算（§6） |

design.md「扩展契约」逐条核对：

- **规则是数据**：满足，工具不写幅度常量。
- **时点参数是契约保留名**：满足，`known_at` 显式报错（§4）。
- **0 行不是证据**：满足，每个实体一行，输入数据集未登记报 `input_absent`。
- **价格精度是序列属性**：满足，按 `price_scale` 渲染。
- **`outputSchema` 不进 `tools/list`**：满足。
- **派生数据可重算、带 `algorithm_version`、不进 `version_id`、不入库**：满足，工具不写表；`tradability` 补算法版本（G7）。
- **SQLite 新类别用独立表**：不适用，没有新表，不改 schema（仍 27）。
- **回填行的 `known_at`**、**成员集与目录快照分离**：不适用。

## 8. PR 拆分和待决问题

### 8.1 拆分

| PR | 内容 | 依赖 |
|---|---|---|
| 本 PR | 本设计（只改文档） | 无 |
| PR-A research 层修复 | G2、G3、G4、G6、G7；G1（成本低，顺手）；共享加载；Q3 选 (a) 时加 `ex_rights_unverified`；每处加测试。三个函数无非测试调用方，导出不受影响；共享加载若动到 `universe::select`，要保证 `universe`（`sqlite_supply` 在用）输出不变，现有 `read_model_tests` 全过 | owner 批准本设计 |
| PR-B 两个工具 | `Handler::Derived`、两个窄输入类型、响应拼装、G8 的登记检查、`known_at` 的显式错误；`RESEARCH_TOOLS`/`NAMES`；CI 预算和 `get_data_capabilities` 上限；`check_current_facts.py` 的工具数；`docs/mcp-tools.md`；更新两处会变错的注释（`research/mod.rs` 第 7 行「Nothing here is reachable from an MCP tool」，`sqlite_supply/m2.rs` 第 511 行「M2 does not derive limit prices」）；测试：与库函数逐项等价（沿用 PR-1 的做法）、fail-closed 各状态各一例（`sz.000631` 缺口、A+H 的 `a_share_total`、主板 `st_unknown`、创业板确定值）、1/10/100 只的耗时 | PR-A、#525 PR-2 |
| PR-C 评测 | 在 `.devin/evals/read-tools-v1.json` 加「能不能买」「涨停价」「市值」几题，部署后在 vm2 复跑 | PR-B 部署 |

`capability_revision` 升级随读取工具系列的最后一个 PR（`docs/mcp-tools.md`）。本系列晚于 PR-2，所以由 PR-B 承担，日更 Skill 的钉值同提交改（#525 §8）。

AC 文字（§1.3）在 PR-B 通过后，由 owner 更新到 #391/#433，或随一个文档 PR 更新；本设计不改 Issue。

### 8.2 待决问题

**owner 已于 2026-11-06 回复「全部按推荐」**：Q3、Q4 记为 [D-81](../decisions.md)，其余记为 [D-82](../decisions.md)。Q2 的上限定为 19,600 B（§10 备注）。下表保留裁决前的原文。

| # | 问题 | 选项 | 推荐 |
|---|---|---|---|
| Q1 | 工具形态 | A 三个；B 两个；C 一个（§2.1） | **B**：状态和涨跌停同参数，合并后「能不能买」1 次调用，省 542 B |
| Q2 | `tools/list` 预算 | a 上调；b 去 `$schema`/`title`；c 移出 `query_data`；d 移出配套发现工具（§6） | **a**，b、c 留给评测后 |
| Q3 | 除权日涨跌停（G5） | (a) 前收日与 `as_of` 之间有除权事件时返回 `unknown` + `ex_rights_unverified`，等 `limit-v2`；(b) 照现算法返回 `known`，只在 `limitations` 提醒；(c) 先做 `limit-v2`（审计 T4）再上工具 | **(a)**：fail-closed，影响的日子占比很小（推断：`adjustment_events` 59,361 行，`series_bars` 约 3,783 万行含全部口径，不复权日线约 1,670 万根，每个事件最多影响一个日子，约 0.35%）；(b) 把已知偏差写成已知值。(c) 要先取证 2010 年后的除权参考价口径，不应阻塞工具 |
| Q4 | 主板涨跌停大多 `unknown`（闭世界证据，审计 T3） | (a) 工具照实返回；(b) 等 T3 再发涨跌停部分 | **(a)**：#433 第 5 项的目的就是让 fail-closed 在生产上可观测。T3 另决，决定后工具不用改，`unknown` 自动变成有值 |
| Q5 | `known_at` | (a) 不收，显式报错；(b) 收，全部返回 `unknown`；(c) 等审计 T5 补了 A 股主表知晓时间再加 | **(a)**，T5 之后再按 (c) 加参数 |
| Q6 | 批量上限和延迟门禁 | 上限：100（共享加载后）/ 10（不改）。门禁：不设 / 设一个 p95 | 上限 **100，先做共享加载**；门禁**不设**，先看 PR-B 的实测再定 |
| Q7 | 响应信封 | (a) 不带 `snapshot_id`、不分页，用输入版本加算法版本复现；(b) 和其它读取工具一样带 `snapshot_id`，经 `Queries` 的缓存（要走 §2.3 路径 2） | **(a)**，100 行内不需要游标 |
| Q8 | 规则书的覆盖缺口（§3.2 `rule_missing`）：主板上市后第 2–5 个交易日（2023-02-17 前）等没有规则行 | (a) 工具照实返回 `unknown`；(b) 另开数据任务补规则行（这是纯数据改动，AC-M2-10；每行要有 `basis_ref`，主板这几天的 10% 就是普通规则，出处要取证）；(c) 让 `listing_phase` 在没有专属行时退回 `seasoned`（改代码，违反「缺行不是兜底」） | **(a) 加 (b)**：不阻塞工具；不做 (c) |

## 9. 未验证

- 所有字节数是手写草案按序列化格式算的，已用 `get_daily_bars` 校准一致（867 B），但真实 schema 由 `schemars` 生成，PR-B 要重测。
- G1–G3 的行为来自本分支上的临时测试，用的是夹具，不是真实库。
- 「主板涨跌停大多 `unknown`」「A 股主表行知晓时间为空」是读代码推出的，没有在 vm2 的只读库上查证（本任务不访问线上）。
- 第 5 节的耗时全是推断。
- G5 的触发条件（前收日与 `as_of` 之间有 cat1 事件）已按 D-81 实现并用夹具测试，仍没有用真实除权样本（如 `sh.600000` 2008-04-24）在真实库上核对。
- 没有核实 pi、kiro、x 的客户端是否依赖 `$schema` 或 `title`（选项 b）。
- 没有核实 `query_data` 在 codemode 里的使用方式（选项 c）。
- 示例响应里的值是占位。

## 10. 审核备注（Claude，2026-11-06）

已对照 main 代码核实 G2、G3、G4（`tradability.rs`）、G5、G6（`price_limit.rs`），与文中一致。

1. **预算基线已变（§6、Q2）。** #525 PR-2 的任务已要求去掉读 schema 根上的 `$schema` 和 `title`，也就是选项 b。这不改语义：MCP 规定没有 `$schema` 时默认 2020-12。pi 的 MCP 适配层（`extensions/mcp/tools.js` 的 `toParameters`）把 schema 原样透传，读的是工具级 `title`，不是 schema 根上的。x 客户端仍未核实，部署前要确认。
   按 §6 的估算，选项 b 之后两个工具加 PR-2 约 19,100 B，还超约 500 B，ETF 名称 PR-3 的 `kind` 参数也要占预算。所以 Q2 实际要决定的是这部分：上调预算（a）还是移出 `query_data`（c）。以 PR-2 的实测为准。
2. **§2.2 `as_of` 缺省的说法要改。** `last_trading_day_upto` 取日历里不晚于今天的最后一天（`sqlite_supply/m2.rs`），日历随日更延伸，所以日更还没跑时，缺省的 `as_of` 是上一个交易日，答案是那一天的，不会落到 `beyond_calendar`。`beyond_calendar` 只在显式传日历之后的日期时出现。没有登记日历时才用今天，此时是 `calendar_missing`。
3. **PR-A 不等 owner。** G1–G4、G6、G7 和共享加载都是修复：把“未知”说成“确定”的错判改回来，三个函数没有非测试调用方。Q3 的 `ex_rights_unverified`（G5）、G8 和 PR-B 等 owner 决定。
4. **Q3、Q4、Q5 与审计 T4、T3、T5 是同一组问题，** 要和决策备忘录 `rigorous-gaps-decisions.md` 一起交给 owner，不要分开问。
