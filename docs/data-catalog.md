# 数据目录（研究数据层）

> **状态**：当前可用数据的入口说明。最后核实 2026-10-27。
> **范围**（owner 2026-10-26 确认）：价量因子，加股本和市值；只做日线；A 股加指数基准；
> ETF 以《ETF 收录前取证调研》（2026-10-26）的结论为准；先做批量导出。首期目标为**严谨回测级**（§3.1）。
> **本文不写**：工具参数 schema（见 §1.2 的契约与 `get_data_capabilities`）、实体数与日期范围
> 等部署事实（见 [STATUS](STATUS.md)）。易变数字只在 §1.3 声明的核实日期内有效。

## 1. 说明与权威入口

### 1.1 本文回答什么

给使用方（owner 本人、AI、下游 x 仓库）一个入口，回答三件事：

1. **现在能拿到什么数据**——三个已发布数据集的身份、覆盖、字段与语义（§2）；
2. **缺什么、为什么缺、计划在哪一期补**（§3）；
3. **按用途现在能做什么、哪些结论不可靠**（§4），以及怎么读、有哪些读取上限（§5）。

本文不复述字段 schema：wire 形状由运行时 `get_data_capabilities` 返回，契约增量见
[generic-data-v7](contracts/generic-data-v7.md)。本文写的是**语义与证据**，不是协议定义。

### 1.2 权威入口

| 内容 | 权威入口 |
|---|---|
| 工具清单、输入输出 schema、调用示例 | 运行时 `get_data_capabilities`（`src/data_supply/registry.rs` 的 `entries()`）；[generic-data-v7](contracts/generic-data-v7.md) |
| 实体数、日期范围、部署版本、generation、新鲜度 | [STATUS](STATUS.md)（唯一权威；本文只链接） |
| 单版本存储、`version_id`、事件日志、年线 stale 分类 | [latest-only-storage](latest-only-storage.md) |
| `.day` 字段、单位与 `r7_raw` 语义 | [day-file](day-file.md)、`evidence/issue-240-r7-analysis/README.md`、`evidence/issue-239-day-volume-amount/README.md` |
| 年线聚合规则 | [yearly](yearly.md) |
| 产品范围与首期交付 | [#1](https://github.com/liqiangcc/x-data-mcp/issues/1) |
| 缺口的需求侧来源 | 《研究数据层需求与缺口》《ETF 收录前取证调研》（2026-10-26 的私有调研报告；结论已在本文逐条复核） |

### 1.3 证据与核实口径

- 每条结论附证据位置：`file:line`（源码/DDL/文档）、Issue/PR，或明确标注的只读实测。
- **只读实测（2026-10-26）** 指：对生产库以 `file:…?mode=ro` + `query_only=1` 执行的小查询，
  或对 active generation 内官方 ZIP 成员的只读解析。查询按主键/范围或流式聚合，未改任何文件。
- 报告数字引用前已重新核实。**与调研报告不一致处已在本文更正**，并在 §7 记录原因。
- 证据分级：**实测** / **推断**（由实测外推，写明依据）/ **未验证**（缺证据，写明取证方法）。
- **官方公告索引的查询边界（2026-10-28 复核）**：上交所公告索引（`queryCompanyBulletinNew.do`）
  在日期区间超过**一个季度**时**静默返回 `total=0`**（不报错）；归档起点实测为 **2000-01**
  （1998–1999 各 1–3 个月窗口均为 0，2000-01 有 21 条、2000Q1 有 252 条）。所以查询必须按
  **≤ 1 季度分段**，且任何「区间内 0 条 ⇒ 不存在该事件」的结论都不成立。D-44 中「公告索引
  1993–1999 返回 0 条」这条依据因此失效（结论保留，依据已换，见 D-44 与 D-59）。
- 「历史」标记的段落只作记录，不是当前事实。

## 2. 已提供的数据

三个**已部署**数据集：`securities.catalog`、`market.daily`、`market.yearly`
（`src/data_supply/wire.rs:9-20`）。三者都用同一个实体身份。

M2 的 6 个解析事实数据集（`market.security_master`、`security_names`、`capital_events`、
`risk_warnings`、`suspensions`、`price_limit_rules`）随 preview.37 **已部署**（v8 的
`contract_version:"v2"` 面上可读）。其中 `security_master`/`capital_events`/`suspensions`
有写入路径；交易所名单与 A 股总股本仍待准入，历史 ST（#308）与简称历史（D-61）写入器
已实现（代码已合并，未部署），停复牌公告（PR-11）写入器也已实现（代码已合并，未部署），
涨跌停规则原文已按 D-59 裁决准入；默认/非 ops 形态下来源未接入或未配置的部分在 item
结果里按 `pending_source` 记录，生产形态（ops + compact）缺 `X_DATA_MCP_SOURCE_STORE`
时四个同源 item 以稳定码 `source_store_unconfigured` 失败（见 §3.2）。未登记版本返回
`absent` 与 0 行（见 §3.1「M2 部署状态」）。
数据库 compact schema：**生产仍是 24，代码是 25（未部署）**。代码里的 schema 25（M3 PR-1，
[#446](tasks/issue-446-m3-delisted-industry.md) §9）加 5 张来源库索引表与
`price_limit_rules.min_band_ticks` 可空列。5 张索引表目前为空、无写入路径；`min_band_ticks`
已由 `limit-v1` 与规则书写入（见 §4 序号 12），导出 `price_limit_rules` 也因此多一列
（旧 parquet 无此列为 null）。

**实体身份**：`{exchange, code}`，`exchange ∈ {sh, sz}`，`code` 为 6 位数字，展示形式
`sh.600000`（`src/data_supply/wire.rs:34-56`）。非规范实体在写入侧 fail-closed
（`src/compact_market.rs:787-795`）。北交所、B 股、基金、指数目前都不在这个身份空间里
（§2.1 覆盖范围、§3）。

### 2.1 `securities.catalog`

**身份**：`{exchange, code}`，另带 `name` 与 `industry`（`wire.rs:173-190` 的 `Row::Catalog`）。

**覆盖范围**：`securities.catalog` 的 selection 仍只接受 `{"scope":"current_sh_sz_a"}`
（`wire.rs` 的 `CatalogScope` 另有 v2-only 的 `sh_sz_a_listed_ever`，只作为
`market.daily` `start_collection` 的写选择，目录读写都显式拒绝它），
含义是冻结规则 S：`f0 ∈ {0,1}` 且 TDX 行业码（`f2`）与证监会形态码（`f5`）都非空
（`src/catalog_tdx/scope.rs:1-8,70-95`）。规则 S 之外的行被显式分到三类并计数：北交所
（`f0=2`）、B 股（`200xxx`/`900xxx`）、异常行（`scope.rs:10-24,84-95`）。
因此：

- 只含**当前**沪深 A 股快照；没有上市日、退市日、ST 状态、历史行业（DDL
  `migrations/tasks/004_catalog.sql:5-11`）。
- 官方 `hsjday.zip` 里有、目录里没有的 A 股代码（退市/摘牌）**不在**本数据集内（§3.2 #8）。
- 目录里有、官方 `.day` 缺成员的实体仍在 catalog 中，但没有行情序列：采集 fail-closed，
  不伪造空序列（`get_data_availability` 对该实体返回 `absent`，见 §2.2 可用性）。

**字段表**

| 字段 | 类型 | 语义 / 单位 | 可空 | 来源与证据 |
|---|---|---|---|---|
| `entity` | `Security{exchange, code}` | 交易所 + 6 位代码；`sh.600000` | 否 | `wire.rs:34-56`；`compact_market.rs:787-795` |
| `name` | string | **当前**简称；不是历史简称，没有有效期 | 否 | 名称取自 `base.zip!infoharbor_ex.code`（`src/catalog_tdx/source.rs:116,155`；`compose.rs:107-111`） |
| `industry` | `assigned{code,name}` / `missing{reason}` / `conflicted{reason}` | TDX 研究行业**二级**（不是证监会行业）；无生效区间 | 否（三态之一） | 二级码由 `f5` 形态推导（`src/catalog_tdx/incon.rs:137-143`），名称取自 `incon.dat` 的 TDXRSHY 段（`compose.rs:113-135`）；DDL `004_catalog.sql:8-9` |
| `version_ref` | `VersionRef` | 目录版本身份 `catalog-<n>`；`integrity_sha256` 覆盖该成员行 | 否 | `wire.rs:126-133`；`004_catalog.sql:1-4,12-14` |

**时间语义**：catalog 是**某一时刻的快照**，没有 `as_of` 字段、没有生效区间；
`catalog_versions` 只记录该版本何时发布、取自哪一代输入（`004_catalog.sql:1-4`）。
用它回答「T 日是否在册」会引入前视偏差（§4.1）。

**版本绑定**：目录与行情不同，仍是多版本存储：`catalog_versions` + `catalog_members`，
`catalog_head` 指向当前版本（`004_catalog.sql:1-14`）。查询可用
`version:{"kind":"catalog_version","version_id":…}` 固定某一版（`wire.rs:97-104`）。

**可用性**：`get_data_availability` 对 catalog 返回 `available`（`src/data_supply/observe.rs:79-91`）。

**复权语义**：不适用——目录不是价格序列，没有复权口径。

**已知限制**：行业是 TDX 研究行业二级且只有当前值；名称只有当前值；无上市/退市/ST；
scope 固定为当前沪深 A 股。

**使用授权（OWNER_RISK_ACCEPTED，见 #399）**：TDX 官方页面写明「此处数据仅供券商后台系统补充
和维护使用」；本项目按 owner 个人本地研究使用，风险由 owner 接受，**不得对外再分发**。这条同时
覆盖由 `base.zip` 派生的名称、行业与上市/退市等字段，也适用于现有生产链路里的 `base.zip`。

**`base.dbf` 的重复行（2026-10-27 验收实测）**：官方文件在 `(SC, GPDM)` 上有**字节完全相同**
的重复记录（深市 `000020` 记录 19/20、`200020` 记录 1660/1661，两代输入一致）。解析规则：
字节相同的重复**合并**为一条，并在主表 item evidence 的 `duplicates_collapsed`
（`[{market, code, records}]`，记录号是文件内的 0 基下标）里留痕；同一代码但**内容不同**的重复
仍 fail-closed（`BaseDuplicate`，错误里给出两条记录号），不做「取第一条/最后一条」的静默选择。
规则与证据见 [DECISIONS](decisions.md) D-55。

**是否满足严谨回测级**：**否**。时点股票池与证券主表随 preview.37 部署（§3.1、§3.2 #6–#7），但退市股行情在 M3，跨年回测仍有幸存者偏差；退市日覆盖不足见 §3.1 的已知限制。

### 2.2 `market.daily`

**身份**：`{exchange, code}` + `period=1` + `adjustment=1`（`src/market_data.rs:11-13`；DDL `migrations/tasks/021_latest_only.sql:11`）。preview.36 起
部署实例同一实体同一周期有 `adjustment=0` 不复权与 `adjustment=1` 前复权两条序列，
`query_data` 只读前复权，见 [latest-only-storage](latest-only-storage.md) §13。

**覆盖范围**：

- 采集范围是目录覆盖 ∩ 官方 `hsjday.zip` 成员，**只有当前成员**；退市/摘牌证券的完整日线
  一直在官方包里，但不在库内（§3.2 #8）。
- 官方 `hsjday` 缺成员的目录实体：没有序列，查询侧是 `absent`，不返回空 bar 列表冒充「无交易」
  （`observe.rs:75-91`；`docs/design.md`「产品闭环与排除范围」：空结果不证明停牌/退市/无交易）。
- **新进入目录的实体不会自动首采**：`plan_market_update` 只把已有日线 head 的实体列为
  eligible，没有 head 的归 `absent`（`src/data_supply/plan.rs:544-551`）。新上市证券必须走
  显式首采（`start_collection`）才会建序列，否则长期 `absent`。

**字段表**（存储列见 DDL `021_latest_only.sql:30-45`；查询形状见 `wire.rs:173-190`）

| 字段 | 存储 | 查询返回 | 单位 / 精度 | 可空 | 来源与证据 |
|---|---|---|---|---|---|
| `date` | `trade_day INTEGER`（1970-01-01 起的天数） | `date` | 交易日（Asia/Shanghai 自然日） | 否 | `021_latest_only.sql:32,40`；`src/compact_market.rs:798-801` |
| `open`/`high`/`low`/`close` | `*_cents INTEGER`（schema 21 库）/ `*_units INTEGER`（代码，schema 22 起） | 十进制字符串 | **元**。已部署实例是 2 位小数 HALF_UP 的前复权价。代码按序列的 `price_scale` 解释整数：不复权 2 位、前复权最多 6 位 | 否 | 已部署：DDL `021:33-36`；`src/qfq.rs:68-74`（`raw_price`、`cent`）；`compact_market.rs:812-822`（非恰好 2dp 直接拒绝）。代码：DDL `022:74-99`；`src/compact_market.rs:17,24`（`MAX_PRICE_SCALE=6`、`PRICE_SCALE_QFQ=6`） |
| `volume` | `volume INTEGER` | 十进制字符串 | **股**（不随复权调整）。手日按 `r7_raw` 标记 + 原始值 VWAP 校验换算 ×100，无法确认时发布 `null` | 是 | `src/official_market.rs:502`（`normalized_volume`，`:101-113`）；单位锚定 #239（`evidence/issue-239-day-volume-amount/README.md`）；手日规则见 [day-file](day-file.md) 与 #381/#382 |
| `amount` | `amount TEXT` | 十进制字符串 | **元**（`.day` f32 值的精确十进制展开，相对误差 ≤ 约 6e-8） | 是 | `official_market.rs:503`（`amount_decimal`，`:78-81`）；#239 同上 |
| `r7_raw` | `r7_raw INTEGER` | **不返回**（不进查询契约） | 原样保留、不解释。A 股 1999–2010 为官方除权基准价（分），2010-09 起退化为 `0x10000` 哨兵 | 是 | `src/day_file.rs:14-18`；`src/market_data.rs:192-196`；`evidence/issue-240-r7-analysis/README.md` |
| `version_ref` | `market_series.version_id` | `VersionRef{version_id, integrity_sha256}` | `version_id = lower(hex(content_digest))` | 否 | DDL `021:26`；`wire.rs:126-133` |
| 元数据 | `source_bindings`、`evidence_bundles` | `metadata{source, units[], adjustment, coverage_*} ` | 每个单位字段带 `status`/`value`/`reason` | 否 | `wire.rs:249-263`；`src/sqlite_supply.rs:682-705` |

`r7_raw` 的边界照 #240 原样写入：**只有** 1999–2010 的 A 股有「官方除权基准价」证据；
2010-09 之后是 `0x10000`，基金/指数语义未锚定。消费方不应把它当价格。

**时间语义**：`date` 是交易日。官方 `.day` 没有停牌占位行，停牌只表现为缺 bar
（[day-file](day-file.md)；§3.2 #10）。因此「T 日收盘后可知」= 该 bar 及其之前的 bar；
「缺 bar」不能反推停牌、退市或无交易。

**复权语义**：

- 前复权，**基准是最新事件**：每根 bar 叠加所有 `event_date > bar_date` 的 cat1 事件的仿射链，
  oldest→newest、无中间舍入，末次 2dp HALF_UP（`src/qfq.rs:7-11,101-127`）。
  preview.36 起部署的代码是**精确有理数链 + 末次一次舍入到序列精度**（`qfq-exact-v1`，
  `src/qfq.rs:204`；前复权 `price_scale=6`）；2dp 实现保留为历史 oracle，不再写库。
- 因此**新的分红送转会改写该实体整段历史**，提交为一次 `replace` 事件
  （[latest-only-storage](latest-only-storage.md) §4、§6）。
- 只取 `gbbq` 的 `category == 1`；其他类别「excluded uninterpreted」并计入发布证据
  （`src/official_market.rs:9-12,596-603`；`src/gbbq.rs:1-17`）。
- `volume`/`amount` **不随复权调整**（`qfq.rs:17-18`；`official_market.rs:13-17`）。

**版本绑定**：`version_id` 是序列内容的顺序折叠摘要（`021:26`；
[latest-only-storage](latest-only-storage.md) §3），**只锚定 bar 取值**，不含 coverage 与来源。
显式 pin 被新 head 取代 → `stale_version`；从未提交 → `version_not_found`
（`src/data_supply/mcp.rs:580-582`；`src/market_storage/mod.rs:30-34`）。
只改元数据时 `version_id` 不变，变化记在 `market_events`（`021:64-112`）。

历史：schema 20 及更早是版本链模型，旧 `version_id` 可以读回当时的内容；schema 21 起不再保留旧值，
只能用事件日志看「变了什么」，看不到旧价格（[latest-only-storage](latest-only-storage.md) §11）。

**可用性**：`get_data_availability` 对已发布的市场序列返回 `unknown`——域层**不根据日期或版本
覆盖推断区间完整**（`observe.rs:75-111`）。同一响应给出 `coverage_evidence`（当前来源声明
`complete`，`official_market.rs:570-572`）、`confirmed_gaps` 与 `unresolved_ranges`。
所以 `available`/`unknown` 是可用性判定，不是覆盖结论。

**已知限制**（按影响排序）：

1. **只有前复权，且 2dp 取整**。只读实测（2026-10-26，全库日线）：12.6% 的 bar 前复权收盘价
   < 2 元；4.5% 的 bar 至少一个价格字段为负（753,414 根）、12,047 根出现零价，涉及 725 个实体
   （前复权代数 `(p − 累计派现 + 配股)/股数因子` 在极低价历史段会变号）。抽样 291 只、921,121 根
   bar 的日收益误差（2dp 取整引入，单位 pp，中位）：<0.1 元 2.9、0.1–0.5 元 1.05、0.5–1 元 0.42、
   1–2 元 0.24、2–5 元 0.14、5–10 元 0.079、≥10 元 0.060；>1pp 的占比分别为
   75%/46%/7.7%/0/0/0/0。→ **价格类因子在 ≥ 2 元区间基本可用，收益类与净值类在低价段不可用。**
   preview.36 起部署的实例另有一条不复权序列，前复权是 `qfq-exact-v1`（最多 6 位小数，
   PR #390/#394）；上面那些 2dp 样本测于部署前，只作历史对照。
2. **前复权序列含未来事件**：以最新事件为基准，除权会改写全历史。它适合「固定快照内做研究」，
   不适合「按时点复现 T 日当时看到的价格」。
3. **只有当前成员**：退市股行情不在库内，跨年回测有幸存者偏差（§4.3）。
4. **可交易性输入不全**：preview.36 起有不复权收盘（前一日收盘可算），preview.37 起有
   停牌 `unresolved_gap` 区间与股本事件；ST 状态（#308，D-58）与涨跌停规则原文（A-7，D-59）
   已准入但写入器与规则入库未完成（§3.2）。
5. **已发布数据里有少量「手日」bar 的 volume 少记 100 倍**：`.day` 在成交量接近 u32 上限时改写
   为「手」并置 `r7_raw=0xC36400xx`。发布路径已修（#381/#382：标记与 VWAP 校验同时成立才换算，
   否则 volume 发布为未知；手日只精确到 100 股），但**当前库内已发布的 bar 仍是修复前的值**——
   只读实测（2026-10-26）19 根、7 只证券，要等这些实体重新采集/重写后才会更新。
6. `r7_raw` 不入查询契约，只能直读存储。

**是否满足严谨回测级**：**否**。不复权序列与复权因子已随 preview.36 部署，证券主表与部分可交易性
随 preview.37 部署（来源缺口见 §3.2）；退市股在 M3（§3.1）。

### 2.3 `market.yearly`

**身份**：`{exchange, code}` + `period=2` + `adjustment=1`。`trade_day` 是该自然年**最后一个
参与交易日**的日序号（`021:30-45`；[latest-only-storage](latest-only-storage.md) §2.1）。

**覆盖范围**：由当前日线序列逐实体派生，覆盖与 `market.daily` 相同
（[latest-only-storage](latest-only-storage.md) §7）。

**字段表**

| 字段 | 语义 / 单位 | 可空 | 来源与证据 |
|---|---|---|---|
| `date` | 该年最后一个参与交易日 | 否 | `src/yearly.rs:126-137` |
| `open` | 该年**首个**参与日的 open；前复权 2dp | 否 | `yearly.rs:126` |
| `high` / `low` | 该年参与日 high 最大 / low 最小 | 否 | `yearly.rs:128-129` |
| `close` | 该年**末个**参与日的 close | 否 | `yearly.rs:130` |
| `volume` / `amount` | 仅当该年**全部**参与 bar 值已知且单位/币种证据已确认时求和，否则 `null`（不填 0） | 是 | `yearly.rs:120-125,271-289` |
| `first_trading_date` / `last_trading_date` | 该年实际首/末参与交易日；读取时按当前日线重聚合得到 | 是 | `wire.rs:178-179`；`src/sqlite_market/series.rs:987-1058` |
| `year_unfinished` | 该年 == 派生 `as_of` 的年份；**不**证明覆盖完整或新鲜 | 是 | `yearly.rs:144,173-180`；[yearly](yearly.md) |
| `r7_raw` | 恒为 `null`（年线禁 `r7_raw`） | 是 | `yearly.rs:131`；DDL `021:37` |
| `lineage` | `base_daily_version_id`、`algorithm_version=local-natural-year-v1`、首末参与日 | 否 | `yearly.rs:17,139-143`；DDL `021:49-58` |

**时间语义**：`as_of` 是派生时显式传入的上海日期，存于 `yearly_series_lineage.as_of_day`
（DDL `021:56`），不允许早于输入末日（`yearly.rs:50-54,81`）。`year_unfinished` 只描述自然年
是否结束（含 12 月 31 日），**不证明覆盖完整或新鲜**（[yearly](yearly.md)）。

年线还会「落后」：日线追加后，年线以 `stale="behind"` + `as_of_day` 返回；日线发生 `replace`
（除权/修订）后年线按行级状态 fail-closed（`absent` + `reason`）
（[latest-only-storage](latest-only-storage.md) §7.1；`wire.rs:137-153`）。

**复权语义**：与日线相同，且是**派生**：聚合要求日线的 `adjustment_evidence == qfq` 且
价格/币种/单位证据已确认，否则拒绝（`yearly.rs:51-70`）。

**版本绑定**：同 latest-only；年线另存 lineage，绑定派生时的日线摘要与事件 seq（DDL `021:49-58`），
用于 fresh / behind / invalid 分类。

**可用性**：同 `market.daily`；`get_yearly_base_years` 给出某个日线 head 下的可用年份
（`src/data_supply/registry.rs:293-299`）。

**已知限制**：继承日线的全部限制；一年一根 bar，不能替代日线做因子；年线是派生数据，
日线变化会重算。

**是否满足严谨回测级**：**否**（随 M1/M2/M3 同步）。

## 3. 缺口清单

### 3.1 首期目标：严谨回测级与里程碑

首期交付就是**严谨回测级**（owner 2026-10-26 最终确认）：在价量、股本和市值范围内，回测
① 没有幸存者偏差、② 按时点正确、③ 可交易性可判定、④ 结果可复现。研究级不再单独交付，
它是首期内部的第一个里程碑 M1。

**M1 研究级子集（P0）**

范围：不复权日线序列与复权因子（cat1）、交易日历（推导）、交易所/中证宽基指数基准与逐指数
口径元数据、研究快照导出（批量）。

以下是**数据目录视角的验收建议**，编号与 #383 M1 设计的 AC-M1-* **不是同一套**（设计里的
AC-M1-1…12 是实现验收依据，见 [实现记录](tasks/issue-383-m1-implementation.md)）：

- AC-M1-1：`adjustment=0` 序列与官方 `.day` 原值逐根相等（OHLC、volume、amount、`r7_raw`）。
- AC-M1-2：用「不复权 + cat1 事件」按 `qfq.rs` 契约重算的前复权，与现有 `adjustment=1` 序列
  逐根比对，不一致率 ≤ 0.5% 且**全部为 ±1 分**。（本任务抽样 20 只 / 68,215 根：99.972% 相同，
  19 根差 1 分，0 根差更多。）
- AC-M1-3：日历覆盖起点不晚于官方最早 A 股 bar，落在日历覆盖区间 `[first_day,last_day]` 内、但日历无该行的 A 股 bar 比例 ≤ 0.01%；`last_day` 之后为 unknown，单列 `stale_bars`，不进门禁。
- AC-M1-4：每只收录指数的 `volume` 单位与 `amount` 口径都有对官方接口的实测证据，并作为
  元数据入库，不在代码里写死。
- AC-M1-5：导出清单含目录版本、每序列 `version_id`、来源 generation、文件 sha256；用清单能
  逐位复现导出文件。

**M2 时点证券主表与可交易性（P1）**

范围：按生效区间的证券主表（证券类型、上市日、退市日、代码复用标记）、股本历史事件、
历史 ST（依赖 #308）、停牌标记（`unresolved_gap` + 候选来源）、按生效日期管理的涨跌停规则表、
T 日股票池 `universe(as_of=T)`。

**M2 部署状态（preview.37，2026-10-27）**：六个数据集
`market.security_master` / `market.security_names` / `market.capital_events` /
`market.risk_warnings` / `market.suspensions` / `market.price_limit_rules` 已随 preview.37 部署
（PR #425 的 dataset 分支与 detailed capabilities `temporal` 声明）；`as_of`/`known_at` 只在
声明为 `true` 的数据集上接受，`known_at_coverage` 为 `partial` 或 `none`
（`src/data_supply/wire.rs:127-146`）。未登记版本返回 `manifest.status=absent` 与 0 行，不服务
无版本数据（`src/sqlite_supply/m2.rs:73-82,498-506`）。

写入路径：`market.security_master`、`market.risk_warnings`、`market.suspensions`、
`market.price_limit_rules` 由日更 run 的内部 item 维护，`market.capital_events` 随每条日线
提交同事务写入；`market.security_names` 的写入器已实现（代码已合并，未部署）。**来源状态**：
交易所上市/退市名单与 A 股总股本仍待准入；历史 ST（D-58）、停复牌公告（D-60）、涨跌停
规则原文（D-59）与简称历史（D-61）已准入。规则书与 `derived:trading_rules` 的写路径已在
代码中实现（仓库内版本化规则数据 + 校验；内容不变为 `no_change`），但 preview.37 实例仍是
0 行，下一次发布后才写入。默认/非 ops 形态下未实现或未配置的部分仍在 item 结果里记为
`pending_source`，对应数据集写 0 行、不登记版本（读取仍是 `absent`），不把未实现的部分
伪造成数据；生产形态（ops + compact）缺来源库时四个同源 item
（`security_master`/`risk_warnings`/`suspensions`/`security_names`）以稳定码
`source_store_unconfigured` 失败，见 §3.2。
**历史 ST 写入器（代码已合并，未部署）**：`market.risk_warnings` 不用当前名称推断历史，`unresolved`
也不伪装成已知区间；默认形态下来源库未配置或未准入时写 0 行并记 `pending_source`，生产 strict 下
缺 store 则失败；都不使用当前值回填。没有规则时涨跌停派生一律 `unknown`。数据量见
[STATUS](STATUS.md)（本文件不写部署数字）。

**M2 已知限制（XDM-74 补测，2026-10-27）**：

- 退市日只有 TDX 整理期口径：`delisting_date` 有值 **152** 行（来源全部 `tdx_arrangement`），
  其余 `unknown`。交易所名单（A-1/A-2）未准入，主表因此比 catalog **多 279 个实体**
  （沪主板 118 / 深主板 116 / 创业板 42 / 科创板 3），这批当日无 bar 且退市日未知，被当成仍在册；
  `universe(as_of=2026-10-23)` 为 5,351，catalog 为 5,226。导出快照的成员带 `delisting_unknown`
  标记（`src/research/universe.rs:508`），严格口径可据此排除。
- 代码复用与拆分仍 pending：`600018` 只有 1 行（2006-10-26 起，无 `effective_to`），没有拆分，
  `ambiguous_code_reuse=0`；A-1/A-2 接入前不把「只有一行」当成「已验证无复用」。
- `market.suspensions` 的读面只返回**覆盖 `as_of`** 的缺口（`row.covers(as_of)`，
  `src/sqlite_supply/m2.rs:150-157`）：按日期范围查时给 `as_of`；范围内其它缺口只在
  `temporal.unknown_known_at_rows` 里计数，不返回行。

验收标准建议：

- AC-M2-1：主表覆盖目录成员与官方包内的退市代码，每个代码的上市日有来源或 `unknown`，
  缺失率与来源一并披露。
- AC-M2-2：`universe(as_of=T)` 与「当日有 bar 且在册」集合一致，差异逐条给原因。
- AC-M2-3：股本历史与 `base.dbf` 当前快照对账（最后一条变动与 `LTAG`/`ZGB` 的比例 ≥ 90%），
  并通过 3 个人工 oracle（`sz000001`、`sh600519` 两次）。
- AC-M2-4：涨跌停派生在 6 个板块×时段组合上的命中率不低于实测基线，且规则来自带依据引用的
  规则表。
- AC-M2-5：ST 或前收缺失时，涨跌停派生返回 `unknown`，不按 10% 计算（fail-closed）。

**M3 退市股与历史行业（收口）**

范围：退市股行情收录（含未来新增）、ETF 收录（作为 `market.daily` 的证券类型、按序列记价格精度，见 [DECISIONS](decisions.md) D-28）、历史行业（从今天起逐代积累 + 替代来源评估）、停牌完整性、
首期验收。

**M3 代码状态（main，2026-10-30）**：PR-1a…PR-9 已合并，**均未部署**。已合并的能力：
来源库与重放框架（PR-1a）、schema 25 的 5 张索引表（PR-1）、交易所来源共用入口
（PR-2）、A-1/A-2 上市/终止上市适配器与主表合并（PR-3）、`sh_sz_a_listed_ever`
首采 scope（PR-4）、日更尾巴与 `derived:delisted_series_check` 不消失检查（PR-5）、
行业快照落库/区间计算/来源库重放（PR-6）、`market.industry_assignments` 行业区间读面
（PR-7）、退役门禁（PR-8）、`research-snapshot.v3` 导出（PR-9）。落地记录见
[来源库](source-store.md)、[交易所来源入口](exchange-source.md)、[日更](daily-sync.md)
与 §5.3；设计拆分见 [M3 设计](tasks/issue-446-m3-delisted-industry.md) §14。
生产实例仍在 schema 24，未做退市股首采、未导入 owner 归档、未配置
`X_DATA_MCP_SOURCE_STORE`，所以这些能力现在都不提供数据：缺口表状态写
「已提供（代码已合并，未部署）」或「部分提供（代码已合并，未部署）」。PR-10/11/12
（历史 ST、停复牌、简称写入器）、私有栈验收与部署不在本批：这三个来源对应的数据集仍写 0 行
并记 `pending_source`。

`#446` 侧的 AC-M3-1…10 与本节下面的 AC-M3-1…4 不是同一套：本文件这 4 条是目录视角的
验收建议；#446 的 10 条实现验收与部署前的私有栈证据清单见 M3 设计 §14.3（每条状态
均为「未执行」）。

验收标准建议：

- AC-M3-1：`universe(as_of=2015-01-05)` 包含 2015 年以来退市的全部证券（可枚举）。
- AC-M3-2：含退市股的 10 年回测，样本数与「当年在册数」逐年一致，偏差逐条解释。
- AC-M3-3：行业归属带生效区间与来源；早于积累起点的日期返回 `unknown`，**不用当前行业冒充**。
- AC-M3-4：一份完整研究快照能在另一台机器上逐位复现同一回测结果。

### 3.2 缺口表

状态取值固定为：**已提供 / 部分提供 / 缺失·计划中 / 缺失·阻塞 / 待取证 / 不在范围**。
代码已合并但生产实例还没有的交付写成 **已提供（代码已合并，未部署）**；部署后写成
**已提供（已部署）** 并在备注里指明部署版本；来源未接入的部分按 `partial`/`pending_source`
如实写，不把「表存在」当作「数据可用」。
「里程碑」是首期内的期；「严谨级」= 该项是否已满足严谨回测级。

| # | 项 | 状态 | 原因 / 阻塞点 | 对应 Issue | 里程碑 | 严谨级 |
|---|---|---|---|---|---|---|
| 1 | 不复权价 + 复权因子 | 已提供（preview.36 部署） | 官方 `.day` 的 OHLC 本身就是不复权原始价（分），前复权是本地产物（`qfq.rs:68-74`；`official_market.rs:9-12`）；cat1 语义已由 #54 冻结。**不需要新来源**。M1 已交付不复权序列（PR #390）、因子事件集（PR #397）与 6dp 前复权（PR #394），随 preview.36 部署 | #1 | M1 | 已部署；数值验收见 [STATUS](STATUS.md) |
| 2 | 交易日历 | 已提供（preview.36 部署） | 没有官方日历文件；只能由指数 `.day` 的日期并集推导，必须标 derived 并带算法版本与输入清单。完整性未验证（一手证据是交易所休市公告）。M1 已交付 `trading_days` + `cal-union-v1` 与覆盖率自检（PR #402），随 preview.36 部署 | #1 | M1 | 已部署（完整性仍需交易所公告） |
| 3 | 指数基准 | 已提供（preview.36 部署） | 官方 `.day` 有交易所/中证宽基；OHLC 缩放（0.01 点）、`volume`=手、`amount`=元、量额口径（全市场/全板块 vs 成分股）已取证。owner 已把指数基准纳入首期范围（2026-10-26），且**只用交易所发布的指数**，通达信自编 880/887 不作基准（D-27）；中证系 4 条宽基算作交易所体系指数，基准取 **9 条序列**（D-31）。M1 已交付 9 条登记与 `index_series_meta`（PR #400），随 preview.36 部署；`market.index` 在 v2 可读，导出里也有 | #246 | M1 | 已部署 |
| 4 | 股本历史 | 部分提供（preview.37 部署） | `gbbq` 的 cat 2/3/5/7/8/9/10 已按 D-41/D-50 准入（档位 `OWNER_RISK_ACCEPTED`）并随 `market.capital_events` 部署：只采用**后值**与事件日，前值不用；`known_at` 全 NULL（事件没有知晓时间），因此只能 as-of 回看、不能做信息集严格回测。剩余偏差：京市 1,479 条无官方对照 | 无（D-41/D-50） | M2 | 部分满足（后值口径） |
| 5 | 市值（总/流通） | 部分提供（可派生，preview.37） | 两个输入都已部署：不复权收盘（preview.36）+ 股本事件后值（preview.37），可用 `float_after`/`total_after` × 不复权收盘自行派生；**没有服务端市值数据集**。A 股总市值对 A+H 公司仍是 `unknown`（`total_after` 是公司总股本，§3.3 偏差 4），用前复权价乘当时股本会得到错误市值 | #1 | M2 | 部分满足（需标注口径） |
| 6 | 上市日 | 部分提供（代码已合并，未部署） | `market.security_master` 用 `base.dbf` 的 `SSDATE` + `infoharbor_spec.cfg` 组装（preview.37 部署）；M3 PR-3 已实现 A-1/A-2 适配器（沪深交易所上市/终止上市名单，只取 A 股，`parser_version=exchange-a1a2-v4`），按「交易所 A-1/A-2 > `SSDATE` > `unknown`」合并，两者不同则 `listing_conflict=1`、取交易所值并保留 `listing_date_tdx`（M2 已有列）；`sz.302132` 的交易所「A股上市日期」是被继承的 2010-08-27，不据此开新区间（区间起点由 confirmed 迁移 2025-02-17 决定）。**未部署，未在本实例实测**：默认/非 ops 形态未配置 `X_DATA_MCP_SOURCE_STORE` 时仍写 `pending_source` 并按 `unknown` 返回；生产形态（`data_service` 的 ops 入口 + compact 后端）未配置或不可读时 `derived:security_master` 以稳定码 `source_store_unconfigured` 失败、run 收尾 `partially_failed`，日线照常、上一版主表仍是 latest。补齐后剩余 `unknown` 按设计估算 0（61/62 由交易所名单补齐，`sh.600849` 靠 A-2 的上市日），数字取自 2026-10-15 快照、未在部署实例重测 | 无（D-45/D-46 已准入） | M2/M3 | 部分满足（`unknown` 如实披露） |
| 7 | 退市日 | 部分提供（代码已合并，未部署） | `infoharbor_spec.cfg` 的退市整理期区间（169 行、2013 年起）随主表部署；M3 PR-3 接入 A-2 终止上市名单（沪 `COMPANY_STATUS=3` 且 `STOCK_TYPE∈{1,8}`；沪名单按 `pageHelp.pageSize=2000` 一次取完、`pageCount>1` 则 fail-closed，深 `1793_ssgs` + `selectModule` `main`/`nm`，响应按 `metadata.tabkey="tab2"` 取终止上市块（`tab1` 暂停上市 0 行且 `pagecount=0`，页码/`pagecount`/行数组同取 `tab2`；无 `tab2` 时只接受唯一有非空 `data` 的块，多义 fail-closed）），优先级「交易所 `DELIST_DATE` > 整理期 end > `unknown`」，冲突时 `delisting_conflict=1` 且两值都存（整理期与退市日口径分开）；退市原因 `NOT_AVAILABLE` 一律 `unknown`。**未部署，未在本实例实测**；默认/非 ops 形态未配置来源库时仍 `pending_source`，生产形态（ops + compact）则以稳定码 `source_store_unconfigured` 失败（日线照常、上一版主表仍是 latest）。补齐后剩余 2 例（`sz.300114` 由 confirmed 迁移处理、`sh.600849` 吸收合并只有 candidate 迁移 → `unknown`），`unknown` 占比估算 `<0.1%`（约 2/5,565），数字取自 2026-10-15 快照、未在部署实例重测 | 无（D-46 已准入） | M2/M3 | 部分满足（2013 起 + 交易所名单） |
| 8 | 退市股行情 | 已提供（代码已合并，未部署） | 官方 `hsjday.zip` 保留了目录之外 A 股代码的完整历史（2026-10-26 快照：335 个代码，其中 251 个末根 bar 在 2015 年及以后），跨代不消失。M3 PR-4 已实现 `market.daily` `start_collection` v2 的 `{"kind":"scope","scope":"sh_sz_a_listed_ever"}`：解析 `security_master` 中沪深 A 股且 `has_series=false` 的实体 ∩ 当前 generation 的 `.day` 成员，预算 1..500，跳过无 bar 成员并记入证据；已有 head 的实体跳过。**未部署、未做真实首采**：代码通过 CI 后仍需发布并显式执行一次，日更 plan 不会自动纳入。M3 PR-5 已补齐日更维护面：eligible 集取「当前目录 ∪ 上一版目录成员 ∩ 已首采」，成员核实后移出目录不会丢掉退市日当天的最后几根 bar；每次日更还有一个只读 item `derived:delisted_series_check`，比对 `record_count`/`first_day`/`last_day`/`content_digest`，序列消失或被改写 → `partially_failed` + 实体清单，不自动重建（历史改写走 `republish_data`）。**同样未部署** | #246（范围）、#446 | M3 | 代码支持（未部署、未首采） |
| 9 | 历史 ST / \*ST | 部分提供（代码已合并，未部署） | 2026-10-28 owner 按市场分档准入（D-58）：**深市** `SSGSGMXX`/`tab2` 的 ST 区间与**沪市 2000-01 起**公告索引的状态事件（事件级）为 `APPROVED_SOURCE`，沪市生效日按公告日/标题显式日期锚定为 `OWNER_RISK_ACCEPTED`，沪市 2000 年前与北交所 `NOT_AVAILABLE`；无公告一律 `unknown`，禁止「无公告 ⇒ 非 ST」。写入器已实现（PR-10/#467）：整表替换 `risk_warning_intervals` 并登记 `derived_dataset_versions`，`derived:risk_warnings` 日更接入，`source_store replay --scope risk_warnings` 可离线重建；空结果记 `not_observed` 并在缺口处切断未闭合区间，不把 0 行当无事件。**未部署，未在本实例实测**：默认/非 ops 形态未配置 `X_DATA_MCP_SOURCE_STORE` 时仍写 0 行/`pending_source`，生产形态（ops + compact）未配置或不可读时 `derived:risk_warnings` 以稳定码 `source_store_unconfigured` 失败（run `partially_failed`，日线照常） | #308 | M2（沪市 2000 年前为硬缺口，见 §3.3 偏差 2） | 部分满足（深市区间 + 沪 2000 起事件级；2000 年前/北交所 `unknown`） |
| 10 | 历史行业 | 部分提供（代码已合并，未部署） | 当前行业（catalog 的 TDX 研究行业二级）已随目录部署；**历史区间不再只能从今天起积累**：M3 PR-6 已实现 `derived:industry_snapshot`（每代 `tdxhy.cfg` 落快照 + 区间变更集；`effective_from` 取发布方 `last_modified`，缺失取获取时间；缺同代 `incon.dat` 时名称为 `unknown`；早于最早观测返回 `unknown`）与 owner 归档 A/B 档的 `source_store import` + `replay --scope industry` 回填；PR-7 已实现读面 `market.industry_assignments`（`as_of`/`known_at`、`status=assigned/unknown` 带 `reason`、`source_snapshot_sha256`/`evidence_grade`）。**未部署、未导入归档**：生产实例还没有行业区间；部署并导入后最早只到归档最早快照（实测 2026-09-30；C 档 2026-09-16 只作交叉核对），更早仍 `unknown`，不能用当前行业冒充。替代来源（证监会行业分类）需取证 | #1、#446 | M3 | 部分满足（区间与来源可查；早于积累起点 `unknown`） |
| 11 | 停牌 | 部分提供（preview.37 部署） | 官方文件没有停牌标记，`.day` 也没有占位行。A-3 已由 D-49 准入、2026-10-28 定交付口径（D-60）：沪市 **2008 年起**可实现 `confirmed`（结构化事件表，`endStopDate`=复牌日前一自然日），**深市首期没有 `confirmed`**（无结构化源，`BULLETIN_TYPE` 过滤不可用），只到 `unresolved_gap`；深市全域可交易性统一 `unknown`（§3.3 偏差 6）。PR-11 写入器已实现（代码已合并，未部署），部署前 `confirmed` 行数仍为 0；默认/非 ops 形态未配置来源库时只有 `unresolved_gap`，生产形态（ops + compact）缺 store 时 `derived:suspensions` 以稳定码 `source_store_unconfigured` 失败 | #404（A-3） | M2（标记）/ M3（闭合） | 部分满足（缺口可查；`confirmed` 未部署） |
| 12 | 涨跌停 | 部分提供（规则已入库，待发布；ST 仍缺） | 结构随 preview.37 部署（`market.price_limit_rules`，按生效日期管理、每行带 `basis_ref`/`basis_status`）。**A-7 已由 D-44 关闭**（2026-10-26，2026-10-28 复核确认，见 D-59）：规则文本 `APPROVED_SOURCE`、R1/R3 起点与两处条文缺口 `OWNER_RISK_ACCEPTED` 且标「推断」、R7 `NOT_AVAILABLE`。规则书（R1–R9：27 行涨跌停规则 + 2 行最小变动单位）已作为版本化规则数据入库，日更 `derived:trading_rules` item 写入两表并登记版本（不变为 `no_change`）；preview.37 实例仍是 0 行，下一次发布后生效。**D-64（2026-10-29）另补两批数据-only 行**：R2 前段 5 行（1998 起点 `OWNER_RISK_ACCEPTED`：深交所简称表首个 ST 事件 + `.day` 锚定；沪市只有 `.day`，D-58 的沪市 2000 年前 ST 状态仍 `NOT_AVAILABLE`；2006/2011/2013/2020-12 条文段 `APPROVED_SOURCE`）与 R6 首日无涨跌幅 5 行（`band_side=none`，派生 `unbounded` 而不是 `unknown`）。剩余偏差：1996-12-16 前（R7）无规则 → `unknown`；主板首日 44%/36% 段（沪 2013-12-13 起、深 2014-06-13 起）仍 `not_encoded` → `rule_missing`；沪市风险警示股前收 ≤0.10 元的一 tick 例外已由 schema 25 的 `min_band_ticks` 落地（沪 2013-01-01…2023-02-16、沪 2023+ 的两行 ST 与深 2011-02-28…2023-02-16 的 ST 行置 1；深 2023+ 两行因 G-4 原文未取得保持 NULL，极低价仍是已知未编码的百分比结果），`limit-v1` 同时保留 1 tick 价格下限。ST 状态未知时，仅 `status_class='all'` 的板块/时段（创业板 2020-08-24 起、科创板，以及 2023 前的首日无涨跌幅段）有确定值，其余 `unknown`（依赖 #9 写入器；规则行了不等于状态有了） | #395（A-7）、#308（ST） | M2 | 部分满足（规则行已入库；ST 未部署） |
| 13 | ETF | 缺失·计划中 | 身份只能由交易所名单锚定（新来源待准入，D-25）；`.day` 对 ETF 的价格单位是**厘**（3 位小数）、`volume` 是**份**（≥4.29e9 份时改写为手）、gbbq 还有 cat11（份额折算）未进 QFQ；现有 `cents()` 要求恰好 2dp，3dp 会被**直接拒绝**（`compact_market.rs:812-822`）。owner 2026-10-26 裁决：作为 `market.daily` 的证券类型、按序列记精度、提供不复权 + cat1/cat11 因子，放 M3（D-28） | #246 | M3 | 未满足 → M3 |
| 14 | 财务数据 | 不在范围 | 首期范围是价量 + 股本/市值，不含财务 | #1 | 不在首期 | — |
| 15 | 分钟线 | 不在范围 | 首期只做日线；`series_bars.trade_day` 是日序号，分钟线要改主键 | #1 | 不在首期 | — |
| 16 | 北交所 | 不在范围 | 暂不纳入；规则 S 已把 `f0=2` 显式排除（`scope.rs:10-24`） | #1、#246 | 不在首期 | — |

> M2 相关项（#4 股本、#5 市值、#6 上市日、#7 退市日、#9 历史 ST、#11 停牌、#12 涨跌停）对应的
> 6 个 dataset 随 preview.37 **已部署**；未登记版本返回 `absent`/0 行。`risk_warnings` 写入器
> 已实现（代码已合并，未部署）：默认/非 ops 形态未配置来源库时写 0 行并记
> `pending_source`，生产形态（ops + compact）缺 store 时以稳定码
> `source_store_unconfigured` 失败。`price_limit_rules` 的规则书与日更写路径已在代码中实现
> （preview.37 实例仍 0 行，待发布）；`suspensions` 的 PR-11 写入器已实现（代码已合并，
> 未部署），默认形态只有 `unresolved_gap`，生产 strict 下缺 store 失败。
> `security_master` 的交易所上市/退市分支（#446 PR-3）代码已合并、未部署：默认形态未配置
> 来源库时仍写 `pending_source`，生产 strict 下以同一稳定码失败。`security_names` 写入器
> 同样已实现（代码已合并，未部署），按同一形态区分。M3 其余已合并能力（退市股首采 scope、
> 不消失检查、行业区间、退役门禁、快照 v3）也一律未部署，见 §3.1「M3 代码状态」。

**ETF 的取证结论**（2026-10-26）：

- 取证报告（2026-10-26 抓取沪深两所名单）实测：官方现行名单与 `hsjday` 的交集是 1,628 只，
  另有 127 只已不在现行名单；官方名单中还有 86 只不在 `hsjday`（只靠 `hsjday` 会静默缺约 5%）。
- 价格缩放：`raw/1000` = 元（3dp）；成交量单位是份，超过阈值切换为手；成交额单位是元。
- 现有前复权公式**不能直接用于 ETF**：价格 `/100` 差 10 倍、只取 cat1 会漏掉份额折算、
  2dp 舍入精度不够。
- 收录不构成容量否决（推断 +68~73MB、日更 +22~30s），但会同时撞上精度、目录/契约、复权三处约束。

### 3.3 首期无法达成的项（如实写入偏差）

1. **历史行业**：早于最早可用观测的行业归属仍**无法补齐**。M3 的代码（PR-6/PR-7，已合并、
   未部署）能把 owner 归档与后续每代 `tdxhy.cfg` 落成带生效区间和来源 sha256 的快照，但归档
   实测最早只到 2026-09-30（C 档 2026-09-16 只作交叉核对，除非 owner 另批），更早历史没有
   快照。→ **首期严谨级在这一项上无法完全达成。**
   剩余偏差：早于 `industry_coverage_start` 的日期只能返回 `unknown`，不能用当前行业补；
   `effective_from` 取发布方 `last_modified`（缺失取获取时间），真实变更日最多晚一个发布周期；
   回填区间没有同代 `incon.dat`，行业名称为 `unknown`，代码与区间仍可用。替代方案：证监会
   《上市公司行业分类结果》有历史发布记录，可作为「带生效日期的官方分类」候选，但必须先取证
   （发布入口、历史覆盖、离线保留边界、与 TDX 研究行业的映射）；两者口径不同（TDX 是研究
   行业二级，证监会是门类+大类），不能直接互换。
2. **SH 历史 ST 的完整性**：可达成的是「有公告的事件」；无公告的事件必须 fail-closed 为
   `unknown`，不能推断为 `normal`。2026-10-28 按市场分档准入（D-58）：沪市 **2000-01 起**的公告
   索引状态事件为 `APPROVED_SOURCE`（事件级），生效日用 GP29/行情锚定为
   `OWNER_RISK_ACCEPTED`；**2000 年之前与北交所为 `NOT_AVAILABLE`**（公告索引归档起点 2000-01，
   见 §1.3）。→ **首期严谨级在「沪市 2000 年前的历史 ST」这一项上无法完全达成。**
   剩余偏差：沪市 2000 年前的风险警示区间一律 `unknown`，回测中这些日期应按「未知」剔除或
   单独报告敏感性，不能当成正常交易；沪市 2000 年起的区间边界依赖 owner-risk 的生效日锚定，
   报告里要注明。写入器未部署期间两市 ST 状态都还是 `unknown`。
3. **交易日历的完整性**：目前只有「指数日期并集」一种推导，且只能证明「不在日历 ⇒ 非交易日」，
   不能证明「在日历 ⇒ 该证券当日可交易」。一手证据需交易所年度休市公告；在此之前日历必须标
   derived，并随快照记录输入清单。
4. **A 股总市值（A+H 公司）**：交易所股本接口只返回当前一期，历史 A 股总股本不可得；`gbbq` 的
   `total_after` 是**公司**总股本（A+B+H），不是 A 股总股本。→ **「A 股总市值」返回 `unknown`。**
   剩余偏差：不得用公司总股本冒充 A 股总股本（对 A+H 公司会把 H 股算进 A 股）；可用的是
   「流通 A 股 × 不复权收盘」与「公司总股本 × 不复权收盘」，后者必须在结果里标注口径
   （`float_a` / `company_total` / `a_share_total`），三者不可混用。
5. **代码复用与代码迁移**：复用期间的历史 bar 无法按发行人拆分；迁移的两个代码共享同一段 bar
   历史。→ **T 日股票池默认包含这两类成员，`strict` 模式剔除。** 剩余偏差：非 strict 的截面会
   把同一发行人计两次，任何回测结论都要写明是否使用 `strict`。M3 PR-7/PR-9 已在 `universe` 与
   导出里给出 `canonical_entity`/`aliases` 和 `code_migrations`（含 `certainty`），代码已合并、
   未部署；candidate 迁移（`sh.600849→sh.601607`）仍不作去重证据。
6. **停牌的 `confirmed` 分市场**：沪市 2008 年起用结构化停复牌事件表可实现 `confirmed`
   （D-49/D-60）；**深市首期没有 `confirmed`**（没有结构化停复牌表，公告索引的
   `BULLETIN_TYPE` 过滤不可用），只到 `unresolved_gap`。→ **深市全域可交易性是 `unknown`
   （`data_gap`），不是 `suspended`；沪市 2008 年前同为 `unknown`。** 剩余偏差：
   `unresolved_gap` 只能说明「数据里没有 bar」，不能区分停牌、全天一字板未成交或数据缺失；
   回测要按「未知」剔除或单独报告敏感性。PR-11 已实现沪市写入器（代码已合入、未部署）：
   沪市 2008 年起可由事件表写 `confirmed`，深市仍只有 `unresolved_gap`。
7. **涨跌停规则已入库，但 ST 状态仍缺**：A-7 已由 D-44 关闭（2026-10-28 复核确认，D-59）：
   规则文本 `APPROVED_SOURCE`、R1/R3 起点与两处条文缺口 `OWNER_RISK_ACCEPTED`（必须标「推断」）、
   R7 `NOT_AVAILABLE`。规则书（R1–R9 的生效段）已作为版本化数据入库，日更 run 的
   `derived:trading_rules` item 把规则写入 `market.price_limit_rules`/`min_tick_rules` 并登记版本
   （内容不变时为 `no_change`，不重复发版本）。每行带 `basis_ref`/`basis_status`；未取证的规则
   只能以 `pending` 入库，派生时一律不选中。→ **规则存在不等于每个日期都有答案**：1996-12-16
   之前（R7）永久 `unknown`，不能用「无涨跌幅限制」兜底；主板 ST 规则行已从 1998-04-28/29
   起分五段落行（D-64），沪市 2000 年前的**状态**仍缺（D-58），深市的状态要等 #308 写入器，所以这些日期
   在 ST 状态未知时仍是 `unknown`（候选集 `{5%,10%}`），不用今天的 5% 覆盖历史。主板首日
   44%/36% 段（沪 2013-12-13 起、深 2014-06-13 起）仍未编码 → `rule_missing`；2023 前的
   首日无涨跌幅段已编码为 `band_side=none`，派生是 `unbounded`（已知无限制，不是
   `unknown`）。ST 状态本身仍是 `unknown`（§3.2 #9 写入器已实现、代码已合并但未部署），所以主板等「ST 与非 ST
   幅度不同」的板块在未知 ST 下仍返回 `unknown`（带 `{5%,10%}` 等候选集）；创业板
   2020-08-24 起与科创板由 `status_class='all'` 的规则行给出确定值，且不把未知 ST 记为
   `normal`。`daily_raw` 不物化涨跌停列（它是解析
   事实），需要涨跌停的使用方必须自己带规则重算，并写明所用规则版本。
8. **交易所来源的 0 行不是证据，历史修订也不可重放**：上交所公告索引在日期区间超过一个季度时
   静默返回 `total=0`（§1.3），所以来源入口必须按 ≤1 季度分段并对每个窗口校验计数；0 行只能
   记 `not_observed`/`unverified_zero`，不能推出「没有 ST/停牌/变更」（M3 PR-2，代码已合并、
   未部署，AC-M3-10）。交易所名单与公告只保留冻结时的当前快照：名单修订、历史 ST 公告完整性、
   深市停牌 `confirmed` 都无法用后来的重抓重放；沪市 2000 年前与北交所 ST 为 `NOT_AVAILABLE`
   （偏差 2）。回测里这些日期按 `unknown` 处理。

## 4. 按用途看现在能做什么

### 4.1 选股

**可以做（附前提）**

- 用当前目录 + 当前行业做**当期**筛选（按代码、名称、行业码）：前提是结论只对「当期」成立，
  不能当作历史时点的在册集合（catalog 无生效区间，§2.1）。
- 用日线成交额/成交量做流动性排序：前提是只看近年样本，并剔除已知的「手日」bar（#381）。
- **时点股票池**（`market.universe`，`selection.kind=universe` + `filter.as_of=T`）：
  前提是退市股行情不在库内（M3），成员有 `has_series=false` 时按「在册但无序列」理解，
  不能当作「不存在」（§3.2 #6–#8）。
- **市值筛选**：用 `market.capital_events` 的 `float_after`（或 `total_after`）× 不复权收盘；
  前提是 `total_after` 是公司总股本（含 B/H），A 股总市值对 A+H 公司是 `unknown`，
  结果必须标注口径（§3.3 偏差 4）。

**结果不可靠**

- **按股价绝对值筛选**（如「股价 < 5 元」）：只有前复权价，除权后价格与当时价格不同；且 2dp 取整
  在低价段误差大（§2.2 已知限制 1）。
- **判断涨跌停**：前一日不复权收盘已随 preview.36 部署，`price_limit_rules` 随 preview.37
  上线；A-7 已由 D-44 关闭（D-59）且规则书已入库（日更 `derived:trading_rules`），preview.37
  实例上仍是 0 行，需下一次发布。ST 状态已按市场分档准入（D-58），写入器已实现、代码已合并但未部署（§3.2 #9）：
  主板等板块在未知 ST 下仍是 `unknown`（ST 与非 ST 幅度不同），只有 `status_class='all'`
  的板块/时段（创业板 2020-08-24 起、科创板）能给出确定值。不能按 10% 自行推算。
**做不了**

- 剔除当时处于风险警示的证券：部署前 `market.risk_warnings` 仍写 0 行（D-58 已按市场分档准入，
  写入器已实现、代码已合并但未部署；§3.2 #9）。
- 确认 T 日停牌：沪市 2008 年起可由结构化事件表给出 `confirmed`（D-60，写入器已实现、
  代码已合并但未部署），
  深市没有结构化公告源，只有 `market.suspensions` 的 `unresolved_gap`（缺 bar 的候选区间）；
  可以据此**标记**「可能停牌」，不能断言「停牌」（§3.2 #11、§3.3 偏差 6）。
- 按 T 日行业做中性筛选：只有当前行业（§3.2 #10）。

### 4.2 因子挖掘

**可以做（附前提）**

- **基于收益率的价量因子**（动量、反转、波动、量比、振幅）：前提是
  ① 只覆盖当前成员；② 用固定快照并在结论里写明 `version_id`；③ 样本尽量落在价格 ≥ 2 元的区间，
  低价段（尤其 < 1 元）的日收益误差可达 0.4–3pp，不能用于精细排序（§2.2 已知限制 1）。
- **价格类因子**（如振幅、最高/最低相对位置）在 ≥ 2 元区间可用；结论必须声明是前复权口径。

**结果不可靠**

- **全历史价量因子**（尤其 1990s–2000s 的低价段）：2dp 误差是 pp 级；负价/零价 bar 会直接破坏
  收益率与对数价格（§2.2 已知限制 1）。
- **截面标准化与行业中性化**：没有时点行业、没有 T 日股票池，截面定义本身不成立。
- **事件因子**：cat1 除权事件语义已冻结，但事件表没有作为数据集提供，只能自己解码官方 `gbbq`
  （§3.2 #1）；其他类别语义未取证。

**做不了**

- 市值因子、换手率因子：缺股本历史（§3.2 #4）。
- 含退市股的因子面板：幸存者偏差会系统性改变因子分布（§4.3）。

### 4.3 回测

**可以做（附前提）**

- **当前成员内、日频、以收盘价成交、忽略涨跌停/停牌的探索性回测**。
  `scripts/analysis/boll.py`（#373/#379）就是这一类：全市场 2026 单年、只用前复权。
  前提：结论只对「当期存续样本」成立，且必须在报告里写明幸存者偏差、不可交易性与精度三项偏差。

**结果不可靠**

- **跨 5 年以上的任何结论**：用今天的目录回测过去，会系统性剔除 2015 年以来退市/摘牌的
  A 股（2026-10-26 快照：251 只；这些股票退市前普遍长期下跌、左尾极重，且是低价/
  小市值/高换手类因子的主要构成）。会**高估**低价、小市值、反转因子收益，**低估**最大回撤。
- **可交易性判定**（一字板、停牌、ST 的 5% 涨跌幅）：输入不全（§3.2 #9、#11、#12）。

**做不了**

- **没有幸存者偏差的回测**：缺退市股行情与时点主表（§3.2 #6–#8）。
- **严格可交易性回测**：缺停牌、ST、涨跌停三类输入。
- **逐位可复现的回测**：`export_research` 自 preview.36 起随发布包部署（`research-snapshot.v2`
  自 preview.37），但它是**离线命令**、不在 MCP 上（D-39）：要复现必须自己保留导出目录与
  `manifest.json`；直读路径（§5.2）仍不绑定版本。
- **参数/口径敏感性**：已部署实例有不复权与前复权两条序列（preview.36 起），但 `query_data`
  只读前复权；不复权要经导出或直读（§5.3）。

## 5. 现在的访问方式和限制

### 5.1 MCP 读取路径与上限

- `query_data`：批量选择 1..100 个实体（`src/data_supply/query.rs:85-86`）；`page_size` 1..1000
  （`query.rs:71-75`）；单次快照上限 100,000 行 / 16MiB（`query.rs:10-11`）；游标分页；服务端
  快照缓存 32 条 / 64MiB（`query.rs:12-13`）。
- 过滤条件：`market.daily`/`market.yearly` 只有日期/年份区间；`securities.catalog` 只有
  `code`/`name_query`/`industry_code`（`wire.rs:79-92`）。**没有**价格、成交量、市值、ST 等谓词。
- 版本选择：`latest_per_entity` / 显式版本映射 / 目录版本（`wire.rs:97-104`）。
- 目录读 scope 只有 `current_sh_sz_a`；`CatalogScope` 的 `sh_sz_a_listed_ever` 可用于
  `market.daily` 的 `start_collection` v2 与 `selection.kind=universe` 的 `scope`（#446 PR-9），
  目录数据集仍一律拒绝（`wire.rs`、`src/data_supply/query.rs`）。`universe.scope` 只接受
  `sh_sz_a_listed_ever`，与 `catalog_version` 互斥，且不传时行为不变。
- `query_data` 是纯读：不触发采集、修复或换源（`src/data_supply/registry.rs:139`）。
- 工具清单、参数与返回形状用 `get_data_capabilities` 查（`registry.rs` 的 `entries()`），本文不复述；
  data 侧当前部署三个入口（单进程三 socket）：`ops` 15 个 data 工具、`research`（只读研究面）
  与 `maint`（本机运维）各 5 / 9，数量以 [STATUS](STATUS.md) 的「服务」表为权威。

### 5.2 过渡期直读（#373 分析脚本）

`scripts/analysis/boll.py` 是过渡期的直读路径：`mode=ro` 打开 sqlite，按 `series_key` 主键范围
读 `series_bars`，不经过 MCP（`scripts/analysis/boll.py:1-29`）。使用时注意：

- **不绑定版本**：脚本只按 `series_key` 读当前行，不记录 `version_id`。两次运行之间发生日更或
  除权 `replace`，结果会变，且输出无法说明用的是哪一版数据。
- `trade_day` 是 1970-01-01 起的天数（`src/compact_market.rs:798-801`），不是 `YYYYMMDD`。
- 脚本只读、无写，不是服务/MCP/契约的一部分，也没有稳定性承诺。
- 需要可复现结论时，必须自己记录 `market_series.version_id` 与目录版本（§5.3 的清单形状）。

### 5.3 研究快照导出（preview.36 起部署，preview.37 起 `research-snapshot.v2`；v3 未部署）

- **随发布包部署**：`export_research` bin（feature `export`，`Cargo.toml:6-17`；默认构建不编译
  它；`scripts/build_release.sh` 自 #413 固定 `--features export` 并在打包后断言该命令存在），
  产物是 Parquet 文件 + `manifest.json`（`src/research/export.rs:266`），默认落
  `/root/x-data-exports/`（`src/research/mod.rs:894`；D-29）。
- **MCP 仍没有 `export_data`**：v8 只设计、不实现（D-39）；工具数与 `capability_revision` 不因此变化。
- **只读**：连接用 `SQLITE_OPEN_READ_ONLY` + `PRAGMA query_only=1`，并要求库的 `user_version` 等于
  代码的 `COMPACT_SCHEMA_VERSION`（当前代码 25，即 D-08 的全新 init；`src/research/read.rs:69-78`）。
- **导出范围**（`research-snapshot.v3`，代码已实现、**未部署**；已部署的仍是 v2）：M1 的 5 个文件
  （日线 `daily_raw`/`daily_qfq` + 因子 `factors` + 日历 `calendar` + 指数 `index_daily`）加 M2 的
  7 个文件（`security_master`、`security_names`、`capital_events`、`risk_warnings`、`suspensions`、
  `price_limit_rules`、`universe_intervals`），v3 再加 M3 的 2 个（`industry_assignments`、
  `code_migrations`），文件名固定、顺序固定（`src/research/mod.rs` 的 `file_plans`；
  `src/research/manifest.rs` 的 `SCHEMA`）。不复权、因子、日历、指数与 M2/M3 数据集在主线读取面上
  不都可读，但在导出里可见（AC-M1-10、AC-M2-11）。`files[]` 每项带 `dataset` 与 `temporal`
  （`as_of`/`known_at`/`known_at_coverage`），所以导出的时序能力跟着文件走。
- **v3 新列与覆盖**：`universe_intervals` 加 `canonical_entity`/`aliases`（与
  `market.security_master`+`universe` 的读模型同值，别名只来自 confirmed 迁移）；
  `industry_assignments` 带 `effective_from`/`effective_to`/`known_at`/`effective_from_basis`/
  `evidence_grade`/`source_snapshot_sha256`，所以每条归属可追到证明它的 `tdxhy.cfg`；
  `code_migrations` 带 `certainty`（candidate 不是证据）。清单加 `industry`（绑定 §9.4.2 的
  内容摘要版本）与 `coverage.delisted_series`/`industry_coverage_start`；早于
  `industry_coverage_start` 的日期在快照里没有行业，消费方必须按 `unknown` 处理，不能用当前行业补。
- **清单必须同时带**：目录版本、每序列 `version_id`、每条序列/派生数据集的来源 generation
  （manifest 与成员 sha256，逐项 `source_generation`；顶部 `generations` 是去重列表）、文件
  sha256，以及已知偏差说明（`src/research/manifest.rs`；`src/research/mod.rs` 的
  `LIMITATIONS`，v3 起含行业起点、回填名称 `unknown` 与 candidate 迁移三条）。原因：
  `version_id` 只锚定 bar 取值，不含 coverage 与来源
  （[latest-only-storage](latest-only-storage.md) §3），单靠它无法复现「这份数据从哪来」。
- **跨代导出（M1 修复 B）**：日更跨过 generation 切换后仍可全量导出；每一项记录自己的来源代，
  只有 binding 查不到（血缘无法证明）才拒绝。`manifest.json` 不写运行时间/WAL，两次同输入导出
  逐字节相同（v3 同一断言已覆盖）。
- **旧快照可读**：schema 升版不改读取方约定——`scripts/export_smoke.py` 同时接受 v2/v3，v2 目录
  不要求 M3 文件、`universe_intervals` 按 5 列校验；`snapshot_id` 定义不变，但 schema 字符串在
  身份里，所以同一输入的 v2/v3 `snapshot_id` 不同、不会互相覆盖。
- **实测**：preview.36 部署后全量 daily 导出 119s、900MB（`daily_raw` 16,737,243 行，见
  [STATUS](STATUS.md)）；含 M2 文件的 v2 导出耗时在 preview.37 部署后回填。v3 新增文件的体积与
  耗时尚未在部署实例测量（M3 数据未上线）；夹具层面 v3 不改变既有文件的编码。本机校验用
  `/root` 下的独立 venv（`scripts/export_smoke.py`，CI 里由 `export` job 跑）。

## 6. 维护规则

1. **谁更新**：数据范围、字段语义或缺口状态变化时，由提出变更的任务在同一 PR 内更新本文；
   部署数字不写进本文，改 [STATUS](STATUS.md)。
2. **事实只在一处**：实体数、日期范围、版本、generation 以 STATUS 为权威；本文只链接。
3. **历史内容**必须显式标「历史」，不删除、不改写成当前事实。
4. **门禁登记**：本文的当前区域登记在 `scripts/check_current_facts.py` 的 `CURRENT_REGIONS`
   （P1-2/#296 引入的机制），按 [STATUS](STATUS.md)「维护规则」的格式约定检查。新增当前态断言时
   先跑 `python3 scripts/check_current_facts.py`。

## 7. 与调研报告的差异（复核记录）

本文写进文档前把调研报告的结论逐条回到源码、DDL、官方文件与生产库复核。以下两处需要更正或
说明，以本文为准：

1. **2dp 取整对价格水平的影响被高估**。调研报告「42.5% 的前复权 bar < 2 元」；只读实测
   （2026-10-26，全库日线）是 **12.6%**，价格分档与库内 `close_cents` 分布一致。原因：该报告
   的 `qfq_round_err2.py` 用 `bisect_right(事件日期, 纪元天数)` 比较两种不同量纲的整数，导致
   **每个事件都被应用到每根 bar**；`qfq_check.py` 用的是同量纲比较，所以它的 99.53% 一致率结论
   仍然成立（本任务抽样复核：99.972%、全部 ±1 分）。
2. **日收益误差表需要在正确的事件链上重测**。报告的分档误差是在同一错误价格序列上算的，不能
   直接引用；按正确事件链重测（291 只 / 921,121 根）得到 §2.2 已知限制 1 的中位数
   （<0.1 元 2.9pp … ≥10 元 0.060pp）。定性结论不变：低价段收益类指标不可用。

其余引用（`.day` 是不复权原始价、退市股行情在官方包内、股本历史语义未取证、上市日/退市日
部分可补、停牌无官方标记、涨跌停取整规则、指数口径）均已复核，未发现需要更正的差异。
