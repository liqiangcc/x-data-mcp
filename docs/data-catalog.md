# 数据目录（研究数据层）

> **状态**：当前可用数据的入口说明。2026-11-05 对照 main `e1e6d99` 与 [STATUS](STATUS.md) 核对过部署状态措辞；
> 逐项审计见 [严谨回测验收审计](tasks/rigorous-backtest-audit.md)。
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
[generic-data-v11](contracts/generic-data-v11.md)（早期契约见 v7/v8/v9/v10）。本文写的是**语义与证据**，不是协议定义。

### 1.2 权威入口

| 内容 | 权威入口 |
|---|---|
| 工具清单、输入输出 schema、调用示例 | 运行时 `get_data_capabilities`（`src/data_supply/registry.rs` 的 `entries()`）；[generic-data-v11](contracts/generic-data-v11.md) |
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
- **官方公告索引的查询边界（2026-10-28 复核；2026-11-01 补正）**：上交所公告索引
  （`queryCompanyBulletinNew.do`）在日期区间超过**一个季度**时**静默返回 `total=0`**
  （不报错）；归档起点实测为 **2000-01**（1998–1999 各 1–3 个月窗口均为 0，2000-01 有 21 条、
  2000Q1 有 252 条）。所以查询必须按 **≤ 1 季度分段**，且任何「区间内 0 条 ⇒ 不存在
  该事件」的结论都不成立。D-44 中「公告索引 1993–1999 返回 0 条」这条依据因此失效
  （结论保留，依据已换，见 D-44 与 D-59）。**但「归档起点 2000-01」不等于「ST 事件从
  2000 年起可观测」**：按 `TITLE=风险警示|特别处理` 过滤时 2000–2003 每年 8 个季度窗口
  全部 `pageCount=0`（XDM-151 实测 2026-11-01），而同期不过滤的 2000Q1/2000Q3 分别有
  252/574 条且标题与 `SECURITY_NAME` 不含这两个词。因此沪市 ST 事件链实际从 **2004Q1**
  开始，2000–2003 永久是 `not_observed`。
- 「历史」标记的段落只作记录，不是当前事实。

### 1.4 严谨回测现状（#383 / #296，对照 [审计](tasks/rigorous-backtest-audit.md)、[裁决备忘录](tasks/rigorous-gaps-decisions.md)，2026-11-06）

**整体尚未达到严谨回测级别。** owner 首期目标（D-30）是无幸存者偏差、按时点正确、可判断能否交易、可复现；
做不到的部分如实写「未满足」，不降格成「部分提供」。读者判断单条数据可用性时，看 `unknown`（状态未知）、
`limitations`（字段里的降级说明）、`reason`（为什么是 `unknown`/`absent`）三个位置。

**状态取值**：**满足** = 这项能力真的做到了；**未满足** = 代码/数据/来源确认做不到；**在办** = 实现或数据任务正在进行，写任务名不写未合并结论。

#### 1.4.1 无幸存者偏差

| 项 | 状态 | 说明 | 依据 |
|---|---|---|---|
| 退市股行情在库 | 满足（已部署） | `sh_sz_a_listed_ever` 首采 338/338 成功（aws-node 2026-10-31，vm2 重建同样执行），日更有只读检查序列不消失、不被改写（`derived:delisted_series_check`） | STATUS「数据」段（M3 行）；`src/data_supply/delisted_series/tests.rs` |
| 时点股票池（A 股） | 满足（已部署） | `universe(as_of=T)` 由主表决定，不看目录；2015-01-05 的成员数 2,595，2015 年以来退市的 256 个都有 bar（2026-10-31 实测） | `src/research/universe.rs`；`src/research/read_model_tests.rs`；#446 评论（XDM-137） |
| 退市 ETF 身份与退市日 | 满足（已部署） | 128 只退市 ETF 已写入主表，精确退市日由 D-72 提供；沪市公告索引最早 2006-08-04，更早的沪市终止公告不可得；深市「已退市且不在 `hsjday`」的 ETF 未验证（本批只能从 `hsjday` 发现候选） | STATUS「M2/M3 表」（退市 ETF 行数）；D-71、D-72；§3.3 偏差 9 |
| 代码迁移与复用 | 满足（已部署） | `code_migrations` 109 行（108 candidate、1 confirmed）；导出的 `universe_intervals` 给出 `canonical_entity`/`aliases`，`candidate` 不作去重证据；`strict` 只在库函数里，MCP 与导出都没有开关 | STATUS「M2/M3 表」；`src/` 内无非测试调用 `.strict(` |
| 研究导出包含退市序列 | 满足（已部署；preview.46 读面生效） | #545 已修复默认选择为「目录头 ∪ 主表沪深 A 股 ∪ 已发布个股日线序列」，所以不在当前目录的退市序列进入 `daily_raw`/`daily_qfq`/`factors`。preview.46 起导出写 `research-snapshot.v5`；2026-11-07 真实库预检（XDM-198，`--dry-run`）读回 `coverage.delisted_series=337`、`schema=v5`、14 files planned / 0 written，与库内已知缺口一致 | #545、#383、`src/research/read.rs:236-258`；审计 §2.1；XDM-198；STATUS「部署」段 |
| 86 只现行 ETF 无日线 | 未满足 | 名单有、`hsjday` 无，全部在沪市 `sh526`（7）、`sh551`（12）、`sh589`（67）三个代码段；`hsjday` 在这三段各 0 个成员，源覆盖缺段（原因未验证）。主表照写、`market.daily` 读面按 `absent`/`source_member_absent` 返回 | §3.3 偏差 10 |

#### 1.4.2 按时点正确（有效时间与知识时间）

| 项 | 状态 | 说明 | 依据 |
|---|---|---|---|
| 不复权价是事实，前复权含未来事件 | 满足（已部署） | `.day` 的 OHLC 本身就是不复权原始价；前复权以最新事件为基准，除权会改写整段历史，只适合固定快照内使用 | §2.2「复权语义」 |
| 主表、简称、风险警示、停牌、行业都带生效区间 | 满足（已部署） | 六个 M2 数据集与 M3 行业都有 `as_of` 过滤；主表 `effective_from`/`effective_to`、风险警示/停牌/行业的区间列 | `src/research/temporal.rs`；`src/sqlite_supply/m2.rs`；`src/research/universe.rs`；§3.1 |
| 带 `known_at` 的 A 股股票池 | **未满足** | A 股主表行 `known_at` 为 NULL，带 `known_at` 参数会把所有 A 股行排除并计数。标准回测用不带 `known_at` 的 `universe(as_of)`（有效时间口径）。`known_at` 路径对 A 股未满足 | D-80；审计 §2.2；备忘录 T5 |
| ETF 的 `effective_from`（区间起点） | 满足（代码已部署；数据待第一次日更） | D-80 裁定：有 `listing_date` 时 `effective_from` 取上市日，`known_at` 仍是首次观察日（拆开生效与知晓）。preview.46 已含 #571；库内仍是 preview.45 的旧取值：1,846 个 ETF 行里 1,845 行 `effective_from` 晚于 `listing_date`，导出的 `universe_intervals` 在 2026-11-05 之前基本没有 ETF 成员。代码已生效，数据要等第一次日更重写主表（XDM-198 未重算主表） | D-80（修订 D-70）；#571；备忘录 §3；XDM-198；STATUS「部署」段 |
| 深市 ST 与简称的 `known_at` 从 2026-11-01 起 | **未满足** | 首次包含该行的快照入库日，更早的知识时间不可得。coverage 口径写两句：有效时间历史从来源起点起可用；知识时间 PIT（`known_at` 过滤）从首次采集日 2026-11-01 起可用。之前的日期，带 `known_at` 查询看不到任何深市历史行 | D-66 Q4；§3.1「简称与历史 ST 的知晓时间」 |
| 股本事件、复权事件、停牌、涨跌停规则无 `known_at` | **未满足** | 这四类数据无知晓时间，带 `known_at` 的市值、停牌、涨跌停查询得不到这些输入，信息集严格回测对它们做不到。如实标 `known_at_coverage: none` | `src/data_supply/wire.rs` 的 `known_at_coverage`；审计 §2.2 |
| 沪市 ST 生效日（公告没有显式生效日时） | 满足（代码已部署；884 行在第一次日更时后移；D-84） | 生效日 = 公告当日或其后第一个交易日的**下一个交易日**（公告日是交易日 → 下一个交易日；周六 → 周二），有显式标题日期时保留显式日期。规则读库内交易日历，不是周末规则；日历未覆盖到「下一个交易日」的事件不写行、状态保持 `unknown`，等下一次日更。档位 `OWNER_RISK_ACCEPTED`（规则推断，不是公告原文）：实施侧 607/617 配对命中，撤销侧 540/544；2004Q1 起才有事件，2000–2003 永久 `not_observed`。preview.46 已部署 R2；库内 884 行沪市事件仍是旧锚定（`effective_from=known_at`），第一次日更会整体后移 1–2 个交易日，`known_at` 仍是公告日（XDM-198 已见主键成对移动，见 STATUS 已知问题） | D-58；D-84 R2；`tasks/gp29-compare-20261106.md`；`tasks/revoke-effective-20261106.md`；XDM-198；§3.3 偏差 2 |
| 行业从 2026-09-30 起 | **未满足** | 归档最早只到 2026-09-30，更早历史没有快照，`industry_coverage_start` 之前一律 `unknown`；5,235 行 assignment 全部 `unknown_dictionary`（归档快照无同代 `incon.dat`，名称为 `unknown`，代码与区间可用）；早于此日的行业中性化做不到 | #446 评论（XDM-137）；§3.3 偏差 1；STATUS「M2/M3 表」 |
| 交易日历 | **未满足** | 指数日期并集推导，没有官方休市公告核对；只能证明「不在日历 ⇒ 非交易日」，不能证明「在日历 ⇒ 该证券当日可交易」；标 derived 并带算法版本与输入清单 | §3.3 偏差 3；§3.2 #2 |

#### 1.4.3 可判断能否交易

| 项 | 状态 | 说明 | 依据 |
|---|---|---|---|
| 沪市 2008 年起有 `confirmed` 停牌 | 满足（已部署） | 20,420 段（2026-10-31 首轮验收）；深市没有结构化来源，只有 `unresolved_gap` | STATUS「M2/M3 表」；D-49、D-60 |
| 涨跌停规则全部是数据 | 满足（已部署） | 27 行价格规则 + 2 行最小变动单位，改规则不改代码 | §3.2 #12；`src/research/price_limit/tests.rs` |
| 沪深主板、深创业板 2020-08-24 前无 `normal` | **部分满足** | **沪市**仍是 `unknown`：写入器不写 `normal`，没有 ST 区间的日子状态是 `unknown`，沪市主板的 ST 与非 ST 幅度不同（5% 对 10%），所以派生返回 `unknown`；沪市闭世界不成立（GP29 比对 2004–2024 召回 69.1%），不写 `normal`。**深市**闭世界已实现（下一行），代码已随 preview.46 部署；数据要等第一次日更重写 `risk_warnings` 后才出现 `normal`，此前线上仍全部 `unknown` | 审计 §2.3；备忘录 T3；`tasks/gp29-compare-20261106.md` |
| 深市风险警示闭世界（写 `normal`） | 代码已部署，数据待第一次日更（D-81） | 深市在简称表覆盖范围内、简称链连续、无 ST/*ST 区间、无 PT/退标记、不在冲突后间隙、主表有该代码且未过退市日的日子写 `normal`；沪市/北交所不写。R1/R4/R5 已合并（#569），闭世界本身见 #578。已知盲区：140 个带 ST 区间的已退市代码不在 GP29 包里，无法验证；22/1,729 个「历史/身份缺口」在 `known_at` 过滤下不可见；5 个区间起点晚 1–2 个交易日。preview.46 已含 #578；库内仍是旧行，第一次日更重写 `risk_warnings` 后出现 `normal` | D-81；#569；#578；XDM-198；备忘录 §1；`tasks/gp29-compare-20261106.md` §5 |
| 沪市生效日规则 R2 | 已部署（preview.46，2026-11-07；D-84） | 无显式生效日时，生效日 = 公告当日或其后第一个交易日的下一个交易日；实施与撤销同一规则；档位 `OWNER_RISK_ACCEPTED`（规则推断，不是公告原文）。`source_store replay --scope risk_warnings` 读 `--db` 的交易日历；fresh init 无日历时该 scope 为 `not_ready`（`all` 仍退出 0），日历未覆盖的公告行延后写入。`risk-warnings-v8` / `risk-warnings-v5`，`inputs_digest` 记录日历输入 | D-84 R2；[取证摘要](tasks/rigorous-gaps-decisions.md) §11 R2；`tasks/gp29-compare-20261106.md`；`tasks/revoke-effective-20261106.md`；[generic-data-v11 §5.8](contracts/generic-data-v11.md) |
| 配股除权日涨跌停基准 | **未满足** | 配股（C4>0）返回 `unknown` + `ex_rights_unverified`（公式准确率约61%） | D-84；审计 §2.3；备忘录 T4 |
| `limit-v2` 2010-09-14 之前官方基准 | 满足（已部署；preview.46 读面生效） | 2010-09-14 前，当日 bar 的 r7_raw 非 0 且非哨兵/手标记时用官方除权基准价（分）；preview.46 已部署这段分支 | D-84；#573 |
| `limit-v2` 2010-09-14 起用公式 | **未满足** | 2010-09-14 起或 r7_raw=0/哨兵时用 gbbq::reference_price；档位 `OWNER_RISK_ACCEPTED`，只经过证伪检验（越界 0.115%），没有官方基准 | D-84；#573；备忘录 T4 |
| 涨跌停规则书未覆盖的历史段 | **未满足** | R7（1996-12-16 前）无规则，返回 `unknown`，不回退到普通档；上市后第 2–5 日（主板 1996-12-16…2023-02-16、创业板 2009-10-30…2020-08-23）返回 `rule_missing`，方案待 owner 裁决；主板首日 44%/36% 段（沪 2013-12-13 起、深 2014-06-13 起）仍 `not_encoded`，同样 `rule_missing`；2023 前的首日无涨跌幅段已编码为 `band_side=none`，返回 `unbounded` | §3.3 偏差 7；§3.2 #12；`tasks/price-limit-rule-gaps.md` |
| `get_tradability`、`get_market_cap` 工具 | 在办（D-82） | 两个只读工具，薄封装 `research::tradability` 等 | D-82；审计 §3 |
| 市值 G2 对账（末端股本对账） | **未满足** | 生产 `reconciliation` 恒 `not_available`；`base.dbf` 的当前股本解析了没入库；快照日 5,224 只与 `base.dbf` 全部精确相等，但两边同是 TDX，不是独立证据。D-84 已定不存 | D-84 T6；审计 §2.3、§3；备忘录 §5；[取证摘要](tasks/rigorous-gaps-decisions.md) §11 T6 |
| A 股总市值（A+H 公司） | **未满足** | 交易所股本接口只返回当前一期，历史 A 股总股本不可得；`gbbq` 的 `total_after` 是公司总股本（A+B+H），不是 A 股总股本。「A 股总市值」如实返回 `unknown`；流通市值与公司总股本市值可算，必须标注口径 | §3.3 偏差 4 |
| 深市全域可交易性 | **未满足** | 深市没有 `confirmed` 停牌，只有 `unresolved_gap`；缺 bar 的日子可交易性如实返回 `unknown`（`data_gap`），不是 `suspended`；回测要按「未知」剔除或单独报告敏感性 | §3.3 偏差 6 |

#### 1.4.4 可复现

| 项 | 状态 | 说明 | 依据 |
|---|---|---|---|
| 同一数据库状态、同一命令，导出字节相同 | 满足（已部署） | `snapshot_id` 和全部文件（含 `manifest.json`）字节相同；`mode=ro` + `query_only`，期间日更改动会拒绝出清单 | `src/research/tests.rs`；#383 评论 3 |
| 清单绑定目录版本、序列 `version_id`、来源 generation | 满足（已部署） | 每条序列的 `version_id`、来源 generation 和 manifest sha256 都在 `manifest.json`；交易所来源冻结进来源库，按 sha256 保存，可离线重放 | `src/research/manifest.rs`；`tests/source_store/exchange.rs` |
| 清单记录二进制版本与算法版本 | 满足（已部署；preview.46 起导出写 v5） | #545 的 `research-snapshot.v5` 加 `producer` 块（`git_sha` 与 `algorithms`），涨跌停 `limit-v2`、市值 `market-cap-v1` 等读时重算的算法版本才能追溯。2026-11-07 预检（XDM-198）读回 `producer.git_sha=c35b425f…` 与 `limit-v2`/`market-cap-v1` 等算法版本 | #545；审计 §2.4；XDM-198；备忘录 §4 |
| 另一台机器逐位复现回测结果 | **未满足** | 不做回测引擎，#549 只比导出文件字节（含 `manifest.json`）；同一二进制导出同一库两次逐字节相同（含 `producer` 后 AC-M1-7）；不同二进制：`snapshot_id` 相同但 `producer.git_sha` 不同，Parquet 文件以 `files[].sha256` 为准 | D-83 #549；审计 §2.4；备忘录 §4；#545 的 `research-snapshot.v5` 契约 |
| 单版本存储：除权会改写整段前复权历史 | **未满足** | 旧 `version_id` 之后无法再读；复现只能靠保留导出目录；导出保留策略见 D-83 T8（不自动删除，只有 owner 能删，配额暂不设，退役代的导出标为不可替代） | [latest-only-storage](latest-only-storage.md)；D-83 T8；#383 评论 5 |
| generation 字节在两代之后退役 | **未满足** | 无法从原始官方文件重新派生旧快照，只能靠已登记的派生事实；退役策略 D-65 | STATUS「旧代回收」；D-65 |
| 来源库重放有已知差异 | 在办（#533） | `replay --verify-only` 的 `risk_warnings` 在 vm2 上报 `drift`：105 行沪市记录的 `source_document_sha256` 不同，原因是同一公告出现在多页时页序不同；#533 已修复，待发布后验证 | STATUS「已知问题」；#533 |


## 2. 已提供的数据

三个基础数据集：`securities.catalog`、`market.daily`、`market.yearly`
（`src/data_supply/wire.rs:9-20`）。三者都用同一个实体身份；另有指数、复权事件、日历
与下面的 M2/M3 数据集，共 13 个，合法组合以 `get_data_capabilities` 为准。

M2 的 6 个解析事实数据集（`market.security_master`、`security_names`、`capital_events`、
`risk_warnings`、`suspensions`、`price_limit_rules`）随 preview.37 **已部署**（v8 的
`contract_version:"v2"` 面上可读）。其中 `security_master`/`capital_events`/`suspensions`
有写入路径；交易所上市/退市名单（A-1/A-2，M3 PR-3 随 preview.39 部署）、历史 ST（#308，D-58）、
简称历史（D-61）、停复牌公告（A-3，D-60）与涨跌停规则书（A-7，D-44/D-59）的写入器都已部署
（数字见 [STATUS](STATUS.md)「数据」段）。A 股总股本（A-4）只准入了当前快照，**没有写入器**，
所以「A 股总市值」仍是 `unknown`。来源库（`X_DATA_MCP_SOURCE_STORE`）没有配置时：默认/非 ops 形态
的 item 结果记 `pending_source` 并写 0 行；生产形态（ops + compact）以稳定码
`source_store_unconfigured` 失败（见 §3.2）。aws-node 已配置来源库；vm2 的 drop-in 逐项配置 STATUS 标注
本轮未核实，但 vm2 第一次生产日更的派生 item 没有失败（STATUS）。未登记版本返回
`absent` 与 0 行（见 §3.1「M2 部署状态」）。
数据库 compact schema：**已部署 27（2026-11-05 preview.45 on vm2-126，D-74）**；aws-node fallback 仍是 26（preview.42 的 fresh init，2026-11-02，已冻结）。schema 25（M3 PR-1，
[#446](tasks/issue-446-m3-delisted-industry.md) §9）加 5 张来源库索引表与
`price_limit_rules.min_band_ticks` 可空列。5 张索引表已由日更的派生 item 与 `--scope` 重放写入
（2026-11-01 只读实测：`industry_snapshots` 7 行、`industry_assignments` 5,235 行、
`exchange_source_snapshots` 1,834 行、`generation_facts` 12 行、`retire_exemptions` 0 行；代码 26 另加
`source_refresh_runs`/`source_refresh_run_sources`/`source_refresh_run_members` 三表并把快照索引主键改为
`(snapshot_id, request_fingerprint)`，见
[STATUS](STATUS.md)「数据」表）；`min_band_ticks`
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
- 官方 `hsjday.zip` 里有、目录里没有的 A 股代码（退市/摘牌）**不在**本数据集内；它们的日线在
  `market.daily`（`sh_sz_a_listed_ever` 首采，§3.2 #8），成员身份看 `market.security_master`。
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

**是否满足严谨回测级**：**否**。时点股票池与证券主表已部署，退市股行情已在库；目录本身仍是当前快照，
时点成员要用 `market.security_master` 与 `universe`（§3.1）。其余偏差见审计第 2 节。

### 2.2 `market.daily`

**身份**：`{exchange, code}` + `period=1` + `adjustment=1`（`src/market_data.rs:11-13`；DDL `migrations/tasks/021_latest_only.sql:11`）。preview.36 起
部署实例同一实体同一周期有 `adjustment=0` 不复权与 `adjustment=1` 前复权两条序列，
`query_data` 缺省读前复权；A 股显式 `adjustment=raw` 读 2dp 不复权序列，ETF 必须显式 `raw`
（[generic-data-v10](contracts/generic-data-v10.md) §1.5），见 [latest-only-storage](latest-only-storage.md) §13。

**覆盖范围**：

- 采集范围是目录覆盖 ∩ 官方 `hsjday.zip` 成员，加上 `sh_sz_a_listed_ever` 首采的退市序列
  （aws-node 2026-10-31 首采 338 条；之后由日更的 `derived:delisted_series_check` 检查不消失，§3.2 #8）。
  ETF 是另一种证券类型，只在 `adjustment=raw` 下可读（§3.2 #13）。
- 官方 `hsjday` 缺成员的目录实体：没有序列，查询侧是 `absent`，不返回空 bar 列表冒充「无交易」
  （`observe.rs:75-91`；`docs/design.md`「产品闭环与排除范围」：空结果不证明停牌/退市/无交易）。
- **新进入目录的实体不会自动首采**：`plan_market_update` 只把已有日线 head 的实体列为
  eligible，没有 head 的归 `absent`（`src/data_supply/plan.rs:544-551`）。新上市证券必须走
  显式首采（`start_collection`）才会建序列，否则长期 `absent`。

**字段表**（存储列见 DDL `021_latest_only.sql:30-45`；查询形状见 `wire.rs:173-190`）

| 字段 | 存储 | 查询返回 | 单位 / 精度 | 可空 | 来源与证据 |
|---|---|---|---|---|---|
| `date` | `trade_day INTEGER`（1970-01-01 起的天数） | `date` | 交易日（Asia/Shanghai 自然日） | 否 | `021_latest_only.sql:32,40`；`src/compact_market.rs:798-801` |
| `open`/`high`/`low`/`close` | `*_cents INTEGER`（schema 21 库）/ `*_units INTEGER`（代码，schema 22 起） | 十进制字符串 | **元**。按序列的 `price_scale` 解释整数：A 股不复权 2 位、前复权 6 位（`qfq-exact-v1`，preview.36 起）、ETF 不复权 3 位。2 位小数 HALF_UP 是 schema 21 时期的口径，只在历史对照里出现 | 否 | DDL `022:74-99`；`src/compact_market.rs:17-24`（`MAX_PRICE_SCALE=6`、`PRICE_SCALE_CENTS=2`、`PRICE_SCALE_LI=3`、`PRICE_SCALE_QFQ=6`）。历史：schema 21 DDL `021:33-36`；`src/qfq.rs:68-74` |
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
3. **前复权序列的成员**：退市股行情已在库（§3.2 #8），但目录只含当前成员；跨年回测要用 `universe` 与
   `sh_sz_a_listed_ever` 的序列，不能用目录选股（§4.3）。
4. **可交易性输入不全**：不复权收盘、停牌区间（沪市 2008 起有 `confirmed`，深市只有 `unresolved_gap`）、
   股本事件、ST 区间（深市 `normal` 代码已随 preview.46 部署、数据待第一次日更；沪市仍无 `normal`）、涨跌停规则行都已入库，但没有派生读面（审计 §3），
   除权日涨跌停基准不准，深市缺 bar 的日子可交易性是 `unknown`（§3.2 #9、#11、#12）。
5. **已发布数据里有少量「手日」bar 的 volume 少记 100 倍**：`.day` 在成交量接近 u32 上限时改写
   为「手」并置 `r7_raw=0xC36400xx`。发布路径已修（#381/#382：标记与 VWAP 校验同时成立才换算，
   否则 volume 发布为未知；手日只精确到 100 股）。修复 `d738488` 在 preview.36 的祖先里；#383 评论 2 在重建后的私有栈库里抽查 `sz.000725` 三根 bar 已是
   修复后的值，部署库在 fresh init 后应同样如此（推断，部署库未单独抽查）。历史：2026-10-26 的只读实测
   在旧库里有 19 根、7 只证券。
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
提交同事务写入；`market.security_names` 的写入器与读取面均已部署（XDM-163 起 `query_data` 读取
`security_name_history` 区间并带 `metadata.verified_through`，未登记版本仍回答 `absent`/0 行）。**来源状态**：
交易所上市/退市名单（A-1/A-2，D-45/D-46）、历史 ST（D-58）、停复牌公告（D-60）、涨跌停规则原文
（D-59）与简称历史（D-61）都已准入并有写入器，A 股总股本（A-4，D-49）只准入了当前快照、没有写入器。
规则书与 `derived:trading_rules` 的写路径随 preview.39 部署（仓库内版本化规则数据 + 校验；内容不变为
`no_change`），STATUS 记录的 27 + 2 行规则已入库。默认/非 ops 形态下来源库未配置的部分仍在 item
结果里记为 `pending_source`，对应数据集写 0 行、不登记版本（读取仍是 `absent`），不把未实现的部分
伪造成数据；生产形态（ops + compact）缺来源库时四个同源 item
（`security_master`/`risk_warnings`/`suspensions`/`security_names`）以稳定码
`source_store_unconfigured` 失败，见 §3.2。
**历史 ST 写入器（已部署）**：`market.risk_warnings` 不用当前名称推断历史，`unresolved`
也不伪装成已知区间；默认形态下来源库未配置时写 0 行并记 `pending_source`，生产 strict 下
缺 store 则失败；都不使用当前值回填。**沪市写入器从不写出 `normal` 区间**，所以没有区间覆盖的
日期永远是 `unknown`；**深市**（D-81，任务 49，代码已随 preview.46 部署、数据待第一次日更）在简称表覆盖范围内、无 ST、
简称无 PT/退/`*`、不在冲突后间隙、且主表有该代码未过退市日的日子写 `normal`
（`validate_set` 只对 `source_kind=szse_name_history` 放行 `normal`）。数据生效前线上仍全部 `unknown`，
主板涨跌停因此在非 ST 区间的日子里也是 `unknown`（审计第 2.3 节）。数据量见
[STATUS](STATUS.md)（本文件不写部署数字）。

**风险警示时点规则（D-81 R1、R4、R5，preview.46 已部署；数据待第一次日更）**：契约全文见
[generic-data-v11 §5](contracts/generic-data-v11.md)。(R1) `known_at` 是日期，`known_at = D`
表示 D 日收盘后才保证已知；收盘后决策用截止 `D`，开盘或盘中决策必须用 `known_at ≤ D-1`
（上一交易日）。过滤本身不变（`≤`，含当日）。(R4) 一期不支持按更正重放：沪市同一
`(公告 id, 事件)` 在日更读取的页里日期不同，`derived:risk_warnings` 以 `source_invalid` 失败，
不再保留第一个日期。(R5，修订 D-68 第 4 项) 同日实施与撤销（沪）或同日两条简称变更（深）只影响该代码：
冲突日写一行 `marker_only_unresolved`，其余代码照常发布，冲突代码与日期进数据集证据
（`same_day_conflicts`：数量加前 20 个样本）；`marker_only_unresolved` 在读路径上不是 `normal`。
算法版本 `risk-warnings-v1` → `v2`，解析器 `v5` → `v6`，内容摘要与 `version_id` 不变，无 schema 变化。

**风险提示标题不再算实施（D-81 GP29 §7，preview.46 已部署；数据待第一次日更）**：「存在被实施退市风险警示及暂停上市风险的
提示(性)公告」陈述风险存在，不是实施决定，与 `将被`/`可能`/`争取` 同类排除；判定是「`存在` 在第一个
`实施`/`实行` 之前，且该动词之后仍出现 `风险`」，只在别处提到 `存在` 或 `实施` 在前的真实施公告不受影响。
取证：14 条沪市公告（全部 2014-12，代码 600076、600146、
600193、600301、600315、600368、600403、600421、600469、600610、600656、600671、600822、600966）
此前被判成 `Implement(DelistingRiskWarning)`，GP29 在这些行附近没有 `2/3` 事件；其中 7 个
（600076、600315、600368、600469、600656、600822、600966）因此一直是开区间，自 2014-12 起约 2,889
个交易日被读成退市风险警示。第一次日更重写时排除这 14 条，7 个开区间消失，这些代码回到 `unknown`
（不是 `normal`）。算法版本 `risk-warnings-v3` → `v4`（`v3` 已由 #578 的深市 `normal` 占用），解析器
`v6` → `v7`；重放与日更写入仍逐行相同，除这 14 条外其余行不变；无 schema 变化、无 `tools/list` 变化。

**风险警示开区间按退市日截断（#587，preview.46 已部署，读面立即生效）**：读面（`get_security_state`、`query_data`、
`get_security_history`）在 query 过滤阶段取 `security_master.delisting_date`，把 `effective_to` 为空的开区间
截到退市日：有效终点 = min(存储的 `effective_to`, 退市日)，返回行的 `effective_to` 写截断后的值。
**退市日是排他上界**（`as_of >= delisting_date` 不再返回），与 `security_master.effective_to` 的区间终点
和 `member_state` 的 `Out(AfterDelisting)` 同口径；退市日当天证券已不在成员内，因此不再读成 ST。
已关闭且 `effective_to <= 退市日` 的区间不变；没有退市日或主表查不到的代码行为不变（没有可用的上界，
不猜）；同一代码被复用时取区间起点所在生命周期行的退市日。截断是读时过滤，导出（
`research::read::risk_warning_rows` 仍带 `effective_to = NULL` 的原始开区间）、`version_id`、
`snapshot_id`、schema、`tools/list` 都不变。契约全文见 [generic-data-v11 §5.7](contracts/generic-data-v11.md)，
决策见 D-86。影响面按 #579 取证是沪 104 + 深 68 个开区间行。

**简称与历史 ST 的知晓时间（XDM-157，已部署）**：深市简称/ST 行的 `known_at`
不再是 NULL，而是“首次包含该行的快照入库日”（`issue-446` §5.3/Q4）：适配器解析后按来源库清单
（`manifest.jsonl` 的 `acquired_at`）的**上海日期**（`crate::calendar::shanghai_date_of`；UTC
时间戳在北京时间 00:00–08:00 会把日子记早一天）从旧到新读回冻结字节，取最早包含该行的一天
（同一 sha 被重复登记时取最小值）；整表重写不改已记的日期，所以第二轮新增行取
第二轮日期、未变行保留第一轮日期；没有任何已登记快照包含该行时 fail-closed。`--scope
security_names`/`risk_warnings` 重放用同一个函数，且不需要数据库（入库日存在于来源库，2026-10
的 fresh-init 重放因此与首次写入给出同一天，`--verify-only` 也能比对 `known_at`）。coverage 按两句写，不合成一句：**有效时间历史
从来源起点起可用；知识时间 PIT（`known_at` 过滤）从首次采集日起可用**——首次采集日按上海日期
是 **2026-11-01**（UTC `2026-10-31T18:43:40Z`，北京时间 02:43；Claude 审核裁定 2026-11-01，
D-66 Q4 原文的 2026-10-31 是 UTC 日期）。两句同时进数据集证据的 `known_at_coverage`
（`effective_history` / `known_at_pit`，后者由 `first_capture_day` 算出，不写死日期），不写成
“简称历史从首次采集日起可用”。数据集级还有 `manifest[].metadata.verified_through`：四个来源 item 的
输入来源里最新的完整刷新确认日取 `min`（`source_refresh_run_sources` 里 `complete=1` 且 run
`succeeded`），没有任何完整 run 时字段缺省，不用页面 `as_of` 猜；它属于数据集“确认到什么
时候”，与行级 `as_of`/`known_at` 是三个字段。导出侧见 §5.3 的 `v4`。

**M2 已知限制（历史：XDM-74 补测，2026-10-27，A-1/A-2 接入前的数字；现值见 [STATUS](STATUS.md)「M2/M3 已知限制」）**：

- 退市日只有 TDX 整理期口径：`delisting_date` 有值 **152** 行（来源全部 `tdx_arrangement`），
  其余 `unknown`。交易所名单（A-1/A-2）未准入，主表因此比 catalog **多 279 个实体**
  （沪主板 118 / 深主板 116 / 创业板 42 / 科创板 3），这批当日无 bar 且退市日未知，被当成仍在册；
  `universe(as_of=2026-10-23)` 为 5,351，catalog 为 5,226。导出快照的成员带 `delisting_unknown`
  标记（`src/research/universe.rs:713-715`）。该标记的条件只是 `delisting_date` 为空，
所有在市股票都满足，**不能用来筛出可能已退市的成员**（审计第 2.1 节）。
- 代码复用与拆分仍 pending：`600018` 只有 1 行（2006-10-26 起，无 `effective_to`），没有拆分，
  `ambiguous_code_reuse=0`；A-1/A-2 接入前不把「只有一行」当成「已验证无复用」。
- `market.suspensions` 的读面只返回**覆盖 `as_of`** 的缺口（`row.covers(as_of)`，
  `src/sqlite_supply/m2.rs:150-157`）：按日期范围查时给 `as_of`；范围内其它缺口只在
  `temporal.unknown_known_at_rows` 里计数，不返回行。

这 5 条同样是目录视角的验收建议，编号与 #391 Issue 里的 AC-M2-1…14 **不是同一套**（例如这里的 AC-M2-3
是股本对账，Issue 的 AC-M2-3 是代码复用）；逐条状态见 [审计](tasks/rigorous-backtest-audit.md) §4.2。

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

**M3 状态**：PR-1a…PR-12 均已合并并部署（都在 preview.39 内，2026-10-31 phase-g fresh init；
历史 ST 的首次成功写入要等 XDM-144（#482）、#485、#489 的修复；现行 vm2-126 为 preview.46，
见 [STATUS](STATUS.md)「数据」段）。
已部署的能力：
来源库与重放框架（PR-1a）、M3 引入的 5 张来源库索引表（PR-1，M3 索引表迁移）、交易所来源共用入口
（PR-2）、A-1/A-2 上市/终止上市适配器与主表合并（PR-3）、`sh_sz_a_listed_ever`
首采 scope（PR-4，已首采 338 条序列）、日更尾巴与 `derived:delisted_series_check` 不消失检查
（PR-5）、
行业快照落库/区间计算/来源库重放（PR-6）、`market.industry_assignments` 行业区间读面
（PR-7）、退役门禁（PR-8）、`research-snapshot.v3` 导出（PR-9）。落地记录见
[来源库](source-store.md)、[交易所来源入口](exchange-source.md)、[日更](daily-sync.md)
与 §5.3；设计拆分见 [M3 设计](tasks/issue-446-m3-delisted-industry.md) §14。
生产实例已是 schema 27（2026-11-05 preview.45 on vm2-126，D-74；aws-node fallback 为 schema 26），`X_DATA_MCP_SOURCE_STORE` 已配置
（`source-store.conf`），owner 归档的行业历史也已导入并由 `--scope industry` 重放。
PR-10/11/12（历史 ST、停复牌、简称写入器）同样已部署：简称 7,344 行、停牌 `confirmed`
20,420 段（preview.41 库）；历史 ST 在 preview.42 的 fresh init 重放后为 1,289 行
（`known_at=2026-11-01`，SSE 覆盖只到 2000Q1）。数字是各自时点的读数，当前值以
[STATUS](STATUS.md) 为准。

`#446` 侧的 AC-M3-1…10 与本节下面的 AC-M3-1…4 不是同一套：本文件这 4 条是目录视角的
验收建议；#446 的 10 条实现验收与私有栈证据清单见 M3 设计 §14.3，首轮执行结果见 §14.12
（preview.39，2026-10-31）。逐条状态见 [审计](tasks/rigorous-backtest-audit.md) §4.4。

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
| 6 | 上市日 | 已提供（已部署） | `market.security_master` 用 `base.dbf` 的 `SSDATE` + `infoharbor_spec.cfg` 组装（preview.37 部署）；M3 PR-3 已实现 A-1/A-2 适配器（沪深交易所上市/终止上市名单，只取 A 股，`parser_version=exchange-a1a2-v4`），按「交易所 A-1/A-2 > `SSDATE` > `unknown`」合并，两者不同则 `listing_conflict=1`、取交易所值并保留 `listing_date_tdx`（M2 已有列）；`sz.302132` 的交易所「A股上市日期」是被继承的 2010-08-27，不据此开新区间（区间起点由 confirmed 迁移 2025-02-17 决定）。**已部署**（preview.39 起）：默认/非 ops 形态未配置 `X_DATA_MCP_SOURCE_STORE` 时仍写 `pending_source` 并按 `unknown` 返回；生产形态（`data_service` 的 ops 入口 + compact 后端）未配置或不可读时 `derived:security_master` 以稳定码 `source_store_unconfigured` 失败、run 收尾 `partially_failed`，日线照常、上一版主表仍是 latest。实测（2026-10-31，#446 评论 XDM-137）：5,565 行，`listing_date` 的 `unknown` 为 0（`exchange_list` 5,559、`tdx_ssdate` 6），上市冲突 2 行（含 `sh.600018`）；当前值见 [STATUS](STATUS.md) | 无（D-45/D-46 已准入） | M2/M3 | 满足（`unknown` 0；冲突两侧值保留） |
| 7 | 退市日 | 部分提供（已部署） | `infoharbor_spec.cfg` 的退市整理期区间（169 行、2013 年起）随主表部署；M3 PR-3 接入 A-2 终止上市名单（沪 `COMPANY_STATUS=3` 且 `STOCK_TYPE∈{1,8}`；沪名单按 `pageHelp.pageSize=2000` 一次取完、`pageCount>1` 则 fail-closed，深 `1793_ssgs` + `selectModule` `main`/`nm`，响应按 `metadata.tabkey="tab2"` 取终止上市块（`tab1` 暂停上市 0 行且 `pagecount=0`，页码/`pagecount`/行数组同取 `tab2`；无 `tab2` 时只接受唯一有非空 `data` 的块，多义 fail-closed）），优先级「交易所 `DELIST_DATE` > 整理期 end > `unknown`」，冲突时 `delisting_conflict=1` 且两值都存（整理期与退市日口径分开）；退市原因 `NOT_AVAILABLE` 一律 `unknown`。**已部署**；默认/非 ops 形态未配置来源库时仍 `pending_source`，生产形态（ops + compact）则以稳定码 `source_store_unconfigured` 失败（日线照常、上一版主表仍是 latest）。实测（2026-10-31，XDM-137）：`delisting_date` 有值 337 行，来源全部 `exchange_list`，`tdx_arrangement` 0；退市冲突 15 行，两侧值都存。其余 5,228 行是在市证券，退市日为空；`sh.600849` 吸收合并只有 candidate 迁移，仍按 `unknown` | 无（D-46 已准入） | M2/M3 | 部分满足（已退市行全有日期；退市原因一律 `unknown`；吸收合并个别无日期） |
| 8 | 退市股行情 | 已提供（已部署，已首采） | 官方 `hsjday.zip` 保留了目录之外 A 股代码的完整历史（2026-10-26 快照：335 个代码，其中 251 个末根 bar 在 2015 年及以后），跨代不消失。M3 PR-4 已实现 `market.daily` `start_collection` v2 的 `{"kind":"scope","scope":"sh_sz_a_listed_ever"}`：解析 `security_master` 中沪深 A 股且 `has_series=false` 的实体 ∩ 当前 generation 的 `.day` 成员，预算 1..500，跳过无 bar 成员并记入证据；已有 head 的实体跳过。**已部署并已首采**：aws-node 2026-10-31 首采 338/338 成功，vm2 的 fresh init 重建同样执行（STATUS）；之后日更 plan 不会自动纳入新的退市序列，需要操作员显式执行（Skill §3.9）。M3 PR-5 已补齐日更维护面：eligible 集取「当前目录 ∪ 上一版目录成员 ∩ 已首采」，成员核实后移出目录不会丢掉退市日当天的最后几根 bar；每次日更还有一个只读 item `derived:delisted_series_check`，比对 `record_count`/`first_day`/`last_day`/`content_digest`，序列消失或被改写 → `partially_failed` + 实体清单，不自动重建（历史改写走 `republish_data`）。实测 checked 337、unchanged 337（STATUS） | #246（范围）、#446 | M3 | 满足（338 条序列在库；导出默认**不一定**含它们，见审计 §2.1） |
| 9 | 历史 ST / \*ST | 部分提供（已部署） | 2026-10-28 owner 按市场分档准入（D-58）：**深市** `SSGSGMXX`/`tab2` 的 ST 区间与**沪市**公告索引的状态事件（事件级）为 `APPROVED_SOURCE`；沪市生效日按 D-84 R2 取「公告日当天或其后第一个交易日的下一个交易日」（显式标题日期优先），档位 `OWNER_RISK_ACCEPTED`；日历未覆盖的事件不写行、状态 `unknown`，北交所 `NOT_AVAILABLE`；无公告一律 `unknown`，禁止「无公告 ⇒ 非 ST」。**沪市实际可观测起点是 2004Q1**：公告索引归档始于 2000-01，但 `TITLE=风险警示|特别处理` 在 2000–2003 每年 8 个窗口全为 `pageCount=0`（XDM-151 实测），这 16 个季度永久 `not_observed`，不得写成「2000 年起覆盖」（见 §1.3 与 §3.3 偏差 2）。写入器已实现（PR-10/#467）：整表替换 `risk_warning_intervals` 并登记 `derived_dataset_versions`，`derived:risk_warnings` 日更接入，`source_store replay --scope risk_warnings` 可离线重建（深市简称表按计划声明的 `TABKEY=tab2` 选块，`tab1` 是空块，live 与 replay 同块；解析器 `risk-warnings-v8`：沪市 `pageHelp.data` 是公告分组 `[[正文, 附件…], …]`，逐文档分类后按 `(公告 id, 事件)` 去重；标题分类按 D-68（撤销 A 并/继续实施 B 记 `Implement(B)`；申请/拟/可能/将被/预计/预审计/将消除/争取与第三方专业文件不计入，标题以「公告」（去尾部括号注记）结尾的发行人公告不套第三方守卫；`撤消`/`解除` 同 `撤销`），分页走 `pageHelp.beginPage`/`endPage` + `pageSize=100`，同窗口重复公告 id 判 `truncated`）；空结果记 `not_observed` 并在缺口处切断未闭合区间，不把 0 行当无事件。**已部署**：默认/非 ops 形态未配置 `X_DATA_MCP_SOURCE_STORE` 时写 0 行/`pending_source`，生产形态（ops + compact）未配置或不可读时 `derived:risk_warnings` 以稳定码 `source_store_unconfigured` 失败（run `partially_failed`，日线照常）。写入器**沪市不写 `normal`**：无区间覆盖的日期是 `unknown`；深市自 D-81 任务 49 起在简称表覆盖内写 `normal`（代码已随 preview.46 部署、数据待第一次日更，见 §1.4.3）。实测：preview.42 重放后 1,289 行（深市区间 + 沪市事件，`known_at` 最早 2026-11-01）；D-68 修复（preview.43）后沪市记录为 884 行（STATUS「已知问题」，aws-node 与 vm2 相同）；preview.46 的 replay 校验因 D-84 R2 日期平移报 `rows_changed`，同主键 sha 差异待 11-09 日更后确认（STATUS「已知问题」） | #308；读面退市日截断见 #587 | M2（沪市 2004Q1 前为硬缺口，见 §3.3 偏差 2） | 部分满足（深市区间 + 沪 2004Q1 起事件级；2004Q1 前/北交所 `unknown`；无 `normal`；开区间在退市日截断，见 §3.1） |
| 10 | 历史行业 | 部分提供（已部署） | 当前行业（catalog 的 TDX 研究行业二级）已随目录部署；**历史区间不再只能从今天起积累**：M3 PR-6 已实现 `derived:industry_snapshot`（每代 `tdxhy.cfg` 落快照 + 区间变更集；`effective_from` 取发布方 `last_modified`，缺失取获取时间；缺同代 `incon.dat` 时名称为 `unknown`；早于最早观测返回 `unknown`）与 owner 归档 A/B 档的 `source_store import` + `replay --scope industry` 回填；PR-7 已实现读面 `market.industry_assignments`（`as_of`/`known_at`、`status=assigned/unknown` 带 `reason`、`source_snapshot_sha256`/`evidence_grade`）。**已部署并已导入归档**：最早只到归档最早快照（实测 2026-09-30；C 档 2026-09-16 只作交叉核对），5,235 行 assignment 全部 `unknown_dictionary`（归档快照没有同代 `incon.dat`，名称为 `unknown`，代码与区间可用）；更早仍 `unknown`，不能用当前行业冒充。替代来源（证监会行业分类）需取证 | #1、#446 | M3 | 部分满足（区间与来源可查；早于积累起点 `unknown`） |
| 11 | 停牌 | 部分提供（已部署） | 官方文件没有停牌标记，`.day` 也没有占位行。A-3 已由 D-49 准入、2026-10-28 定交付口径（D-60）：沪市 **2008 年起**可实现 `confirmed`（结构化事件表，`endStopDate`=复牌日前一自然日），**深市首期没有 `confirmed`**（无结构化源，`BULLETIN_TYPE` 过滤不可用），只到 `unresolved_gap`；深市缺 bar 的日子可交易性是 `unknown`（§3.3 偏差 6）。PR-11 写入器已部署（preview.39 起），`confirmed` 20,420 段（2026-10-31 首轮验收）；默认/非 ops 形态未配置来源库时只有 `unresolved_gap`，生产形态（ops + compact）缺 store 时 `derived:suspensions` 以稳定码 `source_store_unconfigured` 失败。`confirmed` 的 `known_at` 为空（事件表没有公告日，D-49）。零交易日语义（XDM-134，已部署）：confirmed 区间未完全落在日历 `[first_day,last_day]` 内（含整段在 `last_day` 之后、以及部分越界）→ `deferred_beyond_calendar`，区间在覆盖内但整段无交易日（纯周末/假日）→ `skipped_no_trading_day`；两种情况都**不写行**（表约束 `missing_days>0` 不变，不 clamp、不写部分计数）、item 成功，计数与样例进 item outcome 与 `derived_dataset_versions` evidence；deferred 区间的已覆盖子段仍向 gap 派生解释缺 bar（不写行） | #404（A-3） | M2（标记）/ M3（闭合） | 部分满足（沪市 2008 起 `confirmed`；深市只有缺口） |
| 12 | 涨跌停 | 部分提供（规则已入库；无读面；主板无 ST 区间时 `unknown`） | 结构随 preview.37 部署（`market.price_limit_rules`，按生效日期管理、每行带 `basis_ref`/`basis_status`）。**A-7 已由 D-44 关闭**（2026-10-26，2026-10-28 复核确认，见 D-59）：规则文本 `APPROVED_SOURCE`、R1/R3 起点与两处条文缺口 `OWNER_RISK_ACCEPTED` 且标「推断」、R7 `NOT_AVAILABLE`。规则书（R1–R9：27 行涨跌停规则 + 2 行最小变动单位）已作为版本化规则数据入库，日更 `derived:trading_rules` item 写入两表并登记版本（不变为 `no_change`）；已部署，库内 27 + 2 行（STATUS）。**读面：没有**——`limit-v1` 派生只在库函数 `price_limit` 里，MCP 与导出都不暴露（审计 §3），`market.price_limit_rules` 只返回规则行。**D-64（2026-10-29）另补两批数据-only 行**：R2 前段 5 行（1998 起点 `OWNER_RISK_ACCEPTED`：深交所简称表首个 ST 事件 + `.day` 锚定；沪市只有 `.day`，D-58 的沪市 2000 年前 ST 状态仍 `NOT_AVAILABLE`；2006/2011/2013/2020-12 条文段 `APPROVED_SOURCE`）与 R6 首日无涨跌幅 5 行（`band_side=none`，派生 `unbounded` 而不是 `unknown`）。剩余偏差：1996-12-16 前（R7）无规则 → `unknown`；主板首日 44%/36% 段（沪 2013-12-13 起、深 2014-06-13 起）仍 `not_encoded` → `rule_missing`；沪市风险警示股前收 ≤0.10 元的一 tick 例外已由 schema 25 的 `min_band_ticks` 落地（沪 2013-01-01…2023-02-16、沪 2023+ 的两行 ST 与深 2011-02-28…2023-02-16 的 ST 行置 1；深 2023+ 两行因 G-4 原文未取得保持 NULL，极低价仍是已知未编码的百分比结果），`limit-v1` 同时保留 1 tick 价格下限。ST 状态未知时，仅 `status_class='all'` 的板块/时段（创业板 2020-08-24 起、科创板，以及 2023 前的首日无涨跌幅段）有确定值，其余 `unknown`；因为沪市写入器不写 `normal`，沪市主板在没有 ST 区间的日子仍属「状态未知」；深市自 D-81 任务 49 起有 `normal`（代码已随 preview.46 部署、数据待第一次日更），数据生效后深市主板无 ST 区间时给出确定值。**limit-v2（D-84）**：除权日基准价算法。(1) 配股（C4>0）：`unknown` + `ex_rights_unverified`（公式准确率约61%）。(2) 2010-09-14 前，当日 bar 的 r7_raw 非 0 且非哨兵/手标记：官方除权基准价（分）（preview.46 已部署）。(3) 其余（2010-09-14 起或 r7_raw=0/哨兵）：gbbq::reference_price 先 HALF_UP 取整到分，再乘涨跌幅，多事件链式；`price_scale<2` 或参考价≤0 返回 `unknown`。(4) 无事件：前收。**2010-09-14 之后的基准是 `OWNER_RISK_ACCEPTED`**：没有官方基准，只经过证伪检验（越界 0.115%），不等于证明。`ex_rights_unverified` 现在只有配股或非正数参考价才出现。`price_limit` 在 `membership=Out(AfterDelisting)` 时原因码为 `delisted`（#587，与 `not_listed` 分开，答案仍是 `unknown`；其余 `Out(_)` 仍是 `not_listed`） | #395（A-7）、#308（ST）、#587 | M2 | 部分满足（规则行已入库；无读面；主板 `unknown` 为主） |
| 13 | ETF | 部分提供（已部署：身份、采集、日更、`query_data` 读面；导出 `v4` 含 ETF） | 身份由交易所名录锚定：**SSE `FUND_LIST` + SZSE `1945`（D-70 Q1，`ebs` 仅作报价锚）**；退市 ETF 由 **D-71** 的两个公告索引（沪 `COMMON_PL_JJXX_JJGG_L`、深 `etfNotice_disc`）按「候选 ∩ 公告命中」准入，精确退市日由 **D-72** 的公告 PDF 一次性提取结果提供（生产代码不解析 PDF）；`sh_sz_etf_current` 取最新名单，`sh_sz_etf_listed_ever` 取所有已冻结名单 + 退市公告的并集且移出名单不删行。`.day` 对 ETF 的价格单位是**厘**（3 位小数）、`volume` 是**份**（≥4.29e9 份时改写为手）、gbbq 还有 cat11（份额折算）未进 QFQ；精度/量纲与「A 股字节不变」的逐处改动清单见 [M3-2 设计](tasks/issue-509-m3-2-etf.md) §6。owner 2026-10-26 裁决：作为 `market.daily` 的证券类型、按序列记精度、提供不复权 + cat1/cat11 因子（D-28）；2026-11-03 批准 D-70 并追加 D-71/D-72。**PR-A/A2/B/C/C2/D 均已随 preview.45 部署到 vm2-126（2026-11-05）**：ETF 不复权日线并入 `research-snapshot.v4` 的 `daily_raw`，`universe_intervals` 给出 ETF 的 `last_trade_date`/`delisting_date`。ETF 默认 `universe` 仍是 A 股；新 scope 只对 `market.daily` 生效。**客户端经隧道连的是 aws-node（preview.43），看不到 ETF**（D-74） | #246、#509 | M3-2 | 部分满足（vm2 上身份/退市公告来源 + 不复权日线 + 日更；86 只无日线、无年线、无前复权，偏差见 §3.3 偏差 9/10） |
| 14 | 财务数据 | 不在范围 | 首期范围是价量 + 股本/市值，不含财务 | #1 | 不在首期 | — |
| 15 | 分钟线 | 不在范围 | 首期只做日线；`series_bars.trade_day` 是日序号，分钟线要改主键 | #1 | 不在首期 | — |
| 16 | 北交所 | 不在范围 | 暂不纳入；规则 S 已把 `f0=2` 显式排除（`scope.rs:10-24`） | #1、#246 | 不在首期 | — |

> M2 相关项（#4 股本、#5 市值、#6 上市日、#7 退市日、#9 历史 ST、#11 停牌、#12 涨跌停）对应的
> 6 个 dataset 和 M3 的写入器、读面、导出都已部署；未登记版本返回 `absent`/0 行。
> 来源库未配置时的两种形态（默认 `pending_source`、生产 strict 以 `source_store_unconfigured`
> 失败）适用于 `security_master`、`risk_warnings`、`suspensions`、`security_names` 四个同源 item。
> 各项的部署版本与线上读数在 [STATUS](STATUS.md)；仍没有读面的是涨跌停派生、可交易性和市值
> （审计 §3）。

**ETF 的取证结论**（2026-10-26）：

- 取证报告（2026-10-26 抓取沪深两所名单）实测：官方现行名单与 `hsjday` 的交集是 1,628 只，
  另有 127 只已不在现行名单；官方名单中还有 86 只不在 `hsjday`（只靠 `hsjday` 会静默缺约 5%）。
- 价格缩放：`raw/1000` = 元（3dp）；成交量单位是份，超过阈值切换为手；成交额单位是元。
- 现有前复权公式**不能直接用于 ETF**：价格 `/100` 差 10 倍、只取 cat1 会漏掉份额折算、
  2dp 舍入精度不够。
- 收录不构成容量否决（推断 +68~73MB、日更 +22~30s），但会同时撞上精度、目录/契约、复权三处约束。

### 3.3 首期无法达成的项（如实写入偏差）

1. **历史行业**：早于最早可用观测的行业归属仍**无法补齐**。M3 的代码（PR-6/PR-7，已部署）
   能把 owner 归档与后续每代 `tdxhy.cfg` 落成带生效区间和来源 sha256 的快照，但归档
   实测最早只到 2026-09-30（C 档 2026-09-16 只作交叉核对，除非 owner 另批），更早历史没有
   快照。→ **首期严谨级在这一项上无法完全达成。**
   剩余偏差：早于 `industry_coverage_start` 的日期只能返回 `unknown`，不能用当前行业补；
   `effective_from` 取发布方 `last_modified`（缺失取获取时间），真实变更日最多晚一个发布周期；
   回填区间没有同代 `incon.dat`，行业名称为 `unknown`，代码与区间仍可用。替代方案：证监会
   《上市公司行业分类结果》有历史发布记录，可作为「带生效日期的官方分类」候选，但必须先取证
   （发布入口、历史覆盖、离线保留边界、与 TDX 研究行业的映射）；两者口径不同（TDX 是研究
   行业二级，证监会是门类+大类），不能直接互换。
2. **SH 历史 ST 的完整性**：可达成的是「有公告的事件」；无公告的事件必须 fail-closed 为
   `unknown`，不能推断为 `normal`。2026-10-28 按市场分档准入（D-58）：沪市公告索引的状态
   事件（事件级）为 `APPROVED_SOURCE`，生效日按 D-84 R2 取「公告日当天或其后第一个交易日的下一个交易日」
   （显式标题日期优先），档位 `OWNER_RISK_ACCEPTED`；**北交所为 `NOT_AVAILABLE`**（公告索引归档起点 2000-01，见 §1.3）。
   **实际可观测起点是 2004Q1，不是 2000-01**：按 `TITLE` 过滤时 2000–2003 全部为空窗口
   （XDM-151 实测），这 16 个季度永久是 `not_observed`，不得写成「2000 年起覆盖」。
   → **首期严谨级在「沪市 2000 年前的历史 ST」与「2000–2003」这两项上无法完全达成。**
   剩余偏差：2004Q1 之前的沪市风险警示区间一律 `unknown`，回测中这些日期应按「未知」剔除或
   单独报告敏感性，不能当成正常交易；2004Q1 起的区间边界依赖 owner-risk 的 D-84 R2 规则
   （真实数据里标题没有显式日期，全部由公告日加交易日历推出），报告里要注明。
   写入器已部署，但**沪市**没有区间覆盖的日期仍是 `unknown`，从不写 `normal`（审计 §2.3）；深市闭世界代码已随 preview.46 部署、数据待第一次日更（§1.4.3）。
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
   导出里给出 `canonical_entity`/`aliases` 和 `code_migrations`（含 `certainty`），已部署；
   `strict` 只能在库函数里开，MCP 与导出都没有开关；candidate 迁移（108 条，含
   `sh.600849→sh.601607`）仍不作去重证据。
6. **停牌的 `confirmed` 分市场**：沪市 2008 年起用结构化停复牌事件表可实现 `confirmed`
   （D-49/D-60）；**深市首期没有 `confirmed`**（没有结构化停复牌表，公告索引的
   `BULLETIN_TYPE` 过滤不可用），只到 `unresolved_gap`。→ **深市缺 bar 的日子是 `unknown`
   （`data_gap`），不是 `suspended`；有 bar 的深市日 `research::tradability` 返回 `tradable`；沪市 2008 年前缺 bar 的日子同为 `unknown`。** 剩余偏差：
   `unresolved_gap` 只能说明「数据里没有 bar」，不能区分停牌、全天一字板未成交或数据缺失；
   回测要按「未知」剔除或单独报告敏感性。PR-11 沪市写入器已部署：
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
   `unknown`）。D-81 任务 49 起，深市 A 股在简称表覆盖范围内、无 ST、简称无 PT/「退」/`*` 时写 `status=normal`（见 [generic-data-v11](contracts/generic-data-v11.md) §5.5）；沪市与北交所仍全部 `unknown`。深市主板在「ST 与非 ST 幅度不同」的时期（如 1998–2023）下，无 ST 区间的日子从 `st_unknown` 变为 `normal` + 确定涨跌幅；创业板
   2020-08-24 起与科创板由 `status_class='all'` 的规则行给出确定值，且不把未知 ST 记为
   `normal`。`daily_raw` 不物化涨跌停列（它是解析
   事实），需要涨跌停的使用方必须自己带规则重算，并写明所用规则版本。
8. **交易所来源的 0 行不是证据，历史修订也不可重放**：上交所公告索引在日期区间超过一个季度时
   静默返回 `total=0`（§1.3），所以来源入口必须按 ≤1 季度分段并对每个窗口校验计数；0 行只能
   记 `not_observed`/`unverified_zero`，不能推出「没有 ST/停牌/变更」（M3 PR-2，已部署，
   AC-M3-10）。交易所名单与公告只保留冻结时的当前快照：名单修订、历史 ST 公告完整性、
   深市停牌 `confirmed` 都无法用后来的重抓重放；沪市 2000 年前与北交所 ST 为 `NOT_AVAILABLE`
   （偏差 2）。回测里这些日期按 `unknown` 处理。
9. **ETF 退市身份与退市日**：现行 1,717 只 ETF 与 128 只「`hsjday` 有、现行名单无」的退市 ETF
   由 D-71 公告索引准入（沪 82/深 46，2026-11-03 实测 128/128 命中）；`sh_sz_etf_current` 不含这
   128 只，`sh_sz_etf_listed_ever` 含。剩余偏差：**沪市公告索引最早 2006-08-04**，更早的沪市终止
   公告不可得；**深市「已退市且不在 `hsjday`」的成员未验证**（本批只能从 `hsjday` 发现候选）；
   没有 D-72 提取结果的成员 `delisting_date` 仍是 `unknown`（公告日不是生效日），有提取结果的
   128 只写精确终止上市日。PR-C 的 ETF 日线读面已给出 `last_trade_date`（最后一根 `volume>0`
   的 bar）与 `delisting_date`（主表值，未知时字段缺席），零成交 bar 标 `no_trade` 且原始 bar
   保留；两者语义不同，回测可交易窗口上界用 `last_trade_date`，证券存续上界才用
   `delisting_date`。这些已随 preview.45 部署到 vm2-126；研究快照导出（`research-snapshot.v4`）收录 ETF
   行（PR-D，见 [v10 契约](contracts/generic-data-v10.md) §1.6：`daily_raw` 加 `instrument_class`/
   `no_trade`，`universe_intervals` 给 ETF 同时写 `last_trade_date` 与 `delisting_date`）。
   **客户端经隧道连的是 aws-node（preview.43），看不到 ETF**（D-74），所以 ETF 回测要在 vm2 本机
   或经导出做；A 股读面和导出不变。沪市 82 只退市 ETF 的场内简称由 operator 离线脚本
   `scripts/extract_etf_short_names.py`（`etf-short-name-v2`，XDM-203）只从终止公告 PDF 的
   `场内简称`/`基金场内简称` 字段提取：55 只有名称，27 只的 PDF 只有「扩位简称/基金简称/证券简称」，
   按 D-76 Q4 读面返回 `name_basis: unavailable`；脚本不猜、不截断（长度上限 12 字按证据定），
   先取 `pdfinfo` 页数，不越过末页。

10. **ETF 数据面（M3-2 首期）**：

    - **ETF 成员区间起点是上市日，不是首次观察日（D-80，代码已部署；主表数据待第一次日更）**：preview.45 的主表里
      1,846 个 ETF 行有 1,845 行 `effective_from` 晚于 `listing_date`（1,718 行是 2026-11-05），
      所以导出的 `universe_intervals` 在 2026-11-05 之前基本没有 ETF 成员。D-80 起有 `listing_date`
      的 ETF 行 `effective_from` 取上市日，`known_at` 仍是首次观察日；没有 `listing_date` 的行保持首次
      观察日。A 股行不变。带 `known_at=T` 的查询在 T 早于首次观察日时仍排除 ETF 行并计数（这类
      查询对 ETF 仍不是「当时库里能看到什么」的完整重放）。7 只 `SSDATE` 晚于首根 bar 的 ETF
      处理不变：不裁剪 bar，也不拿 bar 覆盖上市日。preview.46 已含 #571；库内仍是 preview.45 的旧取值，第一次日更
      重写主表后生效。
    - **86 只现行 ETF 没有日线**：名单有、`hsjday` 无，全部在沪市 `sh526`（7）、`sh551`（12）、
      `sh589`（67）三个代码段；`hsjday` 在这三段各 0 个成员，同期 2026 年上市的 ETF 有 306 只，
      所以不是「新上市滞后」，而是源覆盖缺段（原因未验证）。主表照写、`market.daily` 读面按
      `absent`/`source_member_absent` 返回，不能当成「该 ETF 不存在」，也不能静默丢。
    - **ETF 没有行业归属**：`tdxhy.cfg` 0 行、`block*.dat` 0 成员，所有按 TDX 研究行业筛选/
      行业中性的读面都不覆盖 ETF；不得用跟踪指数或名称推断行业。
    - **首期只有不复权日线**：没有 ETF 年线，也没有 ETF 前复权序列。`market.daily` 对 ETF
      省略 `adjustment` 返回 `adjustment_required_for_etf`，显式 `qfq` 返回
      `adjustment_unavailable`，只有 `adjustment=raw` 可读 3dp 不复权价；不得把这两种错误
      当成「无数据」或静默返回 raw。
    - **ETF cat1/cat11 因子事实行可读**（`src/data_supply/collect/etf_update_tests.rs` 的
      `etf_cat1_and_cat11_factor_facts_are_readable_through_adjustment_events` 等，合成夹具，
      T14 / AC-M3-2-6）：`query_data` 读 `market.adjustment_events` 只接受 `contract_version=v2`
      与 `single`/`batch` 选择；`selection.kind=universe` 没有 ETF scope（ETF 的两个 scope 只对
      `market.daily` 生效）。有事件的 ETF 返回 `versions[].status=present`、
      `metadata.source=derived:adjustment_events`、`coverage_status=complete`；列为 `effective_day`、`category`
      （1 分红 / 11 份额折算）与 `c1..c4`，值是源 f32 的最短往返小数（`0.661`、`2`、`5`），
      不是 A 股的 2dp。没有因子事件的 ETF（以及没有序列的代码）是 `absent`、无行。
      读面带 `effective_day` 列（v11 起；v10 及以前的部署没有，只能靠 `filter` 日期区间区分）：行仍按
      源（gbbq 解码）顺序而非日期顺序，所以 2021 与 2010 两条 cat11 事件要看这一列；
      **读面不输出 `basis_status`**（cat1 `verified`、cat11 `pending`），它只存在于库里，读面上只能从
      `category` 推断。cat11 仍不进任何复权链。
    - **ETF 的 `market.capital_events`**：同一次提交写入，但只含股本类别 2/3/5/7/8/9/10；
      cat1/cat11 不在其中。ETF 只有 cat1/cat11 记录时没有登记，读回 `absent` 加
      「no registered version」限制说明，零行不等于「没有这类事实」。
    - **7 例 `SSDATE` 晚于 `.day` 首根 bar**（`sh511600/511800/511910/511920/511960/511970/511980`，
      早 10–18 天）：保留官方上市日，不裁剪首 bar，也不拿首 bar 覆盖上市日；回测按上市日截断
      时须知道这段差异。其余 ETF 的上市日与首 bar 相同或首 bar 更晚。

11. **北交所、B 股、LOF 本阶段不收**（owner 2026-11-05，D-79 的 O7）：三类都不进 `securities.catalog`、
    主表和日线读面，也没有排期；[#246](https://github.com/liqiangcc/x-data-mcp/issues/246) 待改写为这三类的延后记录。
    北交所（`f0=2`）和 B 股（`200xxx`/`900xxx`）已被规则 S 显式排除并计数（§2.1）。
    回测要用股票池时，报告须写明池中不含这三类。

## 4. 按用途看现在能做什么

### 4.1 选股

**可以做（附前提）**

- 用当前目录 + 当前行业做**当期**筛选（按代码、名称、行业码）：前提是结论只对「当期」成立，
  不能当作历史时点的在册集合（catalog 无生效区间，§2.1）。
- 用日线成交额/成交量做流动性排序：前提是只看近年样本，并剔除已知的「手日」bar（#381）。
- **时点股票池**（`selection.kind=universe` + `filter.as_of=T`，读 `market.security_master` 等 M2/M3
  数据集）：退市股行情已在库；成员有 `has_series=false` 时按「在册但无序列」理解，不能当作「不存在」
  （§3.2 #6–#8）。带 `known_at` 时 A 股主表行没有知晓时间，会被全部排除（审计 §2.2），所以
  只能用生效日口径。
- **市值筛选**：用 `market.capital_events` 的 `float_after`（或 `total_after`）× 不复权收盘；
  前提是 `total_after` 是公司总股本（含 B/H），A 股总市值对 A+H 公司是 `unknown`，
  结果必须标注口径（§3.3 偏差 4）。

**结果不可靠**

- **按股价绝对值筛选**（如「股价 < 5 元」）：只有前复权价，除权后价格与当时价格不同；且 2dp 取整
  在低价段误差大（§2.2 已知限制 1）。
- **判断涨跌停**：`price_limit_rules`（27 + 2 行规则）可读，但**没有派生读面**：`limit-v1` 只在库函数里，
  MCP 与导出都不给涨跌停价（审计 §3），使用方要自己用规则行、不复权前收和风险警示区间重算。
  A-7 已由 D-44 关闭（D-59）。ST 状态已按市场分档准入（D-58），写入器已部署（§3.2 #9）；D-81 任务 49 起
  深市写 `normal`（沪市不写），沪市主板在没有 ST 区间的日子仍是 `unknown`（ST 与非 ST 幅度不同），只有
  `status_class='all'` 的板块/时段（创业板 2020-08-24 起、科创板）能给出确定值。除权日的基准价是
  除权日基准价（limit-v2，D-84）：配股 `unknown`，2010-09-14 前用官方 r7_raw（有效时），其余用 gbbq 参考价公式（OWNER_RISK_ACCEPTED：证伪检验通过，越界 0.115%）。不能按 10% 自行推算。
**做不了**

- 剔除当时**不处于**风险警示的证券、或断言某日「非 ST」：沪市主板/北交所没有 `normal` 区间，只能说「有 ST 区间」或
  「未知」（§3.2 #9）。深市主板/创业板在简称表覆盖范围内且无 ST 时有 `normal`（D-81 任务 49），但简称表首行之前仍是 `unknown`。
  能做的是排除已知处于 ST 区间的证券，且要把沪市 2004Q1 前、北交所当未知。
- 确认 T 日停牌：沪市 2008 年起可由结构化事件表给出 `confirmed`（D-60，写入器已部署），
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
- **截面标准化与行业中性化**：T 日股票池有了，但时点行业只从 2026-09-30 起且名称未知（§3.3 偏差 1），
  此前的行业中性化做不了。
- **事件因子**：cat1 复权事件已是 `market.adjustment_events`，股本类事件是 `market.capital_events`
  （都没有知晓时间，只能按生效日）；ETF 的 cat1/cat11 同样可经 `market.adjustment_events`
  （v2、`single`/`batch`）读到，但 cat11 份额折算只有事实行、不进复权链，读面也不带
  `basis_status`（§3.2 #13）。读面的 `effective_day` 列自 v11 起提供（见
  [v11 契约](contracts/generic-data-v11.md) §6）。

**做不了**

- 精确的 A 股总市值因子：A+H 公司的 A 股总股本不可得（§3.3 偏差 4）；流通市值与公司总股本市值可以自己
  算，没有读面（审计 §3）。换手率因子：流通股本事件后值可用，口径自己标注。
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

- **确认没有幸存者偏差的回测**：数据层已有退市股行情和时点主表（§3.2 #6–#8）。默认研究导出此前只取
  目录成员、漏掉不在目录的退市序列（审计 §2.1）；代码已修复（#545，默认选择改为目录 ∪ 主表 A 股 ∪
  已发布序列，见 §5.3），代码已随 preview.46 部署；2026-11-07 真实库预检（XDM-198）读回 `coverage.delisted_series=337`，
  与 338 个退市序列的已知缺口一致。退市日未知与代码迁移仍有残余。
- **严格可交易性回测**：停牌（沪 2008 起）、ST（深市与沪 2004Q1 起）、涨跌停规则三类输入都有，
  但没有派生读面、没有 `normal`、深市无 `confirmed` 停牌、除权日涨跌停基准不准。
- **逐位可复现的回测**：`export_research` 自 preview.36 起随发布包部署（preview.46 起已部署 `research-snapshot.v5`），
  但它是**离线命令**、不在 MCP 上（D-39）：要复现必须自己保留导出目录与 `manifest.json`；清单不记录
  二进制版本，派生（涨跌停、市值）读时重算；直读路径（§5.2）仍不绑定版本（审计 §2.4）。
  preview.46 已部署 `research-snapshot.v5` 的 `producer` 块；2026-11-07 预检（XDM-198）读回 `producer.git_sha` 与算法版本。
- **参数/口径敏感性**：不复权与前复权两条序列都在库（preview.36 起）；`query_data` 缺省读前复权，
  A 股显式 `adjustment=raw` 读不复权（v10），导出两者都有（§5.3）。

## 5. 现在的访问方式和限制

### 5.1 MCP 读取路径与上限

- `query_data`：批量选择 1..100 个实体（`src/data_supply/query.rs:85-86`）；`page_size` 1..1000
  （`query.rs:71-75`）；单次快照上限 100,000 行 / 16MiB（`query.rs:10-11`；v2 `universe` 价格读按列式答案计字节，见 [generic-data-v8](contracts/generic-data-v8.md) §4.1）；游标分页；服务端
  快照缓存 32 条 / 64MiB（`query.rs:12-13`）。
- 过滤条件：`market.daily`/`market.yearly` 只有日期/年份区间；`securities.catalog` 只有
  `code`/`name_query`/`industry_code`（`wire.rs:79-92`）。**没有**价格、成交量、市值、ST 等谓词。
- 版本选择：`latest_per_entity` / 显式版本映射 / 目录版本（`wire.rs:97-104`）。
- 目录读 scope 只有 `current_sh_sz_a`；`CatalogScope` 另有 `sh_sz_a_listed_ever`、`sh_sz_etf_current`、
  `sh_sz_etf_listed_ever`，可用于 `market.daily` 的 `start_collection` v2 与 `selection.kind=universe`
  的 `scope`（#446 PR-9、M3-2 PR-A），目录数据集仍一律拒绝（`wire.rs`、`src/data_supply/query.rs:269-300`）。
  `universe.scope` 只对 `market.daily` 生效，与 `catalog_version` 互斥，且不传时行为不变（A 股）。
  `selection.kind=universe` 的全市场截面分页按「实体 × 交易日」行切（`page_size` ≤1,000），
  每页都带完整实体表和版本清单，不是一次调用（审计 §4.7 AC-V8-5，推断）。
- `query_data` 是纯读：不触发采集、修复或换源（`src/data_supply/registry.rs:139`）。
- 工具清单、参数与返回形状用 `get_data_capabilities` 查（`registry.rs` 的 `entries()`），本文不复述；
  data 侧当前部署三个入口（单进程三 socket）：`ops` 16 个 data 工具、`research`（只读研究面）
  与 `maint`（本机运维）各 5 / 9，数量以 [STATUS](STATUS.md) 的「服务」表为权威。

### 5.2 过渡期直读（#373 分析脚本）

`scripts/analysis/boll.py` 是过渡期的直读路径：`mode=ro` 打开 sqlite，按 `series_key` 主键范围
读 `series_bars`，不经过 MCP（`scripts/analysis/boll.py:1-29`）。使用时注意：

- **不绑定版本**：脚本只按 `series_key` 读当前行，不记录 `version_id`。两次运行之间发生日更或
  除权 `replace`，结果会变，且输出无法说明用的是哪一版数据。
- `trade_day` 是 1970-01-01 起的天数（`src/compact_market.rs:798-801`），不是 `YYYYMMDD`。
- 脚本只读、无写，不是服务/MCP/契约的一部分，也没有稳定性承诺。
- 需要可复现结论时，必须自己记录 `market_series.version_id` 与目录版本（§5.3 的清单形状）。

### 5.3 研究快照导出（preview.36 起部署；preview.46 起已部署 `research-snapshot.v5`，含 M3/ETF 行与 `producer`；preview.45 及以前为 `v4`；`#607` PR-E 的 ETF 前复权行已实现、未部署）

- **随发布包部署**：`export_research` bin（feature `export`，`Cargo.toml:6-17`；默认构建不编译
  它；`scripts/build_release.sh` 自 #413 固定 `--features export` 并在打包后断言该命令存在），
  产物是 Parquet 文件 + `manifest.json`（`src/research/export.rs:266`），默认落
  `/root/x-data-exports/`（`src/research/mod.rs:894`；D-29）。
- **MCP 仍没有 `export_data`**：v8 只设计、不实现（D-39）；工具数与 `capability_revision` 不因此变化。
- **只读**：连接用 `SQLITE_OPEN_READ_ONLY` + `PRAGMA query_only=1`，并要求库的 `user_version` 等于
  代码的 `COMPACT_SCHEMA_VERSION`（当前代码 27，即 D-08 的全新 init；`src/research/read.rs:69-78`）。
- **导出范围**（`research-snapshot.v3`，代码已实现并随 M3 部署）：M1 的 5 个文件
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
- **`v4`（XDM-157，随 preview.42 部署）**：`derived[]` 增加可选 `verified_through`（该数据集
  输入来源的最旧完整刷新确认日，来源是 `source_refresh_run_sources` 的 run 记录，不是页面
  `as_of`；没有完整 run 时整键省略，不写 null），清单的身份补齐两个整表数据集
  （`security_names`/`risk_warnings`）的 `version_id`/`algorithm_version`/`source_generation`。
  `derived[]` 是身份的一部分，所以同一输入在 v3 与 v4 下 `snapshot_id` 不同；文件与列不变，
  `scripts/export_smoke.py` 同时接受 v2/v3/v4，`v4` 另校验 `verified_through` 若是字符串则为
  `YYYY-MM-DD`。
- **`v4` 的 ETF 行（M3-2 PR-D，随 preview.45 部署）**：当时按「v4 尚未部署」的判断把 ETF 列并入 v4、
  没有升 v5，但 v4 已随 preview.42 部署，所以**同一个 schema 字符串对应两种列布局**：preview.42/43
  的日线文件 11 列，preview.44/45 的 13 列（审计 §2.4）。读取方应以 `manifest.files[].columns` 为准；
  代码已改为 `v5`（见下一条），`v5` 即这里的 13 列布局。`daily_raw`/`daily_qfq` 在 v3 的 10 列之后追加 `instrument_class`（0 股票 / 2 ETF）与
  `no_trade`（股票 NULL；ETF 为 `volume=0 ∧ amount=0` 推导值），`daily_raw` 并入所有已发布的 ETF 不复权
  序列（3dp，A 股行在前且值不变），`daily_qfq`/`factors` 不含 ETF；`universe_intervals` 追加
  `instrument_class`、`last_trade_date`、`delisting_date`，ETF 区间在 A 股区间之后。可交易窗口上界用
  `last_trade_date`，存续上界用 `delisting_date`，二者未知为 NULL、不互相推断。清单 `series[]` 的 ETF
  条目带同样两个日期，`limitations[]` 增加三条 ETF 说明。契约见
  [v10 §1.6](contracts/generic-data-v10.md)。
- **默认选择包含退市股（幸存者偏差修复，Refs #446 #383）**：不传 `--entities`/`--catalog-version`
  时，个股成员集为「目录头 ∪ 主表沪深 A 股 ∪ 已发布个股日线序列」（`src/research/read.rs` 的
  `default_members`），所以不在当前目录的退市股进入 `daily_raw`/`daily_qfq`/`factors`，
  `coverage.delisted_series` 按导出序列里主表有退市日的实体计数。此前的实现只取目录成员，会漏掉
  listed-ever 首采的 338 个退市序列（vm2 preview.45 dry-run：`delisted_series=0`，series 6,984
  条，库里 A 股实体 5,562 个；preview.46 预检为 337，XDM-198）。`catalog.member_count` 仍只数目录成员，清单 schema 不变；
  显式 `--entities` 行为不变；显式 `--catalog-version` 仍**只取那一版目录的成员**，是有意的
  「固定目录快照」请求，天然带幸存者偏差，清单里以 `catalog_version` 标明。输入序列列表变了，
  所以同一个库的 `snapshot_id` 与修复前不同；现行成员的序列条目与行内容不变，ETF 不变。
  整表文件（`security_master`、`security_names`、`capital_events`、`risk_warnings`、
  `suspensions`、`universe_intervals`、`industry_assignments`）本来就不按目录过滤，修复前后行集一致。
- **`v5` 与 `producer`（审计 T7，Refs #383；preview.46 已部署，XDM-198 预检读回 `v5` 与 `producer`）**：
  - **两种 `v4` 布局的真实情况**：`research-snapshot.v4` 的字符串在 tag `v0.2.0-preview.42`/`43` 与
    `v0.2.0-preview.44`/`45` 上写出不同的列。preview.42/43：`daily_raw`/`daily_qfq` 11 列、
    `universe_intervals` 7 列；preview.44/45（M3-2 PR-D）：13 列、10 列（追加 `instrument_class`、
    `no_trade`，以及 `instrument_class`、`last_trade_date`、`delisting_date`）。其余文件的列相同
    （由 tag 的 `src/research/mod.rs` 核对）。
  - **选了升 `v5`，没有加 `column_layout`**：`v5` 的列就是 preview.44/45 的 `v4` 列，一列不动；
    字符串 `v5` 即「ETF 布局」。理由：(1) 旧目录没有 `column_layout`，新字段无法判定已有的 `v4` 目录，
    判定它们仍要看 `files[].columns`；`v5` 至少让今后写出的目录不再歧义。(2) 仓库内只有两处读这个字符串，
    `scripts/export_smoke.py`（按字符串取列契约）与 `scripts/smoke_release.py`（白名单），都是加一行；
    没有别的消费方，`snapshot_id` 本来就含 schema 字符串，升版不会让新旧目录互相覆盖。
    (3) 契约不需要再多一个要维护的枚举。
  - **读取方怎么判定**：`v5` 一律是 ETF 布局；`v2`/`v3` 一律不是；`v4` 是两种之一，看
    `files[].columns` 是否含 `instrument_class`（`scripts/export_smoke.py` 的 `etf_layout`，三个文件
    的结论不一致时报错）。外部读取方也应当这样做，不要凭 `v4` 假定列数。`v4` 目录不会被重写，
    历史上的 `v4` 歧义不会消失，只是不再新增。
  - **`producer` 块**（`v5` 新增，顶层，与 `algorithms` 并列）：`git_sha` 是写出该清单的二进制的
    `build.rs` 提交 SHA（无 git 检出时为 `"unknown"`）；`algorithms` 是该二进制携带的派生算法标签：
    `price_limit`（读时派生标签 `limit-v2`，D-84；已登记规则数据集 `price_limit_rules` 的 `algorithm_version` 仍是 `limit-v1`）、`market_cap`（`market-cap-v1`）、`tradability`（`tradability-v1`）、`security_master`、`security_names`、
    `risk_warnings`、`suspensions`，取自代码常量。`market_cap` 不是导出文件：导出不含市值，
    它在读时由 `capital_events` 与不复权收盘价重算，标签是快照唯一能说明「用哪个公式」的地方。
  - **`snapshot_id` 不含 `producer`**（审计建议，设计没有相反规定）：§7.3 的身份是
    `{schema, request, catalog, generations, algorithms, series, derived}`，没有二进制版本，
    本次未改。理由：内容相同的快照不应因二进制不同而被拆开；已写入的派生数据集的算法版本
    本来就在 `derived[].algorithm_version`（进身份）。代价：身份相同不保证读时重算的派生
    （涨跌停、市值）相同，那一部分只能看 `producer`。
  - **可复现的边界**：同一二进制导出同一库两次，包含 `manifest.json`（含 `producer`）在内的全部
    文件逐字节相同（AC-M1-7，测试逐字节比较）。**不同二进制**：`snapshot_id` 相同，`producer.git_sha`
    不同，所以 `manifest.json` 字节不同；Parquet 文件字节是否相同没有承诺（写入库版本、编码或
    `created_by` 变化都可能改字节），以 `files[].sha256` 为准。
  - 不做：没有给旧 `v4` 目录补 `producer`；没有改 `snapshot_id` 定义；没有把 `git_sha` 放进
    `algorithms`（`algorithms` 在身份里）。
- **ETF 前复权进入导出（`#607` PR-E，Refs #607；D-89 第 6 条、D-92 Q8 (a)；代码已实现，未部署）**：
  - **`daily_qfq` 收录 ETF 行**：只收**门控放行**的 ETF，行就是库里那条 `Forward` 序列的行
    （`price_scale=6`、`instrument_class=2`），量额与 `daily_raw` 逐字节相同（复权只改四个价格字段）。
    所以 `daily_qfq` 的 ETF 行与 MCP `market.daily` 的 `qfq` 读回同一份字节；A 股行不变，ETF 块是
    文件尾部的一段后缀。**不升日线文件的列版本**（列集合没变，只是多了 `instrument_class=2` +
    `price_scale=6` 这个既有组合，设计 §7.2 (a)）。
  - **被阻断的 ETF 不是「悄悄少一行」**：它保留 `daily_raw` 行、`daily_qfq` 里没有它，清单
    `etf_forward.blocked[]` 逐只给出 `entity` 与 `reason`——`reason` 就是读面
    `details.reason` 的同一个码（`etf_factor_unverified`、`etf_qfq_not_built` 等），另带
    `blocking_events`/`categories`。`etf_forward.admitted[]` 是放行名单，
    `etf_forward.checks_digest` 是判定用的 `etf_checks_digest`（来源库核对记录集合的内容摘要）。
    这三个键进 `snapshot_id` 身份：同一库、换一份核对记录集，是另一份快照。
  - **门控与读面同一个对象**：导出用
    `crate::source_store::etf_factor_read_gate::ForwardGate::from_env()`，与 `data_service` 读
    `market.daily` 时注入的是同一个 `EtfForwardGate`，导出**不自己实现**准入规则（设计 §7.1）。
  - **环境变量 `X_DATA_MCP_SOURCE_STORE`**：命令行导出是独立进程，必须自己解析来源库——用的就是
    读面那个变量（`src/source_store/mod.rs` 的 `CONFIG_ENV`）。**vm2 生产导出必须设置它**，
    例如 `X_DATA_MCP_SOURCE_STORE=/srv/x-data-mcp/source-store export_research --db state/preview.sqlite`；
    `--help` 里也写了这一句。**不设或读不到不是导出失败**：门控按实体 fail-closed，每只 ETF 都不进
    `daily_qfq`、都出现在 `etf_forward.blocked[]` 里并给出原因，其余文件照常写出。这正是「宁可少
    一行也不要悄悄给一条没核对的复权序列」的落地方式。
  - **`factors.parquet` 加 `basis_status`**：ETF 事件的取值来自**来源库核对记录的 `check_status`**
    （`verified_official`/`verified_announcement`/`no_effect`），没有覆盖该
    `(entity, ordinal, evidence_digest)` 的记录就是 `unverified`；A 股事件是固定值
    `gbbq-verified`（M1 口径，逐事件没有核对记录，编一个就是谎报验证）。
    **不用** `adjustment_events.basis_status`（那是 schema 27 遗留列，D-92 Q2 (c)）。
    这一列让 `factors.parquet` 自己升版：清单 `files[].schema_version` 记 `factors=2`，日线文件是
    `null`（不升版，设计 §7.2/§7.3「两者独立版本化」）。快照 schema 字符串仍是 `v5`——日线列没变，
    不该让所有消费方跟着升。
  - **`--adjustment raw` 不变**：不出 ETF 前复权行、不写 `daily_qfq` 文件，日线字节与来源库无关。
    `factors.parquet` 例外：`basis_status` 是事件自身的属性，与写了哪些日线文件无关，所以 raw 导出
    的 `factors` 仍依赖核对记录集（清单 `etf_forward.checks_digest` 会反映这一点，`snapshot_id` 随之不同）。
  - **`scripts/export_smoke.py` 的新断言**（`--snapshot-dir`）：`daily_qfq` 的 ETF 行必须
    `price_scale=6`；每个出现在 `daily_qfq` 的 ETF 在 `manifest.series[]` 里有 `qfq` 条目（即
    `market_series` 里有 `Forward` 行）且在 `etf_forward.admitted[]` 里；`daily_raw` 里的每只 ETF
    要么放行要么阻断（既不在放行也不在阻断就是「悄悄漏掉」，报错）；`factors.basis_status` 的取值
    必须在允许集合内，ETF 行不得冒用 A 股固定值。这些规则是纯函数
    （`etf_admission_problems`），`--self-check` 用构造的清单逐条证明它会拒绝坏形状，CI 先跑
    `--self-check` 再跑真快照。
- **D-94：减法式前复权不能直接算收益率（Refs #621）**：本快照的 `daily_qfq` 是**减法式**复权
  （D-89 §1）：事件把价格减去一个偏移量（现金分红、份额折算折算成价格差），**保持价格差、不保持
  价格比**。所以：
  - 早期价格可能 **≤ 0**（0 或负数）：分红累积超过当时的股价时就会这样。已知受影响的是
    5,562 只 A 股中的 747 只，以及 ETF `sh.510050`、`sh.510880`（#621 记录的范围）。
  - 因此 `qfq[t]/qfq[t-1]` 在任一端 ≤ 0 时**没有意义**（符号翻转、除零、量级乱跳）。
  - **算收益率请用 `daily_raw` + `factors`**：原始价配合事件因子做比值运算，A 股与 ETF 一样。
  - `daily_qfq` 是「差值可加」的价位序列，适合做价差、价差回归、需要同一价位基准的图；不适合
    当收益率序列。
  - 自本期起 `daily_qfq` 含 class 2（ETF）行；这条限制对 A 股与 ETF **同样适用**，不是 ETF 专属。
    清单 `limitations[]` 里也有这条（消费方读清单即可看到，不必读文档）。
- **旧快照可读**：schema 升版不改读取方约定——`scripts/export_smoke.py` 同时接受 v2/v3/v4/v5，v2 目录
  不要求 M3 文件、`universe_intervals` 按 5 列校验；`snapshot_id` 定义不变，但 schema 字符串在
  身份里，所以同一输入的 v2/v3/v4/v5 `snapshot_id` 不同、不会互相覆盖。
- **保留与跨机校验（D-83，备忘录 T8，Refs #383 #549）**：单版本存储下，导出目录是复现回测输入的唯一记录
  （除权会改写整段前复权历史，旧 `version_id` 之后读不到；重新导出得到的是库当前状态，`snapshot_id` 不同）。
  规则如下，全部是人的规则，代码只提供只读列表：
  - **不自动删除。** 没有定时清理、没有按龄或按数量淘汰，`export_research` 与发布脚本不删导出目录。
  - **只有 owner 能删。** 删除是 owner 在机器上手动做的事；仓库不提供删除命令。
  - **配额暂不设。** 体积或数量上限由 owner 日后另行裁决；在此之前磁盘紧张时找 owner，不要自行挑最旧的删。
  - **不可替代。** 清单里任一来源 generation（`generations[]`）在 acquire 库里的状态是 `evidence_only`
    （字节已退役，无法从官方文件重新派生）的导出，标为不可替代：它是那次回测输入的唯一副本。
    generation 还是 `active`/`superseded` 的导出只是「暂时可替代」，因为 generation 保留最近两代
    （D-65），退役后同一目录会变成不可替代；即使 generation 还在，重新导出也得不到同一个 `snapshot_id`，
    除非库一字未变。退役 generation 时不拦截，列表工具是看这一点的地方。
  - **`export_research --list-snapshots`**（只读，不提供删除；`--out` 默认 `/root/x-data-exports/`）：
    对导出根目录下每个快照给出大小、创建时间（文件系统的出生时间，取不到时用修改时间，字段
    `created_at_source` 说明；拷贝到别的机器后这个时间会变，不属于快照身份）、`schema`、每个来源 generation 的当前状态（`active`/`superseded`/
    `evidence_only`，读 acquire 库的 `generations` 表；`--acquire-db` 指定，不给则取 `--db` 同级
    `generations/acquire.sqlite`，部署布局里 `state/preview.sqlite` 与 `state/generations/` 正是这个关系）
    和 `irreplaceable`。标准输出是 JSON，标准错误是文字摘要（`--quiet` 去掉文字）。
    **如实标出问题而不是报干净**：没有 `manifest.json`、不是合法 JSON、目录名不等于 `snapshot_id`、
    清单列出的文件缺失或大小不符、目录里有清单没列的文件，都是 `status: "damaged"` 并写原因；
    acquire 库读不到、generation 不在库里、或库里同 id 的 manifest sha256 不同，状态是 `unknown`，
    `irreplaceable` 为 `null`（不是 `false`）；根目录里的散文件、符号链接、崩溃遗留的 `.tmp-*`
    列在 `unexpected_entries`。有任一项需要关注时退出码为 2，全部干净为 0。
    列表只核对存在与大小，不读文件内容算哈希（每个快照数百 MB 到 GB）；字节核对用下一条。
  - **跨机校验**：`scripts/export_smoke.py --snapshot-dir <目录>` 先核对字节，再读 Parquet。字节核对包括：
    清单每个文件存在、大小等于 `files[].bytes`、sha256 等于 `files[].sha256`；用清单去掉
    `snapshot_id`/`producer`/`files`/`limitations`/`export` 后的身份部分按 RFC 8785 重算 `snapshot_id`
    （v2 到 v5 同一规则）并与清单、目录名比较；目录里没有清单没列的文件。任何一个字节不同都报错并非零退出。
    `manifest.json` 本身不在 `files[].sha256` 里，脚本会打印它的 sha256，两台机器比较这个值，或用
    `--manifest-sha256 <值>` 让脚本比较（`producer`/`limitations`/`export` 只有它能覆盖）。
    这个校验不需要库、不需要网络、不需要 `export_research` 二进制，只要导出目录本身；
    它比的是导出文件字节，不含回测引擎和参数。
- **实测**：preview.36 部署后全量 daily 导出 119s、900MB（`daily_raw` 16,737,243 行，见
  [STATUS](STATUS.md)）；含 M2 文件的 v2 导出耗时没有回填。v3/v4 新增文件的体积与耗时没有在部署实例上
  测量（STATUS 注明本轮未重跑导出）；夹具层面 v3 不改变既有文件的编码。**默认导出的退市序列：代码已修复
  （上一条，#545）；2026-11-07 预检已读回 `coverage.delisted_series=337`（XDM-198）**（审计 §2.1）。本机校验用
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
