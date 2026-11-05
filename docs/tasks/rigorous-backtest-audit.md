# 严谨回测验收审计（#383 / #391 / #433 / #446 / #509 / #308 / #387）

日期：2026-11-05。代码：main `e1e6d99`（该提交的 CI run `42017473021` 为 success）。
部署事实以 [STATUS](../STATUS.md) 为准：vm2-126 为日更与主数据生产机（preview.45、schema 27、`generic-data-v10`），
aws-node 冻结在 preview.43 / schema 26 / `generic-data-v9`，客户端经隧道仍连 aws-node。

## 0. 方法与限制

- 只读代码、测试、文档和 Issue。**没有编译、没有运行 Rust 测试**，没有访问线上机器、交易所或 tailnet。
  「测试位置」表示该测试存在并断言了所述行为，其通过状态取自 main 的 CI。
  本地跑过：`check_current_facts.py`、`check_public_hygiene.py`、`v8_token_budget.py`。
- 线上数字（行数、耗时、覆盖）只引用 STATUS、SLO 和 Issue 评论里已有的记录，并标注来源；本审计不重新测量。
- AC 状态只用三个值，不看 Issue 开关：
  - **已满足**：代码和测试能证明；括号里写位置。
  - **未满足**：代码、测试或文档证明做不到，或 AC 文字已被后续裁决取代（后一种在备注里写明「AC 文字过期」）。
  - **无法从仓库验证**：要靠线上数据、仓库外证据或别的仓库；写出已有的线上记录。
- 「推断」表示由代码阅读或仓库内数字推出，没有实测。

## 1. 结论

1. **现在不能称为严谨回测级。** 四项标准各有数据层的进展，也各有一个会让结论出错的缺口（第 2 节）。
2. **#433 的读面至今没有入口，而且 D-75 的 10 个新工具也没有覆盖它**（第 3 节）。
   `price_limit`、`tradability`、`market_cap` 只是库函数，除测试外没有任何调用方。
3. 阅读代码发现五项 Issue 里没有记录的问题。都是从代码推断，没有在真实库上验证，排期前先验证：
   - 默认研究导出按**目录成员**选股票序列，338 个退市序列不在目录里，会被漏掉（第 2.1 节）。
   - 生产写入器从不写出 `normal` 风险状态，沪深主板在没有 ST 区间的日子里涨跌停一律 `unknown`（第 2.3 节）。
   - 涨跌停基准用除权前最后一根 bar 的不复权收盘，没有按除权参考价算（第 2.3 节）。
   - A 股主表行没有 `known_at`，带 `known_at` 的股票池会把所有 A 股排除（第 2.2 节）。
   - 全市场截面每页都带全部实体和版本清单，一次调用做不到设计的 ≤40K token（第 4.7 节 AC-V8-5）。
4. 78 条 AC 的结果：满足 41，部分满足 11，未满足 14，无法从仓库验证 12（第 4.8 节）。
   未满足的 14 条里有 9 条是 AC 文字过期或预算被后续裁决上调，需要改写（附录 A 给出建议文字）。
5. `docs/data-catalog.md` 有大量「未部署」「待发布」「待准入」的过期说法，已修正；不确定的未改，列在第 5 节。

## 2. 严谨回测四项标准

### 2.1 无幸存者偏差

**现在能做到**

- 退市股行情已在库：`sh_sz_a_listed_ever` 首采 338/338 成功（#446 评论，XDM-137，2026-10-31），日更有只读的 `derived:delisted_series_check`
  检查序列不消失、不被改写（`src/data_supply/delisted_series/tests.rs:64`）。
- 时点股票池 `universe(as_of)` 由主表决定，不看目录：`src/research/universe.rs`，十个历史日与「当日有 bar 且在册」的集合对比
  （`src/research/read_model_tests.rs:609`）。2015-01-05 的成员数 2,595，2015 年以来退市的 256 个都有 bar（#446 评论，XDM-137）。
- 退市 ETF 128 只已写入主表并带精确退市日（STATUS，vm2 部署后读数）。

**残余偏差**

| 偏差 | 影响 | 依据 |
|---|---|---|
| **默认研究导出疑似不含退市序列（推断，未验证）** | 导出是可复现回测的唯一产物，若漏掉退市股，导出的回测仍有幸存者偏差，而清单里 `coverage.delisted_series` 会是 0 或偏小 | `src/research/read.rs:236-258`：`stock_series` 在目录选择下只保留 `catalog_members` 里的实体；`src/research/mod.rs:944-950` 的 `plan` 用目录成员调用它。退市股按定义不在当前目录。M3 设计 §10.4 和 `read.rs:1094` 的注释都说「成员集取自主表」，代码不是这样。测试 `src/research/tests.rs:557` 里被当作退市的 `sz.000001` 同时是目录成员，所以测试发现不了。#446 评论里 AC-M3-2 的导出交叉核对写明「未跑」。显式 `--entities` 列出退市代码可以绕过 |
| `delisting_unknown` 标记不能用来筛出「可能已退市」的成员 | 标记的条件只是 `delisting_date` 为空（`src/research/universe.rs:713-715`），所有在市股票都满足，当前主表 5,228 行如此（STATUS）；真正「已退市但没有退市日」的只有 `sh.600849` 一类个别情形。data-catalog 旧文说「严格口径可据此排除」会把全部在市股票排除 | `src/research/universe.rs:713-715`；`src/research/read_model_tests.rs:827`（一个有停牌的在市成员带该标记）；STATUS 的 M2/M3 表 |
| 代码迁移与代码复用 | 同一发行人可能被算两次；108 条 `candidate` 迁移只是线索，不参与去重 | STATUS（`code_migrations` 109 行：108 candidate、1 confirmed）；`strict` 只在 Rust 库里，MCP 与导出都没有入口（`src/` 内无非测试调用 `.strict(`） |
| 导出的 `universe_intervals.flags` 只带 `ambiguous_issuer` | 消费方无法在该文件里区分退市日未知和候选迁移重叠；这两个标记只在 `security_master.parquet` 的 `flags` 列和 MCP 的 `universe` 里 | `src/research/universe.rs` 的 `class_intervals`；`src/research/parquet.rs:1458-1464` |
| 86 只现行 ETF 没有日线；沪市退市 ETF 公告最早 2006-08；深市「已退市且不在 `hsjday`」的 ETF 未验证 | ETF 回测仍有来源缺口 | data-catalog §3.3 偏差 9、10 |
| 不含北交所、B 股；`hsjday` 缺 4 个官方成员 | 范围边界，已在 #246 / README 写明 | README、STATUS |

### 2.2 时点正确

**现在能做到（有效时间口径，`as_of`）**

- 不复权价是事实，不含未来事件；前复权含未来事件（以最新事件为基准），只适合固定快照内使用（data-catalog §2.2）。
- 主表、简称、风险警示、停牌、行业都带生效区间，`as_of` 可查（`src/research/temporal.rs`，`src/sqlite_supply/m2.rs`）。
- 风险警示、简称、行业的 `known_at` 有值：沪市取公告日（`src/official_security/risk_warnings.rs:1083-1100`），
  深市取「首个包含该行的快照入库日」，首次采集日 2026-11-01（`src/sqlite_market/risk_warnings.rs:103-109`；data-catalog §3.1）。

**残余偏差**

| 偏差 | 影响 | 依据 |
|---|---|---|
| **A 股主表行没有 `known_at`** | 对 `market.security_master` 或 `universe` 传 `known_at` 会把所有 A 股行排除并计数，只剩 ETF 行；A 股的「当时已知」股票池做不到，只能用生效日 | `src/official_security.rs:905` 和 `src/security_master/assembly.rs:809`、`:456` 写 `known_at: None`；只有 ETF 行（`assembly.rs:314`）有值（推断：未在线上验证） |
| 股本事件、复权事件、停牌、涨跌停规则没有 `known_at` | 带 `known_at` 的市值、停牌、涨跌停查询得不到任何这些输入；信息集严格的回测对它们做不到 | `src/sqlite_market/security.rs:48` 的说明；`src/sqlite_supply/m2.rs:518-520`；`src/official_security/a3.rs:931-932`；`src/data_supply/wire.rs:141-155`（`known_at_coverage: none`） |
| 深市 ST 与简称的 `known_at` 是 2026-11-01 | 在这之前的日期，带 `known_at` 的查询看不到任何深市历史行 | data-catalog §3.1「两句口径」；`src/security_master/risk_warnings.rs:53-56` |
| 沪市 ST 生效日按公告日锚定（owner 风险接受）；2000–2003 永久 `not_observed`；2004Q1 起才有事件 | 区间边界可能比真实生效日早或晚；2004 年前沪市 ST 一律 `unknown` | D-58；data-catalog §3.3 偏差 2 |
| 行业从 2026-09-30 起才有，且全部 `unknown_dictionary`（名称未知，代码可用） | 此前的行业中性化做不到 | #446 评论（XDM-137）；data-catalog §3.3 偏差 1 |
| 交易日历是指数日期并集的推导，没有官方休市公告核对 | 只能证明「不在日历 ⇒ 非交易日」 | data-catalog §3.3 偏差 3 |
| `as_of` 缺省取 `chrono::Local::now()` | 主机时区是 UTC、业务是上海时，缺省日期可能差一天；影响「最新」类查询的可复现性 | `src/data_supply/query.rs:247`；`docs/tasks/issue-525-read-tools.md` §6 已记为待办 |
| AC-T02 要求的同日公告、生效次日、盘中未知、更正公告规则没有写成文 | 同日冲突是 fail-closed 报错；更正靠「同一请求取最新快照」；生效次日和盘中没有规则 | `src/security_master/risk_warnings.rs:150-156`；`tests/source_store/risk_warnings.rs:101` |

### 2.3 可判断能否交易

**现在能做到（库函数，生产不可读，见第 3 节）**

- `tradability` 的判定顺序固定：不在股票池、非交易日、`confirmed` 停牌、`unresolved_gap`、无 bar、有 bar（`src/research/tradability.rs:140`）。
  只有「有 bar」是肯定证据；缺 bar 只能是 `unknown`，从不返回 `suspended`（`src/research/read_model_tests.rs:987`）。
- 沪市 2008 年起有 `confirmed` 停牌（20,420 段，STATUS），深市没有结构化来源，只有 `unresolved_gap`。
- 涨跌停 `limit-v1` 的规则全部是数据：27 行价格规则加 2 行最小变动单位（`src/trading_rules/a7-d44-v1.json`），改规则不改代码
  （`src/research/price_limit/tests.rs:688`）。

**残余偏差**

| 偏差 | 影响 | 依据 |
|---|---|---|
| **生产里没有 `normal` 状态，主板涨跌停几乎全是 `unknown`（推断）** | 没有 ST 区间的日子，状态是 `unknown` 而不是 `normal`；主板的 ST 与非 ST 幅度不同（5% 对 10%），所以派生返回 `unknown`。只有创业板 2020-08-24 起和科创板（规则与状态无关）有确定值，以及处在 ST 区间的日子 | `src/security_master/risk_warnings.rs:417-446` 的 `validate_set` 不接受 `Normal`；`src/research/universe.rs:801` 无区间即 `Unknown`；`src/research/price_limit.rs:580-598`、`:617`。设计 §6.3 明确要求「闭世界」证据才能得到 `normal`，D-58 又禁止「无公告 ⇒ 非 ST」，两者之间的覆盖证明没有实现 |
| **除权日的涨跌停基准没有按除权参考价算（推断）** | 除权除息日，交易所以除权参考价为基准；`limit-v1` 用除权前最后一根 bar 的不复权收盘，除权日的涨跌停价会偏离 | `src/research/price_limit.rs:446-447` 只读 `raw_daily_close_before`，文件里没有读取 `adjustment_events` 或 `capital_events`；`src/research/price_limit/tests.rs` 没有除权用例；设计 §8.3 写的基准是除权参考价，data-catalog §3.3 没有披露 |
| AC-M2-9 的命中率回归不存在 | 无法量化派生的涨跌停价与实际 `high == limit_up` 的吻合度 | `src/research/price_limit/tests.rs:850` 只验证取整方向；基线数字在仓库外 |
| 盘中临时停牌不记 `confirmed`；一字板、成交量为 0 的 bar 仍判 `tradable` | 「有 bar」不等于能按该价成交；A 股行没有 `no_trade` 标志（只有 ETF 有） | `docs/tasks/issue-446-m3-delisted-industry.md` §14.4（`stopTime` 为 930/AM/PM 的行不写 `confirmed`）；`src/data_supply/columnar.rs:29`；成交判定在 #391 范围外 |
| 深市全域可交易性是 `unknown` | 回测要把深市缺 bar 当未知剔除或单独报告 | data-catalog §3.3 偏差 6 |
| 市值：A 股总市值（A+H）永远是 `unknown`；G2 末端对账没有参考值来源 | 流通市值和公司总股本可算，但没有 `low_confidence` 判定；`CurrentShares` 解析了没入库 | `src/research/market_cap.rs:30-34`；`src/security_master.rs:209-218`；非测试代码里没有任何地方设置 `current_float_reference` |

### 2.4 可复现

**现在能做到**

- 同一数据库状态、同一命令，导出两次的 `snapshot_id` 和全部文件（含 `manifest.json`）字节相同
  （`src/research/tests.rs:1558`；线上记录：#383 评论 3，preview.36 补测）。
- 导出在 `mode=ro` + `query_only` 下运行，期间日更改动会拒绝出清单（`src/research/tests.rs:1647`、`:1261`、`:1675`）。
- 清单绑定目录版本、每条序列的 `version_id`、来源 generation 和 manifest sha256（`src/research/manifest.rs`）。
- 交易所来源冻结进来源库，按 sha256 保存，可离线重放（`tests/source_store/exchange.rs:637`、`:1325`）。

**残余偏差**

| 偏差 | 影响 | 依据 |
|---|---|---|
| 清单不记录二进制版本，`algorithms` 只有序列摘要、前复权、因子、日历四项 | 股票池、涨跌停、市值是读时重算，算法版本（如 `limit-v1`、`market-cap-v1`）不在快照身份里，换二进制可能给出不同派生结果 | `src/research/manifest.rs:59-66` |
| 同一个 `research-snapshot.v4` 字符串对应两种列布局 | preview.42/43 的 v4 日线文件 11 列，preview.45 的 v4 为 13 列（加 `instrument_class`、`no_trade`）；契约文档称「v4 从未部署」与提交历史不符。`snapshot_id` 不含列布局，同一无 ETF 的库在两个版本上可能 `snapshot_id` 相同、字节不同（推断） | `git show 803b35c:src/research/mod.rs` 的 `DAILY_COLUMNS` 无 `no_trade`；`docs/contracts/generic-data-v10.md` §1.6 |
| 单版本存储：除权会改写整段前复权历史，旧 `version_id` 之后无法再读 | 复现只能靠保留导出目录；导出保留策略是 #383 评论 5 列出的延后项，至今没有裁决 | `docs/latest-only-storage.md`；#383 评论 5 |
| generation 字节在两代之后退役 | 无法从原始官方文件重新派生旧快照，只能靠已登记的派生事实 | STATUS「旧代回收」；D-65 |
| 来源库重放有已知差异 | `replay --verify-only` 的 `risk_warnings` 在 vm2 上报 `drift`：105 行沪市记录的 `source_document_sha256` 不同，原因是同一公告出现在多页时页序不同；**修复待开发** | STATUS「已知问题」 |
| `replay --scope all` 不含停牌 | 停牌 `confirmed` 无法随 `all` 一并断网重建；停牌单独 scope 在 XDM-137 报 `drift`（`inputs_changed`） | `src/source_store/replay.rs:161-170`；#446 评论 1 |
| 「逐位复现同一回测结果」（AC-M3-8）没有承诺 | 导出可复现不等于回测可复现：回测引擎、参数和快照保留都不在本仓库范围 | #446 AC-M3-8 的注记 |
| 直读脚本不绑定版本 | `scripts/analysis/boll.py` 只按 `series_key` 读当前行 | data-catalog §5.2 |

## 3. #433 的读面：`price_limit`、`tradability`、`market_cap` 有没有读取入口

**没有。** 逐个入口核对：

| 入口 | `price_limit` | `tradability` | `market_cap` |
|---|---|---|---|
| 库函数 | `src/research/price_limit.rs:282` | `src/research/tradability.rs:140` | `src/research/market_cap.rs:316` |
| 非测试调用方 | 无 | 无 | 无 |
| `research` 入口（`RESEARCH_TOOLS`，`src/data_supply/registry.rs:44-50`，5 个只读工具） | 无 | 无 | 无 |
| `ops` / `maint` 入口 | 无 | 无 | 无 |
| `query_data` 数据集（`Dataset` 枚举 13 个，`src/data_supply/wire.rs:11-49`） | 只有规则行：`market.price_limit_rules`（`src/sqlite_supply/m2.rs:498-551`，注释写明「M2 does not derive limit prices」） | 只有 `market.suspensions` 的缺口行，没有判定 | 只有 `market.capital_events` 的 `float_after`/`total_after`/`suspicious`，没有链断点和 `low_confidence` 判定 |
| 研究导出（`export_research`） | 不写：`src/research/mod.rs:760-762` 明确不写 `price_limits.parquet`；`daily_raw` 不含派生列（`src/research/tests.rs:2013`） | 不写 | 不写 |
| D-75 的 10 个新工具（`docs/tasks/issue-525-read-tools.md` §4） | 只有 `get_price_limit_rules`（规则行） | 无 | 无 |
| v8 契约承诺（`docs/contracts/generic-data-v8.md:490`） | 「规则数据到位后作为独立 dataset 挂到 `query_data`」，从未做 | 同左 | 同左 |

后果：

- AC-M2-6…10 的 fail-closed 行为只有夹具测试，**在生产上不可观测**（#433 第 5 条与 #391 评论 2 已说明，现状没有变化）。
- 使用方只能自己组合：用 `query_data` 读 `market.daily`（`adjustment=raw`，v10 起 A 股也可读）、
  `market.price_limit_rules`、`market.risk_warnings`、`market.suspensions`、`market.capital_events`，再用自己的代码重算。
  这样得不到 `limit-v1` 的 `unknown` 传播，也得不到市值的 G1 链断点和 `suspicious` 过滤。
- 市值的 G2 对账在生产里不可能运行：参考值（`base.dbf` 的当前股本）解析后没有入库。
- 这三项是 #433 第 5 条的内容，Issue 仍开着；D-75 没有把它们排进去。建议任务 T2 和 T6 覆盖。

## 4. 逐条 AC 核对

记法：**满足** = 已满足，**未满足**，**线上** = 无法从仓库验证。「位置」是代码或测试；「线上记录」是 Issue 评论或 STATUS 里已有的实测，不是本审计测得的。
各 Issue 的 AC 编号互相重名（如 #383 的 AC-M1-* 与 data-catalog 里的 AC-M1-* 不是一套），下面一律按 Issue 正文的编号。

### 4.1 #383 M1 研究数据底座（Issue 已关闭，部署 preview.36）

| AC | 状态 | 依据 |
|---|---|---|
| AC-M1-1 不复权与 `.day` 逐根相等，volume 按 #381 归一 | 满足 | `tests/service/official_market.rs:548`、`:613`、`:658`；线上记录：20 实体 69,954 根 0 处不等（#383 评论 1）。#381 修复 `d738488` 是 preview.36 的祖先；评论 1 称 0 根可复现，评论 2 更正为扫描脚本错误 |
| AC-M1-2 6dp 前复权与精确有理数参考实现逐位相等 | 满足 | `tests/algorithms/qfq_exact.rs:108`、`:200`（Python `Fraction` 语料摘要）；线上记录：279,816 个价格字段 0 处不等 |
| AC-M1-3 6dp 按 2dp 舍入后的差异只在半分位、逐条可枚举 | 满足 | `tests/algorithms/qfq_exact.rs:263` |
| AC-M1-4 落在日历外的 bar ≤ 0.01% 且逐条列出 | 满足 | `src/sqlite_market/calendar/tests.rs:427`、`src/trading_calendar/tests.rs:208`；线上记录：39/16,745,336 = 2.33e-06 |
| AC-M1-5 日历不因某指数缺 bar 判成非交易日 | 满足 | `src/trading_calendar/tests.rs:64`、`tests/service/official_market.rs:329` |
| AC-M1-6 指数单位与口径有实测证据，无证据不收录 | 满足 | `src/index_registry.rs:98`（`require_evidence`）、`src/sqlite_market/index/tests.rs:346`；证据本身（108 个 OHLC 比值）在仓库外 |
| AC-M1-7 导出两次 `snapshot_id` 与全部文件 sha256 相同 | 满足 | `src/research/tests.rs:1558`；线上记录：#383 评论 3 同库两次导出全部文件含 `manifest.json` 相同。评论 1 称 manifest 只差 `export.started_at`，其后已改为不写运行时间（`src/research/export.rs:231-236`） |
| AC-M1-8 导出期间发生日更则拒绝出清单 | 满足 | `src/research/tests.rs:1647`、`:1080` |
| AC-M1-9 导出只读，库与 WAL 不被写入 | 满足 | `src/research/read.rs:57-70`、`src/research/tests.rs:1261`、`:1675` |
| AC-M1-10 工具数、dataset、wire 形状不变；不复权/因子/日历/指数不能通过 `query_data` 取到 | **未满足（AC 文字过期）** | 后续 PR #410 起因子、日历、指数是 `query_data` 的 dataset（`src/data_supply/wire.rs:21-31`），v10 起 A 股 `adjustment=raw` 也可读。评论 1 已指出。「指数不可经 `market.daily` 见」仍成立（`src/data_supply/mcp/tests.rs:1774`），可保留这一半 |
| AC-M1-11 一次全市场 run 五类数据与同一 generation 一致，单项失败有 item 级结果 | 满足 | `src/data_supply/task_v2/tests.rs:86-102`；线上记录：preview.36 重建 10 个派生 item 全成功（#383 评论 4）。两个越界指数 bar 由 D-51 剔除，见 `src/sqlite_market/index/tests.rs:449` |
| AC-M1-12 schema 21→22 全新 init，旧库不被改写，旧二进制拒绝新 schema | 满足（语义已前移） | `src/sqlite_tasks.rs:471-475`、`src/sqlite_tasks/tests.rs:1444`、`:1857`；现行代码是 schema 27，旧库拒绝的断言对应 24→25 等后续版本；线上记录：回滚演练需连二进制一起回退（#383 评论 1、2） |

### 4.2 #391 M2 按时点的证券主表与可交易性（Issue 已关闭，部署 preview.37）

| AC | 状态 | 依据 |
|---|---|---|
| AC-M2-1 每个代码有上市日或 `unknown` 并带来源，缺失率可枚举 | 无法从仓库验证 | 线上记录：5,565 行，`listing_date` unknown 0，来源 exchange_list 5,559 + tdx_ssdate 6（#446 评论 0，XDM-137）。夹具层面 `src/security_master/assembly/tests.rs:77`、`:135` |
| AC-M2-2 退市日：交易所名单覆盖的有值，仅整理期的标 `arrangement_only`，都无的 `unknown` | 无法从仓库验证 | 线上记录：337 行 `exchange_list`，其余 5,228 `unknown`，`tdx_arrangement` 0。AC 文字里的「`arrangement_only`」不是实现使用的词，实际是 `delisting_source` 取值（`src/security_master.rs:262`），文字应改 |
| AC-M2-3 代码复用：同一 `(code,T)` 只命中一行，命中两行返回 `ambiguous_code_reuse` | 满足（夹具）；真实复用样本无法验证 | `src/security_master/assembly/tests.rs:135`；线上 `sh.600018` 有 2 行、`issuer_resolution=ambiguous` 1 行（STATUS） |
| AC-M2-4 十个历史日股票池与「当日有 bar 且在册」一致，差异逐条给原因 | 满足 | `src/research/read_model_tests.rs:609`；线上记录：XDM-74 补测 only_db=0，XDM-137 逐年差 −3..−10 且归类（#391 评论 3，#446 评论 0） |
| AC-M2-5 股本状态：`sz000001`、`sh600519`（两次事件）+ 39 条抽样事件，3 个人工 oracle 通过，cat5/7/8 不出现 | **未满足（AC 文字过期）** | cat5/7/8 已由 D-50 准入并参与状态，见评论 2 与 #433 第 6 条。`src/research/read_model_tests.rs:1242` 测试状态函数，但仓库内没有 39 条抽样事件或 `sz000001`/`sh600519` 的 oracle 夹具，抽样在仓库外。需要改写 AC 并另做验证 |
| AC-M2-6 A+H 的 A 股总市值返回 `unknown`，不用 C4 冒充 | 满足（库函数）；生产不可观测 | `src/research/read_model_tests.rs:1088`。没有读面（第 3 节） |
| AC-M2-7 ST 状态缺失时涨跌停返回 `unknown`，不按 10% 算 | 满足（库函数）；生产不可观测 | `src/research/price_limit/tests.rs:1049`、`:1067` |
| AC-M2-8 `sz000631` 2006-04-28…2009-06-04 `tradable=unknown` + `unresolved_gap`，不返回 `suspended` | 满足（库函数）；生产不可观测 | `src/research/read_model_tests.rs:987`、`src/security_master/suspensions/tests.rs:100`；线上记录：窗口 754 个缺失日（#391 评论 3）。`market.suspensions` 只返回覆盖 `as_of` 的缺口（STATUS） |
| AC-M2-9 六个板块×时段规则来自规则表且有 `basis_ref`；`high == limit_up` 命中率不低于基线 | 部分：规则部分满足，命中率未满足 | 规则：27 行全部 `basis_status=verified`（`src/trading_rules/a7-d44-v1.json`、STATUS）。命中率回归没有实现（`src/research/price_limit/tests.rs:850` 只验证取整方向）；基线在仓库外 |
| AC-M2-10 规则表插入新行，无代码改动，派生按日期切换 | 满足（库函数） | `src/research/price_limit/tests.rs:688`、`:670`、`:825`（实现里无幅度常量） |
| AC-M2-11 从解析事实层重算，与导出物化值逐行相等，`algorithm_version` 与输入 `version_id` 可查 | **未满足（AC 文字过期，且不成立）** | 评论 2 已指出可追溯性只在库内。设计 §12.4 决定涨跌停、市值、可交易性**不物化进导出**（`src/research/mod.rs:760-762`），所以不存在「导出物化值」可比。能做的是：导出的解析事实可重算，派生需要自己带规则版本 |
| AC-M2-12 工具数、dataset、wire 形状不变；`as_of`/`known_at` 仍被拒绝 | **未满足（AC 文字过期）** | 6 个 M2 dataset 已进 wire，`as_of`/`known_at` 按 dataset 声明接受（`src/data_supply/wire.rs:141-155`、`src/data_supply/wire/normalize/tests.rs:459`）。评论 2 已指出 |
| AC-M2-13 #308 未批准时日更 ST 数据集 `pending_source`，其它正常，逐 item 可查 | 满足 | `src/security_master/assembly/tests.rs:77`；线上记录 XDM-70（#391 评论 2）。该前提已不再成立：D-58 已准入，写入器已部署 |
| AC-M2-14 schema 22→23 全新 init 链尾追加，旧二进制拒绝 | 满足（语义已前移） | 同 AC-M1-12 |

### 4.3 #433 M2 后续（Issue 仍开）

Issue 正文是 6 条跟踪项，不是编号 AC。逐条核对：

| 项 | 状态 | 依据 |
|---|---|---|
| 1. 交易所名单适配器补齐退市日、代码变更、代码复用 | 部分满足 | 代码：`src/official_security/a1a2.rs`；线上记录：退市日有值 337（原 279 个未知已补），`sz.300114→sz.302132` 已 confirmed。`sh.600018` 的上市冲突在主表里是 2 行并存，未被拆成两段使用；`sh.600849` 仍只是 candidate |
| 2. 历史 ST（#308）接入 `risk_warnings` | 部分满足 | 写入器已部署，vm2 上 1,289 行（STATUS）；沪市 2004Q1 前、北交所永久 `unknown` |
| 3. 交易规则原文入库后涨跌停才有值 | 部分满足 | 规则 27 + 2 行已入库（STATUS）；但主板在没有 ST 区间的日子仍是 `unknown`（第 2.3 节），所以「有值」只覆盖创业板 2020-08-24 起、科创板和处于 ST 区间的日子 |
| 4. 证券简称历史（S-6） | 部分满足 | 深市 7,344 行；沪市、北交所 `NOT_AVAILABLE`（D-61）。此条正文「来源待准入」已过期 |
| 5. 读面：涨跌停、可交易性、市值 | **未满足** | 第 3 节。D-75 没有覆盖 |
| 6. AC 文字更新（AC-M2-5/11/12） | **未满足** | #391 的 AC 文字没有改；附录 A 给出建议文字 |

### 4.4 #446 M3 退市股、历史行业与交易所名单（Issue 仍开）

线上记录来自 XDM-137 的 preview.39 首轮验收（#446 评论 0，2026-10-31）和后续评论 1、2；之后又换了 fresh init，数字见 STATUS。

| AC | 状态 | 依据 |
|---|---|---|
| AC-M3-1 `universe(as_of=2015-01-05)` 返回当日在册全部证券，2015 年以来退市的可枚举，成员带 `membership_basis` | 无法从仓库验证 | 线上记录：2,595 个成员，256 个 2015 年后退市且全有 bar。夹具：`src/research/read_model_tests.rs:609` |
| AC-M3-2 含退市股的 10 年回测逐年样本数与在册数一致，偏差逐条给原因 | 无法从仓库验证 | 线上记录：2015–2025 逐年 delta −3..−10，原因归类为 3 个迁移别名 + 24 个整年停牌代码；该核对用的是库内统计，**导出交叉核对「未跑」**（#446 评论 0）。结合第 2.1 节的导出疑点，这一条不能算通过 |
| AC-M3-3 行业带区间/来源/sha256/证据等级，早于起点 `unknown`，不用当前行业冒充 | 满足（读面）；覆盖有限 | `src/research/industry/tests.rs:216`、`:473`；线上记录：起点 2026-09-30 前 `before_coverage`，5,235 行全部 `unknown_dictionary` |
| AC-M3-4 归档的 7 个 `tdxhy.cfg` 回填，重复导入幂等，不可回填标 `unknown` | 无法从仓库验证 | 导入幂等有代码（`src/source_store/import.rs:14`、`:142`）；线上记录：`replay --scope industry` match，entries 11，7 快照（#446 评论 0）；7 个不同内容各一条快照记录这一核对没有贴在 Issue 里 |
| AC-M3-5 generation 退役时缺派生事实则拒绝并给清单，补齐后成功 | 无法从仓库验证 | 门禁测试：`src/generation_gate/tests.rs:75`、`:101`、`:113`、`:142`、`:167`；线上记录：场景 ①② 已复现，③④ 在 XDM-159 以豁免路径完成了两代退役（#446 评论 2）。其中「来源库 blob 读不回」的线上场景没有单独记录 |
| AC-M3-6 连续两代，退市股序列记录数/末根 bar/摘要不变，改写则显式失败 | 无法从仓库验证 | 逻辑：`src/data_supply/delisted_series/tests.rs:64`、`:110`；线上记录：`derived:delisted_series_check` checked 337、unchanged 337，第 1 代日更与 vm2 第一次跨代日更各一次（STATUS）；「改写/消失」的负例只在夹具里 |
| AC-M3-7 A-1/A-2 与 `base.dbf` 对账，退市日 unknown ≤2 并逐条解释，冲突全部落表 | 满足（线上记录） | 退市日 `unknown` 的口径已变：337 个已退市行全部有日期，5,228 个未退市行为 NULL；冲突 2 行上市、15 行退市，两侧值保留（#446 评论 0）。夹具 `src/security_master/assembly/tests.rs:425` |
| AC-M3-8 另一台机器用同一快照逐位复现回测结果 | **未满足（Issue 已声明不在本轮承诺）** | 导出字节可复现（AC-M1-7）；回测结果的复现不在仓库范围；见第 2.4 节 |
| AC-M3-9 同一来源冻结后断网重放，5 个使用方只从冻结快照重建，结果逐位相同 | 无法从仓库验证 | `replay --scope all` 含 5 个 scope（`src/source_store/replay.rs:161-170`），停牌不在其中。线上记录：preview.42 的 fresh init 五个 scope 全部 `database=match`（STATUS）；之后 vm2 的 `risk_warnings` 报 `drift`（STATUS「已知问题」）。停牌 `confirmed` 重放有条件（`inputs_changed`） |
| AC-M3-10 0 行/静默截断窗口标 `not_observed` 或失败，分段 ≤1 季度并校验分页计数 | 满足 | `src/exchange_source/request.rs:187`、`src/exchange_source/tests.rs:270`、`:293`、`:650`、`tests/source_store/exchange.rs:454`、`:485`、`:504` |

### 4.5 #509 M3-2 ETF 日线收录（Issue 仍开，AC 为草案）

preview.45 已部署身份、采集、读面、日更和导出；部署和首采后的线上记录在 STATUS（ETF 1,760 条序列，1,631 只日更成功，128 个退市日，86 只无日线）。

| AC | 状态 | 依据 |
|---|---|---|
| AC-M3-2-1 身份只来自交易所名录；现行 1,717 写入；双向差集为空；86 只 `absent`；128 只候选退市标 `unknown` | 部分满足，余项无法验证 | 名录适配器与重放：`src/official_security/etf_list/tests.rs`、`tests/source_store/exchange.rs:1325`。**与 AC 文字不一致**：128 只不再是 `unknown`，D-71/D-72 追加了退市公告来源并写入精确退市日（STATUS 128 个退市日），AC 文字过期。名录与 `ebs` 的代码集差集为空这一条是取证事实，仓库内无断言 |
| AC-M3-2-2 ETF 精度 3dp；A 股 16,354 条序列 `version_id` 与导出 sha256 改动前后逐一相同 | 部分满足 | 导出侧：`src/research/tests/etf.rs:95`、`:391`（A 股前缀、其余文件字节相同）。「改动前后 16,354 条 `version_id` 相同」需要旧库对照，仓库内不可验；`daily_raw`/`universe_intervals` 因新列整体 sha 必然变，契约已说明（v10 §1.6） |
| AC-M3-2-3 ETF 量为份；手日按标记+量额一致性还原 ×100 写 `volume_quantum=100`；判不清 fail-closed | 满足 | `src/official_market.rs:4247`、`:4292`；`src/data_supply/collect/tests.rs`（`etf_hand_days`） |
| AC-M3-2-4 新 `CatalogScope` 只用于 `market.daily`，`securities.catalog` 拒绝，默认 universe 不含 ETF | 满足 | `src/data_supply/mcp/tests.rs:2779`；`docs/contracts/generic-data-v10.md` §1.1 |
| AC-M3-2-5 首采走 `start_collection` v2，≤500/批，报告 absent，不静默丢 | 满足 | `src/data_supply/collect/tests.rs:621`、`:718`、`:1109`；线上记录：1,631/1,631（STATUS） |
| AC-M3-2-6 首期只交付不复权 + cat1/cat11 因子事实行，带 `basis_status` | 满足 | `src/sqlite_market/derived/tests.rs:226`；schema 027。因子本身**没有任何读面**：ETF 因子不进 `daily_qfq`、`factors` 导出，`market.adjustment_events` 是否对 ETF 开放未在代码里找到测试（推断：可读，未验证） |
| AC-M3-2-7 `adjustment` 参数，ETF 缺省显式报错，能力修订升 v10，`tools/list` 在批准上限内 | 满足 | `src/data_supply/mcp/tests.rs:2932`、`:2946`；`tests/mcp/v8_tool_surface.rs:127`（research 12,021 ≤ 12,200 B，CI run 42017473021） |
| AC-M3-2-8 两份名录冻结进来源库，`replay` 可离线重建身份；不提交交易所原始字节 | 满足 | `tests/source_store/exchange.rs:1325`；`tests/fixtures/exchange-etf-list/` 是合成夹具 |
| AC-M3-2-9 ETF 加入后日更/年线/首采/库体积按 SLO 口径实测并记录 | 无法从仓库验证 | 线上记录：vm2 单次实测 A 股+ETF 6,872 项 write-loop 280.0 s，ETF only 37.0 s（SLO.md、STATUS）；SLO 判定归 owner |
| AC-M3-2-10 幸存者偏差、86 只缺序列、ETF 无行业/无年线逐条写入数据目录 §3.3 与读面说明 | 满足 | data-catalog §3.3 偏差 9、10；v10 §1.6 `limitations[]` |

### 4.6 #308 历史风险警示离线输入准入（Issue 仍开，阶段标签「待澄清」）

Issue 正文的 AC 是「来源准入合同」，不是数据覆盖。D-58（2026-10-28，Issue 评论 15）把 Final Gate 改为按市场分档，并说明「准入阻塞解除」，但 Issue 的复选框没有勾。按正文逐条：

| AC | 状态 | 依据 |
|---|---|---|
| AC-S01 至少一个候选有官方 issuer、可离线保留的原始字节、稳定身份，否则 `NOT_AVAILABLE` | 满足 | 深市 `SSGSGMXX/tab2`、沪市公告索引为 `APPROVED_SOURCE`（评论 15）；字节冻结进来源库 `tests/source_store/exchange.rs:276` |
| AC-S02 访问方式与缓存/长期保留边界有证据，无授权文本如实记 owner-risk | 满足 | 再分发 `NOT_AVAILABLE`、仅本地研究（评论 15、D-20）；沪市生效日锚定为 `OWNER_RISK_ACCEPTED` |
| AC-S03 历史起止、交易所范围、更新频率、修订方式已确认 | 满足（含限制） | 深市 1994-01-03 起；沪市可观测起点 2004Q1，2000–2003 永久 `not_observed`；北交所 `NOT_AVAILABLE`；修订靠同请求取最新快照（`tests/source_store/risk_warnings.rs:101`）。更新频率由日更与 R2 刷新承担 |
| AC-T01 announcement、known、effective 三类时间的来源与缺失策略已冻结 | 满足（含限制） | `src/official_security/risk_warnings.rs:1083-1100`、`src/sqlite_market/risk_warnings.rs:103-109`。深市 `known_at` 是入库日，不是公告日（真实公告日不可得） |
| AC-T02 同日公告、生效次日、盘中未知、更正公告有明确 point-in-time 规则 | **未满足** | 只有同日冲突 fail-closed（`SameDayConflict` 等，`src/security_master/risk_warnings.rs:150-156`）和更正靠最新快照；「生效次日」「盘中未知」没有成文规则。「公告标题含（更正后）」只是分类时不隐藏文档类型（`src/official_security/risk_warnings.rs:1830`） |
| AC-Q01 未找到记录不等于 `normal`；只有覆盖完整性证据允许闭世界结论 | 满足 | `src/research/universe.rs:801`（无区间即 `Unknown`）；`src/sqlite_market/risk_warnings.rs:101-103`。**副作用见第 2.3 节**：因为没有闭世界证据，生产里永远得不到 `normal` |
| AC-Q02 冲突、缺失、代码复用、名称变化均 fail-closed | 满足 | `src/security_master/risk_warnings.rs:150-160`（`SameDayConflict`、`BrokenChain`、`Duplicate`）；缺失走 `not_observed` 窗口切断区间 |
| AC-E01 三个预冻结样本能从官方原始字节重放事件区间与 digest | 无法从仓库验证 | 仓库内是合成夹具（`tests/fixtures/exchange-risk-warnings/README.md`）；三个样本的 oracle 在 Issue 评论里（Stage B），重放结果贴过（XDM-137 `replay --scope risk_warnings --verify-only` match，其后 vm2 报 `drift`，STATUS） |
| AC-E02 样本通过不外推全市场，全市场导入前有独立 coverage gate | 满足 | `CoverageWindow`（`src/security_master/coverage.rs`）与 `not_observed` 切断；覆盖起点写进数据集证据（`src/sqlite_market/risk_warnings.rs:106-111`） |
| AC-C01 `securities.status_history` schema 与 version/lineage/query 契约经评审冻结 | **未满足（AC 文字过期）** | 实际 dataset 是 `market.risk_warnings`，不是 `securities.status_history`（`src/data_supply/wire.rs:40`）；字段也不同（`normalized_status` 取值 8 个，含 `marker_only_unresolved`）。需要改写 AC |
| AC-X01 #298 的 `legacy_approximate` 继续标注 unavailable，只有固定 status version 才能升级 | 无法从仓库验证 | #298 已关闭，下游 `x` 仓库的字段不在本仓库；仓库内搜不到 `legacy_approximate` |
| Final Gate 明确 APPROVED / OWNER_RISK / NOT_AVAILABLE | 满足 | 评论 15 的分档，D-58 |

### 4.7 #387 v8 面向 AI 的 MCP 工具面（Issue 仍开）

线上落地：preview.36 起 v8 在 ops 上，preview.38 起三 socket，隧道在 aws-node 指向 `research.sock`（STATUS）。注意 vm2 无隧道（D-74）。

| AC | 状态 | 依据 |
|---|---|---|
| AC-V8-1 研究入口 5 工具、≤11,000 B，与写/核实/采集工具交集为空 | **未满足（预算已按裁决上调）** | 工具数与交集：`tests/mcp/v8_tool_surface.rs:38`、`:412`。字节：实测 12,021 B，CI 上限已改为 12,200 B（`tests/mcp/v8_tool_surface.rs:127`）。**D-75 将 research 再上调到 18,600 B 并新增 10 个工具**，那时 AC-V8-1 的「5 个工具」也不再成立 |
| AC-V8-2 ops ≤27,500 B、maint ≤14,500 B、acquire 不变 | **未满足（预算已上调）** | ops 实测 30,748 B，上限 30,800 B；maint 13,523 B 满足；acquire 6,040 B 在仓库内没有断言。ops 工具数设计是 15，现 16 |
| AC-V8-3 任意入口不含 `outputSchema`；`output_schema` 与 registry 逐字节相等 | 满足 | `tests/mcp/v8_tool_surface.rs:105-141`、`tests/mcp/data_stdio.rs:102`、`tests/mcp/data_supply.rs:1328` |
| AC-V8-4 T1 单股一年日线 1 次、≤5.0K token | 满足（夹具） | `fixtures/v8/query-panel-242.json` 实测 4,127 token（`scripts/v8_token_budget.py`）；`tests/mcp/v8_read_v2.rs:182` |
| AC-V8-5 T2 某日全市场涨幅前 10：1 次调用、≤40K token | **未满足（推断，需线上实测）** | 夹具只有 3 个实体（340.8 token，预算 400）。代码阅读：① 分页按行切，一行是一个实体一天（`src/sqlite_supply.rs:1036`），`page_size` 上限 1,000（`src/data_supply/query.rs:329-333`、`:684`），2 个交易日 × 5,224 实体 = 10,448 行，至少 11 页，不是 1 次；② 每页的响应都带全部实体列表和全部版本清单（`src/data_supply/columnar.rs:903-968` 用 `plan.entities` 与完整 manifest），按夹具形状放大，仅 `versions[]` 一项约 20.9 万 token（5,224 行 × 约 40 token），整页约 24.6 万 token。设计 §5.5 估算的 138.6 KB / 34.7K token 没有计入每实体一行的 `versions[]`，也没有计入分页。设计 U-3「全市场截面的真实字节」从未实测 |
| AC-V8-6 T4 某行业 236 只：≤2 次、≤2K token | 满足（夹具） | `src/data_supply/mcp/tests.rs:2268`（facets 与目录一致）；`fixtures/v8/query-catalog-industry.json` 244 token（预算 300） |
| AC-V8-7 发现成本 ≤3.5K token，无过滤 capabilities ≤1.5 KB | 部分满足 | 紧凑目录 ≤1,500 B：`tests/mcp/v8_tool_surface.rs:284-309`。研究入口 `tools/list` 12,021 B ≈ 3.0K token（按 4 B/token），加 1.5 KB 约 3.4K，在预算内；D-75 实施后 18,600 B ≈ 4.7K token 将超出。未实测 |
| AC-V8-8 T3 数据是否最新：1 次、≤600 B | 部分满足 | 一次调用、一行聚合：`src/data_supply/mcp/tests.rs:2232-2262`。该测试的字节断言是 ≤700 B，比 AC 的 600 B 宽 100 B，仓库内没有断言 ≤600 B，实际字节数未知 |
| AC-V8-9 T5/T6/T7 Devin eval：次数与 token 不高于 v7 | 无法从仓库验证 | Devin 不可用，Ops-C 改用 pi 实测（#387 评论 2）；`tests/mcp/v8_task_v2.rs` 只覆盖诊断形状与 token 预算 |
| AC-V8-10 14 个拒绝逐条友好或可操作，业务语义不变 | 部分满足 | `src/data_supply/wire/normalize/tests.rs` 有 12 个 `rejection_*` 测试，`src/data_supply/task_v2/tests.rs:82`（`item_id` 往返）、`:185`（`task_contract_version` / `wire_contract_version`）覆盖设计表的第 9、14 行。测试编号与设计 §7.2 的 15 行不是一一对应，第 12、13 行（`update_data.items` 判别、写 selection 命名）没有找到对应测试 |
| AC-V8-11 `as_of`/`known_at` 返回 `reserved_for_m2` | **未满足（AC 文字过期）** | M2 已按 dataset 声明接受，其它 dataset 返回 `unsupported_for_dataset`（`src/data_supply/wire/normalize/tests.rs:452-459`，测试注释写明替代 AC-V8-11） |
| AC-V8-12 text 分级：研究入口 text==structuredContent；ops 大响应摘要；三档可用 | 部分满足 | 三档可用与默认值：`tests/mcp/v8_read_v2.rs:306`、`:382`。Ops-C 实测摘要不够用，**默认改回 `full`**（#387 评论 2，D-36 退回分支），所以「编排入口大响应 text 是有界摘要」不是现行行为 |
| AC-V8-13 `capability_revision=generic-data-v8`，data 工具 24 | **未满足（AC 文字过期）** | 现为 v10、25 个工具 |
| AC-V8-14 文档与门禁全绿 | 满足 | `check_current_facts.py`、`check_public_hygiene.py` 本地通过；CI run 42017473021 success |

### 4.8 合计

| Issue | 满足 | 部分满足 | 未满足 | 无法从仓库验证 | 合计 |
|---|---|---|---|---|---|
| #383 | 11 | 0 | 1 | 0 | 12 |
| #391 | 8 | 1 | 3 | 2 | 14 |
| #433（6 项跟踪项） | 0 | 4 | 2 | 0 | 6 |
| #446 | 3 | 0 | 1 | 6 | 10 |
| #509 | 7 | 2 | 0 | 1 | 10 |
| #308 | 8 | 0 | 2 | 2 | 12 |
| #387 | 4 | 4 | 5 | 1 | 14 |
| **合计** | **41** | **11** | **14** | **12** | **78** |

注意：#391 的 AC-M2-6/7/8/10 记为「满足」，依据是库函数的夹具测试；它们在生产上不可观测（第 3 节）。

## 5. `docs/data-catalog.md` 的过时说法

按你的要求，有把握的已改（本 PR 的 `docs/data-catalog.md`），没把握的只列出。判断依据是代码、`git merge-base --is-ancestor` 对
部署版本的祖先检查和 STATUS。

### 5.1 已修正

| 位置（修正前） | 旧说法 | 现在 | 依据 |
|---|---|---|---|
| 头部 | 「最后核实 2026-10-27」 | 2026-11-05，对照 main `e1e6d99` | — |
| §1.1、§1.2 | 契约指向 `generic-data-v7` | 指向 v10，v7/v8/v9 作历史 | STATUS：`generic-data-v10` 已部署 |
| §2 开头 | 「三个已部署数据集」 | 三个基础数据集，共 13 个 | `src/data_supply/wire.rs:11-49` |
| §2 开头 | 「交易所上市/退市名单与 A 股总股本已准入」「写入器已随 preview.39 部署」混写 | 分开写：A-1/A-2、A-3、A-7、ST、简称写入器已部署；A-4 只准入当前快照，**没有写入器** | `src/` 内没有 A-4 适配器；`market_cap.rs:30-34` |
| §2.1、§2.2 覆盖范围 | 「只有当前成员」「退市日线不在库内」 | 338 条退市序列在库，经 `sh_sz_a_listed_ever` 首采 | #446 评论（XDM-137）；STATUS |
| §2.2 身份、字段表 | 「`query_data` 只读前复权」；价格「已部署实例是 2 位小数」 | 缺省前复权，A 股显式 `raw` 可读（v10）；6dp / 3dp | `docs/contracts/generic-data-v10.md` §1.5；`src/compact_market.rs:17-24` |
| §2.2 已知限制 3、4、5 | 「只有当前成员」「写入器与规则入库未完成」「库内仍是修复前的值（19 根）」 | 退市序列在库；写入器均已部署；`d738488` 是 preview.36 祖先，重建后已修复，旧数字标历史 | `git merge-base --is-ancestor d738488 fcb86ab`；#383 评论 2 |
| §3.1「M2 部署状态」 | 「交易所上市/退市名单与 A 股总股本仍待准入」「规则书 preview.37 实例仍是 0 行，下一次发布后才写入」「历史 ST 写入器（代码已合并，未部署）」 | 名单已准入并部署；规则书已入库 27 + 2 行；写入器已部署 | `07d6178`、`8bd0592`、`d39a327`、`716c8e1` 均是 preview.39 的祖先；STATUS |
| §3.1「M2 已知限制」 | 用 2026-10-27 的数字（279 个多出实体、152 行退市日）作为现状 | 标为历史，指向 STATUS | STATUS 现为 337 行 |
| §3.1 同上 | 「`delisting_unknown` 标记……严格口径可据此排除」 | 该标记条件只是退市日为空，所有在市股票都满足，不能用来筛 | `src/research/universe.rs:713-715` |
| §3.1「M3 代码状态」 | 标题「代码状态（main，2026-10-30）」「下表『未部署』字样保留为编写时的状态」 | 「M3 状态」，PR-1a…12 全部部署；改说各数字是各自时点读数 | 提交祖先检查 |
| §3.1 | 「#446 的 10 条验收……每条状态均为『未执行』」 | 首轮已于 2026-10-31 执行，见 M3 设计 §14.12 | #446 评论 0 |
| §3.1 M2 验收建议 | 与 #391 Issue 的 AC-M2-* 同编号但内容不同，没有说明 | 加一段说明（M3 一节已有同类说明） | 对照 #391 正文 |
| §3.2 第 6–13 行状态格 | 「代码已合并，未部署」「preview.37 部署」「待发布」 | 「已部署」或具体状态；第 12 行补「无读面」 | 同上；审计 §3 |
| §3.2 第 6、7、9、10 行正文 | 「未部署，未在本实例实测」「数字取自 2026-10-15 快照」 | 换成 2026-10-31 的实测值 | #446 评论 0 |
| §3.2 第 8 行 | 「未部署、未做真实首采」 | 已首采 338/338，已加不消失检查 | 同上 |
| §3.2 第 13 行 | 「身份/采集/读面代码已实现，未部署」「PR-D……（未部署）」 | 已随 preview.45 部署到 vm2；**客户端连的 aws-node 看不到 ETF** | STATUS 总览；D-74 |
| §3.2 表后引用块 | 「preview.37 实例仍 0 行，待发布」「M3 其余已合并能力……一律未部署」 | 全部已部署；仍没读面的是涨跌停派生、可交易性、市值 | 同上 |
| §3.3 偏差 1、2、5、6、7、8、9 | 「已合并、未部署」「写入器未部署期间两市 ST 都是 unknown」 | 已部署；改为「没有区间覆盖的日期是 unknown，从不写 normal」；`strict` 无 MCP/导出入口 | `src/security_master/risk_warnings.rs:417-446` |
| §4.1 | 「部署前 `risk_warnings` 仍写 0 行」「preview.37 实例上仍是 0 行，需下一次发布」「判断涨跌停……不能按 10% 自行推算」 | 改为现状：无派生读面、无 `normal`、除权日基准不准 | 审计 §2.3、§3 |
| §4.2 | 「事件表没有作为数据集提供」「截面标准化……没有时点行业、没有 T 日股票池」「市值因子……缺股本历史」 | `market.adjustment_events`、`capital_events` 已是数据集；股票池有、行业从 2026-09-30 起；市值可自己算但无读面、A 股总市值 `unknown` | `src/data_supply/wire.rs:25-39` |
| §4.3 | 「没有幸存者偏差的回测：缺退市股行情与时点主表」「逐位可复现……v2」 | 数据层已有，导出是否含退市序列未验证；现行 `v4`，清单无二进制版本 | 审计 §2.1、§2.4 |
| §5.1 | 「`universe.scope` 只接受 `sh_sz_a_listed_ever`」 | 4 个值，只对 `market.daily` | `src/data_supply/query.rs:269-300` |
| §5.3 | 标题「`v4` 代码已合并未部署」；「当前代码 25」；「v4 本身从未部署」；「M3 数据未上线」 | `v4` 已部署；27；v4 已随 preview.42 部署，同一字符串对应两种列布局 | `git show 803b35c:src/research/mod.rs`；STATUS |

### 5.2 不确定，没有改

| 位置 | 说法 | 为什么没改 |
|---|---|---|
| §3.2 第 4 行 | 「部分提供（preview.37 部署）」，备注「京市 1,479 条无官方对照」 | 部署版本对，数字取自 2026-10-27，没有新的对照数据；STATUS 的 128,338 行是 2026-11-01 的值 |
| §3.2 第 5 行 | 「市值……可派生，preview.37」 | 同上；「没有服务端市值数据集」仍然成立 |
| §3.2 第 12 行 | 「剩余偏差」里 D-64 的行数、`min_band_ticks` 的取值清单 | 没有逐行对 `a7-d44-v1.json` 核对每个日期段；只核对了 27 行的总数和 `basis_status` |
| §3.2 第 13 行及「ETF 的取证结论」段 | 1,628/127/86 等取证数字 | 数字属 2026-10-26 的取证报告，与 #509 的 1,631/128/86 有差；报告在仓库外，无法核对 |
| §3.3 偏差 8 | 「名单修订、历史 ST 公告完整性……都无法用后来的重抓重放」 | 与 STATUS 的 R1–R5 刷新机制（`full` 检测修订）有张力，没有逐条核对 |
| §2.3 年线价格精度 | 「`open` ……前复权 2dp」 | `src/yearly.rs` 没有显式 scale 常量，年线是否随前复权升到 6dp 我没有确认 |
| §1.3 | 「官方公告索引的查询边界」整段 | 这是 2026-10-28 至 11-01 的实测，未重测；与 D-44/D-59 一致 |
| §2.2 已知限制 1 | 全库统计（12.6%、753,414 根、725 个实体） | 2dp 时代的统计，已标历史；退市序列加入后总体会变，没有重算 |
| 审计 §4.5 AC-M3-2-6 | ETF 的 cat1/cat11 事实行能否经 `market.adjustment_events` 读到 | 代码里没找到 ETF 的 adjustment_events 读取测试；#509 AC-6 仅说「事实行入库」。不是 data-catalog 的说法，列在这里是因为 T14 要确认 |
| `docs/STATUS.md` 里的同类说法 | 「aws-node 上 11-03 日更首次发布沪市区间」等 | 不在本任务范围（只改 data-catalog）；这些带「历史」标记 |

## 6. 建议任务

规模是粗估：**S** ≤ 1 天，**M** 2–4 天，**L** 1–2 周；按已有 PR 的体量估，没有实测。依赖里的 T 编号指本节。
优先级按「会让回测结论出错」排：T1、T2 先于其它。

| 编号 | 任务 | 范围 | 依赖 | 规模 |
|---|---|---|---|---|
| T1 | **核实并修复默认导出遗漏退市序列** | 先在 vm2 只读库上跑 `export_research --dry-run`，看 `coverage.delisted_series` 与 `series[]` 是否含 338 条退市序列。若确实缺：`stock_series` 的成员集改为目录 ∪ 主表 A 股 ∪ 已发布序列（与 M3 设计 §10.4 一致），加一个「退市代码不在目录」的夹具测试（现有夹具的退市股同时是目录成员），改 `LIMITATIONS`。导出 schema 不变 | 无（先验证） | S（验证）+ M（修复） |
| T2 | **派生读面：涨跌停价、可交易性、市值** | 在 D-75 的工具批里加三个只读工具（或 `query_data` 的三个 dataset），薄封装 `price_limit`、`tradability`、`market_cap`，响应带 `reason`、`algorithm_version`、`rules_version`、`temporal`。市值要带口径（`float_a`/`company_total`/`a_share_total`）。要先定：返回单日单实体还是区间；`research` 的 `tools/list` 预算（D-75 已上调到 18,600 B）是否放得下 3 个工具（每个约 700 B） | T3 之后更有意义；可先做 `tradability` 与市值 | M |
| T3 | **闭世界证据：让 `normal` 可得** | 为 SZ（1994-01-03 起的简称表覆盖）与 SH（2004Q1 起事件级公告）分别定义「覆盖到该日且无区间 ⇒ `normal`」的条件，写进 `risk_warning_intervals` 的覆盖表或读时规则；沪市 2004Q1 前、北交所维持 `unknown`。需要 owner 裁决：SH 的事件级来源是否足以做闭世界结论（#308 AC-Q01 当前的口径是「不能」）。没有这条，主板涨跌停在读面上几乎全是 `unknown` | owner 裁决（D-58 的延伸） | M（实现）+ owner 决定 |
| T4 | **除权日涨跌停基准** | `limit-v1` 在除权日用交易所除权参考价。参考价可由 `adjustment_events`（cat1）与前收算出，或直接用 `.day` 第 8 字段 `r7_raw`（1999–2010 是官方除权基准价，2010-09 起退化，#240）。先取证 2010 年后的口径，再定算法版本 `limit-v2`，旧版本结果可重算。加除权日夹具 | 无 | M |
| T5 | **A 股主表行的 `known_at`** | 给 A 股 `security_master` 行一个有出处的知晓时间（例如交易所名单的首次冻结日，与 ETF 一致），使带 `known_at` 的股票池可用。需区分「上市日（生效）」与「入库日（知晓）」；上市日早于首次采集的行只能记入库日，这是该口径的固有偏差，要写明 | 无；与 T3 无关 | S–M |
| T6 | **市值的 G2 对账来源入库** | `base.dbf` 的 `ZGB`/`LTAG` 当前值解析后没有存。加存储列或派生表，让 `market_cap` 的 `low_confidence` 能在生产运行；A-4 的当前 A 股总股本（沪深已准入当前快照）可顺带入库，用于 `a_share_total` 的「当前值」口径（历史仍 `unknown`） | 可与 T2 同批 | M（含 schema 迁移，按 D-08 fresh init） |
| T7 | **导出清单补算法版本与二进制版本** | 清单 `algorithms` 增加 `limit-v1`、`market-cap-v1` 等派生算法版本与产生它的二进制 `git_sha`；明确 `snapshot_id` 是否应包含它们（建议不含，只记录，避免内容相同的快照被拆开）。同时解决「同一 `v4` 字符串两种列布局」：要么升 `v5`，要么在清单里加 `column_layout` 并在契约里写明 | 无 | S |
| T8 | **导出保留与 generation 退役的对账** | #383 评论 5 延后的「导出保留策略」至今没有裁决。定：导出目录保留多久、谁清理、`snapshot_id` 与来源 generation 退役的关系（generation 字节退役后，旧快照只能靠导出本身复现） | owner 裁决 | S（文档与脚本）+ owner 决定 |
| T9 | **全市场截面的真实字节与分页** | 在 vm2 上对 `selection.kind=universe` + `cross_section` 的 2 个交易日全市场做一次真实调用，记录页数、字节、token。若如推断需要 ≥11 页且每页带全部 `versions[]`：让截面响应省略逐实体 `versions[]`（保留 `catalog_version` 与摘要），或把截面分页改为按实体切。改动影响 wire 形状，要走契约版本。与 #295 / XDM-14 的全市场筛选并行 | 无；与 D-75 的 `get_market_snapshot` 同一个入口，应在其 PR-1 前定 | S（测）+ M（改） |
| T10 | **AC 文字改写（文档）** | 按附录 A 改 #391 的 AC-M2-5/11/12、#387 的 AC-V8-1/2/11/13、#383 的 AC-M1-10、#308 的 AC-C01、#509 的 AC-M3-2-1，并更新 Issue 复选框与阶段标签。只改 Issue/文档，不改代码 | 无 | S |
| T11 | **#308 的 AC-T02 成文** | 写出同日公告、生效次日、盘中未知、更正公告的 point-in-time 规则，并各加一个测试。当前只有同日冲突 fail-closed。规则需 owner 确认（尤其「沪市生效日按公告日」在盘后公告时是否应推到次日） | owner 裁决 | S–M |
| T12 | **修 `replay --verify-only` 的 `risk_warnings` drift** | STATUS 已记「修复待开发」：同一公告出现在多个查询页时，replay 与日更派生的页顺序不同，105 行沪市记录的 `source_document_sha256` 不同。让两条路径用同一个页序（例如按请求指纹 + 页号排序），加回归测试 | 无 | S |
| T13 | **停牌进入 `replay --scope all`** | 当前 `all` 只含 5 个 scope，停牌需要单独跑，且曾报 `inputs_changed`。让停牌的重放输入摘要不依赖 listed-ever 首采的时间线，再纳入 `all` | T12 之后做更稳 | M |
| T14 | **ETF 因子与成员的读面检查** | 确认 ETF 的 `market.adjustment_events`（cat1/cat11 事实行）经 `query_data` 可读，补一个测试；决定 ETF 名称搜索（D-75 的待办）与 ETF 年线/前复权是否排期（#509 PR-E，依 owner） | 无 | S（确认）/ M（前复权） |

不建议现在做：**把派生结果物化进导出**（`price_limits.parquet`）。设计 §12.4 有意不物化，原因是同一 `version_id` 的文件不应随规则变化；T2 的读面加 T7 的算法版本已够用。

### 6.1 审核补充（Claude，2026-11-05）

本文写于 main `e1e6d99`，下面是审核时核实或之后变化的内容，优先于上文：

| 项 | 状态 |
|---|---|
| T1 | **已验证，确有缺陷。** vm2 preview.45 上 `export_research --dataset daily --adjustment raw --dry-run`：`coverage.delisted_series=0`，`series` 6,984 条（5,224 A 股 + 1,760 ETF）；库里 A 股实体 5,562 个，338 个退市股不在导出里。第 2.1 节的「推断，未验证」到此结束。修复在办 |
| T2 | 已裁决（D-78），设计在办 |
| T9 | `universe` 分页和 `versions` 瘦身在办。`get_market_snapshot` 已随读取工具 PR-1（#536）合并，「应在其 PR-1 前定」不再成立 |
| T12 | 已由 #533 修复，等发布后用 `replay --verify-only` 确认 |
| T3、T4、T5、T6、T8、T11 与附录 A | 需要 owner 判断的部分汇总到决策备忘录，尚未裁决 |

第 1 节第 3 条列的其余问题仍是代码推断，没有在真实库上验证。

## 附录 A：AC 文字改写建议

供 T10 使用。每条给出原文要点、问题、建议文字。

| AC | 问题 | 建议文字 |
|---|---|---|
| #383 AC-M1-10 | 「不复权/因子/日历/指数不能通过 `query_data` 取到」已不成立 | 「`market.daily(sh.000001)` 返回 `absent`（指数只经 `market.index`）；A 股不复权经显式 `adjustment=raw` 读取」 |
| #391 AC-M2-5 | 要求 cat5/7/8 不出现在状态里；D-50 已准入 | 「状态 = 生效日 ≤ T 的最后一条 D-50 准入类别（2/3/5/7/8/9/10）的后值；`suspicious` 事件不参与；链断点之后为 `unknown`；人工 oracle 仍取 `sz000001`、`sh600519`，抽样事件清单随 Issue 提交」 |
| #391 AC-M2-11 | 要求与「导出物化值」逐行相等；涨跌停/市值/可交易性不物化 | 「导出的解析事实可由 generation 字节重算（逐行相等）；派生结果由读时算法版本（`limit-v1`、`market-cap-v1`）与输入 `version_id` 决定，同输入同结果」 |
| #391 AC-M2-12 | 「`as_of`/`known_at` 仍被拒绝」 | 「`as_of`/`known_at` 仅在 `get_data_capabilities` 声明 `temporal` 的数据集上接受，其它数据集返回 `invalid_arguments` + `unsupported_for_dataset`」 |
| #387 AC-V8-1 | research 5 工具 ≤11,000 B | 「research 入口工具集与写/核实/采集工具名交集为空；`tools/list` 字节不超过 CI 当前上限（`tests/mcp/v8_tool_surface.rs`，D-75 后 18,600 B）」 |
| #387 AC-V8-2 | ops ≤27,500 B / 15 工具 | 「ops 16 工具、≤30,800 B；maint 9 工具、≤14,500 B」（以 CI 常量为准） |
| #387 AC-V8-5 | 全市场截面 1 次、≤40K token | 实测前不改数字；加「分页后总调用数与总 token」两栏，T9 实测后定 |
| #387 AC-V8-11 | `reserved_for_m2` | 同 #391 AC-M2-12 |
| #387 AC-V8-13 | `generic-data-v8` / 24 工具 | 「`capability_revision` 与工具数以 `get_data_capabilities` 和 STATUS 为准，不在 AC 里写死」 |
| #308 AC-C01 | dataset 名 `securities.status_history` | 「`market.risk_warnings` 的 schema、version、lineage、query 契约已冻结（`docs/contracts/generic-data-v8.md`）」，并说明 `normal` 不会写入 |
| #509 AC-M3-2-1 | 128 只候选退市「标 `unknown`」 | 「128 只退市 ETF 经 D-71 公告索引命中并带 D-72 提取的精确退市日；无提取结果的成员 `delisting_date` 为 `unknown`」 |

## 附录 B：证据索引

- 部署事实：[STATUS](../STATUS.md)；性能门禁：[SLO](../SLO.md)；决策：[DECISIONS](../decisions.md) D-30、D-44、D-50、D-58…D-61、D-64、D-68、D-70…D-75。
- 设计：[M1](issue-383-m1-research-data.md)、[M2](issue-391-m2-security-master.md)、[M3](issue-446-m3-delisted-industry.md)（§14.12 为首轮验收）、[M3-2](issue-509-m3-2-etf.md)、[v8](issue-387-v8-ai-tool-surface.md)、[读取工具拆分](issue-525-read-tools.md)。
- Issue 评论（仓库外）：#383 评论 1–5、#391 评论 2–5、#446 评论 0–2、#387 评论 0–2、#308 评论 15、#433 评论 0。
- 本地跑过：`python3 scripts/check_current_facts.py`、`python3 scripts/check_public_hygiene.py`、`python3 scripts/v8_token_budget.py`。没有编译、没有运行 Rust 测试。
- CI：main `e1e6d99` 的 run `42017473021` 为 success（job `rust`、`export`）。

## 附录 C：Also noticed（不在本任务范围）

- `docs/STATUS.md` 与 `README.md`、`docs/design.md`、`docs/daily-sync.md` 里还有类似的「`pending_source`」「未部署」措辞（`README.md:59-66`、`docs/design.md:418-423`、`docs/daily-sync.md:28-33`），本任务只改了 `docs/data-catalog.md`。
- `docs/design.md:418-423`：「PR-10/11/12……未做」与已部署事实相反。
- `docs/exchange-source.md:156-158`：R1 的 `ReusePolicy` 注册 9 个来源，代码 `reuse.rs` 现在是 11 个（`policies: [ReusePolicy; 11]`）。
- `src/research/read.rs:1094-1097` 的注释称 `daily_raw` 成员集含退市尾部，与同文件 `stock_series` 的实现不符（T1 的一部分）。
- `docs/contracts/generic-data-v10.md` §1.6「v4 从未部署」与 preview.42 已部署 v4 不符。
