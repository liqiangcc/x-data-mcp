# generic-data-v10 契约（M3-2 ETF 身份与 scope 词汇）

**状态**：**代码面（未部署）**。当前部署仍是 [generic-data-v9](generic-data-v9.md) / compact schema 26；
本文件记录 M3-2 第一个改 wire 的 PR-A（Refs #509）带来的增量，以及后续 PR-B/PR-C/PR-C2 对同一
revision 的补充（不再次升 capability revision）。代码的 fresh compact schema 已是 27（PR-B 的
`027_etf_factor_events.sql`），部署实例仍是 26。

## 1. 相对 v9 的增量

| 项 | v9 | v10 |
|---|---|---|
| `capability_revision` | `generic-data-v9` | `generic-data-v10` |
| data 工具数 | ops 16 / research 5 / maint 9 | **不变** |
| `CatalogScope` 取值 | `current_sh_sz_a`、`sh_sz_a_listed_ever` | 增加 `sh_sz_etf_current`、`sh_sz_etf_listed_ever` |
| `market.daily` 默认 universe | 目录版本（A 股） | **不变** |
| `market.daily` ETF 采集 | 无 | v2 `selection.kind=scope` 接受两个 ETF scope；只写不复权日线 |
| `securities.catalog` | 只接受 `current_sh_sz_a` | **不变**（新值在运行时被拒绝） |

### 1.1 scope 词汇的作用域

- `sh_sz_etf_current`：`security_master` 中 `instrument_class=2`、身份区间仍开放且未被退市公告
  关闭的成员（最新名单成员）。
- `sh_sz_etf_listed_ever`：所有已冻结 ETF 名单快照 ∪ 退市公告命中的 class=2 成员；移出名单
  不删行，PR-B 写入 ETF 日线后还并入已有 ETF 序列。
- 两个新值**只**由主表解析入口接受：`market.daily` 的 `selection.kind=universe` 与 v2
  `start_collection` 的 `selection.kind=scope`。`securities.catalog`（读与写）拒绝它们，
  错误信息指向 `current_sh_sz_a`。
- `market.daily` 不填 `selection.scope` 时仍解析目录版本（A 股），成员集与 v9 相同。
- PR-B 的 v2 `start_collection` 已实现 `sh_sz_etf_current` 与 `sh_sz_etf_listed_ever`：
  成员只取 `instrument_class=2`（不得放宽成所有 class），预算 1..500；名单有、`.day` 无的成员
  以 `absent` + `reason=source_member_absent`（空成员为 `source_member_empty`）写入每个 item 的
  frozen evidence，不静默丢、也不伪造成失败 item。A 股 `sh_sz_a_listed_ever` 的成员/报告口径不变。
- ETF scope 分批（XDM-193）：两个 ETF scope 的 `start_collection` 在解析出「尚无序列且 `.day` 有 bar」的成员后，按规范实体（`sh.xxxxxx`/`sz.xxxxxx`）排序，只取前 500 个作为本次任务成员；未入批的数量写入每个 item 的 frozen evidence 字段 `remaining_count`（不新增请求参数、不改 `tools/list`）。`absent` 仍按截断前的全部候选统计。重复调用即得到 500/500/…/余数，已有序列的成员不会再入批；全部采完后再调用仍返回 `nothing to collect`。同一批成员的未终态任务沿用既有 scope 冲突语义（`range_conflict`）。`sh_sz_a_listed_ever` 不变：超过 500 仍整体拒绝，evidence 不含 `remaining_count`。
- ETF 采集不按 `security_master.effective_from/effective_to` 截取历史 bar：这两个字段是身份观测/
  退市上界，不是上市区间；上市日在 `listing_date`，可交易上界按 D-72 取「最后有成交日」。

### 1.2 来源与解析器

ETF 身份来源在来源库注册，不新增 MCP 工具：

| `source_id` | 请求 | 用途 | parser |
|---|---|---|---|
| `sse_etf_list` | SSE `commonSoaQuery.do?sqlId=FUND_LIST` | 沪市 ETF 身份 + `listingDate` + `subClass` + `INDEX_CODE` | `etf-list-v1` |
| `szse_etf_list` | SZSE `ShowReport?SHOWTYPE=xlsx&CATALOGID=1945` | 深市 ETF 身份（上市日取 `base.dbf SSDATE`） | `etf-list-v1` |
| `sse_fund_delist_ann` | SSE `commonQuery.do?sqlId=COMMON_PL_JJXX_JJGG_L&TITLE=终止上市` | 沪市退市 ETF 公告索引（按季分段，分页无效） | `etf-delist-ann-v1` |
| `szse_etf_notice_ann` | SZSE `announcement/annList`（POST，`channelCode=etf_notice_disc`） | 深市退市 ETF 公告索引（`pageSize=50`、`announceCount` 为总数） | `etf-delist-ann-v1` |
| `etf_delist_pdf` | 人工确认的终止上市公告 PDF | D-72 精确退市日原文证据 | `pdf-v1`（只冻结，不解析） |
| `etf_delist_extract` | D-72 提取 CSV | 每条带 `doc_sha256`/页码/片段/提取器/置信度 | `etf-delist-extract-v1` |

`ebs` 只是报价锚，不是身份来源（D-70 Q1）。两个来源的冻结字节、`exchange_source_snapshots`
索引行与 `replay --scope exchange` 语义沿用 M3 §6/§9.4；`parser_version` 在快照清单里仍是
envelope 解析器（`exchange-sse-page-v2` / `exchange-szse-xlsx-v1`），业务解析器版本
`etf-list-v1` 记录在 `derived:security_master` 的 item evidence。

### 1.3 写入语义

- `security_master` 增加 `instrument_class=2` 行；`listing_source` 沪市 `exchange_list`、
  深市 `tdx_ssdate`。名单移除不等于退市：移出名单的成员保留 class=2 行并以第一次缺失
  观测作 `effective_to`；只有 D-71 公告命中才把身份标为退市 ETF。
- `effective_from`/`known_at` 是该成员第一次出现在已冻结名单快照或退市公告的日期。
- 退市 ETF：`delisting_date` 仅在 D-72 提取结果存在时写精确日（`delisting_source=exchange_list`，
  证据里带 PDF sha 与提取记录）；只有公告索引时保持 `NULL`（unknown），公告日作为证据另存。
  日更与 replay 共用同一落库函数：`effective_to=终止上市日`、`delisting_reason_class=other`；
  零提前量（公告日=终止上市日）时 `effective_from=终止上市日-1`，区间非空且上界不被 `+1` 改写。
  同一代码同时有名单历史事实与提取事实时：`effective_from`/`known_at` 取两者的最小值
  （已在名单里出现过的代码不会被提取事实把起点往后挪）；有提取结果时精确终止上市日
  **覆盖**名单的 `first_missing` 观测，只有没有提取结果时才取名单边界的最小值。
- 身份写入器是来源无关的：各来源产出 `security_master::assembly::MemberFact`，
  `member_rows` 统一合并（同一代码取最早出现、上市日冲突 fail-closed）并写 class=2 行。
- A 股 `security_master` 行内容不变；`market_series`、A 股导出与 A 股 `query_data` 输出不变。
- PR-A/PR-A2 无新 migration：D-72 的提取记录与 PDF 只进来源库；`security_master` 现有列足够表达
  `delisting_date`/`effective_to`。PR-B 例外：cat11 事实与逐行 `basis_status` 需要
  `027_etf_factor_events.sql`（fresh compact schema 27），仍按 D-08 全新 init，不做原地升级。

### 1.4 PR-B：精度、量纲与不复权日线

- `.day` 价格按品种取 scale：股票 `price_scale=2`（分），ETF `price_scale=3`（厘）；
  ETF 必须走 `SeriesShape::etf`，写成 2dp 是静默 10× 错价。股票路径的常量与输出不变。
- 成交量统一为**份**：股票手日月额一致性按 divisor=100 判定，ETF 按 divisor=1000；
  手日仍 ×100 还原，ETF 手日另写 `volume_quantum=100`，A 股既有手日保持 NULL（D-70 Q9）。
  r7 标记与量额一致性矛盾时 ETF 的 item 失败，不写部分序列。
- ETF 单位证据：`price_unit=yuan`（3dp）、`volume_unit=share`、`amount_unit=yuan`；
  不复权 evidence 明确 cat1/cat11 只作事实、不进入价格链。
- 因子事实：`adjustment_events` 在 schema 27 允许 category 1 与 11，并新增逐行
  `basis_status`（`verified`/`pending`）。A 股 cat1 行仍为 `verified`、`c1..c4` 仍是
  2dp HALF_UP，evidence 形状不变；ETF cat1/cat11 的 `c1..c4` 按源 f32 最短往返十进制
  （`0.661`/`0.93584`/`2`）存储、`c_bits_*` 原样，`algorithm_version` 为
  `gbbq-etf-factors-f32-shortest-v1`；ETF cat11 行标 `pending`，且不参与任何前复权链
  （D-70 Q3）。
- 首采/补采仍用 `market.daily` `start_collection` v2。
- D-72：退市 ETF 尾部零成交 bar 原样入库，不截断、不补齐；深市尾部缺 bar 不补。

### 1.5 PR-C：ETF 读面、口径参数与版本元数据

**输入参数（只对 `market.daily` 生效）**

- `query_data`、`get_data_availability`、`list_data_versions` 增加可选顶层
  `adjustment: "raw" | "qfq"`。省略时按 `qfq` 解析，A 股请求与输出不变；其他 dataset
  携带该字段返回 `invalid_arguments`（`market.yearly`/`market.index` 仍只有各自固定口径）。
- A 股显式 `adjustment=raw` 读取 2dp 不复权序列；显式 `adjustment=qfq` 与省略相同。
- ETF 首期只有不复权序列（D-70 Q3）：
  - 省略 `adjustment` → `adjustment_required_for_etf`（显式错误，不返回空页、不静默 raw）；
  - `adjustment=qfq` → `adjustment_unavailable`（ETF 前复权未交付）；
  - `adjustment=raw` → `market.daily` 返回 3dp（厘）/份的不复权 bar。
- `market.yearly` 对 ETF 返回 `absent`（D-70 Q6：首期不出 ETF 年线），与 index 经
  `market.daily` 的处理一致；单只 ETF 与 batch 中的 ETF 成员都只是 absent，不影响同一 batch
  的 A 股成员。真出现 ETF 年线存储行时按 `integrity_error` fail-closed。
- shape 解析只有一个入口：head、可见性、availability、version list 与
  `read_public_series` 都按 `market_series` 行的 `instrument_class`/`price_scale` 解析，
  并要求 `security_master` 的品种一致。ETF 序列主表缺行、或存储的 class/scale 与解析结果
  不一致时返回 `integrity_error`，绝不回退成股票 2dp 读法。
- shape 与 `delisting_date` 对同一 code 的**全部** `security_master` 历史行取 `DISTINCT`；
  出现多个 class 或多个退市日时 fail-closed（代码复用场景有意如此），不静默取最新行。

**版本元数据（可加字段，A 股不出现）**

- ETF `version_manifest[].metadata` 增加：
  - `price_scale=3`；
  - `last_trade_date`：该序列最后一根 `volume>0` 的 bar（D-72 可交易窗口上界）；
  - `delisting_date`：`security_master.delisting_date`；字段缺席表示 unknown（没有 D-72
    提取结果），不是从末根 bar 推断。`last_trade_date` 与 `delisting_date` 语义不同：
    前者是最后成交日，后者是终止上市日；回测可交易窗口用前者。
- A 股/指数行的 metadata 不新增这些字段，也不改变既有 `units`/`adjustment` 形状。
- ETF 尾部零成交 bar（`volume=0` 且 `amount=0`）在 `market.daily` 行上标
  `no_trade=true`（推导，不加存储列）；A 股行不出现该字段。`no_trade` 为真不等于退市，
  原始 bar 仍保留。

**能力表达**

- `get_data_capabilities` 在带 `dataset` 或 `operation` 的详细视图增加 `series[]`
  （无过滤的 compact catalog 仍是工具表，不内联形状）：每条声明 `dataset`、`instrument_class`、
  `adjustment`、`price_scale`、`price_unit`、`volume_unit`、`volume_quantum`、`default`。
  其中 `market.daily`/ETF/`raw` 为 `price_scale=3`、`price_unit=yuan`、
  `volume_unit=share`、`volume_quantum=100`、`default=false`；A 股 qfq 仍是默认。
- 不新增工具，不加 migration；fresh compact schema 仍为 27。

### 1.6 PR-D：研究快照导出 `research-snapshot.v4` 收录 ETF

导出不是 MCP 工具（AC-M1-10），所以本节不改 `tools/list`、能力修订或 wire 形状。版本仍记为
`x-data-mcp.research-snapshot.v4`：v4（XDM-157）在代码里已合并但**从未部署**（已部署的导出是
v3），所以 ETF 列并入 v4，不再升 v5。读取方以 `manifest.files[].columns` 判断列集；v2/v3 目录
的列集不变，仍按各自契约可读（`scripts/export_smoke.py` 按 schema 版本取列契约）。

**哪些 ETF 进导出**

- 目录选择（默认、`--catalog-version`）：所有已发布的 ETF 不复权日线序列（目录没有 ETF，
  D-70 Q5）。`--entities` 显式列表：点名的 ETF 也可选；点名但无已发布序列时
  `EXPORT_NOT_FOUND`；点名 ETF 却只要 `--adjustment qfq` 时 `EXPORT_INVALID`
  （`adjustment_unavailable`），不返回空文件。
- ETF 序列的定位复用读面的单一 shape 入口（`resolve_read_shape`）：主表缺行、class/scale 不一致、
  存储了 ETF 前复权或年线都按 `EXPORT_INTEGRITY` fail-closed。
- `daily_qfq`、`factors`、年线都不含 ETF（D-70 Q3/Q6）；`index_daily`、`calendar`、`capital_events`
  等不因 ETF 改变。

**列（追加在 v3 列之后，按位置读取的消费者不受影响）**

| 文件 | 新增列 | 说明 |
|---|---|---|
| `daily_raw`、`daily_qfq` | `instrument_class`（INT8，0 股票 / 2 ETF） | `daily_qfq` 恒为 0 |
| 同上 | `no_trade`（BOOLEAN，可空） | 股票行 NULL；ETF 行 `volume=0 ∧ amount=0` 为 true，否则 false；推导，不加存储列 |
| `universe_intervals` | `instrument_class`（INT8） | A 股区间块在前、ETF 区间块在后 |
| 同上 | `last_trade_date`、`delisting_date`（DATE，可空） | 只在 ETF 行出现；NULL = 未知；股票行恒 NULL（股票的终点是 `member_to`） |

- ETF 价格列沿用 `DECIMAL(18,6)`，`price_scale=3`；零成交 bar 与其他 bar 一样原样保留。
- `last_trade_date` 是最后一根 `volume>0` 的 bar（回测可交易窗口上界）；`delisting_date` 是
  D-72 公告提取的终止上市日（证券存续上界）。公告没有「最后交易日」，两者不互相推断。
- 两个日期与 `no_trade` 来自读面同一组函数（`series::last_trade_day`、
  `series::etf_delisting_date`、`series::is_no_trade_bar`），导出与 `query_data` 元数据不会各算各的；
  同一 code 的主表历史行出现多个退市日时导出 `EXPORT_*` fail-closed。
- ETF 区间沿用 A 股区间规则：起点取上市日，缺失时取首根 bar；终点取 `effective_to` 与退市日的
  较早者；没有上市日也没有 bar 的 ETF 不出行。

**清单**

- `series[]` 的 ETF 条目（`instrument_class="etf"`，`price_scale=3`）增加 `last_trade_date`、
  `delisting_date`（未知为 `null`），属于身份：新增成交 bar 或重新提取公告都会改变 `snapshot_id`。
  股票/指数条目的键不变。
- `limitations[]` 增加三条：ETF 只在 `daily_raw`（无前复权/因子/年线）、ETF 成员集是已发布序列
  （幸存者偏差、无 `.day` 成员的 ETF 无 bar 行）、`no_trade`/`last_trade_date`/`delisting_date`
  的推导与「不编造最后交易日」。

**A 股不变（AC-M3-2-2 的可执行定义）**

- 同一数据库加入 ETF 前后：A 股与指数序列的 `SeriesEntry`（version_id、digest）和清单条目逐一相同；
  `daily_qfq`、`factors`、`calendar`、`index_daily`、`security_names`、`capital_events`、
  `risk_warnings`、`suspensions`、`price_limit_rules`、`industry_assignments`、`code_migrations`
  字节（sha256）相同；`daily_raw` 与 `universe_intervals` 的 A 股行是文件前缀，且 v3 的各列值逐一相同
  （见 `src/research/tests/etf.rs`）。
- 两个日线文件与 universe 文件因新增列，整体 sha256 相对改动前必然不同；「改动前」的 v3/v4 字节
  不可复现，因为 `snapshot_id` 本来就含 schema 字符串。

### 1.7 PR-C2：ETF 日更（`plan_market_update` / `run_market_update`）

**输入参数（只对 `market.daily` 生效）**

- `plan_market_update` 与 `run_market_update` 的 `market.daily` 分支增加可选 `scope`：
  - 省略：与 v9 相同，只含「当前 A 股目录 ∩ 已采集」。省略 `scope` 时 `plan_id` 的规范载荷、
    `PlanView` 响应与冻结 item 的字节都**不变**（`scope`、`series` 键在值缺席时不序列化，
    由 `plan_id_matches_canonical_byte_format` 的固定摘要锁定）。
  - `sh_sz_etf_current`：只含「`sh_sz_etf_current`（`security_master` class=2、身份区间开放、
    退市来源仍 unknown）∩ 已有 ETF 不复权序列」。退市 ETF（不在 current）与名单有、`.day` 无的
    成员都不进日更。
  - `sh_sz_a_and_etf_current`：上面两类放在同一个计划、同一个任务里（Q8 的「同任务」形态）。
    这是为避免每个组合都占一个新取值而加的第三个枚举值，不是两个 scope 的列表；ETF 与 A 股
    分任务就是各发一次 plan/run。
- `scope` 进入 `plan_id` 的规范载荷（仅在存在时），所以同一时刻三种范围的 `plan_id` 互不相同；
  `run_market_update` 必须带与 plan 相同的 `scope`，否则重算出的计划不同，返回 `plan_stale`。
  `market.yearly` 的 plan/run 没有 `scope` 字段（带上会被拒绝），其成员集不含 ETF
  （D-70 Q6）。
- ETF 计划 item 的 base 通过 `resolve_read_shape(…, Daily, Some(Raw))` + `read_visible_shaped`
  解析（`SeriesShape::etf(Raw)`，3dp），不走股票 forward/raw 成对的 head 逻辑；冻结输入带
  `"series":"etf_raw"`，执行器据此走 ETF 路径。`refresh_range` 为 `[first_day, target_day]`，
  不含 `proven_gap_ranges`。

**ETF item 语义（只有不复权一条序列，不碰股票 forward/raw 成对逻辑）**

| 来源变化 | 结果 |
|---|---|
| `.day` 成员字节与 base 相同，且 cat1/cat11 事件摘要不变 | `no_change`（版本不变，`data_versions +0`） |
| `.day` 成员字节相同，事件摘要变（例如新增一次分红） | `metadata`：不写 bar，更新来源绑定，同一事务整组重写该 ETF 的 cat1/cat11 因子事实 |
| `.day` 成员是 base 成员的前缀追加（新日期都晚于 base 末根） | 追加尾部 bar；手日 bar ×100 并写 `volume_quantum=100`；同一事务整组重写因子事实 |
| 追加的尾部有 bar 落在 accepted range 之外（plan 的 `end_date` 早于成员最新一根） | item 失败，`etf_update_unverified:`；来源绑定指向整个成员，存储序列必须是成员到最新 bar 的前缀，所以不做部分发布，用覆盖最新 bar 的 `end_date` 重新 plan |
| 其他任何变化（改写历史、截短、重叠） | item 失败，错误以 `etf_history_rewrite:` 开头并带 `first_different_day=YYYY-MM-DD`；**不**整段重写，序列与因子事实保持原样 |
| 标记与量额矛盾（手日标记存在但量额不一致，或反之） | item 失败（`.day volume unit is contradictory on <date>`），不部分写 |

- ETF 的连续性证明只看 `.day` 成员字节：不复权价格与 cat1/cat11 事件无关，事件摘要变化不构成
  「历史被改写」（股票路径里事件摘要变化会让 QFQ 链失效，ETF 没有 QFQ 链）。
- 不复权 evidence 文本不再带 cat1/cat11 计数（PR-B 的文本含计数；ETF 尚未部署，所以直接去掉）：
  版本 evidence 在 typed 更新中逐字节比较，一次新分红不能被看成价格序列的语义漂移。
- 回执只有 raw 事件（没有 forward 事件）；重放与 `item_receipt` 在 forward 缺席时回落到 raw 事件，
  股票 item 总有 forward 事件，查询不变。
- ETF item 失败不影响同一任务里的 A 股 item（各自在自己的事务里记录结果）。ETF item 不进 typed
  批量准备，逐 item 执行（见部署前待办里的 SLO 实测）。
- **ETF-only 计划不追加 17 个服务器内部派生 item**（9 个指数基准、交易日历、5 个全市场数据集、
  行业快照、退市序列检查）：它们是 A 股/全局事实，不计入 ETF 批次的耗时。含 A 股的计划
  （省略 `scope` 或 `sh_sz_a_and_etf_current`）保持原样。
- 退市序列检查（`derived:delisted_series_check`）仍只登记 `instrument_class=0`（设计 P17），
  有退市日和不复权序列的 ETF 不登记、也不进 `skipped`；有测试断言。

**部署前测量要用的参数形态（本 PR 不测 SLO，§11.3 的 A/B 在上线任务里做）**

| 形态 | `plan_market_update` | `run_market_update` | 说明 |
|---|---|---|---|
| A 股单任务（基线，不变） | `{"dataset":"market.daily","contract_version":"v1","end_date":D}` | 加 `request_id`、`target_day`、`plan_id` | 与 v9 完全相同 |
| ETF 单任务 | 同上加 `"scope":"sh_sz_etf_current"` | 同上加同一 `scope` | ~1,631 item，无派生 item |
| A 股 + ETF 同任务 | 同上加 `"scope":"sh_sz_a_and_etf_current"` | 同上加同一 `scope` | ~6,869 item，含 17 个派生 item |
| ETF 与 A 股分任务 | 先 A 股单任务，完成后再 ETF 单任务（同一时刻只能有一个未终态的 `market.daily` 更新任务，`plan_in_progress`） | | 两次耗时分别记录 |

## 2. 字节预算与部署前待办

- `tools/list` 实测（本 PR，沙箱 `cargo test --locked --test mcp
  v8_tool_surface::tools_list_stays_within_entry_budgets_without_output_schema`）：
  research **12,021 B**（预算 12,200）、ops **30,488 B**（预算 30,800）、maint
  **13,523 B**（预算 14,500）。相对 PR-B 合并基线（research 11,823 / ops 30,290）各
  **+198 B**，来自 `adjustment` 字段；仍在 v9 §2 的已批准上限内，未申请新上限。
- PR-C2 实测（CI 上同一测试的打印值，代码 `1c5c686`）：research **12,021 B**（不变，这两个工具
  不在 research 入口）、ops **30,748 B**（预算 30,800，余量 52 B）、maint **13,523 B**（不变）。
  ops 相对 PR-C 基线 30,488 **+260 B**，来自 `plan_market_update`/`run_market_update` 两个
  `market.daily` 分支的可选 `scope` 字段：用一个紧凑内联 schema
  `{"enum":["sh_sz_etf_current","sh_sz_a_and_etf_current"]}`（`wire.rs` 的 `plan_scope`，
  仿 `daily_adjustment`，无 `$defs`、无 `type`、无说明文字），按 +260 B 推算在 ops 的 `tools/list` 里出现 4 次
  （每个工具 2 处）。**ops 上限不变，仍是 v9 §2 的 30,800 B**，未申请新上限。
  首版实现用 `PlanScope` 枚举的默认 schema（ops 30,918 B）和带 `type`/`null` 的紧凑 schema
  （30,868 B）都超限，所以才去掉 `type` 与 `null`；`scope` 可选，缺席（而不是 `null`）表示仅 A 股。
- 本契约只随部署事实翻转时更新 [STATUS](../STATUS.md) 与严格 Skill 的 `capability_revision`。
