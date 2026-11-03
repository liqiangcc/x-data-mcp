# generic-data-v10 契约（M3-2 ETF 身份与 scope 词汇）

**状态**：**代码面（未部署）**。当前部署仍是 [generic-data-v9](generic-data-v9.md) / compact schema 26；
本文件记录 main 上第一个改 wire 的 M3-2 PR（PR-A，Refs #509）带来的增量。

## 1. 相对 v9 的增量

| 项 | v9 | v10 |
|---|---|---|
| `capability_revision` | `generic-data-v9` | `generic-data-v10` |
| data 工具数 | ops 16 / research 5 / maint 9 | **不变** |
| `CatalogScope` 取值 | `current_sh_sz_a`、`sh_sz_a_listed_ever` | 增加 `sh_sz_etf_current`、`sh_sz_etf_listed_ever` |
| `market.daily` 默认 universe | 目录版本（A 股） | **不变** |
| `securities.catalog` | 只接受 `current_sh_sz_a` | **不变**（新值在运行时被拒绝） |

### 1.1 scope 词汇的作用域

- `sh_sz_etf_current`：`security_master` 中 `instrument_class=2` 且来自当前已冻结 ETF 名单的成员。
- `sh_sz_etf_listed_ever`：首期等于当前 ETF 身份集；PR-B 写入 ETF 日线后并入已有 ETF 序列。
- 两个新值**只**由主表解析入口接受：`market.daily` 的 `selection.kind=universe` 与 v2
  `start_collection` 的 `selection.kind=scope`。`securities.catalog`（读与写）拒绝它们，
  错误信息指向 `current_sh_sz_a`。
- `market.daily` 不填 `selection.scope` 时仍解析目录版本（A 股），成员集与 v9 相同。
- PR-A 的 `start_collection` 仍只实现 `sh_sz_a_listed_ever`；ETF scope 的采集在 PR-B 落地，
  当前返回显式错误而不是静默降级。

### 1.2 来源与解析器

ETF 身份来源在来源库注册，不新增 MCP 工具：

| `source_id` | 请求 | 用途 | parser |
|---|---|---|---|
| `sse_etf_list` | SSE `commonSoaQuery.do?sqlId=FUND_LIST` | 沪市 ETF 身份 + `listingDate` + `subClass` + `INDEX_CODE` | `etf-list-v1` |
| `szse_etf_list` | SZSE `ShowReport?SHOWTYPE=xlsx&CATALOGID=1945` | 深市 ETF 身份（上市日取 `base.dbf SSDATE`） | `etf-list-v1` |

`ebs` 只是报价锚，不是身份来源（D-70 Q1）。两个来源的冻结字节、`exchange_source_snapshots`
索引行与 `replay --scope exchange` 语义沿用 M3 §6/§9.4；`parser_version` 在快照清单里仍是
envelope 解析器（`exchange-sse-page-v2` / `exchange-szse-xlsx-v1`），业务解析器版本
`etf-list-v1` 记录在 `derived:security_master` 的 item evidence。

### 1.3 写入语义

- `security_master` 增加 `instrument_class=2` 行；`listing_source` 沪市 `exchange_list`、
  深市 `tdx_ssdate`；`delisting_date=NULL`（名单移除不等于退市）。
- `effective_from`/`known_at` 是该成员第一次出现在已冻结名单快照的获取日。
- A 股 `security_master` 行内容不变；`market_series`、A 股导出与 A 股 `query_data` 输出不变。

## 2. 未验证 / 部署前待办

- `tools/list` 字节数增量：新 scope 两个字符串出现在 `market.daily` 的 scope 枚举上，
  本机未编译/未实测；部署前按 v9 的同一测量入口记录，超过 30,800 B 时按已批准上限规则申请。
- 本契约只随部署事实翻转时更新 [STATUS](../STATUS.md) 与严格 Skill 的 `capability_revision`。
