# generic-data-v10 契约（M3-2 ETF 身份与 scope 词汇）

**状态**：**代码面（未部署）**。当前部署仍是 [generic-data-v9](generic-data-v9.md) / compact schema 26；
本文件记录 M3-2 第一个改 wire 的 PR-A（Refs #509）带来的增量，以及后续 PR-B 对同一 revision
的补充（不再次升 capability revision）。代码的 fresh compact schema 已是 27（PR-B 的
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
  `basis_status`（`verified`/`pending`）。A 股 cat1 行仍为 `verified`，evidence 形状不变；
  ETF cat11 行标 `pending`，且不参与任何前复权链（D-70 Q3）。
- 首采/补采仍用 `market.daily` `start_collection` v2；Q4 的 `adjustment` 读取参数与
  ETF 不复权读面属于 PR-C，不在本契约的 PR-B 增量内。
- D-72：退市 ETF 尾部零成交 bar 原样入库，不截断、不补齐；深市尾部缺 bar 不补。

## 2. 未验证 / 部署前待办

- `tools/list` 字节数增量：新 scope 两个字符串出现在 `market.daily` 的 scope 枚举上，
  本机未编译/未实测；部署前按 v9 的同一测量入口记录，超过 30,800 B 时按已批准上限规则申请。
- 本契约只随部署事实翻转时更新 [STATUS](../STATUS.md) 与严格 Skill 的 `capability_revision`。
