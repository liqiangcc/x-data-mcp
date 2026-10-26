# M2 详细设计：按时点的证券主表与可交易性（#391）

- 任务：设计任务（#391 的前置设计），角色 agent。**只出设计**：不改代码、不改 schema、不改 MCP 契约、不推进 #308、不部署。
- 关联 Issue：[#391](https://github.com/liqiangcc/x-data-mcp/issues/391)（本设计对应）、#1（产品范围）、#54（cat1 冻结与 Final Gate 标准）、#55（来源原则）、#246（收录扩展）、#308（历史 ST 来源准入）、#383（M1 设计）、#387（v8 设计）。
- 输入报告（仓库外，2026-10-26；正文按章节引用）：调研报告①「研究数据层需求与缺口」（§4.4–4.10、§7 的 M2 部分）、调研报告②「gbbq 股本类事件语义取证」、调研报告③「全仓库只读分析」。
- 本任务的小批量抽样核对（仓库外，随任务交付）：`work/evidence-notes.md` 与 `work/evidence_manifest.json`（每份证据的 URL、获取时间、sha256）。抽样**不是**来源准入证据包。
- 依据：`docs/decisions.md`（D-20 来源原则、D-22 非 cat1 不入生产、D-23 上市日、D-24 退市整理期、D-25 四类官方披露准入、D-30 严谨回测级与两项首期偏差）、[docs/design.md](../design.md)「后续数据扩展要求」、M1 设计 [issue-383](issue-383-m1-research-data.md)（schema 22、解析事实层、导出）、v8 设计 [issue-387](issue-387-v8-ai-tool-surface.md) §4.6（`as_of`/`known_at` 预留）、[docs/data-catalog.md](../data-catalog.md) §3。
- 基线：main `6d01163`（含 M1 PR-1 的 schema 22 结构）；生产库 schema 21，2026-10-26 只读复核（§2.1）。
- 本设计经 Claude 审核后，才按 §16 拆实现任务与来源准入任务；审核前不写实现代码。

---

## 0. 结论摘要

| # | 设计点 | 结论 | 证据级 |
|---|---|---|---|
| 1 | 证券主表 | 按**生效区间**记录：行身份 `(exchange, code, effective_from)`，`effective_to` 开区间；同一代码可有多个不重叠区间（代码复用、代码迁移），重叠即 fail-closed | 设计（反例实测：`sh.600018` SSDATE 2006-10-26、首根 bar 2000-07-19；`sz.300114`→`sz.302132` 同一段 bar 挂在两个代码上） |
| 2 | 每个属性两个时间 | 上市日、退市日/整理期、简称、股本、风险警示、停牌、规则全部带 `effective_from/to` 与 `known_at`；`known_at` 缺失即 `NULL`，不得用文件 mtime 或事件日冒充 | 已确认原则（分离点 3；#308 AC-T01；M1 BR-M1-9） |
| 3 | 上市日 | 交易所名单 > `base.dbf SSDATE` > `unknown`；实测 2,7xx 个两者都有的代码**零冲突**（过滤 `STOCK_TYPE=1` 后），62 个 `SSDATE='0'` 的退市代码由交易所名单补上 61 个 | 实测（§2.3 E-1/E-2/E-13） |
| 4 | 退市日 | 交易所终止上市名单 > TDX `infoharbor_spec.cfg` 整理期 end > `unknown`；335 个退市代码里 **333 个**被交易所名单覆盖（含科创板 3 家），**2 个覆盖不到的是代码迁移与吸收合并** | 实测（E-3/E-4/E-8/E-14） |
| 5 | 股本历史 | 只采用 cat 2/3/9/10 的**后值 + 事件日**（18,086 条）；前值只作审计；cat5/7/8 不入库。流通市值 = 不复权收盘 × 流通 A 股 | 已裁决（D-22）+ 实测（E-9） |
| 6 | 市值口径 | `total_after` 是公司总股本（A+B+H），**不得**称为 A 股总市值；A 股总股本只有当前快照（SSE 逐股接口 / SZSE 1110 列），历史不可得 → A+H 的 A 股总市值在来源准入前返回 `unknown` | 实测（E-1/E-5；000002 差 22.07 亿 = H 股） |
| 7 | 历史 ST | 表、状态机、fail-closed 语义在 M2 设计并实现（用夹具测）；**生产数据入库以 #308 Final Gate 变为 `APPROVED_SOURCE` 为前提**，M2 不推进也不绕过 | #308 现状（`OWNER_RISK_ACCEPTED`） |
| 8 | 停牌 | 「交易日无 bar」只能产生 `unresolved_gap`，不能产生 `suspended`；`suspended` 需要公告或 GP29 证据 + `known_at` | 实测（E-11：`sz000631` 2006-04-27→2009-06-05 无 bar，0.76→10.30 元） |
| 9 | 涨跌停 | 规则按生效区间存表（板块 × ST 档 × 上市阶段 × 最小变动 × 取整），每行带 `basis_ref`；无出处或 ST 未知 → `unknown`，不按 10% 兜底；派生不入库 | 设计 + 实测（E-12：HALF_UP 10% 命中 1.76%，floor 0.95%） |
| 10 | T 日股票池 | `universe(as_of=T)` = 上市日 ≤ T < 退市日 ∧ 区间命中 ∧ 未被代码迁移取代；**股票池 ≠ 可交易**，停牌股仍在池内；目录版本只是主表在「今天」的投影 | 设计 |
| 11 | 存储 | **schema 23**（追加 `023_security_master.sql`，不改已合并的 022）：主表、简称、股本事件、风险警示区间、停牌区间、涨跌停规则、最小变动规则；非 bar 数据集登记复用 M1 的 `derived_dataset_versions` | 设计（依赖 M1 PR-4） |
| 12 | 契约 | M2 本身**不改 MCP 契约**；新数据集与 `as_of`/`known_at` 的接受由 v8 的 dataset 分支 + capabilities 的逐数据集 `temporal` 声明启用 | 设计（v8 §4.6 的接法） |
| 13 | 可行性 | 主表/股本/股票池/停牌 `unresolved_gap`/规则表可达；**SH 历史 ST 完整性、历史 A 股总股本、规则原文**三项不可达或需准入；偏差逐条列在 §13 | 实测 + 设计 |

**三个最容易被做错的点**（本设计专门守住）：

1. **「在册」不等于「可交易」**。股票池回答「当时市场上有这只证券」，可交易性回答「当天能不能买」。把停牌股从池子里删掉，等于用可交易性污染了成员集，回测的样本选择偏差会从「幸存者偏差」变成「停牌偏差」。
2. **「缺 bar」不是证据**。它是「我们不知道」的同义词。数据层能给的只有 `unresolved_gap`；给 `suspended` 必须有公告或 GP29 事件，并且要能说清 `known_at`。
3. **涨跌停的未知要往上冒**。主板 ST 是 5%、非 ST 是 10%，ST 未知时算出任何一个数都是猜。默认返回 `unknown`（显式请求诊断时给候选集），绝不用 10% 兜底。

---

## 1. 范围

### 1.1 本次包含

1. 证券主表：身份与生效区间、上市日、退市日/退市整理期、代码复用与代码迁移。
2. 证券简称历史（区间 + `known_at`），来源分 SZ 官方 dated 表 / GP29 事件 / 交易所名单快照。
3. 股本历史事件（cat 2/3/9/10 的后值与事件日）、流通市值/换手率口径。
4. 风险警示区间（表 + 状态机 + fail-closed），**不含未准入的生产数据**。
5. 停牌区间（`confirmed` / `unresolved_gap`）与可交易性判定。
6. 交易规则表（涨跌停幅度、ST 档、上市首日/前 N 日、最小价格变动单位、取整）与涨跌停派生。
7. T 日股票池 `universe(as_of=T)` 的定义、特殊情况与接口。
8. 新来源准入计划（每个来源的发布方/获取方式/格式/覆盖/频率/稳定性/证据要求/阻塞关系）。
9. schema 23、派生与算法版本、日更维护、导出新文件/新列、v8 的 `as_of`/`known_at` 启用方式、契约影响。
10. 分离点落位、6 个扩展场景走查、`docs/design.md` 待改处、实现拆分、本机/CI 验证方式。

### 1.2 本次不包含

实现代码；退市股行情入库（M3）；ETF/北交所/B 股；历史行业；财务/公告全文；分钟线；回测引擎与成交判定；下游 `x` 仓库改动；部署与发布；**推进或绕过 #308**；来源准入的实际下载与证据包（只做小批量抽样核对）。

### 1.3 与既有裁决的关系

| 裁决 | 本设计如何吸收 |
|---|---|
| D-20（一手官方来源 + 逐个过 #308 门禁） | §10 每个来源单列证据要求与阻塞关系；未准入来源的数据不进生产表（`pending_source`） |
| D-22（gbbq 非 cat1 暂不作为生产语义） | §5 只采用 cat 2/3/9/10（R-3 准入范围），cat5/7/8 不入库；类别只作诊断字段 |
| D-23（上市日取 `SSDATE`，缺失由交易所名单补） | §4.2 来源优先级 + 冲突策略；补充实测：`base.dbf` 字段在 #6 契约里仍是 UNANCHORED，需字段锚定证据包（§10 S-7） |
| D-24（退市整理期取 TDX 表，要过门禁） | §4.3 整理期与摘牌日分开；TDX 表只覆盖 2013 年起，交易所名单补更早与吸收合并类 |
| D-25（四类官方披露进入准入评估） | §10 把「上市/退市名单、风险警示公告、停复牌公告」落成 S-1…S-4；证监会行业分类不在 M2 |
| D-30（首期严谨级；两项偏差） | §13 偏差清单：SH 历史 ST、历史行业（M3）、历史 A 股总股本、规则原文 |
| M1 的 schema 22 与解析事实层 | §9 追加 023；派生输入全部来自 `adjustment=0` 日线、因子事件、日历 |
| v8 的 `as_of`/`known_at` 预留 | §12 给出语义、接受条件与逐数据集 `temporal` 声明；M2 不改 v8 已冻结的拒绝行为 |

---

## 2. 现状事实（2026-10-26 复核）

### 2.1 只读实测（生产库 + 官方包）

| 项 | 实测值 | 核对方式 |
|---|---|---|
| 生产库 schema | 21（代码侧 `COMPACT_SCHEMA_VERSION=22`） | `pragma user_version`、`src/sqlite_tasks.rs:40` |
| `catalog_members` | 5,228；`catalog_head` = `catalog-1791873196258239513` | `select count(*)`、`catalog_head` |
| `market_series` 日线 | 5,224 条，16,749,060 根 bar，`first_day=7657`（1990-12-19）、`last_day=20749`（2026-10-23） | `group by period, adjustment` |
| `market_series` 年线 | 5,224 条，74,557 根 bar | 同上 |
| `hsjday.zip` 成员 | 12,222；A 股代码 5,561；**不在当前目录 335**；目录成员里没有 `.day` 的 1 个（`sz.001381`）；两者并集 5,562 | 成员名解析 + 与 `catalog_members` 求差 |
| `base.dbf` | 8,933 行 / 40 字段；A 股行 5,577；`SSDATE` 缺失（`'0'`）74 行；**335 个退市代码全部在表内**，其中 62 个 `SSDATE` 缺失 | DBF 头 + 逐记录读 |
| `infoharbor_spec.cfg` | 5,030 B / 169 行，格式 `market\|code\|0\|start\|end`，首行 2013-02-08；`600001`/`600005` 不在表内，`000004` 在（end=20260714） | `base.zip` 解出副本 + sha256 `83b44fa0…` |
| `gbbq` 类别 | 207,282 条；cat1 69,283 / cat2 4,866 / cat3 4,929 / cat5 117,183 / cat7 19 / cat8 684 / cat9 7,987 / cat10 304；京市 cat2/3/5/7/8/9/10 共 5,939 | 重算调研报告② 的 `gbbq_all.tsv` |

### 2.2 代码事实（`file:line`）

| 事实 | 位置 |
|---|---|
| `Dataset` 只有 3 个取值（catalog/daily/yearly） | `src/data_supply/wire.rs:10-17` |
| `Filter` 只有 `Daily`/`Yearly`/`Catalog` 三个变体，**没有时点参数** | `src/data_supply/wire.rs:79-93` |
| `CatalogScope` 只有 `current_sh_sz_a` | `src/data_supply/wire.rs:72-76` |
| 读路径只校验实体格式与访问权，**不校验目录成员** | `src/data_supply/query.rs:79-99` |
| schema 22 只落了 `evidence_bundles`/`market_series`/`series_bars`/`yearly_series_lineage`/`market_events` | `migrations/tasks/022_research_data.sql:16-189` |
| `derived_dataset_versions`、`adjustment_events`、`trading_days`、`index_series_meta` **尚未落地**（M1 PR-4/5/6 未合并） | `grep -rn 'derived_dataset_versions' src migrations` 无命中 |
| 迁移链是固定长度数组，加一个迁移要同时改数组长度与顺序断言 | `src/sqlite_tasks.rs:41-46` |
| `base.dbf` 全部字段在 #6 契约里是 **UNANCHORED**（模块文档明示） | `src/catalog_tdx/mod.rs:9-11` |
| DBF 保守解析器存在（可复用，但不赋语义） | `src/dbf.rs:75` |
| gbbq 只处理 cat1 | `src/official_market.rs:9-12`、`src/qfq.rs:79-83` |

### 2.3 外部抽样核对（只用于支撑设计判断，不是准入证据）

完整清单见本任务交付的 `work/evidence-notes.md`（E-1…E-15）与 `work/evidence_manifest.json`（仓库外）。与本设计直接相关的六条：

- **E-1/E-2 交易所上市名单**：深交所 `CATALOGID=1110`（**2,906 行，只有在册**）字段含「A股上市日期 / A股总股本 / A股流通股本」；上交所 `COMMON_SSE_CP_GPJCTPZ_GPLB_GP_L` 的 `STOCK_TYPE=1`（主板 A）返回 **1,845 行**，其中 `STATE_CODE=2`（在册）1,632、`=3`（终止上市）145、`=7` 68 —— **不按状态过滤会把 145 个退市股当在册**；`STOCK_TYPE=2`（B 股）54 行、`STOCK_TYPE=8`（科创板）621 行。**上交所对 A+B 公司返回两行**（`STOCK_TYPE=1` A 股、`=2` B 股），两行 `LIST_DATE` 不同（`600680`：A 1993-10-18 / B 1994-10-20）；不按 `STOCK_TYPE` 过滤会把一只证券数成两只、并造出假的日期冲突。**分页必须用 `pageHelp.beginPage/endPage`**：只传 `pageHelp.pageNo` 会重复返回第一页（实测 `pageNo=2` 仍以 `600000` 开头，加上 `beginPage=2&endPage=2` 才返回 `601568` 起的 845 行）。
- **E-3/E-4 交易所终止上市名单**：上交所 `COMPANY_STATUS=3&STOCK_TYPE=1` 返回 **145** 个 A 股（另有 13 个 B 股；科创板 `STOCK_TYPE=8` 3 个：`688086`/`688287`/`688555`），最早退市日 2001-04-25，含 `600001`（2009-12-29，吸收合并）与 `600002`（2006-04-24）；深交所 `CATALOGID=1793_ssgs&selectModule=main|nm` 返回 170+41 行（去重 211，其中 B 股 23 → A 股 **188**），最早 1999-07-12，含 `000003`（2002-06-14）。**两者都不需要 2013 年这个 TDX 边界。**
- **E-5 A 股股本结构**：上交所 `COMMON_SSE_CP_GPJCTPZ_GPLB_GPGK_GBJG_C` 返回**当前一期**（`TRADE_DATE=20261023`）：`600028`（中石化）A 股 970.39 亿股、`601398`（工行）2,696.12 亿股（均不含 H 股）；`600087`（已退市）返回空。→ 有 A 股口径，但没有历史。
- **E-6/E-7 公告索引**：上交所 `queryCompanyBulletinNew.do` 有 `BULLETIN_TYPE_DESC`（「停复牌提示性公告」）+ `SSEDATE` + PDF URL；深交所 `annList` 有 `publishTime`+`secCode`，但 `searchkey` **不做标题过滤**（传「停牌」返回 3,932 条无关结果）。→ 「关键词检索 0 条」不能证明「没有公告」。
- **E-13 覆盖核对（本任务新算）**：335 个退市 A 股代码里 **333 个**被交易所终止上市名单覆盖（沪 145 + 科创 3 + 深 188 = 336 个代码）；**覆盖不到的 2 个是 `sz.300114`（2025-02-14 后代码迁移为 `sz.302132`，两段 bar 历史相同）与 `sh.600849`（2010-02-03 后无 bar，无终止上市记录，属吸收合并）**。62 个 `SSDATE='0'` 的代码里 **61 个**被名单补上，只剩 `sh.600849`。
- **E-14 日期一致性（本任务新算）**：`base.dbf SSDATE` 与交易所 A 股 `LIST_DATE` 在 2,7xx 个两者都有的代码上**零冲突**（差异全部来自把 B 股行也算进来的误读）。这与调研报告①「15 例差异」不是同一个比较（那是 `SSDATE` vs `.day` 首根 bar）。

---

## 3. 设计总览

### 3.1 分层与数据流

```
原始证据层   generation 里的 zip/dbf 字节（不可变，绑 sha256）
             └ hsjday.zip  base.zip(base.dbf, infoharbor_spec.cfg)  gbbq.zip
             交易所名单/公告的离线快照（准入后按 generation 冻结）
                 │  每个来源一个 Adapter（分离点 1）
                 ▼
解析事实层   不复权日线(M1)  因子事件(M1)  上市/退市事件  简称区间  股本事件
             风险警示区间  停牌区间  规则行
                 │  纯函数（分离点 2，带 algorithm_version）
                 ▼
派生层       T 日股票池  可交易性  涨跌停价  流通市值/换手率  交易日历(M1)  前复权(M1)
                 │
                 ▼
研究读模型   src/research/：universe / member_state / tradability / price_limits / market_cap
                 │  三个入口适配（分离点 5）
                 ├── MCP v8（dataset 分支 + as_of/known_at）
                 ├── 导出 bin（Parquet + 清单）
                 └── 运维（task_sqlite check 深检、来源对账）
```

### 3.2 数据集与层

| 数据集 | 层 | 存储 | 版本身份 | 入口 |
|---|---|---|---|---|
| `market.security_master` | 解析事实（来源聚合） | `security_master` | 内容摘要 → `version_id` | v8 dataset / 导出 / 运维 |
| `market.security_names` | 解析事实 | `security_name_history` | 同上 | v8 / 导出 |
| `market.capital_events` | 解析事实 | `capital_events` | 同上 | v8 / 导出 |
| `market.risk_warnings` | 解析事实（**需 #308**） | `risk_warning_intervals` | 同上 | v8 / 导出 |
| `market.suspensions` | 解析事实（`unresolved_gap` 是派生） | `suspension_intervals` | 同上 | v8 / 导出 |
| `market.price_limit_rules` | 规则数据 | `price_limit_rules` + `min_tick_rules` | 同上 | v8（只读）/ 导出 |
| `market.universe` | 派生 | **不存**（区间视图） | 由主表 + 迁移表决定 | v8 `selection.kind=universe` / 导出 |
| 涨跌停价、可交易性、市值 | 派生 | **不存** | `algorithm_version` | v8 / 导出 |

规则：**派生不进 `version_id`**（沿用 M1 §3.10 第 2 条），只带 `algorithm_version` 与输入版本。

---

## 4. 证券主表

### 4.1 身份与生效区间

```text
行身份 = (exchange, code, effective_from)      effective_to 开区间，NULL = 至今
```

- **为什么不引入全局 `security_id`**：`market_series.entity` 已经是 `sh.600000` 这种「代码即实体」的身份；再造一个全局 ID 会要求每个读路径维护两套身份映射，而 M2 真正需要的是「同一代码在不同时间属于不同主体」这一件事——用不重叠区间就能表达。`issuer_key` 作为**列**保留来源给的发行人键（公司全称 / orgId），用于把区间归组。
- **代码复用**（同一代码先后属于两个上市主体）：`sh.600018` 的 `SSDATE=2006-10-26`，但 `.day` 首根 bar 是 2000-07-19（调研报告① §4.5）。主表在 `effective_from=2006-10-26` 开一行，之前的历史 bar 仍挂在同一 `entity` 上——**M2 不能把历史 bar 按发行人拆开**。处理：该行 `issuer_resolution='ambiguous'`，`universe` 默认仍包含它（当时确实可交易），`strict` 模式可剔除；见 §13 的「代码复用」偏差。
- **代码迁移**（同一主体换代码，历史 bar 复制到新代码）：实测 `sz.300114`（bars 2010-08-27…2025-02-14）与 `sz.302132`（bars 2010-08-27…2026-10-23）**同一段历史挂在两个代码上**。主表必须有 `code_migration` 事实（`from_code`、`to_code`、`effective_from`、`source`），否则 `universe` 会把同一主体数成两只、回测收益翻倍。迁移事实的来源：交易所名单的代码变更/公告（候选，需准入），未准入前用 `.day` 首根 bar 相同 + 旧代码停止 + 新代码出现做**候选**，并且 `certainty='candidate'`，默认**保留旧代码到其最后一根 bar、新代码从其首根 bar 起**，重叠区间用 `strict` 模式剔除（fail-closed）。

### 4.2 上市日

| 优先级 | 来源 | 字段 | 覆盖（实测） | 备注 |
|---|---|---|---|---|
| 1 | 交易所上市名单（S-1） | SSE `LIST_DATE`（`STOCK_TYPE=1`）/ SZSE「A股上市日期」 | 当前在册 + 终止上市名单里的 333/335 | **必须按 `STOCK_TYPE=1` 过滤**，否则 A+B 公司出两行 |
| 2 | TDX `base.dbf`（S-7） | `SSDATE` | A 股 5,577 行，74 行缺失 | #6 契约里仍是 UNANCHORED，需字段锚定证据包 |
| 3 | `unknown` | — | 剩余 | 不得用 `.day` 首根 bar 代替 |

冲突策略：两值都保留（`listing_date_tdx`、`listing_date_exchange`），取交易所值，`listing_conflict=1`，冲突清单可枚举。实测过滤 `STOCK_TYPE=1` 后**零冲突**，所以这条规则的成本是零，但必须有——否则将来名单修订时没有地方记录分歧。

`listing_date` 的**语义边界**：它是「A 股上市日」。A+B 公司的 B 股上市日单独记（`b_listing_date`），公司整体上市日不进主表（没有用途，且会与 A 股日混淆）。

### 4.3 退市日与退市整理期

| 事实 | 来源 | 语义 |
|---|---|---|
| `delisting_date`（摘牌日，开区间上界） | 交易所终止上市名单（S-2）> TDX `infoharbor_spec.cfg` 的 `end` > `unknown` | `T < delisting_date` 即在册 |
| `arrangement_start` / `arrangement_end` | TDX `infoharbor_spec.cfg`（S-8） | 退市整理期区间；整理期内**仍可交易**，状态为 `delisting_arrangement` |
| `delisting_reason_class` | 交易所名单/公告（候选） | `arrangement`（整理期退市）/ `absorption`（吸收合并）/ `other`；未取证时 `unknown` |

实测边界：TDX 表 169 行、2013 年起；`600001`/`600005` 不在表内；`000004` 的 `end=20260714` 与深交所 `zzrq` 一致。交易所名单补上 333/335，剩下 2 个是代码迁移与吸收合并（E-13）。

`delisting_date` 与 `.day` 末根 bar 的关系：末根 bar 通常是摘牌前最后一个交易日（`000004`：末根 2026-07-13、`zzrq` 2026-07-14）。**主表不存「末根 bar 日」**——那是行情事实，读时从序列取。

### 4.4 证券简称历史

| 市场 | 来源 | 覆盖 | 语义 | 状态 |
|---|---|---|---|---|
| SZ | 深交所 `SSGSGMXX/tab2`（简称变更） | 7,483 行，1994-01-03…2026-09-28（#308 已冻结，sha256 `d9e1992…`） | 变更日期 + 变更前/后简称 | 候选已验证，**准入归 #308** |
| SH | **无 dated name-history**（#308 结论）；`OTHER_ABBR` 不是曾用名 | — | — | 硬缺口 |
| SH/BJ | GP29（`field_id=29`，value2 = 名称标记类） | 全包 57,517 条 field29、8,773 条非零（排除 `gpsh999999` 后 6,857） | 事件日 + 标记类别，**不是区间**，也不给新旧简称 | 候选，需 #308 准入 |
| 全市场 | 交易所名单的当前简称 | 当前 | 只有当前 | 快照，不是历史 |

设计：

- 表 `security_name_history(exchange, code, effective_from, effective_to, name, known_at, source_kind, source_record_id)`；`source_kind ∈ {szse_name_history, gp29_marker, exchange_snapshot, unknown}`。
- **区间闭合只对 SZ 成立**；SH/BJ 的区间由「事件日 + 下一个事件日」推断时，必须 `certainty='unbounded'`，并且 `known_at` 只能来自公告（没有公告就 NULL）。
- 名称不参与任何派生（涨跌停、股票池都不用名称），只作展示与核对。这条要写进读模型：`universe` 不因名称缺失而剔除成员。

### 4.5 主表版本与目录版本的关系

- 目录（`securities.catalog`）是主表在**「今天」的投影**：`catalog_members` = `universe(as_of=最后一个交易日)` 的子集（实测差异：目录 5,228、有序列 5,224、官方包 A 股代码 5,561）。
- **M2 不改目录的语义**：`CatalogScope::CurrentShSzA` 仍是「当前沪深 A 股」。历史成员集合走 `market.security_master` + `universe(as_of)`。
- 目录版本（`catalog-…`）只用于「今天」的查询；`universe(as_of=T)` 的复现靠主表版本（`security_master` 的内容摘要），不靠目录版本。v8 的 `selection.kind=universe` 因此有两个模式：带 `catalog_version`（今天）与带 `as_of`（历史），二者互斥（同时给 → 拒绝）。

---

## 5. 股本历史

### 5.1 准入范围与语义声明

只采用 **cat 2/3/9/10** 的 `(effective_day, float_after, total_after)`：

- `float_after` / `total_after` 来自 f32 位模式，单位万股、2 位小数 → **换算成股是精确整数**（0.01 万股 = 100 股），所以库里存 `INTEGER` 股，不存浮点。
- 前值（`float_before`/`total_before`）**只作审计列**，不参与状态；语义声明写死为「TDX 上一条记录的状态，可能滞后」（调研报告② §9 第 1 条）。
- `category` 只作诊断字段，**生产语义不依赖 cat 标签**（cat2 与 cat9 不可区分，调研报告② §4.6）。
- 不入库：cat5（混合桶，含 002868 群体级反例）、cat7（19 条，5/19 抽样有 5,148 万股级前值错误）、cat8（继承上游 cat5 错误）。它们进证据包的 `excluded` 计数，不进生产表。

实测条数：cat2 4,866 + cat3 4,929 + cat9 7,987 + cat10 304 = **18,086 条**（其中京市 1,479 条按 §13 偏差处理）。

### 5.2 状态函数

```text
float_shares(T) = float_after of the last event with effective_day <= T   (无事件 → unknown)
total_shares(T) = total_after of the same event                            (同上)
```

- 同一 `effective_day` 多条事件：按 gbbq 解码顺序 `ordinal` 取最后一条，并把「同日多事件」计入诊断。
- 事件链一致性检查（门禁，不是猜测）：`event[i].float_after != event[i+1].float_before` 时记 `chain_gap` 计数（调研报告②实测 0.1–2.4% 会触发，已知原因），不阻断，但进证据包。
- 单事件合理性门禁（调研报告② §9 第 4 条）：总股本单条变动 > 50%、或流通股减少而总股本不变 → 该事件 `suspicious=1`，默认**不参与状态**（fail-closed），可用 `allow_suspicious` 显式放开。这条能捕获 002868、600810 两类反例。

### 5.3 市值与换手率口径

| 量 | 公式 | 口径 | 状态 |
|---|---|---|---|
| 流通市值 | `close_raw(T) × float_shares(T)` | 流通 A 股（不含 B/H） | 可达 |
| 换手率 | `volume(T) / float_shares(T)` | 同上 | 可达 |
| 公司总市值 | `close_raw(T) × total_shares(T)` | **A+B+H 全部股份**，把 H 股价折成 A 股价 | 可达，但必须标 `scope='company_total'` |
| A 股总市值 | `close_raw(T) × a_share_total_shares(T)` | A 股总股本 | **不可达**：只有当前快照（E-5），历史无来源 |

→ 主表与读模型里，市值字段必须带 `share_scope ∈ {float_a, company_total, a_share_total}`；`a_share_total` 在来源准入前返回 `unknown`，**不用 `company_total` 冒充**（A+H 会高估 15–30%，调研报告② §3.3）。

### 5.4 `known_at`

gbbq 没有公告日（调研报告② §3.4/§7.5）→ `capital_events.known_at` 恒 `NULL`。后果必须写清：**带 `known_at` 的查询会排除全部股本事实**（§12.2），这是「不知道当时是否已知」的正确表达，不是缺陷。想按生效日用，就显式用 `as_of`（不带 `known_at`），并在响应里标 `known_at_coverage='none'`。

---

## 6. 风险警示（历史 ST）

### 6.1 #308 的现状（只读引用，不推进）

- Final Gate = **`OWNER_RISK_ACCEPTED`**，不是 `APPROVED_SOURCE`；`securities.status_history` 不得进入生产采集。
- 已达成的：SZ 有官方 dated name-history（7,483 行）并已验证 1,729 个 GP29 SZ 事件里 1,654 个同日命中；BJ 的 GP29 状态事件都有官方公告行；GP29 全包可枚举可重放。
- 缺口：**SH 缺 dated name-history**（`OTHER_ABBR` 不是曾用名）；BJ 缺 dated 表；GP29 不能独立给警示原因与 `known_at`；早期 SSE 公告索引稀疏；TDX 包滚动更新、授权未冻结；`code4` 可能是撤销、退市整理/终止、或普通更名（`600743` 反例）；`code2/3` 可能是保留标记的改名（`600753` 反例）。

### 6.2 M2 交付到什么程度

| 能力 | M2 是否交付 | 前提 |
|---|---|---|
| 表结构、状态词汇、`raw_status_code` 保留、`announced_at`/`known_at`/`effective_*` 四列 | **是**（实现 PR 用夹具测） | 无 |
| 状态机与 fail-closed 规则（无公告事件 → `unresolved`，不得 `normal`） | **是** | 无 |
| SZ 区间（name-history + GP29 + 公告闭合） | **否**（等 #308 批准） | #308 |
| SH 区间（只有公告能闭合，GP29 仅候选） | **否**；批准后也只能「有公告的事件」 | #308 |
| BJ 区间 | **否** | #308 |
| `known_at` 的秒级精度 | **否**（M2 只承诺日粒度；秒级只在部分接口有） | 来源能力 |
| 「今天」的 ST 标记核对 | **是**（可选） | 上交所名单的 `STATE_CODE=7` 与深交所当前简称（E-1/E-2）；**只能核对 `as_of=今天`，不得据此造历史区间**（#308 明令禁止用当前名称推断历史） |

### 6.3 状态词汇与区间

```text
normal | other_risk_warning | delisting_risk_warning | delisting_arrangement
| terminated_or_delisted | suspended | marker_only_unresolved | unknown
```

（与 #308 Stage C 的 `status-c-status-history-contract.json` 一致，不另造一套。`suspended` 保留在词汇表里是为了与 #308 对齐，M2 不往这张表写停牌行——停牌在 `suspension_intervals`。）

- 区间 `[effective_from, effective_to)`；`effective_to` 由**下一个事件**决定，没有下一个事件时开区间。
- `announced_at`（公告时间）与 `known_at`（消费侧最早可用时间）分开；`known_at` 只能来自公告/发布证据。
- 一个证券在某日**没有任何区间覆盖**时，状态是 `unknown`，不是 `normal`。要得到 `normal`，必须有「官方来源覆盖到该日且当日无警示」的证据（闭世界），而 #308 明确说这不成立。→ **默认所有日期都是 `unknown`，只有被区间覆盖的日期才有确定状态。**
- `code4` 的三种结局用 `raw_status_code` + 事件类区分，不归一。

### 6.4 对涨跌停的影响（必须在设计里说清）

主板 ST 5%、非 ST 10%。ST 状态未知 → 涨跌停 `unknown`（§8.5）。这意味着 **#308 未批准前，主板涨跌停派生在大部分日期不可用**——这是正确的 fail-closed，不是设计缺陷。可选诊断：显式 `allow_ambiguous=true` 时返回 `{5%, 10%}` 候选集 + `reason='st_unknown'`，但**不选一个**。

---

## 7. 停牌与可交易性

### 7.1 为什么「没有 bar」不是证据

实测（E-11）：`sz000631` 在 2006-04-27（0.76 元）与 2009-06-05（10.30 元）之间**没有任何 bar**，价格跳变 +1255%。这既可能是长期停牌，也可能是数据缺口。官方 `.day` 没有停牌占位行（调研报告① §4.9：17,988,372 根 A 股 bar 里 OHLC 为 0 的为 0）。→ 数据层只能给「缺口」，不能给「停牌」。

### 7.2 候选来源与准入要求

| 来源 | 能给出什么 | 准入需要的证据 | 状态 |
|---|---|---|---|
| 交易所停复牌公告（S-4） | 停牌/复牌公告 + 日期（SSE 有 `BULLETIN_TYPE_DESC='停复牌提示性公告'`） | 固定样本（≥3 类事件：临时停牌、重大资产重组停牌、复牌）、公告类型可枚举、`known_at`（发布时间）、历史覆盖 | 候选 |
| GP29 `value1`（#308 Stage B） | 「是否复牌日/停牌天数」 | 全市场完整性证明、与公告的一致性、包滚动与授权 | 候选（属 #308） |
| 单日涨跌幅/低流动性/缺 bar | — | **禁止**（#308 明确列出） | 不可用 |

### 7.3 表与派生

```text
suspension_intervals(exchange, code, start_day, end_day,
                     certainty ∈ {confirmed, unresolved_gap},
                     missing_days, reason ∈ {exchange_announcement, gp29_resume, missing_bar_no_event},
                     known_at, source_kind, source_record_id, evidence_digest)
```

- `confirmed`：有公告/GP29 证据，`known_at` 可查（公告缺失时 NULL → 带 `known_at` 的查询排除）。
- `unresolved_gap`：**派生**。定义：`calendar` 里属于该证券在册区间的交易日，落在「前一根 bar」与「后一根 bar」之间且没有 bar，连续段合并为一行，`missing_days` 计数。它**不表示停牌**，只表示「这几天没有行情数据，原因未知」。
- 不生成 `unresolved_gap` 的情形：上市前、退市后、非交易日、以及**最后一根 bar 之后**（那是退市/摘牌，不是缺口）。
- 单日缺口也算 `unresolved_gap`（可能是数据缺失），但 `missing_days=1` 的缺口与长缺口在诊断上分开统计。

### 7.4 可交易性判定（顺序固定，fail-closed）

```text
1. 不在 universe(as_of=T)                        → false  (not_listed / delisted)
2. T 不在交易日历                                  → false  (non_trading_day)
3. 有 confirmed 停牌区间覆盖 T                      → false  (suspended)
4. 有 unresolved_gap 覆盖 T                        → unknown (data_gap)
5. 该证券在 T 没有 bar 且没有任何缺口解释            → unknown (no_bar)
6. 有 bar                                         → true
```

- **唯一的肯定证据是「有 bar」**；`false` 需要明确事件；其余一律 `unknown`。
- 返回值带 `reason_code` 与 `evidence_ref`（区间行 / 公告 id），便于使用者复核。
- 不在 M2 做成交判定（涨跌停能不能买到、流动性够不够），那是回测引擎的事。

---

## 8. 交易规则与涨跌停派生

### 8.1 规则表（分离点 6）

两张小表，不做「规则 DSL」：

```sql
price_limit_rules(
  rule_id TEXT PRIMARY KEY,
  market TEXT NOT NULL,          -- sh / sz / bj / all
  board TEXT NOT NULL,           -- main / gem / star / bse / all
  status_class TEXT NOT NULL,    -- normal / st / star_st / delisting_arrangement / all
  listing_phase TEXT NOT NULL,   -- first_day / first_5_days / seasoned / all
  band_bp INTEGER,               -- 基点，1000 = 10%；band_side='none' 时为 NULL
  band_side TEXT NOT NULL,       -- both / up_only / none
  rounding TEXT NOT NULL,        -- half_up / floor / ceil
  effective_from INTEGER NOT NULL,
  effective_to INTEGER,
  basis_ref TEXT NOT NULL,       -- 出处：URL + 条款 + 抓取时间 + sha256
  basis_status TEXT NOT NULL,    -- verified / pending
  evidence_digest BLOB NOT NULL,
  UNIQUE(market, board, status_class, listing_phase, effective_from),
  CHECK(effective_to IS NULL OR effective_to > effective_from)
)

min_tick_rules(rule_id, market, board, effective_from, effective_to,
               tick_units INTEGER, price_scale INTEGER, basis_ref, basis_status, evidence_digest)
```

- **唯一键 + 测试**保证同一 `(market, board, status_class, listing_phase)` 在任一日期**恰好命中一行**；命中 0 行或 ≥2 行都 fail-closed（`rule_missing` / `rule_ambiguous`），不做「取最后一条」。
- `basis_status='pending'` 的行**不参与派生**：规则未取证 ≠ 可以用默认值。
- 代码里不出现任何幅度常量（评审项：`src/research/` 的派生代码里不得出现 `0.05`/`0.10`/`0.20`/`1000` 这类字面量，幅度只能来自规则行）。

### 8.2 规则清单（M2 需要覆盖的时段）与出处状态

| # | 规则 | 生效区间 | 出处（`basis_ref` 目标） | 状态 |
|---|---|---|---|---|
| R1 | 沪深主板 ±10% | 1996-12-16 起 | 交易所交易规则/公告原文 | **pending**（原文未抓） |
| R2 | 主板风险警示股票 ±5% | 现行 | 交易所交易规则「风险警示股票」条款 | **pending** |
| R3 | 创业板 ±10% | 2009-10-30…2020-08-21 | 深交所创业板交易规则（当时版本） | **pending** |
| R4 | 创业板 ±20%，新股前 5 个交易日无涨跌幅限制 | 2020-08-24 起 | 深交所《创业板交易特别规定》 | **pending** |
| R5 | 科创板 ±20%，新股前 5 个交易日无限制 | 2019-07-22 起 | 上交所《科创板股票交易特别规定》 | **pending** |
| R6 | 主板新股首日 44% 等历史分段 | 2014-01-01 起（更早分段待考） | 交易所公告 | **pending** |
| R7 | 1990–1996 上海多次调整、1992-05-21 放开 | 1990-12-19…1996-12-15 | 交易所档案/公告 | **pending（很可能 NOT_AVAILABLE）** |
| R8 | 最小价格变动单位 0.01 元（A 股） | 全期 | 交易所规则 + 实测（`.day` 价格为整数分） | **pending（规则）/ 实测支持** |
| R9 | 取整：四舍五入到 0.01 元 | 全期 | 实测（E-12：HALF_UP 1.76% vs floor 0.95%） | **实测支持，规则原文待补** |
| R10 | 北交所 ±30% | — | — | 不在首期（BSE 不在池） |

**结论**：M2 的规则表结构可落地，但**规则内容依赖一个独立的「交易规则来源准入」取证任务（S-9）**；在此之前，所有涨跌停派生返回 `unknown`，并带 `reason='rule_pending'`。这是本设计最硬的阻塞项之一，必须如实写进 AC 与 Issue。

### 8.3 派生算法

```text
输入：prev_close_raw（M1 的 adjustment=0 序列里 T 之前最后一根 bar 的收盘，整数分）
      board(T)、status_class(T)、listing_phase(T)、规则行、min_tick
输出：limit_up_units / limit_down_units / rule_id / basis_ref / algorithm_version
```

- `prev_close` 用**最后一根 bar**（不是「前一个交易日」）：停牌复牌后交易所的前收就是停牌前最后收盘。缺 bar 时向前找，找不到（上市首日）→ 由 `listing_phase` 规则处理。
- 计算在整数域：`up = floor((prev_cents × (10000 + band_bp) + 5000) / 10000)`；`down = floor((prev_cents × (10000 − band_bp) + 5000) / 10000)`（`half_up`）。`band_side='none'` → 返回 `unbounded`（不是 `unknown`：规则明确说无限制）。
- 最小变动单位：结果按 `min_tick_rules` 的 `tick_units` 对齐（A 股 1 分 = 1 unit）。
- **不复权**：输入必须是不复权价。用前复权价算会在除权日错（前复权序列没有跳空，交易所的涨跌停基准是除权参考价）。
- 派生**不入库**（Q5 默认）：读时算，导出时物化；`algorithm_version='limit-v1'`。

### 8.4 上市阶段

`listing_phase(T)` = 相对 `listing_date` 的位置：`first_day`（T = 上市日）、`first_5_days`（T 在上市后 5 个交易日内）、`seasoned`。`listing_date=unknown` 时 phase 也是 `unknown` → 涨跌停 `unknown`。

### 8.5 未知的传播

| 缺失项 | 结果 |
|---|---|
| 规则行缺失 / `basis_status='pending'` | `unknown` + `rule_missing`/`rule_pending` |
| ST 状态未知（主板） | `unknown` + `st_unknown`（显式诊断时给 `{5%,10%}` 候选） |
| 上市日未知 | `unknown` + `listing_unknown` |
| 前收缺失 | `unknown` + `no_prev_close` |
| 规则命中 ≥2 行 | `unknown` + `rule_ambiguous`（配置错误，测试必须拦住） |

---

## 9. 存储设计（schema 23）

### 9.1 为什么是 23，不是改 022

- 022 已合并（`src/sqlite_tasks.rs:45`）且 M1 的 PR-4/5/6 还会往 schema 22 里加表（`adjustment_events`、`derived_dataset_versions`、`trading_days`、`index_series_meta`）。M2 再改 022 会让两条线同时编辑同一个迁移文件。
- 规则：**一个里程碑一个迁移，追加在链尾，合并后不再编辑**。M2 的迁移号 = M1 最后一个迁移号 + 1（当前计划 023；若 M1 的后续 PR 改用 023，则 M2 顺延为 024，内容不变）。
- 依赖：023 必须排在 M1 PR-4（`derived_dataset_versions`）之后；若 PR-4 晚于 M2 的 PR-1，M2 的登记表写入先落空，用「数据集版本登记」这一段延后到 PR-4 之后（实现拆分里标出）。

### 9.2 表

```sql
-- 1) 主表（解析事实，来源聚合）
CREATE TABLE security_master(
  exchange TEXT NOT NULL CHECK(exchange IN ('sh','sz','bj')),
  code TEXT NOT NULL,
  effective_from INTEGER NOT NULL,
  effective_to INTEGER,
  instrument_class INTEGER NOT NULL CHECK(instrument_class IN (0,1,2)),
  issuer_key TEXT,
  issuer_resolution TEXT NOT NULL CHECK(issuer_resolution IN ('issuer_key','code_date','ambiguous')),
  listing_date INTEGER,
  listing_date_tdx INTEGER,               -- 冲突时保留另一来源
  listing_source TEXT NOT NULL CHECK(listing_source IN ('exchange_list','tdx_ssdate','unknown')),
  b_listing_date INTEGER,
  delisting_date INTEGER,
  delisting_date_tdx INTEGER,
  delisting_source TEXT NOT NULL CHECK(delisting_source IN ('exchange_list','tdx_arrangement','unknown')),
  delisting_reason_class TEXT NOT NULL CHECK(delisting_reason_class IN ('arrangement','absorption','other','unknown')),
  arrangement_start INTEGER, arrangement_end INTEGER,
  listing_conflict INTEGER NOT NULL CHECK(listing_conflict IN (0,1)),
  delisting_conflict INTEGER NOT NULL CHECK(delisting_conflict IN (0,1)),
  known_at TEXT,
  source_binding_digest BLOB NOT NULL REFERENCES source_bindings(source_binding_digest),
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  PRIMARY KEY(exchange, code, effective_from),
  CHECK(effective_to IS NULL OR effective_to > effective_from),
  CHECK(listing_date IS NULL OR listing_source <> 'unknown'),
  CHECK(delisting_date IS NULL OR delisting_source <> 'unknown')
) STRICT;

-- 2) 代码迁移（同一主体的代码变更）
CREATE TABLE code_migrations(
  from_exchange TEXT NOT NULL, from_code TEXT NOT NULL,
  to_exchange TEXT NOT NULL, to_code TEXT NOT NULL,
  effective_from INTEGER NOT NULL,
  certainty TEXT NOT NULL CHECK(certainty IN ('confirmed','candidate')),
  source_kind TEXT NOT NULL, source_record_id TEXT, known_at TEXT,
  evidence_digest BLOB NOT NULL,
  PRIMARY KEY(from_exchange, from_code, effective_from)
) STRICT;

-- 3) 简称历史
CREATE TABLE security_name_history(
  exchange TEXT NOT NULL, code TEXT NOT NULL,
  effective_from INTEGER NOT NULL, effective_to INTEGER,
  name TEXT NOT NULL,
  certainty TEXT NOT NULL CHECK(certainty IN ('interval','unbounded')),
  known_at TEXT,
  source_kind TEXT NOT NULL CHECK(source_kind IN ('szse_name_history','gp29_marker','exchange_snapshot','unknown')),
  source_record_id TEXT,
  evidence_digest BLOB NOT NULL,
  PRIMARY KEY(exchange, code, effective_from)
) STRICT;

-- 4) 股本事件（解析事实；只 cat 2/3/9/10）
CREATE TABLE capital_events(
  exchange TEXT NOT NULL, code TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
  effective_day INTEGER NOT NULL,
  category INTEGER NOT NULL CHECK(category IN (2,3,9,10)),
  float_before_bits INTEGER NOT NULL, total_before_bits INTEGER NOT NULL,
  float_after_bits INTEGER NOT NULL,  total_after_bits INTEGER NOT NULL,
  float_after INTEGER NOT NULL CHECK(float_after >= 0),   -- 股（由万股 2dp 精确换算）
  total_after INTEGER NOT NULL CHECK(total_after >= 0),
  prev_is_lagged INTEGER NOT NULL DEFAULT 1 CHECK(prev_is_lagged IN (0,1)),
  suspicious INTEGER NOT NULL DEFAULT 0 CHECK(suspicious IN (0,1)),
  known_at TEXT,                                           -- gbbq 无公告日 → NULL
  source_binding_digest BLOB NOT NULL, evidence_digest BLOB NOT NULL,
  PRIMARY KEY(exchange, code, ordinal)
) STRICT;
CREATE INDEX capital_events_code_day ON capital_events(exchange, code, effective_day, ordinal);

-- 5) 风险警示区间（生产数据需 #308）
CREATE TABLE risk_warning_intervals(
  exchange TEXT NOT NULL, code TEXT NOT NULL,
  effective_from INTEGER NOT NULL, effective_to INTEGER,
  raw_status_code TEXT NOT NULL,
  normalized_status TEXT NOT NULL CHECK(normalized_status IN (
    'normal','other_risk_warning','delisting_risk_warning','delisting_arrangement',
    'terminated_or_delisted','suspended','marker_only_unresolved','unknown')),
  announced_at TEXT, known_at TEXT,
  source_kind TEXT NOT NULL CHECK(source_kind IN (
    'szse_name_history','exchange_announcement','gp29_marker','unresolved')),
  source_record_id TEXT, source_document_sha256 TEXT, source_generation_id TEXT,
  evidence_digest BLOB NOT NULL,
  PRIMARY KEY(exchange, code, effective_from)
) STRICT;

-- 6) 停牌区间
CREATE TABLE suspension_intervals(
  exchange TEXT NOT NULL, code TEXT NOT NULL,
  start_day INTEGER NOT NULL, end_day INTEGER NOT NULL,
  certainty TEXT NOT NULL CHECK(certainty IN ('confirmed','unresolved_gap')),
  missing_days INTEGER NOT NULL CHECK(missing_days > 0),
  reason TEXT NOT NULL CHECK(reason IN ('exchange_announcement','gp29_resume','missing_bar_no_event')),
  known_at TEXT, source_kind TEXT NOT NULL, source_record_id TEXT,
  evidence_digest BLOB NOT NULL,
  PRIMARY KEY(exchange, code, start_day),
  CHECK(end_day >= start_day)
) STRICT;

-- 7) 规则表（§8.1 两张）
```

要点：

1. **`suspicious` 默认参与状态的方式是「不参与」**：状态函数跳过 `suspicious=1` 的事件（§5.2），除非显式放开。
2. **`capital_events` 存 bits 与股两套**：bits 是不可变事实，股是派生（可重算）；测试断言 `to_shares(bits) == float_after`。
3. **`known_at` 全是 TEXT（ISO8601）而不是日序号**：秒级来源将来能直接写进来，不用改类型。
4. 主表与事件的 `version_id` 用内容摘要（`sha256` 规范序列化），规则同 M1 §3.10；空数据集非法。

### 9.3 版本与登记

- 每个数据集一个 `derived_dataset_versions` 行（`dataset_kind` 扩到 `security_master`/`security_names`/`capital_events`/`risk_warnings`/`suspensions`/`price_limit_rules`）。**这要求 M1 的 PR-4 把 `dataset_kind` 的 CHECK 写成可扩展的，或由 023 重建该表**——接口要求写进 §16 的依赖。
- `universe` 不单独登记版本：它由 `security_master` + `code_migrations` + `trading_days` 决定，导出清单里记三者的 `version_id` 即可（派生可重算，§3.2）。

### 9.4 迁移与回滚

- 023 追加到 `COMPACT_MIGRATIONS`（数组长度 4 → 5，`src/sqlite_tasks.rs:41-46` 的顺序断言同步）；`EXPECTED_COMPACT_SCHEMA` 22 → 23（`scripts/check_current_facts.py:15`）。
- 全新 init 与 18→compact 升级的空表分支都可建；**不做原地数据迁移**（D-08）。
- 回滚：旧二进制 + 归档的 schema 22 库；导出产物独立，不受影响。
- 旧二进制遇 schema 23 必须明确拒绝（现状 `unsupported schema` 路径，已有测试）。

---

## 10. 新来源准入计划

每个来源都要按 #308 的「离线输入准入」模式：**固定样本 + 完整覆盖证据 + 授权边界 + owner 裁决**。下表的「历史覆盖」与「更新频率」是本任务抽样所见，**不是准入结论**。

### S-1 交易所上市名单（沪/深）

| 项 | 内容 |
|---|---|
| 发布方 | 上交所、深交所 |
| 获取方式 | 上交所 `query.sse.com.cn/sseQuery/commonQuery.do?sqlId=COMMON_SSE_CP_GPJCTPZ_GPLB_GP_L`（`STOCK_TYPE=1` 主板 A / `=8` 科创板，且必须带 `STATE_CODE`/`COMPANY_STATUS` 与 `pageHelp.beginPage/endPage`）；深交所 `www.szse.cn/api/report/ShowReport?SHOWTYPE=xlsx&CATALOGID=1110` |
| 格式 | JSON（SSE）/ XLSX（SZSE，inlineStr） |
| 历史覆盖 | **只有当前在册**（沪市 A 2,318 = 主板 `STATE_CODE ∈ {2,7}` + 科创板在册部分；深市 2,906；两者之和与已发布日线序列数 5,224 一致，属推断口径）；历史靠「逐代落库」 |
| 更新频率 | 每个交易日（推断：字段随新股/更名变化） |
| 稳定性 | 高（交易所官网）；接口参数与分页形状可能变，需契约测试；**必须按 `STOCK_TYPE`/`STATE_CODE` 过滤、按 `beginPage/endPage` 翻页** |
| 需要的证据 | 固定 3 个证券 × 上市日/简称/A 股股本三方对照；A+B 公司 `STOCK_TYPE` 语义；分页与总数字段一致性；`recordcount` 与实际行数一致 |
| 阻塞 | 上市日的「官方优先来源」；不阻塞用 TDX-only 的降级实现 |
| 建议 | **单独来源准入取证任务 A-1**；阻塞 AC-M2-1 的「交易所来源」分支 |

### S-2 交易所终止上市名单（沪/深）

| 项 | 内容 |
|---|---|
| 发布方 | 上交所、深交所 |
| 获取方式 | 上交所同一 sqlId + `COMPANY_STATUS=3`，`STOCK_TYPE ∈ {1,2,8}`；深交所 `CATALOGID=1793_ssgs&TABKEY=tab2&selectModule=main\|nm` |
| 格式 | JSON |
| 历史覆盖 | 上交所 145 A 股 + 3 科创板（最早退市日 2001-04-25）；深交所 188 A 股（最早 1999-07-12） |
| 更新频率 | 退市事件驱动（低频） |
| 稳定性 | 高；深交所必须带 `selectModule`，否则 0 行 |
| 需要的证据 | 335 个退市代码的覆盖对账（实测 333，未覆盖 `sh.600849` 与 `sz.300114`）；`DELIST_DATE`/`zzrq` 与 `.day` 末根 bar 的关系；吸收合并类是否在名单内（`600001` 在） |
| 阻塞 | 退市日的官方来源；阻塞 AC-M2-2 的「交易所来源」分支 |
| 建议 | **单独来源准入取证任务 A-2**（与 S-1 同一发布方，但语义与覆盖不同，分开取证） |

### S-3 风险警示公告（#308 范围）

| 项 | 内容 |
|---|---|
| 发布方 | 上交所、深交所、北交所 |
| 获取方式 | 上交所 `queryCompanyBulletinNew.do`（`BULLETIN_TYPE`）；深交所 `api/disc/announcement/annList`（按 `stock` + 日期）；北交所 `productInfoResult.do` |
| 历史覆盖 | 深交所名称变更表 1994 起；上交所公告索引早期稀疏（1998–1999 多个窗口为空） |
| 稳定性 | 中（接口参数变过：`queryCompanyBulletinNew` 要全大写参数） |
| 需要的证据 | **#308 已列全**（AC-S01…AC-X01），M2 不重复、不替代、不推进 |
| 阻塞 | ST 生产数据；阻塞 AC-M2-7 的生产分支 |
| 建议 | **不拆新任务**：归 #308；M2 只消费它的裁决结果 |

### S-4 停复牌公告

| 项 | 内容 |
|---|---|
| 发布方 | 上交所、深交所 |
| 获取方式 | 上交所 `queryCompanyBulletinNew.do` + `BULLETIN_TYPE_DESC='停复牌提示性公告'`；深交所 `annList`（标题级，需逐条判读） |
| 格式 | JSON + PDF |
| 历史覆盖 | 上交所可回溯到 2000 年前后（样本 2000–2001 有 PDF）；深交所索引覆盖近年 |
| 更新频率 | 事件驱动，交易日多次 |
| 稳定性 | 中；深交所 `searchkey` 不做标题过滤（E-7），**不能用「检索 0 条」证明没有公告** |
| 需要的证据 | 3 类事件（临时停牌/重大资产重组停牌/复牌）× ≥3 个样本；`BULLETIN_TYPE` 枚举完整性；公告类型与「停牌/复牌」的映射；`known_at`（发布时间）；「公告未找到 ≠ 未停牌」的闭世界证明（多半拿不到 → 只能 `unresolved_gap`） |
| 阻塞 | `confirmed` 停牌；不阻塞 `unresolved_gap` 与可交易性 `unknown` |
| 建议 | **单独来源准入取证任务 A-3**；**不阻塞** M2 主体实现 |

### S-5 A 股总股本（沪/深）

| 项 | 内容 |
|---|---|
| 发布方 | 上交所、深交所 |
| 获取方式 | 上交所 `COMMON_SSE_CP_GPJCTPZ_GPLB_GPGK_GBJG_C`（逐股，`TRADE_DATE` 一期）；深交所 `CATALOGID=1110` 的「A股总股本 / A股流通股本」列 |
| 格式 | JSON / XLSX |
| 历史覆盖 | **只有当前一期**（E-5）；退市证券返回空 |
| 更新频率 | 每日（股本变动后更新） |
| 稳定性 | 中（上交所接口在 JS 里，参数可能变） |
| 需要的证据 | A+H 样本 4 只（`000002`/`000898`/`600028`/`601398`）的 A 股口径；与 gbbq `total_after` 的差额 = H/B 股数；「历史不可得」的证明（同一接口只返回一个 `TRADE_DATE`） |
| 阻塞 | **历史** A 股总市值（不可达）；不阻塞流通市值（gbbq 的 `float_after` 够） |
| 建议 | **单独来源准入取证任务 A-4**；结论很可能是「当前快照可用、历史 NOT_AVAILABLE」——那也要写成裁决，不能默认 |

### S-6 证券简称历史（SZ 官方 dated 表）

| 项 | 内容 |
|---|---|
| 发布方 | 深交所 |
| 获取方式 | `www.szse.cn/api/report/ShowReport` `SSGSGMXX/tab2`（#308 已冻结） |
| 历史覆盖 | 7,483 行，1994-01-03…2026-09-28 |
| 稳定性 | 中（快照会更新，需逐代落库） |
| 需要的证据 | #308 已做（sha256 `d9e1992…`、1,654/1,729 同日命中、22 个缺口） |
| 阻塞 | SZ 简称区间；归 #308 |
| 建议 | **不拆新任务**：归 #308 |

### S-7 TDX `base.dbf` 字段锚定（不是新来源，是字段语义取证）

| 项 | 内容 |
|---|---|
| 发布方 | 通达信（官方文件） |
| 获取方式 | 已在手（`base.zip!base.dbf`） |
| 需要的证据 | `SSDATE` = A 股上市日的**字段级锚定**：≥20 个样本 × 与交易所名单逐一对照；`ZGB`/`LTAG` 的单位与口径（万股 2dp、`LTAG` = 流通 A 股）已有实测，但按 #6 契约仍是 UNANCHORED，要单独写一份锚定证据（字段偏移、类型、样本、sha256） |
| 阻塞 | 上市日的 TDX 来源；不阻塞交易所来源分支 |
| 建议 | **单独取证任务 A-5**（与 S-1 互补：一个证明官方名单，一个证明 TDX 字段） |

### S-8 TDX `infoharbor_spec.cfg`（D-24 的准入）

| 项 | 内容 |
|---|---|
| 发布方 | 通达信 |
| 获取方式 | 已在手（`base.zip!infoharbor_spec.cfg`，5,030 B / 169 行，sha256 `83b44fa0…`） |
| 需要的证据 | 格式与字段语义（`market\|code\|0\|start\|end`）、与 `.day` 末根 bar 的 152/169 一致、与交易所终止上市日的对照、滚动保留边界（行是否永久保留）、不含 2013 年前与吸收合并的证明 |
| 阻塞 | 退市整理期区间；不阻塞交易所来源分支 |
| 建议 | **并入 A-2 或单独 A-6**（建议单独，因为它是 TDX 私有格式，与交易所名单的证据类型不同） |

### S-9 交易所交易规则原文（涨跌停/首日/最小变动）

| 项 | 内容 |
|---|---|
| 发布方 | 上交所、深交所 |
| 获取方式 | 交易所「业务规则」栏目（上交所入口已抽样：`www.sse.com.cn/lawandrules/sselawsrules/`）；深交所规则库 |
| 格式 | HTML/PDF |
| 历史覆盖 | 现行规则可得；**历史版本（1996/2009/2019/2020 分段）需要逐条找当时的规则文本或修订公告** |
| 稳定性 | 中（栏目改版、PDF 迁移） |
| 需要的证据 | 每条规则：条款号 + 生效日期 + 原文片段 + 抓取时间 + sha256；至少覆盖 R1–R6；R7（1990–1996）很可能 NOT_AVAILABLE，要写成裁决 |
| 阻塞 | **涨跌停派生**（最硬的阻塞项之一） |
| 建议 | **单独来源准入取证任务 A-7**，且**阻塞** AC-M2-9 的生产验收 |

### 10.1 阻塞关系汇总

| 实现项 | 阻塞来源 | 未准入时的降级 |
|---|---|---|
| 主表（上市日） | S-1 / S-7 | TDX-only，缺失 `unknown`；交易所分支标 `pending_source` |
| 主表（退市日） | S-2 / S-8 | TDX 整理期 only（2013 起），其余 `unknown` |
| 简称历史 | #308（S-6） | 只有当前快照 + GP29 事件（`unbounded`） |
| 风险警示 | #308（S-3/S-6） | 全部 `unknown`（不是 `normal`） |
| 停牌 `confirmed` | S-4 / #308（GP29） | 只有 `unresolved_gap` |
| A 股总市值 | S-5 | `unknown`（流通市值不受影响） |
| 涨跌停 | S-9 | `unknown` + `rule_pending` |

**不阻塞**的项：schema、股本事件、股票池、`unresolved_gap`、可交易性判定框架、导出、v8 接线。这些是 M2 可以独立合并的部分。

---

## 11. T 日股票池

### 11.1 定义

```text
universe(as_of = T) = {
  row ∈ security_master
  : instrument_class = 0（A 股）
  ∧ row.effective_from ≤ T ∧ (row.effective_to IS NULL ∨ T < row.effective_to)
  ∧ (row.listing_date IS NULL ∨ row.listing_date ≤ T)
  ∧ (row.delisting_date IS NULL ∨ T < row.delisting_date)
  ∧ 未被 code_migrations 在 ≤ T 的时点取代
}
```

特殊情况与处理：

| 情形 | 处理 | 理由 |
|---|---|---|
| `listing_date = unknown` | 若 `T ≥ 该代码首根 bar 日` 则**包含**，标 `membership_basis='bar_evidence'`；否则排除 | bar 是「当时确实交易过」的证据；上市日未知不能变成「不存在」 |
| `delisting_date = unknown` | 只要 `T ≥ 首根 bar` 就包含，标 `delisting_unknown` | 同上；退市日未知时不能假设已退市 |
| 退市整理期 | **包含**，`status='delisting_arrangement'` | 整理期内可交易 |
| 停牌 | **包含**（股票池 ≠ 可交易） | 见 §7 |
| 代码复用（`issuer_resolution='ambiguous'`） | 包含，带标记；`strict=true` 时排除 | M2 无法拆 bar |
| 代码迁移（`300114`→`302132`） | 在 `effective_from` 之前只保留旧代码，之后只保留新代码；重叠区间在 `strict=true` 时全部排除并告警 | 否则同一主体被数两次 |
| 上市前/退市后 | 排除 | 定义 |
| 非交易日 | 空集（或 `as_of` 取最近一个交易日，由调用方决定） | 不在池定义里猜 |

### 11.2 与目录版本的关系

- **目录是主表在今天的投影**，不是历史成员集合的来源。`universe(as_of=今天)` 与 `catalog_members` 的差异必须逐条解释：实测目录 5,228 vs 已发布序列 5,224（4 个成员没有序列，其中 1 个连 `.day` 都没有：`sz.001381`）vs 官方包 A 股 5,561（335 个不在目录）。
- `universe(as_of=T)` 的复现靠 `security_master` 的 `version_id`，**不靠** `catalog_version`。
- 退市股**行情入库**是 M3：M2 的 `universe` 会包含 335 个退市代码，但其中大部分在库里没有序列（只有官方包里有）。读模型必须区分「在池」与「有数据」：`has_series=false` 是合法状态，不能因此把成员从池里删掉。这条是 M2/M3 的接口边界。

### 11.3 接口（设计，不实现）

```text
research::universe(as_of, known_at?, strict=false) -> Vec<Member>
Member { entity, exchange, code, listing_date, delisting_date, status,
         issuer_resolution, membership_basis, known_at, flags[] }
```

- 面板/截面的 `selection.kind=universe` 复用这个函数（v8 §4.3）。
- 导出写成**区间**而不是逐日展开：`universe_intervals(entity, member_from, member_to, basis, flags)`；5,200 只 × 平均 3 段 ≈ 1.5 万行，比 5,200×8,800 的逐日矩阵小三个数量级。

---

## 12. 派生、日更、导出、v8 与契约

### 12.1 日更维护

| item | 写集 | 依赖 | 失败语义 |
|---|---|---|---|
| 逐实体日线 item（M1） | 不复权 + 前复权 + `adjustment_events` + **`capital_events`（同一份 gbbq 解码）** | generation | 现状不变 |
| `derived:security_master`（1 个） | 主表 + 简称 + 上市/退市 + 迁移 | 交易所来源（准入后）+ TDX 文件 | 失败 → 该 item 失败；**不影响**已提交的行情 |
| `derived:risk_warnings`（1 个） | 风险警示区间 | #308 批准后才有写路径；未批准时写 0 行 + `pending_source` | 同上 |
| `derived:suspensions`（1 个） | `confirmed`（来源）+ `unresolved_gap`（派生） | 日历 + 主表 + 公告 | 同上 |
| `derived:trading_rules`（1 个） | 规则表（只在规则来源更新时写） | S-9 | 同上 |

- 为什么主表不按实体拆 item：主表是**全市场一致性**的事实（代码复用、迁移、退市名单要一起看），按实体拆会产生「一半新一半旧」的中间态。
- 失败语义沿用 M1 §9.2：内部 item 失败 → `partially_failed`，不静默跳过。
- 耗时估算：主表约 5,600 行 + 简称约 2 万行 + 股本事件 1.8 万行（已在逐实体 item 里）+ 停牌区间（首代约 10–20 万行）→ 首代 +5–15s，稳态每代 +1–3s（推断，需实测）。

### 12.2 `known_at` 的查询语义（v8 启用方式）

| 参数 | 位置 | 语义 | 默认 |
|---|---|---|---|
| `as_of` | `filter`（约束**记录集**） | 区间数据：`effective_from ≤ as_of < effective_to`；`universe`：成员判定时点 | 缺省 = 今天（最后一个交易日） |
| `known_at` | 顶层（约束**整条查询的信息集**） | 只返回 `known_at ≤ known_at` 的行；`known_at IS NULL` 的行**排除并计数** | 缺省 = 不过滤 |

- 响应必须带 `temporal: {as_of, known_at, unknown_known_at_rows, datasets_without_known_at[]}`，让使用者知道「这条结论用了多少未知知晓时间的事实」。
- **不接受 `known_at` 的数据集**（股本事件）要显式报 `known_at_coverage='none'`，而不是静默返回全部行。
- 两个参数同时给时，先按 `known_at` 过滤信息集，再按 `as_of` 选区间（顺序不能反：否则会用到「T 日还不知道」的后续事件）。

### 12.3 v8 的接线（不改 v8 已冻结的行为）

- v8 当前**拒绝** `as_of`/`known_at`（`reserved_for_m2`）。M2 只提供数据与读模型，**不改这个拒绝**。
- 启用方式：v8 的 `get_data_capabilities` 增加**逐数据集**的 `temporal` 声明：

```json
{"dataset":"market.security_master","temporal":{"as_of":true,"known_at":true,"known_at_coverage":"partial"},
 "columns":[...]}
```

- 解码层规则：参数只在目标数据集声明支持时接受；否则 `invalid_arguments` + `unsupported_for_dataset`（替代 `reserved_for_m2`）。这样「M2 数据已落地、v8 还没发版」和「v8 发了、某个数据集还没准入」两种中间态都不会产生错误的接受。
- 新增数据集（不加工具）：`market.security_master`、`market.security_names`、`market.capital_events`、`market.risk_warnings`、`market.suspensions`、`market.price_limit_rules`；`market.universe` 作为 `selection.kind=universe` + `as_of` 的**语义**而不是新 dataset（避免「成员集」与「成员属性」两个入口）。
- 契约版本：M2 不升 `capability_revision`；这些 dataset 与参数随 v8 的升版一起进（v8 已计划升 `generic-data-v8`）。**若 v8 先发版而 M2 数据未准入**，v8 的 `temporal` 声明就是 `false`，参数继续被拒绝——这是设计允许的中间态。

### 12.4 导出（M1 的 `export_research` 扩展）

| 文件 | 行数量级 | 列 |
|---|---|---|
| `security_master.parquet` | 5,650 | exchange, code, effective_from/to, instrument_class, issuer_key, issuer_resolution, listing_date, listing_source, delisting_date, delisting_source, arrangement_start/end, known_at, flags |
| `security_names.parquet` | ~20,000 | exchange, code, effective_from/to, name, certainty, known_at, source_kind |
| `capital_events.parquet` | 18,086 | exchange, code, ordinal, effective_day, category, float_after, total_after, known_at(NULL), suspicious |
| `risk_warnings.parquet` | ~5,000（准入后） | exchange, code, effective_from/to, normalized_status, raw_status_code, announced_at, known_at, source_kind |
| `suspensions.parquet` | 10–20 万 | exchange, code, start_day, end_day, certainty, missing_days, reason, known_at |
| `price_limit_rules.parquet` | ~30 | rule_id, market, board, status_class, listing_phase, band_bp, band_side, rounding, effective_from/to, basis_ref, basis_status |
| `universe_intervals.parquet` | ~15,000 | entity, member_from, member_to, basis, flags |
| `price_limits.parquet`（可选） | 16.7M | entity, trade_day, limit_up, limit_down, state, rule_id |

- **`daily_raw.parquet` 不加派生列**：它是「解析事实」，涨跌停是派生；混在一起会让「同一 `version_id` 的日线文件内容随规则变化」。要涨跌停就取 `price_limits.parquet`（同一快照、同一绑定）。
- 清单 `schema` 升到 `x-data-mcp.research-snapshot.v2`：`files[]` 增加 `dataset` 与 `temporal` 字段，`limitations` 增加三条（ST 缺口、股本 `known_at` 缺失、规则 pending）。`snapshot_id` 的定义不变（规范 JSON 前 32 hex）。
- 体积：新增文件合计约 10–30MB（Parquet 压缩后），相对 M1 的 ~340MB 可忽略；`price_limits` 是可选项，约 200MB（估算，未实测）。

### 12.5 契约影响

| 面 | M2 是否变化 | 说明 |
|---|---|---|
| 工具数量 / `capability_revision` | 否 | M2 不加工具、不升版 |
| dataset 枚举 / wire 字段 | 否（M2 本身） | 新 dataset 随 v8 进 |
| 导出清单 | **是**（`research-snapshot.v1` → `v2`） | 新增 `files[].dataset`/`temporal`；旧读取方按「未知文件忽略」兼容，但 `snapshot_id` 会变 |
| 下游 `x` 仓库 | 否 | 不碰；P-02 未决 |
| 数据库 schema | 22 → 23 | 全新 init，不做原地迁移 |

---

## 13. 可行性逐项与必须写明的偏差

| 项 | 可行性 | 偏差 / 代价 |
|---|---|---|
| 主表（身份 + 区间） | **可达** | 代码复用期间的历史 bar 无法按发行人拆分（§4.1） |
| 上市日 | **可达（99.96%）** | 5,562 个 A 股代码里只有 2 个既无 `SSDATE` 也无交易所上市日：`sh.600849`、`sz.001381` → `unknown` |
| 退市日 | **可达（99.4%）** | 2/335 覆盖不到（代码迁移、吸收合并）；2013 年前的普通摘牌靠交易所名单（已覆盖） |
| 退市整理期 | **可达（2013 起）** | TDX 表不含 2013 年前与吸收合并；需 D-24 门禁 |
| 简称历史 | **部分** | SH/BJ 无 dated 源 → `unbounded`；`known_at` 多数为 NULL |
| 股本历史 | **可达** | 只有后值 + 事件日；`known_at` 全 NULL；京市 1,479 条无官方对照（调研报告② §7.2） |
| 流通市值/换手率 | **可达** | 依赖 `float_after` 与不复权收盘，两者都在手 |
| A 股总市值（A+H） | **不可达** | 只有当前快照；历史 `unknown` |
| 历史 ST | **部分（SZ）/ 硬缺口（SH）** | 见 §6；首期偏差第 2 条（data-catalog §3.3） |
| 停牌 | **部分** | `confirmed` 需公告准入；`unresolved_gap` 可达 |
| 可交易性 | **可达（带 unknown）** | 大量日期会是 `unknown`，这是正确结果 |
| 涨跌停 | **结构可达，内容待准入** | 规则原文未取证前全部 `unknown`；1990–1996 可能 NOT_AVAILABLE |
| T 日股票池 | **可达** | 退市股行情入库是 M3；`has_series=false` 是合法状态 |

**必须写进 `docs/data-catalog.md` §3.3 的新增偏差**（M2 实现 PR 里做）：

1. 历史 A 股总股本不可得 → A+H 公司的「A 股总市值」返回 `unknown`，不得用公司总股本冒充。
2. 代码复用与代码迁移：复用期间的历史 bar 无法按主体拆分；迁移的重叠区间默认包含，`strict` 模式剔除。
3. 停牌的 `confirmed` 覆盖取决于公告准入；在此之前大量日期为 `unresolved_gap`（可交易性 `unknown`）。
4. 涨跌停：规则未取证前为 `unknown`；早期（1990–1996）可能永久不可得。

---

## 14. 分离点落位

| # | 分离点 | 本设计的设计点 | 边界靠什么守住 | 过度设计防线 |
|---|---|---|---|---|
| 1 | 来源接入层 ↔ 领域层 | S-1/S-2/S-4/S-5/S-9 各一个 Adapter（放在现有 `src/official_files`/`src/catalog_tdx` 邻近模块）；TDX 的 `base.dbf`/`infoharbor` 复用现有读取；每个 Adapter 过 #308 门禁 | **依赖方向**：领域层只收「上市事件 / 退市事件 / 简称区间 / 公告事实 / 规则行」，代码里不出现 `ssgsgmxx`、`infoharbor`、`commonQuery` 这类来源名（可 grep）；**测试**：领域层单测不构造任何来源结构体 | 不新建 crate；不加动态插件；一个来源 = 一个模块 + 一处注册 |
| 2 | 原始证据 → 解析事实 → 派生 | 原始：generation 字节（绑 sha256）；解析事实：主表/简称/股本/风险警示/停牌/规则；派生：股票池、可交易性、涨跌停、市值 | **测试**：每个派生有 `algorithm_version` 且能只从解析事实重算（AC-M2-11）；派生不进 `version_id` | 不新增派生结果表（涨跌停/市值/股票池都不入库） |
| 3 | 生效时间 ↔ 知晓时间 | 每张事实表都有 `effective_from/to` + `known_at`（+ `announced_at` 在 ST）；查询先按 `known_at` 过滤信息集，再按 `as_of` 选区间 | **契约**：`temporal` 声明逐数据集；**测试**：`known_at` 排除 NULL 行并计数；ST 的 `known_at` 不能来自事件日 | 不造「时间轴 DSL」；不预建秒级时间列 |
| 4 | 存储模型 ↔ 研究读模型 | `src/research/` 的 `universe`/`member_state`/`tradability`/`price_limits`/`market_cap`；表名与 SQL 只出现在存储适配 | **依赖方向**：MCP 与导出只调读模型；**测试**：换存储实现不改响应 schema（golden） | 不为每张表建 trait；读模型直接吃只读连接 |
| 5 | 核心用例 ↔ 入口适配 | 核心：读模型 + 派生纯函数；入口：v8 dataset 分支、导出 bin、运维深检 | **契约**：三个入口的 `universe`/`price_limits` 结果逐字节相同；**测试**：入口只做参数适配 | 不复制查询逻辑；不做「一键回测」 |
| 6 | 交易规则 ↔ 数据 | `price_limit_rules` + `min_tick_rules`，按生效区间；每行 `basis_ref` | **测试**：同一键在任一日期恰好命中一行；`grep` 派生代码无幅度常量；创业板 2020-08-24 切换只改数据 | 不造规则 DSL；不做表达式求值 |
| 7 | 编排 ↔ 原子能力 | 原子：`universe`、`member_state`、`tradability`、`price_limits`、一次导出；编排由 AI 显式组合 | **契约**：工具不返回 `next_actions`；不新增「选股+回测」工具 | 不把 Skill 流程塞进工具 |

**顺手消除的循环依赖**：M2 会碰 `market_data`/`publish`/`storage` 三者（主表读模型要读序列、`Adjustment` 的枚举已由 M1 扩展）。改到相关代码时按调研报告③ §2.3 顺手拆（把 `market_data` 对 `publish::EffectiveVersion` 的依赖改为领域自有类型），**不单独立项**。

---

## 15. 扩展场景走查（6 个）

先读 `docs/design.md`「后续数据扩展要求」：本设计与之**一致**（新来源只改 Adapter；新类别用独立模型；扩展契约要求 scope 命名带类别、版本引用带 `dataset_kind`）。需要补的 4 处单列在 §15.2。

| # | 场景 | 会改哪些层 / 分离点 | 新增 | 修改 | 能否「只改适配层 + 一处登记」 |
|---|---|---|---|---|---|
| 1 | 新增证券类型（ETF / 指数 / 北交所 / B 股） | 分离点 1、2、6；主表的 `instrument_class` 与 `price_scale` | 主表一行（类型 + 交易市场 + 规则行）；ETF 还需身份名单（M3） | `security_master.instrument_class` 的取值（已预留 2）；`universe` 的类型过滤；规则表的 `market/board` 行；ETF 的 `price_scale=3`（M1 已预留） | **指数：能**（主表一行 + 规则行）。**ETF：不能只改适配层**——需要身份名单、量额单位（份/手）、cat11；**B 股：不能**（币种字段是结构改动，且 B 股上市日/代码规则不同）；**北交所：能（设计上）**——`market='bj'` + 规则行 + 目录 scope，但目录 scope 是 M3 的事 |
| 2 | 新增一个官方来源 | 分离点 1 | 新 Adapter + #308 证据包 + `source_bindings.role` | 装配处一处注册；`pending_source` 列表；capabilities 的 `supported_sources` | **能**。领域层只认事实，不认来源名；新来源不涨工具数 |
| 3 | 新增字段或数据集（如财务） | 分离点 2、4 | 独立领域模型 + 类型化表 + migration + 读模型投影 + 导出文件（或 v8 dataset 分支） | `derived_dataset_versions.dataset_kind`；导出清单 `files[]` | **不能**：`docs/design.md` 已裁定新类别用独立模型；强行塞进主表会让主表变成万能表 |
| 4 | 新增频率（分钟线） | 分离点 2、4 | 新 `period`/独立表 + 秒级时间戳 | `series_bars` 的主键语义（日是 `trade_day` 整数） | **不能**：存储跟着数据走（M1 §12 已裁定） |
| 5 | 交易规则变更（涨跌停幅度） | 分离点 6 | 规则表一行（生效区间 + `basis_ref`） | **无代码改动** | **能**（M2 落地后）。创业板 2020-08-24 就是这一行的现实例子 |
| 6 | 新增使用方或入口（v8 之后再加导出格式/入口） | 分离点 4、5 | 新入口适配（`EntryKind` 一个变体 + allowlist）或新格式 writer（`Snapshot` → 字节流） | 预算表加一行；入口封闭性测试加一条 | **能**：入口是枚举 + 静态表，格式是纯函数；代价是每个入口多一个 systemd 单元与就绪探针 |

### 15.1 达不到「只改适配层」的三处及原因

- 场景 1 的 ETF/B 股：精度（厘）、币种（港币/美元）、身份名单三处是**结构**差异，不是语义声明。
- 场景 3 的财务：`docs/design.md` 已裁定独立领域模型。
- 场景 4 的分钟线：`trade_day` 是日序号，分钟线要改主键或新增表。

不为达标强行抽象（复杂度预算）。

### 15.2 需要修改 `docs/design.md` 的地方（单列，不在本 PR 改）

1. 「后续数据扩展要求」→「三类扩展及影响范围」：补**派生数据**这一类（股票池、可交易性、涨跌停、市值、复权价、日历），规定「必须可从下层重算 + 带 `algorithm_version` + 不进 `version_id`」。
2. 「扩展契约」：补**按生效日期管理的规则数据**（交易规则）与**时点证券主表**两类；写明「目录快照只是主表在某一时点的投影」。
3. 「扩展契约」：补一条「**事实行必须同时带生效时间与知晓时间**；`known_at` 缺失时按数据集声明 coverage，不得回填」。
4. 「产品闭环与排除范围」：在「不将保存历史版本等同于无前视偏差回测数据」后补一句「前复权序列本身含未来事件；时点复权只能用不复权 + 因子」（M1 已建议，合并时一并改）。

---

## 16. 实现拆分（9 个可独立合并的 PR + 7 个来源准入任务）

通用约束：本机不编译，Rust 正确性只由 PR 上的 CI 证明；每个 PR 跑 §17 的本机门禁。**PR-1 必须先行**（没有表就没有写入路径）。

| PR | 范围 | AC | 测试计划（CI） | 依赖 |
|---|---|---|---|---|
| **PR-1** schema 23 + 表结构 | 023 迁移（§9.2 七张表）；`COMPACT_MIGRATIONS` 长度与顺序断言；`EXPECTED_COMPACT_SCHEMA` 23；`derived_dataset_versions.dataset_kind` 扩展（或重建）；空表升级路径 | AC-M2-14 | 迁移在全新 init 与空表升级两条路径可建；旧二进制拒绝 schema 23；唯一键/CHECK 冲突测试 | 无（但需 M1 PR-4 的 `derived_dataset_versions` 先落地，否则登记段延后） |
| **PR-2** TDX 解析事实 | `base.dbf` 的 `SSDATE`/`ZGB`/`LTAG` 读取（复用 `src/dbf.rs`，加字段锚定常量）；`infoharbor_spec.cfg` 解析；`capital_events` 写入（cat2/3/9/10，整组替换）；`security_master` 的 TDX-only 行 | AC-M2-1（TDX 分支）、AC-M2-2（TDX 分支）、AC-M2-5 | 夹具：3 个 `base.dbf` 行 + 3 个 `infoharbor` 行 + 6 条 gbbq 事件；`to_shares(bits)` 断言；cat5/7/8 不入库；`suspicious` 门禁 | PR-1 |
| **PR-3** 主表组装 + 复用/迁移 | 交易所名单 Adapter 的**接口**（trait 不必；一个函数 + `pending_source`）；区间合并与重叠检测；`code_migrations`（confirmed/candidate）；冲突记录 | AC-M2-3、AC-M2-13 | 重叠 → `ambiguous_code_reuse`；`600018` 夹具；`300114`/`302132` 迁移夹具；来源缺失 → `pending_source` | PR-2 |
| **PR-4** 研究读模型 | `src/research/`：`universe`、`member_state`、`tradability`、`market_cap`；`as_of`/`known_at` 语义；`strict` 模式 | AC-M2-4、AC-M2-6、AC-M2-8 | 10 个历史日的成员集合（夹具）；`known_at` 排除 NULL 并计数；停牌 → `unknown`；A+H 的 A 股总市值 `unknown` | PR-3 |
| **PR-5** 规则表 + 涨跌停派生 | `price_limit_rules`/`min_tick_rules` 读写；`limit-v1` 纯函数（整数域、HALF_UP、phase、ST）；`unknown` 传播 | AC-M2-9、AC-M2-10 | 6 个板块×时段夹具；命中率回归（用 E-12 的样本口径）；规则缺失/命中 2 行 → `unknown`；`grep` 无幅度常量 | PR-1、PR-4 |
| **PR-6** 停牌与可交易性 | `unresolved_gap` 派生（区间合并、`missing_days`）；`confirmed` 写入接口（来源未准入时 0 行）；可交易性判定顺序 | AC-M2-8 | `sz000631` 夹具（2006-04-27→2009-06-05）；上市前/退市后不生成 gap；单日 gap 与长 gap 分开 | PR-4 |
| **PR-7** 导出 | `research-snapshot.v2`；7 个新文件；清单 `dataset`/`temporal`；`price_limits` 可选 | AC-M2-11 | 清单确定性（同输入同 `snapshot_id`）；新文件列类型；`daily_raw` 不含派生列；只读 | PR-4、PR-5、PR-6 |
| **PR-8** 日更集成 | 逐实体 item 加 `capital_events`；4 个 `derived:` 内部 item；失败语义；耗时记录 | AC-M2-13 | 内部 item 失败 → `partially_failed`；`pending_source` 不阻塞；进度形状不变（golden） | PR-2…PR-6 |
| **PR-9** v8 接线（**依赖 v8 实现**） | dataset 分支、`temporal` 声明、`as_of`/`known_at` 接受条件、错误码 | AC-M2-12 | 参数在未声明数据集上被拒；`temporal=false` 时仍拒；三个入口结果一致 | PR-4、v8 |

拆分原则：PR-1 只建表、不改行为；PR-2 只落 TDX 解析事实（**不依赖任何新来源准入**）；PR-3 之后才引入交易所来源接口（未准入时降级）；PR-5 的规则内容永远来自数据，测试用夹具规则；PR-9 不在 M2 里发版，只提供接线。

### 16.1 来源准入任务（独立 Issue，不计入实现 PR）

| ID | 任务 | 阻塞的实现项 | 建议 |
|---|---|---|---|
| A-1 | 交易所上市名单准入（沪/深） | 上市日的交易所来源 | 单独 Issue，按 #308 模式；样本 ≥3 证券 × 上市日/简称/A 股股本 |
| A-2 | 交易所终止上市名单准入（沪/深，含科创板与 B 股过滤） | 退市日的交易所来源 | 单独 Issue；必须解释 2 个未覆盖代码（`sh.600849`、`sz.300114`）与 B 股/科创板的 `STOCK_TYPE` 过滤 |
| A-3 | 停复牌公告准入 | `confirmed` 停牌 | 单独 Issue；**不阻塞**主体实现 |
| A-4 | A 股总股本准入（沪/深，当前快照） | 历史 A 股总市值（结论很可能是 NOT_AVAILABLE） | 单独 Issue；结论要写成裁决 |
| A-5 | `base.dbf` 字段锚定（SSDATE/ZGB/LTAG） | TDX 上市日分支 | 单独 Issue；与 A-1 互补 |
| A-6 | `infoharbor_spec.cfg` 准入（D-24） | 退市整理期 | 单独 Issue；或并入 A-2（建议单独） |
| A-7 | 交易规则原文准入（R1–R7） | **涨跌停派生** | 单独 Issue；**最硬阻塞项** |
| — | 历史 ST（S-3/S-6） | ST 生产数据 | **归 #308**，M2 不拆新任务 |

---

## 17. 本机与 CI 验证方式

- 本机**禁止** `cargo build/test/check/clippy/nextest/run`；只允许 `cargo fmt`。
- 本机能跑：`git diff --check`、`cargo fmt --check`、`python3 scripts/check_current_facts.py`、`python3 scripts/check_daily_sync_eval.py`、`python3 scripts/check_deploy_facts.py`、`python3 scripts/check_public_hygiene.py`，以及 `.github/workflows/ci.yml` 里的各 `python3 -B -m unittest discover -s scripts -p '<test_x>.py'`（`test_runtime_peer.py` 需要 sudo 与编译产物，跳过并注明）。
- Rust 正确性只能由 PR 上的 GitHub Actions 证明；没有 CI 结果时一律写「未运行」。
- 每个实现 PR 的 CI 验证方式（写进 PR 正文）：
  1. `cargo fmt --check`、`cargo clippy --all-targets --all-features --locked -- -D warnings`；
  2. `cargo nextest run --locked --no-fail-fast` + `cargo test --doc`；
  3. `cargo build --locked`；
  4. 迁移与 schema 断言（现有 `schema_upgrade` 类 + `check_current_facts`）；
  5. 需要真实 generation/官方来源的 AC（AC-M2-1/2/4/9）不在 CI 里伪造：用固定小夹具覆盖逻辑，真实全量验收放到部署前的私有栈步骤，结果贴 PR/Issue。

---

## 18. 实测、未验证与待确认

### 18.1 本次新增实测（可复现）

全部在本任务交付的 `work/` 目录（仓库外），命令都在 `systemd-run --scope -q -p MemoryMax=150M -p MemorySwapMax=0` 下运行。

| # | 结论 | 证据 |
|---|---|---|
| 1 | 交易所名单带上市日/A 股股本；上交所 A+B 公司返回两行且 `LIST_DATE` 不同 | `szse_1110.xlsx`、`sse_list.json`、`sse_list_600680.json` |
| 2 | 交易所终止上市名单覆盖 333/335 退市代码；未覆盖 2 个是代码迁移与吸收合并 | `sse_terminated_all.json`、`szse_delisted/*.json`、本地对账 |
| 3 | 62 个 `SSDATE='0'` 的退市代码，交易所名单补上 61 个（只剩 `sh.600849`） | 同上 + `base.dbf` |
| 4 | `SSDATE` 与交易所 A 股 `LIST_DATE` 在 2,7xx 个代码上零冲突 | 同上 |
| 5 | A 股股本结构快照只覆盖当前一期，退市证券为空 | `sse_gbjg_*.json` |
| 6 | 停复牌公告有官方分类字段；深交所 `searchkey` 不做标题过滤 | `sse_bulletin_suspend.json`、`szse_annlist_suspend.json` |
| 7 | 涨跌停取整 HALF_UP 最优（1.76% vs floor 0.95% vs ceil 1.11%） | 300 只 × 2025 年起，121,085 对 |
| 8 | `300114`/`302132` 共享同一段 bar 历史（2010-08-27 起） | `hsjday.zip` 成员解码 |

### 18.2 未验证（如实列出）

| # | 项 | 为什么未验证 | 取证方法 |
|---|---|---|---|
| U-1 | 规则原文与生效日期（R1–R7） | 本任务只到交易所「业务规则」入口页 | A-7 |
| U-2 | 交易所名单的历史版本与修订策略 | 只取了当前快照 | A-1/A-2 |
| U-3 | 公告索引的历史覆盖（尤其 1998–2001） | 只抽了 2026-10 一周 | A-3 |
| U-4 | `infoharbor_spec.cfg` 的行是否永久保留 | 只有一代快照 | A-6（逐代落库 + 对照） |
| U-5 | 京市股本事件的官方对照 | 调研报告②未覆盖 | 全国股转系统 + 北交所公告（M3） |
| U-6 | `unresolved_gap` 的全市场行数 | 需要全量扫描 5,561 个成员 | 部署前私有栈实测（估算 10–20 万行） |
| U-7 | 主表/停牌的首代与稳态耗时 | 未实现 | PR-8 实测 |
| U-8 | `price_limits.parquet` 的体积 | 未实现 | PR-7 实测 |
| U-9 | 全市场 ST 未知导致的涨跌停 `unknown` 比例 | 依赖 #308 | #308 批准后实测 |

### 18.3 待确认（与 Issue 的 Q1–Q7 对应）

见 #391 的「待确认问题」。本设计对每一项都给了默认方案，审核时只需确认或否决。

---

## 19. Also noticed（范围外，只记录）

1. **上交所列表接口对 A+B 公司返回两行**（`STOCK_TYPE=1/2`，`LIST_DATE` 不同）。任何「按代码取一行」的读取都会造出假冲突；`STOCK_TYPE` 必须进契约。同类问题在 `COMPANY_STATUS=3` 与科创板（`STOCK_TYPE=8`）上也存在。
2. **上交所列表默认返回多种状态**：`STOCK_TYPE=1` 的 1,845 行里 1,632 行是 `STATE_CODE=2`（正常上市）、145 行 `=3`（已终止）、68 行 `=7`（**风险警示，样本全部是 ST/*ST**）；`STOCK_TYPE=8`（科创板）621 行 = 605 + 3 + 13。不按状态过滤会把退市股当在册。反过来，`STATE_CODE=7` 是**当前**风险警示标记的官方来源（只能用于「今天」的核对，不能产生历史区间）。
3. **上交所分页只认 `pageHelp.beginPage/endPage`**：只传 `pageHelp.pageNo` 会重复返回第一页（实测 `pageSize=1000&pageNo=2` 仍以 `600000` 开头），静默截断数据；深交所 `ShowReport` 的 `pageSize` 参数也不被采纳（固定 20 行/页），必须按 `pagecount` 翻页。
4. **深交所 `1793_ssgs` 必须带 `selectModule`**，否则 `recordcount=0`——「查到 0 行」很容易被当成「没有退市股」。
5. **深交所 `annList` 的 `searchkey` 不做标题过滤**（传「停牌」返回 3,932 条无关结果）。「关键词检索 0 条」不能当覆盖证据。
6. `base.dbf` 含 8,933 行、覆盖 335 个退市代码，但 `src/catalog_tdx/mod.rs:9-11` 明示其字段 UNANCHORED；目录实现「不使用 base.dbf」是当时的正确决定，M2 要用它就必须先做字段锚定（A-5）。
7. **`sz.300114` 与 `sz.302132` 的 bar 历史完全相同**（都从 2010-08-27 起）。`market_series.entity` 无法表达代码迁移，`universe` 必须靠 `code_migrations` 去重，否则回测收益翻倍。
8. `src/sqlite_tasks.rs:41` 的 `COMPACT_MIGRATIONS` 是固定长度数组，加迁移要同时改长度与顺序断言；`check_current_facts.py:15` 的常量也要同步，漏一处 CI 红。
9. M1 的 `derived_dataset_versions.dataset_kind` 若在 PR-4 里写成封闭枚举，M2 的 6 个新数据集会要求再重建一次该表。**建议 M1 的 PR-4 把该 CHECK 写成开放集合或预留取值**——这是两条线的接口要求，不是 M2 能单方面决定的。
10. `src/data_supply/query.rs:79-99` 不校验目录成员：只要库里有序列就能读到任意合法实体。M2 引入 `security_master` 后，「实体在库但不在主表」会变成一种真实状态，读路径需要一条明确规则（拒绝还是返回 `unknown` 成员），这属于 v8 的范围，M2 只记录。
11. 调研报告①的「15 例 `SSDATE` 与首根 bar 差异」与本任务的「`SSDATE` vs 交易所 `LIST_DATE` 零冲突」是两个不同的比较，对外引用时不要混用。
12. `docs/data-catalog.md` §3.2 第 6 行的「62 只缺失」是**335 个退市代码子集**的数字；`base.dbf` 全表 A 股缺失是 **74** 只。两个数字都对，但必须写清范围。
