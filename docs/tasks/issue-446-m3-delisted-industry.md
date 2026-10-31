# M3 详细设计：退市股行情、历史行业与交易所来源入口（#446）

- 任务：设计任务（#446 的前置设计），角色 agent。**只出设计**：不改代码、不改 schema、不改 MCP 契约、不部署、不合并。
- 关联 Issue：[#446](https://github.com/liqiangcc/x-data-mcp/issues/446)（本设计对应）、#1（产品范围）、#246（收录范围）、#433（M2 后续：A-1/A-2 适配器）、#396/#398（A-1/A-2 准入）、#308（历史 ST，补充裁决）、#387（v8）。
- 输入报告（仓库外，随任务交付）：本任务 `work/evidence-notes.md` 与 `work/evidence_manifest.json`（本地只读实测 L-1…L-3、外部抽样 E-1…E-4、sha256）。抽样**不是**新的来源准入证据包。
- 依据：`docs/decisions.md` D-25（四类官方披露逐个准入）、D-26（历史行业逐代积累）、D-28（ETF 边界）、D-30（严谨回测级）、D-44（A-7 已准入）、D-45/D-46（A-1/A-2 字段裁决）；[docs/data-catalog.md](../data-catalog.md) §3.1/§3.2；M2 设计 [issue-391](issue-391-m2-security-master.md)（写法与既有结论）。
- 基线：main `32e81ce`（schema 24；M2 数据集已部署 preview.37/38）；生产库 2026-10-28 只读复核（§2.1）。
- 补充裁决（owner 2026-10-28）：历史 ST、停复牌、简称历史的分档与边界见 #446 正文「补充裁决」；本设计把这些来源与 A-1/A-2 收进**同一套交易所来源入口**（§6）。
- 本设计经 Claude 审核后，才按 §14 拆实现任务；审核前不写实现代码。

---

## 0. 结论摘要

| # | 设计点 | 结论 | 证据级 |
|---|---|---|---|
| 1 | 退市股成员集 | 目录只描述「今天」，成员集来自**证券主表 ∩ 官方 `hsjday` 成员**；`securities.catalog` 仍是当前快照。`CatalogScope` 新增 `sh_sz_a_listed_ever`，只允许解析自主表的入口，catalog 数据集拒绝它 | 实测：不在当前目录的 A 股 **338** 个（2026-10-15 快照），1,251,129 根 bar；338 个全部在 `base.dbf`，276 个已在主表 |
| 2 | 退市股行情 | 首次采集用 `start_collection` 的 `selection.kind=scope`（std::按主表解析，338 项）一次性建序列；日更**不改**已有退市序列，只做不消失检查 | 设计；首次采集耗时按 M2 实测外推 ≈22s（估算） |
| 3 | `universe(as_of)` | 沿用 M2 定义（上市日 ≤ T < 退市日 ∧ 区间 ∧ 未被 ≤T 的迁移取代）；退市日 `unknown` 时按「未退市」包含并带 `delisting_unknown`；`has_series` 与成员资格分离 | 已实现（`src/research/universe.rs:1-30`）；M3 只补数据与 canonical/alias |
| 4 | 代码变更 | `sz.300114→sz.302132` 2025-02-17 **confirmed**：旧代码区间在迁移日结束，股票池只计一次；旧代码不建重复序列（其 3,421 根 bar 与 302132 的前缀仅 1 根不同，属源重复）；`sh.600849→sh.601607` 只有公告索引，保持 **candidate**，不用于去重 | 实测（D-46；本任务 bar 比对） |
| 5 | 历史行业 | 每代 `tdxhy.cfg` 落**快照 + 区间变更集**；`effective_from` 取发布方 `last_modified`（缺失取获取时间），下一快照的起点即上一区间终点；早于最早快照（2026-09-30）返回 `unknown`，不用当前值冒充 | 7 个不同 `tdxhy.cfg`（9 个 `base.zip`）实测；窗口内只发生成员增删、无重分类 |
| 6 | 归档回填 | 分三档：A（manifest + publisher `last_modified`）可定区间；B（只有内容 sha 与获取时间）可定区间但标 `acquisition_time`；C（仓库内 2026-09-16 Windows 客户端文件）**只作交叉核对**，除非 owner 另批否则不产生区间；更早的日期 `unknown` | 实测：7 份归档 cfg；仓库文件与最早归档差 8 行增、2 行重分类（`sh.601777`） |
| 7 | 行业字典 | 行业代码→名称来自 `gbbq.zip!zhb.zip!incon.dat`，同样随 generation 退役丢失；行业快照必须同时冻结字典（`zhb` 内的 `incon.dat` + sha256）；回填期无同代字典时名称记 `unknown`，代码与区间仍可用 | 代码事实（`src/catalog_tdx/source.rs:124-144`） |
| 8 | 交易所来源入口 | 五类交易所来源（A-1/A-2/历史 ST/停复牌/简称）共用一条链路：获取 → sha256 冻结 → 计数校验 → 逐代保留 → parser 版本 → 失败语义；使用方只写解析器与写入器 | 设计；owner 2026-10-28 补充裁决 |
| 9 | 沪公告索引 | 归档起点 2000-01，查询必须按 **≤1 季度**分段；每段校验返回计数，0 行是 `not_observed` 而不是「无事件」；分页用 `beginPage/endPage`（`pageNo` 会重复第一页） | owner 裁决；本任务抽样 1/5 季度都返回 `data:[]`，证明「0 行无意义」 |
| 10 | 裁剪前提 | generation 退役前必须已登记覆盖 `base.zip`/`gbbq.zip`/`hsjday.zip` 的派生事实（行业快照、行业字典、资本/因子事件、日线）；缺失则拒绝退役并给清单；当前「保留最新两个被取代代际」的宽限期不变 | 设计；现状 `ARTIFACTS` 3 个、`retire_bytes`（`src/generation/mod.rs:34,563`） |
| 11 | 存储 | **schema 25**：行业快照/行业区间、交易所来源快照、generation 派生事实登记、退役豁免；沿用 022/023 的表不重建；不可再生原始字节持久在库外**来源库**（§9.4），DB 表只是它的索引 | 设计 |
| 12 | 契约 | 不改已冻结行为：新增 v2-only `market.industry_assignments`、`CatalogScope` 新值与 `selection.kind=scope`、导出 `research-snapshot.v3`；`x` 下游改用主表/股票池取成员 | 设计 |
| 13 | 可行性 | 退市股行情、股票池、行业区间、来源入口、裁剪门禁均可达；历史行业**早于 2026-09-30 不可达**（标 `unknown`）；北交所与沪 2000 前 ST **NOT_AVAILABLE**；偏差逐条列在 §11 | 实测 + 设计 |
| 14 | fresh-init 持久性 | 生产部署是「归档旧库 + 全新 init」，只能从 active generation 再生数据；因此 M3 的不可再生事实（历史 `tdxhy.cfg`/`incon.dat`、交易所来源原始响应、回填归档）放进**系统自有的库外内容寻址来源库 + 只追加清单**，DB 表全部可重建；fresh-init 后由 maint 命令重放；退役门禁以「按 sha 读回原始字节」为准 | 设计（本 PR 补缺口） |

**四个最容易被做错的点**（本设计专门守住）：

1. **「不在目录」不等于「已退市」**。目录是当前快照；把目录当成员集会永久丢掉 338 个退市代码，把主表当「当前名单」又会把已退市证券当成仍在册。两者职责分开：目录回答「今天叫什么、属于什么行业」，主表 + 股票池回答「某天在不在册」。
2. **缺快照不是「没有变化」**。行业只有观测到的快照；两个快照之间的变更日不可知，早于最早快照只能 `unknown`。用当前行业回填历史，或用「0 行」证明无变更，都是伪造历史。
3. **0 行不是证据**。交易所公告索引在区间过长时会静默返回 `total=0`；来源入口必须分段并校验计数，否则会把「查询失败」写成「没有停牌/没有 ST」。
4. **归档不是系统的一部分**。owner 私有归档只能当一次性导入源；派生事实与「原文可从归档重解析」都不构成持久性——fresh-init 部署会清空 DB，只有系统自有的来源库能按 sha 读回不可再生字节（§9.4）。

---

## 1. 范围

### 1.1 本次包含

1. 退市股行情：成员集、`CatalogScope` 扩展、与主表/上市退市日的关系、首次采集、日更与跨代不消失检查、对 `x`/v8 的影响。
2. 历史行业：快照/区间模型、生效日取法与偏差、证据等级与归档回填、早于起点 `unknown`、读面（数据集名/wire 形状/`as_of`/`known_at`）、行业字典。
3. 证监会行业分类候选：只写评估结论与依赖，不在本设计内接入。
4. 交易所来源共用入口：A-1/A-2、历史 ST（#308）、停复牌（A-3）、简称历史（S-6）的获取、冻结、保留、版本、与 generation 的关系、失败语义；沪公告索引的 ≤1 季度分段与计数校验。
5. generation 保留与裁剪：裁剪前必须落哪些派生事实、退役门禁、磁盘估算。
6. 存储（schema 25）、派生、日更维护、导出列、契约影响、可行性逐项偏差。
7. fresh-init 下不可再生事实的持久性：系统自有的库外来源库、只追加清单、重放路径、退役门禁依据、一次性导入、保留与回滚（§9.4）。
8. 分离点落位、6 个扩展场景走查、实现拆分（含 ST/A-3/S-6 各一个写入器 PR）与 owner 裁决项。

### 1.2 本次不包含

实现代码；ETF 收录（D-28，另拆 M3-2）；北交所/B 股；分钟线、财务、公告全文；历史 ST、停复牌、简称的**字段细节**（归各自 Issue/PR，本设计只定共用入口与写入器边界）；证监会行业分类接入；规则书实现（A-7 已由 D-44 准入，另有任务）；回测引擎；下游 `x` 仓库改动；部署与发布；完整研究快照规格（本 Issue AC-M3-8；数据目录原 AC-M3-4，建议另拆）。

### 1.3 与既有裁决的关系

| 裁决 | 本设计如何吸收 |
|---|---|
| D-25（四类官方披露逐个准入） | §6 的每个来源带 `basis_status`；未达门槛的窗口写 `unknown`/`not_observed`，不写「无事件」 |
| D-26（历史行业逐代积累） | §5 全节；回填只按证据等级进行，早于最早快照一律 `unknown` |
| D-30（严谨回测级；两项首期偏差） | §11 偏差清单；历史行业与沪 2000 前 ST 如实写 `unknown` |
| D-44（A-7 已准入） | 规则表不归 M3；§7 只引用它对可交易性的依赖 |
| D-45/D-46（A-1/A-2） | §7 合并规则、冲突记录、剩余 `unknown`；`600849` 仍是 candidate，不用于去重 |
| D-28（ETF） | §13 场景 1 的边界；不在本设计内实现 |
| M2 设计的 schema 23/24 与读模型 | §9 只追加 025，不重建 022/023；`universe`/`security_master` 语义不变，只补数据 |

## 2. 现状事实（2026-10-28 复核）

### 2.1 只读实测

| 项 | 实测值 | 核对方式 |
|---|---|---|
| 生产库 schema | 24；`catalog_members` 5,226；`security_master` 5,503（`delisting_date` 非空 153，全部 `tdx_arrangement`） | 只读 URI（含 WAL） |
| `hsjday.zip`（2026-10-15） | 12,463 个 `.day`；A 股前缀 5,561；**不在目录 338**；末根 bar ≥2015 的 254；bar 合计 **1,251,129** | 成员名 + 逐成员读首/末 bar |
| 338 个不在目录的代码 | 276 个已在主表（152 个有退市日）；62 个不在；最新 `tdxhy.cfg` 里 0 个 | 与 DB、cfg 求差 |
| `base.dbf`（最新 `base.zip`） | 8,906 条 / 40 字段；338 个代码全部在表内；不在主表的 62 个 = `SSDATE='0'` 的 62 行 | DBF 头 + 逐记录 |
| 行业归档 | 26 条索引、9 个不同 `base.zip`、**7 个不同 `tdxhy.cfg`**；`last_modified` 09-30…10-28；`manifest_match=true` | 归档索引 + 逐 cfg 解析 |
| 行业变更 | 6 个相邻变更集：新增 5 行、删除 6 行；共同代码的 `T`/`X` 重分类 **0 行** | 同上 |
| 仓库内 2026-09-16 `tdxhy.cfg` | 与最早归档差 8 行增、0 删、**2 行重分类**（`sh.601777` `X260501→X260102`；`920229` 不在 S 内） | 同上 |
| `sz.300114` / `sz.302132` | 300114 bar 2010-08-27…2025-02-14（3,421 根）；302132 从 2010-08-27 起（3,834 根）；两者重叠 3,421 根中仅 **1 根**不同（2015-09-10） | `hsjday` 逐 bar 比对；DB `market_series` |

### 2.2 代码事实（`file:line`）

| 事实 | 位置 |
|---|---|
| `CatalogScope` 只有 `current_sh_sz_a` | `src/data_supply/wire.rs:366-369` |
| `WriteSelection` 只有 `explicit`/`catalog`/`default`；explicit 预算 1..100 | `src/data_supply/wire.rs:1043-1055`、`src/data_supply/collect.rs:96` |
| `Selection::Universe` 接受 `catalog_version`；M2 数据集配合 `as_of`/`known_at` | `src/data_supply/wire.rs:355-363`、`src/data_supply/query.rs:198-209` |
| `market.daily` 的 `universe_members` **只读当前目录**，忽略 `as_of` | `src/sqlite_supply.rs:342-344` |
| 日更 eligible 集 = 当前目录 ∩ ever-started；无 head → `absent` | `src/data_supply/default_selection.rs:138-177`、`src/data_supply/plan.rs:559-590` |
| 读路径只校验实体格式与访问权，不校验目录成员 | `src/research/universe.rs:22-30` 与 M2 设计 §2.2 |
| `exchange_source(None)` 固定 `pending_source`，无适配器 | `src/security_master/assembly.rs:131-145`、`src/data_supply/derived.rs:300` |
| `base.zip!{tdxhy.cfg,infoharbor_ex.code,base.dbf}`、`gbbq.zip!zhb.zip!incon.dat` | `src/catalog_tdx/source.rs:1-9,124-144` |
| 行业二级解析：`X` 码 → `#TDXRSHY` 名称 | `src/catalog_tdx/compose.rs:87-135`、`src/catalog_tdx/incon.rs:119-145` |
| generation 退役删除 3 个原始文件 | `src/generation/mod.rs:34,557-578` |
| 自动保留最新 2 个被取代代际 | `src/acquire/service.rs:41-43,1064-1094` |
| schema 24；迁移数组固定长度 | `src/sqlite_tasks.rs:45-50`、`scripts/check_current_facts.py:15` |
| 导出 `research-snapshot.v2`；7 个 M2 文件 | `src/research/manifest.rs:42`、`src/research/mod.rs:610-622` |

### 2.3 外部抽样核对（只用于支撑设计判断，不是准入证据）

完整记录见 `work/evidence-notes.md`（E-1…E-4）；与本设计直接相关的三条：

- **E-1/E-2**：上交所上市名单 `STOCK_TYPE=1` 返回 1,845 行（不分状态），`COMPANY_STATUS=3` 返回 145 行终止上市（含 `600001` 2009-12-29）；与 M2 设计一致。`pageHelp.pageSize` 实测被忽略，一次返回全部；分页必须用 `beginPage/endPage`。
- **E-3**：深交所 `CATALOGID=1110` 当前在册 **2,906 行**（XLSX，545,223 B）；与 M2 的 2,906 一致。
- **E-4**：深交所终止上市 `1793_ssgs` 本次连接重置后重试 HTTP 500，未取得新样本；沿用 M2 的 211 行去重/188 个 A 股的结论，并列入未验证。
- **沪公告索引**：用通用参数抽样时 1 季度与 5 季度窗口都返回 `data:[]`，无法区分「参数不对」与「确实 0 行」；这正是 owner 要求「0 行不是证据」的现实依据。分段规则本身来自 owner 裁决，本任务未独立复现。

---

## 3. 设计总览

### 3.1 分层与数据流

```text
原始证据层   generation 字节（base.zip / gbbq.zip / hsjday.zip，绑 sha256）
             库外来源库（内容寻址 blobs + 只追加清单）：行业成员/字典抽取物 + 全部交易所原始响应字节
             交易所来源快照（沪深名单/公告索引/简称变更/停复牌，按 sha256 冻结，跨代保留）
                 │  每个来源一个 Adapter（分离点 1）；DB 只是来源库的索引（§9.4）
                 ▼
解析事实层   日线(M1)  因子/股本事件(M1/M2)  上市/退市/迁移  风险警示区间  停牌区间  简称区间
             行业快照 + 行业区间  行业字典(code→name)
                 │  纯函数（分离点 2，带 algorithm_version / parser_version）
                 ▼
派生层       T 日股票池  可交易性  涨跌停  市值/换手  交易日历  前复权
             退市股首次采集清单  跨代不消失检查
                 │
                 ▼
研究读模型   src/research/：universe / industry / member_state / tradability / ...
                 │  三个入口适配（分离点 5）
                 ├── MCP v8（dataset 分支 + as_of/known_at）
                 ├── 导出 bin（Parquet + 清单）
                 └── 运维（maint：来源库导入/重放、退役门禁、回填）
```

### 3.2 数据集与层

| 数据集 | 层 | 存储 | 版本身份 | 入口 |
|---|---|---|---|---|
| `market.security_master` | 解析事实（来源聚合） | `security_master` | 内容摘要 | v8 / 导出 / 运维 |
| `market.industry_assignments`（新） | 解析事实 | `industry_assignments` | 快照 sha 链 + 内容摘要 | v8（v2）/ 导出 / 运维 |
| `market.risk_warnings` / `market.suspensions` / `market.security_names` | 解析事实（已有） | 021/023 表 | 内容摘要 | v8 / 导出 |
| `market.daily` | 价格序列 | `market_series` | 已有 | v8 / 导出 |
| T 日股票池 | 派生 | **不存**（区间视图） | 主表 + 迁移表 | v8 `selection.kind=universe` |
| 首次采集清单、跨代不消失检查 | 派生 | **不存** | `algorithm_version` | 日更 plan / 运维 |

规则沿用 M2：**派生不进 `version_id`**，只带 `algorithm_version` 与输入版本；交易所快照是**来源事实**，进 `source_bindings`。

## 4. 退市股行情

### 4.1 成员集与 `CatalogScope`

两类问题必须分开：

| 问题 | 数据源 | 语义 |
|---|---|---|
| 今天叫什么、属于什么行业 | `securities.catalog`（现有 scope=`current_sh_sz_a`） | 当前快照；不因退市股而改变 |
| 某天在不在册、有没有序列 | `security_master` + `market_series` + 官方 `hsjday` 成员 | 时点成员集；退市股在里面 |

设计：`CatalogScope` 新增枚举值 `sh_sz_a_listed_ever`（serde：`"sh_sz_a_listed_ever"`，v2-only）。它不是一个「更宽的目录」，而是**解析自主表的成员 scope**：

- 只允许出现在 `market.daily` 的选择与写入路径（`selection.kind=scope`，见 §9.3）。
- 私 `securities.catalog` 仍只接受 `current_sh_sz_a`；把新值传给 catalog 直接 `invalid`（fail-closed），不做降级。
- 解析规则：`security_master` 中 `instrument_class=0` 且 `exchange ∈ {sh,sz}` 的行，交集为当前 active generation 的 `hsjday.zip` 成员；不读 `catalog_members`。
- 数量：2026-10-28 的并集是 `5,226（目录）+ 338（不在目录）= 5,564`。主表当前 5,503 行 = 5,226 + 277 个目录外代码；A-1/A-2 补齐 62 个 `SSDATE='0'` 代码后预期 **5,565 行**（比并集多 1 行，属主表已有而 338 枚举未覆盖的代码，需逐条解释）。

`securities.catalog` 不收录退市股的理由：目录的字段是**当前值**（名称、行业、来源），退市股没有当前值；把退市股写进目录会让「今天在册」的语义失效，也会让下游把历史成员当成当前成员。

### 4.2 与主表/上市退市日的关系

- 身份仍由 `security_master` 的 `(exchange, code, effective_from)` 提供；退市股不进目录，但**必须在主表**有行。
- 当前主表 5,503 行里有 276 个不在目录的代码；剩下的 62 个因为 `base.dbf` 的 `SSDATE='0'` 被 TDX 分支跳过，等 A-1/A-2 的交易所上市日补齐后进入主表（§7）。
- `market.daily` 的实体不必在主表有行才能被读（读路径不校验目录成员），但**没有主表行就不能进股票池**，也无法解释交叉/退市日。所以顺序是：先补主表，再建序列。
- 退市日来源优先级：交易所终止上市名单 > `infoharbor_spec.cfg` 的整理期 end > `unknown`（M2 §4.3 不变）；M3 只新增交易所分支。

### 4.3 `universe(as_of)` 语义

定义沿用 M2 设计 §11.1，M3 不改公式：

```text
universe(as_of = T) = { row ∈ security_master
  : instrument_class = 0
  ∧ effective_from ≤ T ∧ (effective_to IS NULL ∨ T < effective_to)
  ∧ (listing_date IS NULL ∨ listing_date ≤ T)
  ∧ (delisting_date IS NULL ∨ T < delisting_date)
  ∧ 未被 code_migrations 在 ≤ T 的时点取代 }
```

M3 补充三条：

1. **`has_series` 不参与成员资格**。退市股首次采集前 `has_series=false` 是合法状态；采集后变 `true`。
2. **退市日 `unknown` 不等于已退市**：只要 `T ≥ 首根 bar` 就包含，带 `delisting_unknown`；`strict=true` 可剔除。
3. **代码变更的 canonical/alias**：confirmed 迁移在 `effective_from` 前只返回旧代码、之后只返回新代码；成员行新增（v2 附加字段）`canonical_entity` 与 `aliases[]`。`candidate` 迁移只打 `candidate_migration_overlap` 标记，不改变成员身份（`sh.600849` 仍在池内，退市日 `unknown`）。

AC-M3-1 的可执行形式：`market.security_master` + `selection.kind=universe` + `filter.as_of=2015-01-05`，逐条核对「当年在册」与「2015 年以来退市且有 bar」的并集；不在目录不是排除理由。

### 4.4 首次采集

退市股没有日线 head，现有日更 `plan` 永远不会把它们列为 eligible（`default_selection` 只取目录成员）。M3 新增一条**显式**采集路径：

```text
start_collection{dataset: market.daily, contract_version: v2,
  selection: {kind: "scope", scope: "sh_sz_a_listed_ever"}}
```

- 解析成实体列表：`universe(as_of=today)` 中 `has_series=false` ∧ active generation 的 `hsjday` 有成员；2026-10-28 为 338 项（个别无 bar 的成员跳过并列入诊断）。
- 预算是 `1..500`（而不是 explicit 的 1..100）：一个任务 338 项，item 级发布/验证语义不变。列表是冻结的（同 `catalog` 选择），任务证据记录解析时的主表 `version_id` 与 generation。
- 已存在 head 的实体不走这条路径（幂等：重复提交只采缺失的）。

### 4.5 日更维护与跨代不消失

退市股序列是**终态序列**（末根 bar 不再前进），日更不改它，但有两件事必须做：

1. **新退市成员的尾巴不能丢**：股票在某一代从目录消失后，当日 plan 的 eligible 集会把它排除，可能丢掉退市前最后几根 bar。设计：日更 eligible 集从「当前目录」扩为「当前目录 ∪ 上一版目录中有序列的成员」，只对 `last_day < target_day` 的实体继续 `update_data`；数量有界（每年几十个）。这是 M3 的行为变更，必须用基线用例证明不会把活跃股错当退市股。
2. **不消失检查**（每次 plan-run 的一个内部 item）：对 `universe(as_of=today)` 中 `has_series=true` 且 `delisting_date ≤ today`（或 `last_day` 早于窗口下限）的实体，校验 `record_count`/`last_day`/`content_digest` 与上一代登记一致；不一致或缺失 → item 失败并列出实体，**不静默重建、不静默删除**。历史被源改写时走 `republish_data`，不走静默覆盖。

日更失败语义沿用 M1/M2：内部 item 失败 → `partially_failed`；不阻塞已提交的其它 item；退市股采集是显式任务，不计入日更 `plan`。

#### 4.5.1 PR-5 实现落位（2026-10-30，XDM-113）

- **eligible 集**：`resolve_default_daily` 的成员集改为「当前目录头 ∪ 上一版目录成员」
  （`src/data_supply/default_selection.rs`；上一版 = `catalog_versions` 里 head 插入顺序的前一条，
  成员读 `catalog_members` 投影表，仍然逐条校验 head 版本本身）。与「有首次采集
  execution-start 标记」取交集不变：没采过的成员不会因为出现在上一版就变成日更目标。
  选择证据新增 `previous_catalog_version_id` 与 `retained_members`（**只**由上一版保留的
  实体，正常为空），两者都进 evidence digest，所以宽集/窄集的证据不会互相重放。真正决定
  item 与否仍是 §4.5 的 `last_day < target_day`（`plan.rs` 的 `already_current` 判定），
  活跃股的产出与改动前逐位相同。
- **一个必须说清的边界**：目录发布仍受 AC-08 `MemberDisappeared` 约束——真实退市导致的
  成员缺席会暂停发布、等核实放行。因此现实数据里「当前成员 ⊆ 上一版成员」，union 今天
  不改变任何产出；它是"核实放行后成员真的消失"时的兜底。夹具因此直接写入一条少一个成员
  的目录版本行（`publish_fixture` 无法产生该状态），读回走与解析器同一个完整性校验。
- **不消失检查**：`derived:delisted_series_check`（`src/data_supply/delisted_series.rs`；
  item 装配 `src/data_supply/derived.rs`，登记解析在 `src/data_supply/worker.rs` 的
  plan-run 分支）。只读，不写业务表：
  - worker 在**实体 item 之前**解析登记：主表里 `delisting_date <= today`、非
    `issuer_resolution='ambiguous'`、且有**不复权**日线序列的实体，记
    `record_count`/`first_day`/`last_day`/`content_digest`(`version_id`)/`last_event_seq`
    /`delisting_day`，冻结进 item 的 `frozen_inputs`。同一任务恢复执行时读**存下来的**
    baseline（不是重新解析），所以本次 run 早先提交的改动不会被当成"上一代"而漏检。
  - item 最后执行（在所有写者之后），逐个比对当前事实。登记值本身越过退市日
    （`last_day > delisting_day`）先判 `last_day_beyond_delisting`：这是不可能的
    状态，即使本次一个字节都没变也不能算 `unchanged`。其余：完全一致 → `unchanged`；
    **唯一允许的变化**是尾巴补采——`first_day` 不变、`last_day` / `record_count` 增加、
    `last_day ≤ delisting_day`，且登记以来的事件全是 `append`（append 路径已证明新 bar
    是旧条目的前缀）→ `tail_extended`，item 成功。
  - 其余一律失败并列出实体：`missing_series`、`first_day_changed`、`history_shrunk`、
    `content_digest_changed`（同区间换内容）、`last_day_beyond_delisting`、
    `transition_unrecorded`（事实变了但没有事件）、`history_rewritten`（事件不是 append）。
    失败 → item `success=false` + evidence 里登记值/观测值，run `partially_failed`，
    **不重建、不删除**；被源改写的历史走 `republish_data`。
  - 不猜终态：退市日未知（`delisting_date=NULL`）或代码被两个发行主体复用的实体不登记，
    理由写进 evidence 的 `skipped`（`no_series` / `issuer_ambiguous`）。§4.5 的
    「`last_day` 早于窗口下限」不另设谓词：非退市但停更的实体由它自己的 item 与缺口
    证据暴露，用"末根不动"反推退市正是 §4.5 禁止的「把活跃股错当退市股」。
  - 首次登记发生在写主表那一代**之后**的一代（第一代 plan-run 自己写主表），与 AC-M3-6
    「连续推进两代 generation」一致；legacy schema 18 无主表/序列事实，item 不加入集合。
  - **升级注意**：`ensure_derived_items` 要求已存在的派生 item 集合与请求集合完全一致，
    所以升级前受理、尚未跑完的 plan-run 任务在恢复时会以
    `derived item set is partially present` 失败（fail-closed，不改写旧任务）。日更窗口
    内升级宜先让在飞任务结束。
- **代价（SLO）**：每代两次只读——登记时一次序列摘要全表 scan（`market_series` 按
  `(entity, period, adjustment)` 索引，行数 = 实体数）+ 检查时逐实体按主键查一次；
  上限是终态实体数（§8.4 估算 <50ms/代，与 `docs/SLO.md` 的 <600s 门禁无冲突）。
  真实规模待首次生产验证：hsjday 的 335 个目录外代码里目前只有 152 个有退市日。
- **夹具**：`tests/mcp/plan_run.rs::a_withdrawn_member_keeps_its_terminal_series_in_the_daily_plan`
  （成员移出目录后仍在 eligible、活跃股不变、check 登记并 `unchanged`、末根=退市日的 3 根
  终态 bar 不动）、`::a_terminal_series_that_grows_past_the_delisting_day_fails_the_check_without_a_rewrite`
  （源在退市日之后又给了一根 bar：第一代写主表、第二代登记即判
  `last_day_beyond_delisting` → `partially_failed` + 实体清单，已落库的序列不被检查
  重写；其余失败原因在单代夹具里无法自然发生，改冻结
  输入构造，由 `delisted_series/tests.rs` 的判定表覆盖）、
  `src/data_supply/delisted_series/tests.rs`（判定表，含 `tail_extended` 的 append 允许
  分支与各失败原因）、`src/data_supply/default_selection/tests.rs`（union、保留成员证据、
  「没采过就不选」）。尾巴**补采**的端到端链路与其它跨代 envelope 路径一样，需要两代
  source；本 PR 的夹具只覆盖同一代内的登记/比对/失败，跨代补采由 AC-M3-6 的两代演练验收。

### 4.6 对 `x` 下游与 v8 的影响

- `x` 的 Security Master / `KlineReader` 目前按目录或自建名单枚举；M3 后**必须**从 `market.security_master` 的 `universe` 取成员（含 `has_series`），否则跨年回测仍缺退市股。这是下游改动，不在本仓库实现。
- v8：`market.daily` 的实体集合变大，但行形状、版本语义不变；`selection.kind=universe` 已存在，新增 `scope` 是 v2 附加字段。`securities.catalog` 不变，因此不能把「目录成员数」当作「可回测成员数」。
- v8 冻结行为不变：v1 请求仍拒绝 `universe`/`scope`；`market.industry_assignments` 是 v2-only 新数据集，`get_data_capabilities` 增加一行。

## 5. 历史行业变更集

### 5.1 模型

两层：**快照**（不可变的原始事实）与**区间**（可查询的派生事实）。

```text
industry_snapshot（一代 tdxhy.cfg 一份）
  ├─ member_sha256            tdxhy.cfg 内容 sha256（主键语义）
  ├─ base_zip_sha256          承载它的 base.zip
  ├─ generation_manifest_sha256  若来自 generation；归档回填为 NULL
  ├─ source_kind              generation | archive | repo_evidence
  ├─ publisher_last_modified  官方文件时间（可为空）
  ├─ observed_at              获取完成时间（可为空）
  ├─ ingested_at              本系统第一次拿到该内容的时间
  ├─ evidence_grade           A | B | C
  └─ row_count / scope_member_count / dictionary_sha256

industry_assignments（区间，读面）
  entity(exchange, code), scheme='tdx_research_industry', level=2,
  x_code, t_code, name?, name_status, effective_from, effective_to?, known_at,
  effective_from_basis(publisher_last_modified|acquisition_time|evidence_commit_date),
  snapshot_id → industry_snapshot, evidence_grade
```

- **区间由相邻快照计算，不存快照全量行**（全量行只用于重算与审计：只存 `member_sha256` + 统计；如需复查，按 `member_sha256` 从来源库 blob 重解析，§9.4）。
- 同一代码出现在相邻快照且 `(t_code, x_code)` 不变 → 不产生新行，只延长区间；变化 → 旧区间 `effective_to = 新起点`，新行开启。
- 从某快照消失的代码 → 旧区间在**该快照起点**结束（我们只能证明「从这次观测起不在」，不能证明具体日期）。
- 区间不重叠、不空洞（在观测窗口内）；同一 `(entity, effective_from)` 唯一，冲突 fail-closed。
- 一个快照 5,666–5,669 行；区间表初始约 5,586 行（scope S 内）、每年增量估计几百行。

### 5.2 生效日取法与偏差

官方 `tdxhy.cfg` **没有生效日**，`base.zip`/HTTP 只有 `last_modified`。规则（已裁决，owner 2026-10-28，Q1 按默认）：

| 情形 | `effective_from` | `basis` | 偏差 |
|---|---|---|---|
| 有发布方 `last_modified`（日更代） | 该时间（UTC 日期，取日） | `publisher_last_modified` | 变更实际发生时间 ≤ 该值；最多晚一个发布周期 |
| 无 `last_modified`、有获取时间 | 获取完成日 | `acquisition_time` | 最多晚一个获取周期；方向总是「偏晚」 |
| 仓库证据（仅 C 档，owner 另批） | 提交日 2026-09-16 | `evidence_commit_date` | 可能早于真实生效日（无官方时间） |
| 两快照同一天 | 按 `ingested_at` 排序，后到者起点 = 同日；前区间收缩为同日结束（零长度不写），冲突入诊断 | — | 同日多变更不可分辨 |

偏差方向固定为「**不提前**」：宁可让历史多一段时间用旧行业，也不用未来行业冒充过去。这条写进数据目录 §3.3。`known_at`：前向运行 = 获取完成时间（或首次入库时间，取较晚者）；回填 = 本次回填入库时间（2026-10-28），**不是**历史获取时间——否则等于声称当时已用上该数据。

### 5.3 证据等级与归档回填

| 等级 | 材料 | 能否定区间 | 限制 |
|---|---|---|---|
| **A** | `base.zip` + `manifest_match=true` + `tdxhy.cfg` sha256 + 发布方 `last_modified` + 获取时间 | 能 | 区间起点 = `last_modified`；`known_at` = 回填入库时间 |
| **B** | 内容寻址的 `base.zip`/`tdxhy.cfg` + sha256 + 获取时间，但无 `last_modified`/manifest | 能（标 B） | 起点 = 获取时间；偏差更大，读面必须能看到等级 |
| **C** | 有内容的 `tdxhy.cfg`，但无官方 URL/manifest/时间（如仓库内 2026-09-16 Windows 客户端文件） | 默认**不能** | 只作交叉核对；如 owner 另批，可按提交日定区间并标 `evidence_commit_date` |
| **D** | 无 sha256 或无法验证内容 | 不能 | 不进入任何表 |

回填流程（一次性 maint 命令，不是日更；输入是**来源库**，不是 owner 私有目录）：

0. 先按 §9.4.4 把归档 A/B 档导入来源库；回填/重放只从来源库读（幂等）。
1. 读来源库清单，按 `tdxhy.cfg` sha256 去重；同一内容多条记录取**最早的非空 `last_modified`**与最早 `observed_at`。
2. 校验每个 blob 的 sha256 与成员名（读回不一致即 `source_store_corrupt`）；校验 `tdxhy.cfg` 能被清单登记的 `parser_version` 完整解析（fail-closed）。
3. 按 §5.2 定起点，生成快照 + 区间；证据等级写入每行。
4. 仓库内 2026-09-16 文件默认只写入 `industry_snapshot(source_kind='repo_evidence', evidence_grade='C')` 供核对，不生成区间。
5. 回填幂等：同一 `member_sha256` 重复执行不新增行。

本任务实测的 7 份快照可回填为 6 个变更边界（09-30 → 10-12 → 10-13 → 10-16 → 10-19 → 10-24 → 10-28）；10-13/10-14/10-15 是同一份 cfg，只算一个快照。更早日期（2026-09-30 之前）全部 `unknown`。

### 5.4 早于积累起点

- 全局 `coverage_start = 2026-09-30`（最早 A 档快照的 `last_modified`）。
- `as_of < coverage_start` → 读模型返回 `unknown{reason=before_coverage}`，**不返回目录当前行业**，也不返回空行让调用方误读成「无行业」。
- 若 owner 批准 C 档回填，`coverage_start` 可提前到 2026-09-16，但仍早于该日 `unknown`。
- 代码在某快照缺失但 `as_of` 在区间内 → `unknown{reason=missing_in_snapshot}`；不在快照之间做插值。

### 5.5 读面

**数据集**：`market.industry_assignments`（v2-only）。

| 字段 | 语义 |
|---|---|
| `entity` / `exchange` / `code` | 主表身份（与 `market.daily` 同一实体格式） |
| `scheme` | 固定 `tdx_research_industry`（与目录一致，不混其他口径） |
| `level` / `code` | 二级研究行业代码（与目录 `IndustryInfo` 一致） |
| `name` / `name_status` | 名称；字典缺失时 `null` + `unknown_dictionary` |
| `effective_from` / `effective_to` | 区间；最后一段 `effective_to=null`（仍有效，不是永久） |
| `known_at` | 知晓时间；回填行 = 回填入库日 |
| `effective_from_basis` / `evidence_grade` | §5.2/§5.3 |
| `source_snapshot_sha256` | 可追到哪份 cfg |

查询语义（M2 模式）：`filter.as_of` 约束区间，顶层 `known_at` 约束信息集；`known_at IS NULL` 的行在 `known_at` 过滤下排除并计数。新增 `status` 字段：`assigned` / `unknown`（带 `reason`），保证 AC-M3-3 的 `unknown` 可观测。

实现落位：`src/research/industry.rs`（新读模型）+ `src/sqlite_supply` 分支；MCP 与导出只调读模型，不写 SQL。

### 5.6 行业字典（容易漏的一项）

`tdxhy.cfg` 只有 `X` 码，名称来自 `gbbq.zip!zhb.zip!incon.dat` 的 `#TDXRSHY`（`src/catalog_tdx/source.rs:124-144`）。`incon.dat` **也在 generation 里**，退役同样丢失。设计：

- 每代同时落 `industry_dictionary` 派生事实：`incon.dat` 的 sha256 + `#TDXRSHY` 的 `(code, name)` 行 + `parser_version`；`incon.dat` 抽取物同时进来源库（§9.4），否则 fresh-init 后字典无法重建。
- 归档回填没有同代 `incon.dat`：`name=null`、`name_status='unknown_dictionary'`，代码与区间仍然可用；不得用 2026-10 的字典给 2026-09 的区间「补名字」而不标偏差。
- 仓库内 2026-09-16 的 `incon.dat` 可作为 C 档字典作交叉核对，不自动使用。

#### 5.6.1 PR-6 实现落位（2026-10-29）

- **没有 `industry_dictionary` 表**：§9.2 的 DDL（schema 25，PR-1 已合并）只有
  `industry_snapshots.dictionary_sha256`。字典因此按「来源库字节 + 快照里的 sha」落库，
  名称在写入/重放时从 `incon.dat` 重解析，与 Q7「字节不进 DB」一致。§5.6/§9.2/§9.4.2
  文字里的 `industry_dictionary` 指这份「来源库字节 + sha 索引」，不是第六张表。
- **配对规则**：`tdxhy.cfg` 与同代 `incon.dat` 按清单 `origin` 配对——generation 用
  `generation:<manifest sha256>`，归档导入用 `<batch>:<容器目录>`（见
  `src/sqlite_market/industry.rs` 的 `pairing_key`）。配对不上 → 该快照
  `dictionary_sha256=NULL`、所有行 `unknown_dictionary`（回填没有同代字典的正常结果）。
- **存储去重与时间线分离**（2026-10-29 审核修订）：`industry_snapshots` 每个内容一行，
  该行的时间与等级按 §5.3 第 1 步在**该内容的全部登记**上合并——最早的非空
  `last_modified`、最早的 `observed_at`/`ingested_at`、最好的等级（A 优于 B）；
  时间线按**观测序列**算，不只按内容去重：
  - 观测按 `(起点日, ingested_at, sha256)` 排序，只折叠**相邻**且内容相同的观测；
  - 一段相邻相同内容的观测（run）取段内最早的起点作起点、等级取该内容全部登记的
    最好值、`known_at` 取全部登记的最早入库/导入时间；
  - 内容变化即开新区间，包括**变回旧内容**：A → B → A 是两处变更、三段区间。
    内容去重只解决存储（blob 与快照行），不解决历史。
- **只有带发布方 `last_modified` 的登记才是观测**（2026-10-29 第二轮审核修订）：
  没有 `last_modified` 的登记只证明「我们在某时刻持有这份字节」，不证明「发布方当时
  的内容就是它」——归档里的旧副本、测试夹具副本的 mtime/获取时间往往晚于真实发布。
  真实形状：09-30 那份 `tdxhy.cfg` 有 11 条登记，只有 1 条带 `last_modified`（09-30），
  其余 10 条无 `last_modified`、时间为 10-08…10-12；若把它们当观测，排序会把它们
  排到 10-12 的新内容之后，制造出虚假的「变回 09-30 内容」，同日冲突规则还会吞掉
  10-12 那段。因此：
  - 观测集合 = 该内容**带 `last_modified` 的登记**（每条各自成观测，相邻同内容折叠）；
  - 只有无 `last_modified` 登记的**内容**，整份内容只贡献**一个**候选边界：取其
    全部无 `last_modified` 登记中最早的获取时间（basis `acquisition_time`，B 档），
    因而它只出现一次、不能制造回退；
  - 无 `last_modified` 的登记仍然参与该内容的**存储行**合并（等级、时间取该内容全部
    登记的最优值），只是不占时间线位置；区间的等级与 `known_at` 按该 run 的观测合并
    （run 之外的后到登记不能追溯升级或前移早先的区间，否则日更无法逐位复现）；
  - 日更里这类观测若该内容已在表中则直接跳过（不再延长区间）；若某内容的区间是以
    获取时间开的，之后又来了发布方时间，日更 fail-closed（那要重算整段的起点与
    `known_at`），指向 `replay --scope industry`；
  - 日更 generation 的获取都带 `last_modified`，A → B → A 仍按三次观测处理。
- **日更的 run 判定来自观测日志**：`apply_on` 的短路条件是「这次观测与**上一条**观测内容相同」，
  由来源库清单给出前一条 `tdxhy.cfg` 登记的 sha，而不是「该 sha 在表里出现过」。
  上一条内容相同 → 只是延长未闭合区间（段内证据可在原地改善）；上一条内容不同（含 A → B → A）
  → 新开区间。已经公布的区间起点不会因为后到的观测而改写（那要重算整表），
  这类情况日更 fail-closed 指向 `replay --scope industry`。
- **`tdxhy.cfg` 内同代码冲突**：同一文件里同一代码出现两条不同 `(t_code, x_code)` →
  item 失败（`industry_snapshot_invalid`），不猜哪条赢；完全相同的重复行折叠。
- **只有 `X` 一级码的行**：没有二级祖先（`code_level != 2/3`）时不进区间表，也不猜一个
  上级代码。

### 5.7 证监会行业分类（只写结论与依赖）

结论：**不在 M3 接入**。它是另一套口径（证监会门类/大类），与 TDX 研究行业二级**不能直接互换或映射**；接入需要：① 单独的来源准入（D-25 门类）；② 发布入口/历史覆盖/再分发边界的证据包；③ 与 TDX 代码的映射表（无官方 crosswalk，需人工规则 + 抽样）；④ 历史生效日的官方依据；⑤ 双口径并存的契约与读面（两个 `scheme`，不得合并成一行）。依赖：M3 已交付的证券身份、退市覆盖与 `issuer_key` 是任何映射的前置；不先有它们，映射会把退市股/代码复用搞错。评估结论写入数据目录缺口表 #10 的备注，不承诺时间。

## 6. 交易所来源共用入口（5 个使用方）

### 6.1 为什么必须共用

五个来源都是「从交易所网站取当前快照、按 sha256 冻结、需要可复现」；各自抓取会产生五套重试/分页/归档/错误语义，且没人能回答「这条 ST 事实来自哪一天的哪次响应」。共用一条链路，使用方只提供 `source_id`、请求模板、解析器与写入器。

### 6.2 五个使用方与边界

| # | 使用方 | 来源/接口 | 分档（owner 2026-10-28） | 本设计展开程度 |
|---|---|---|---|---|
| 1 | A-1 上市名单 | 沪 `COMMON_SSE_CP_GPJCTPZ_GPLB_GP_L`（`STOCK_TYPE=1` 主板/`=8` 科创，按 `STATE_CODE` 过滤）；深 `CATALOGID=1110`（XLSX） | `APPROVED_SOURCE`（D-45） | 适配器 + 合并规则（§7） |
| 2 | A-2 终止上市名单 | 沪 `COMPANY_STATUS=3`（`STOCK_TYPE∈{1,2,8}`，B 股过滤）；深 `CATALOGID=1793_ssgs&selectModule=main\|nm` | `APPROVED_SOURCE`（D-46） | 适配器 + 合并规则（§7） |
| 3 | 历史 ST（#308） | 深 `SSGSGMXX/tab2` 的 ST 区间（`APPROVED_SOURCE`）；沪 2000-01 起公告索引元数据的状态事件（`APPROVED_SOURCE`），生效日 GP29/行情锚定 `OWNER_RISK_ACCEPTED`；沪 2000 前与北交所 `NOT_AVAILABLE` | 分层如上 | 只定入口与写入器 PR；字段细节归 #308 |
| 4 | 停复牌（A-3） | 沪 `GW_PL_JYTS_TFPXX`（2008 起）；深首期无 `confirmed` | 沪 `confirmed` `APPROVED_SOURCE`；深 `unresolved_gap` 仍可派生 | 只定入口与写入器 PR |
| 5 | 简称历史（S-6） | 深 `SSGSGMXX/tab2`；沪/北无 dated 源 | 深 `APPROVED_SOURCE`；沪/北 `unknown`/`unbounded` | 只定入口与写入器 PR |

A-7（规则书）已由 D-44 准入，不在 M3；引用关系：`price_limit_rules` 的 ST 档依赖使用方 3。

### 6.3 共用机制

```text
fetch（HTTP，bounded retries，Referer/UA）
  → freeze（原始字节 → sha256 → 内容寻址存储 + 索引行）
  → validate（HTTP 状态、格式、分页计数、分段计数、行数下界）
  → parse（source 专属 parser，带 parser_version）
  → bind（source_binding：sha256 + 请求指纹 + 分档状态）
  → consumer writer（A-1/A-2/ST/A-3/S-6 各自的写入器）
```

**获取（fetch）**

- 每个 source 一个规范化请求指纹（URL + 排序后的参数 + 分页/分段参数）；同指纹、同响应 sha256 不产生新快照。
- 分页：上交所只用 `pageHelp.beginPage/endPage`（实测 `pageNo` 只传会重复第一页）；深交所按 `pagecount` 翻页（`pageSize` 不被采纳）。
- **分段：沪公告索引归档起点 2000-01，单请求区间 ≤1 季度**；按季度循环，逐段校验：
  - HTTP 200 但 `data:[]` 且该段应有数据 → `unverified_zero`，**任务失败**，不写「无事件」；
  - 有数据但 `pageHelp.total`/行数与翻页结果不一致 → `truncated`，失败；
  - 只对「有官方依据可空」的窗口（如未来日期）允许 0 行。
- 失败分三类：网络/HTTP → `source_unavailable`；格式/计数校验 → `source_invalid`；分段局部失败 → `partially_failed`（已冻结的快照保留，未覆盖窗口不出行）。

**冻结（freeze）**

- 原始字节内容寻址存进**系统自有的来源库**（`<source-store>/blobs/<sha256>`，与行业抽取物同一存储，§9.4.1），并追加清单行；`exchange_source_snapshots` 表只存索引（`snapshot_id` = sha256，`store_sha256`、`source_id`、`request_fingerprint`、`url`、`fetched_at`、`http_etag`、`http_last_modified`、`as_of`、`row_count`、`page_count`、`parser_version`、`basis_status`）。
- 冻结发生在解析前：解析失败也保留字节，供诊断与复现；写入顺序固定为「blob + fsync → 清单 + fsync → DB 索引」，DB 行缺了可由重放补（§9.4.2）。
- 不做「只存解析结果」：原始响应是 AC-M3-9 断网重放的唯一依据。
- 不存进 owner 私有目录：那不是部署的一部分，fresh-init 或换机后无法保证按 sha 读回（§9.4）。

**保留与版本**

- 按 sha256 去重，**永不自动删除**（全部为小文件：沪深名单日更下估计每秒不到 2MB，但去重后一年 <50MB）；它们和行业抽取物一起留在来源库，不随 generation 退役（§9.4.5）。
- 每个 consumer 派生数据集登记 `source_binding_digest`，绑定「用了哪些 snapshot_id + parser_version」；同一绑定重跑必须逐位相同。
- 与 generation 的关系：交易所快照**不是** generation 的第 4 个 artifact（`ARTIFACTS` 保持 3 个，`src/generation/mod.rs:34`），而是在派生 item 运行时按需获取并冻结；`generation_manifest_sha256` 记在快照的「使用记录」上，便于回答「这一代主表/ST 用的是哪次响应」。
- 退役：交易所快照不随 generation 退役删除（它们不在 generation 目录里，且体积小）；退役门禁对不可再生字节的要求是「已在来源库且能按 sha 读回」，对派生事实的要求是「已登记」（§8.3）。

**失败语义**

- 获取失败不阻塞 TDX 日更与已提交行情；对应 consumer 的 derived item 失败或 `partially_failed`，旧版本继续可读。
- `basis_status` 低于写入所需门槛时，writer 拒写并给 `source_not_admitted`；不是「写 0 行冒充空数据」。
- `NOT_AVAILABLE` 的窗口（沪 2000 前 ST、北交所、深市停牌 `confirmed`）写 `unknown` 行或只记 `pending_source`，不产生缺口结论。

### 6.4 上交所公告索引的获取约束（专列）

| 约束 | 做法 | 验证 |
|---|---|---|
| 归档起点 2000-01 | 分段下界固定 2000-01-01；早于此不请求 | 配置断言 |
| >1 季度静默 `total=0` | 每请求 ≤1 季度，循环 | 请求指纹含 `begin/end`；单测断言区间 ≤93 天 |
| 0 行不可当证据 | 每段校验 `data` 非空或 `total>0`；否则 `unverified_zero` | 夹具：空响应 → 任务失败 |
| 分页 | `beginPage/endPage`，按返回的 `pageCount` 翻完 | 夹具：两页拼接 |
| 字段 | `BULLETIN_TYPE_DESC`、`SSEDATE`、代码、PDF URL；ST 的生效日仍需 GP29/行情锚定（`OWNER_RISK_ACCEPTED`） | 归 #308 写入器测试 |

本任务用通用参数对 1 季度与 5 季度窗口各请求一次，两者都返回 `data:[]`——**无法区分参数错误与真 0 行**，因此不能作为任何覆盖结论；只作为「必须校验」的现场证据（`work/evidence-notes.md` U-1）。

---

## 7. A-1/A-2 合并规则与剩余 unknown

### 7.1 适配器输入输出

复用 M2 已留好的 seam：`ExchangeSnapshot { as_of, listing, delisting, migrations }`（`src/security_master/assembly.rs:109-145`）。M3 实现的是把两个来源解析成这个结构：

- A-1：`(exchange, code, listing_date, A股总股本?, 简称?, as_of)`。只取 `APPROVED_SOURCE` 字段（D-45）：上市日、沪终止上市 A 股上市日、深 A 股总股本、当前简称快照；沪 A 股总股本与 A 股流通股本 `OWNER_RISK_ACCEPTED`（可存但必须标口径）。
- A-2：`(exchange, code, delisting_date, listing_date, reason?)`。退市原因、历史暂停上市区间、名单历史版本都 `NOT_AVAILABLE` → `unknown`。
- 代码变更：`sz.300114→sz.302132` 2025-02-17 `confirmed`；`sh.600849→sh.601607` 只有公告索引 → `candidate`（绝不用于去重）。

### 7.2 合并与冲突

| 字段 | 优先级 | 冲突处理 |
|---|---|---|
| 上市日 | 交易所 A-1/A-2 > `base.dbf SSDATE` > `unknown` | 两者都有且不同 → `listing_conflict=1`，取交易所，保留 `listing_date_tdx`（M2 已有列） |
| 退市日 | 交易所 A-2 `DELIST_DATE` > TDX 整理期 end > `unknown` | `delisting_conflict=1`，两值都存；整理期不是退市日，口径分开 |
| 退市整理期 | `infoharbor_spec.cfg`（2013 起） | 与 A-2 区间交叉时不合并成一行；以交易日读面 `status` 区分 |
| 总股本/流通股本 | 交易所当前快照（`OWNER_RISK_ACCEPTED` 部分标口径） | 历史值 `unknown`；不得用当前值回填历史 |
| 代码迁移 | A-2 confirmed；bar 证据只产 candidate | candidate 只做提示，不折叠股票池 |

过滤规则（必须进测试）：沪市 `STOCK_TYPE=1` 主板 A + `=8` 科创板；`=2` B 股排除。同一公司 A+B 两行 `LIST_DATE` 不同，不按 `STOCK_TYPE` 过滤会造出假冲突。深市 `1793_ssgs` 必须带 `selectModule`，并按代码前缀与 A 股列剔 B 股。状态：沪 `STATE_CODE=3` 才是终止上市，`=7` 是风险警示。

### 7.3 补齐后剩余 unknown（估算）

| 项 | 当前 | 接入后 | 剩余 |
|---|---|---|---|
| 不在目录的 338 个代码的退市日 | 152 有值（`tdx_arrangement`） | A-2 覆盖 336/338（沪 145 + 科创 3 + 深 188） | **2**：`sz.300114`（代码迁移，已由迁移规则处理）与 `sh.600849`（吸收合并，无终止上市记录） |
| 上述代码的上市日 | 276 有值 | 交易所名单补 61/62；`sh.600849` 靠 A-2 的上市日 | 0（若 A-2 覆盖） |
| 主表 5,503 → 预期 | — | +62 个 `SSDATE='0'` 代码 | 约 5,565 行，逐条可解释 |
| 退市日 `unknown` 占比 | 279/5,503（5.1%） | 约 2/5,565 + 迁移侧 | `<0.1%`（估算，部署前私有栈实测） |

数字以本任务 2026-10-15 快照为准；2026-10-26 的 335 个口径同样落在 336 覆盖率内。深市终止上市名单本次未取到，188 个沿用 M2 证据，列入未验证。

### 7.4 PR-3 修复一落位：SSE 名单分页与字段（2026-10-30，XDM-123）

范围：**只改 SSE**。深市 `parse_szse` 的 block 选择是同一取证报告的第二处修复，由后续任务做，
本任务不碰深市解析（`rows`/`parse_szse` 的深市分支行为不变）。依据是 2026-10-30 的线上真实响应
取证（`reports/x-data-mcp/a1a2-forensics.md`），不是旧实现反推。

- **分页方言**：`PagingFormat::SsePageHelp` 改发 `pageHelp.pageNo=<n>&pageHelp.pageSize=2000`
  （新常量 `SSE_PAGE_SIZE`），不再发 `beginPage`/`endPage`；`SSE_PARSER_VERSION` bump 到
  `exchange-sse-page-v2`，`from_parser_version` 同步，旧快照按旧版本登记、不会被当新方言重放。
  实测依据：两种分页参数都只回第 1 页同一批 50 行，而 `pageHelp.pageNo=1&pageHelp.pageSize=2000`
  一次返回全部 1,845 行（`pageCount=1`、`total=1,845`，自洽）。`parse_sse` 的页码读
  `pageHelp.pageNo`，缺失回退 `beginPage`。
- **旧版本可重放**：`from_parser_version` 仍接受 `exchange-sse-page-v1`（新增
  `SSE_PARSER_VERSION_V1` 常量），否则已冻结的 v1 快照在 `--scope exchange` 重放时
  判 `unknown_parser`、索引行消失。v1 页没有 `pageHelp.pageNo`，两种版本读到的页号相同，
  重算结果不变；新写入一律用 v2。
- **单页 fail-closed**：A-1/A-2 的四个 SSE 计划标记「一次取完」（`AcquisitionPlan::single_page_only`）。
  第 1 页信封 `pageCount>1` 即失败，错误码 `page_size_ignored`，detail 以
  `declared total … but collected …` 开头并写明 `pageCount` 与 `pageSize`。**不回退多页循环、
  不静默去重通过**：分页失效时拿到的只是首页，凑不出窗口总数。`pageCount=1` 的计划只发 1 次
  请求（既有循环在 `page >= page_count` 处结束）。
- **字段**：`code_of` 读 `A_STOCK_CODE`，缺失回退 `COMPANY_CODE`；两者都存在且不等 →
  `source_invalid`（不猜）。`PARSER_VERSION` bump `exchange-a1a2-v1` → `exchange-a1a2-v2`，
  否则旧快照会按新规则重放。行内 `STOCK_TYPE`/`LIST_DATE`/`DELIST_DATE` 解析不变。
- **夹具**（`tests/fixtures/exchange-a1a2/`）：四个 SSE 文件按真实字段名
  （`A_STOCK_CODE`/`COMPANY_CODE`/`COMPANY_ABBR`/`STATE_CODE_STOCK`/`PRODUCT_STATUS`）与真实
  `pageHelp`（`total` 非 0、`pageNo`/`pageSize=2000`/`pageCount=1`）改写，保留一行对抗性
  `STOCK_TYPE=2`（B 股）与一行 `DELIST_DATE:"-"`；新增负例 `sse_query_page_ignored.json`
  （`pageCount=2` 而两页返回完全相同的 50 行 → 断言 `declared total … but collected …`）。
  README 的 sha 表同提交更新。
- **文档**：`docs/exchange-source.md` §2/§6 的分页方言与失败码；`docs/data-catalog.md`
  §3.2 第 6/7 行的 `parser_version` 与分页口径。状态词只用文件开头声明的固定取值
  （「代码已合并，未部署」），不写部署事实，不改 SLO 门禁与 `docs/STATUS.md`。
- **端到端分页测试换方言**：`tests/source_store/exchange.rs` 原用「SSE 两页名单」验证「按页冻结 + 重放
  字节一致」。新方言下名单 `pageCount>1` 是 fail-closed（`page_size_ignored`），这个场景对名单已不成立；
  因此改用**深市多页方言**（`PAGENO` + `metadata.pagecount`，本任务不改其解析）保护同一不变量，
  请求断言仍逐页检查 `PAGENO=1`/`PAGENO=2`（不放宽到「不检查请求」）；另新增一条
  `exchange-sse-page-v1` 冻结页的重放测试，验证重放别名（不然后续重放会把旧快照判 `unknown_parser`）。
- **影响面（未验证项）**：`derived:risk_warnings`（PR-10）的沪市公告复用同一个
  `PagingFormat::SsePageHelp`，因此也改发 `pageHelp.pageNo`/`pageSize=2000`。该公告端点
  本次未重新取证，部署前需在私有栈验证翻页行为；本任务不改 PR-10 代码。
- **验证**：本机不编译，Rust 正确性只由 PR 的 CI（fmt、clippy `-D warnings`、nextest、
  export feature）证明；推送前跑 `scripts/check_current_facts.py` 与
  `prepush_check.py`，输出摘要贴 PR 正文。

### 7.5 PR-3 修复二落位：SZSE 名单按 `tabkey` 选块（2026-10-30，XDM-124）

状态：**PR #470，CI 全绿（run 41054257928：`rust`/`export` 均 pass），待审核；未合并、未部署**。范围只含深市 A-1/A-2 名单；沪市分页与字段已在 §7.4 交付。
依据同一份 2026-10-30 真实响应取证（`reports/x-data-mcp/a1a2-forensics.md` §0 第 2/4 点、
§(b) 第 3 点）：深市响应恒为两个 block——`tab1` 暂停上市 0 行 `pagecount=0`，`tab2`
终止上市有数据——而 `parse_szse` 取 `blocks.first()`，于是 `page_count is 0`；
`a1a2::rows` 也读同一个空 block，行数同样为 0。

- **选块规则（两处一致）**：`validate::parse_szse` 与 `a1a2::rows` 共用同一个选择函数。
  先按 `metadata.tabkey == "tab2"` 选；没有 `tab2` 时只接受**唯一一个 `data` 非空的 block**；
  单个 block 的旧形状仍可读；多 block、无 `tab2`、又有多个非空 `data`（或全空）时
  `source_invalid`，**不再静默退回 `blocks.first()`**。页码/`pagecount`/`recordcount`
  与行数组都取自同一个被选中的 block。
- **`page_count is 0`**：不是放宽零页校验；选到 `tab2` 后读它的 `pagecount`（真实 main=9、
  nm=3），`tab1` 的 0 不再进入信封。`pageNo`/`pageno` 两种拼写继续兼容，且同样从被选中的
  block 读取。
- **解析器版本**：信封 `exchange-szse-page-v1` → `exchange-szse-page-v2`。v1 快照仍可重放，
  但按 v1 的旧语义（`[0]` 第一个 block）重算行数/页数，不会用新选块规则改写旧索引；
  新写入一律 v2。业务字段 parser `exchange-a1a2-v2` → `exchange-a1a2-v3`，避免旧证据按
  新选块规则重放。
- **夹具**：`szse_delisting_main.json` / `szse_delisting_nm.json` 改为两个 block
  （`tab1` 空 + `tab2` 有数据），`metadata` 带 `tabkey/pageno/pagesize/pagecount/recordcount`，
  日期用带连字符形态；新增 `szse_delisting_main_p2.json`（`PAGENO=2`）证明分页。真实样本的
  main/nm 首页无交集，因此删除「`000003` 同时在 main/nm」的去重断言，改在单测里用构造的
  「同一代码出现在两个模块」场景覆盖合并去重，并在夹具 README 注明该场景是构造的。
- **测试**：信封（`tab2` 优先、唯一非空 block 回退、多义 block fail-closed、v1 旧语义）、
  `a1a2::rows` 与信封选块一致、两页 main 采集（`PAGENO=1`/`PAGENO=2`）与重放快照数；
  `tests/source_store/exchange.rs` 增加一条 `exchange-szse-page-v1` 冻结页的重放测试，证明
  旧快照仍按第一个 block 重算。去重断言换成构造的单测：同一代码出现在 main/nm 两页时只留
  一条事实（真实首页无交集，README 已注明构造）。
- **既有夹具的一处对账**：`000003` 的 TDX `SSDATE` 夹具原为 `1991-07-03`，与真实深市 r3 的
  `ssrq=1991-01-14` 不同；为保持「与 `SSDATE` 零冲突」基线仍成立，把该 TDX 夹具改为
  `1991-01-14`（真实响应值），不是放宽冲突断言。
- **文档**：`docs/exchange-source.md` §2/§6 的 SZSE 信封/选块口径；`docs/data-catalog.md`
  §3.2 第 7 行的深市终止上市请求与选块说明。不改 SLO 门禁、`docs/STATUS.md` 部署事实，
  不部署、不联网。
- **验证**：本机不编译，Rust 正确性只由 PR 的 CI（fmt、clippy `-D warnings`、nextest、
  export feature）证明；推送前跑 `scripts/check_current_facts.py` 与
  `prepush_check.py --repo <worktree> --base origin/main`，输出摘要贴 PR 正文。
  首轮 CI 的唯一失败是 `snapshot_ids` 断言：它按**计划种类**计 7 项，而深市 main 的两页
  在同一个 `szse_delisting_main` 项里；断言期望被误写成 8。已改为 7，并保留「每个种类
  的 id 列表都非空」检查；请求数 8 与两页采集断言不变，没有放宽覆盖。

### 7.6 PR-3 修复三落位：A-1/A-2 块选择、代码列与形状守卫（2026-10-31，XDM-140）

状态：**未合并、未部署**（本分支实现，Rust 正确性待 PR CI；Refs #446）。

依据 `reports/x-data-mcp/135/report.md` §4：用 preview.38 产品二进制 + 真实冻结字节实测，
A-1/A-2 端到端在采集/解析层 fail-closed，`derived:security_master` 不产出新主表。三个缺陷
互相独立，逐条修：

- **D1 选块**（`src/exchange_source/validate.rs`）：`szse_block` 原先把
  `metadata.tabkey=="tab2"` 当作第一条规则，`CATALOGID=1110` 的真实响应是 4 块而只有
  `tab1` 有数据（`pagecount=146`、`recordcount=2905`），于是读到空块、`page_count is 0`。
  现在按**请求声明的 `TABKEY`** 选块；没有声明时选**唯一非空块**；多个块都非空则
  `source_invalid`，不再「优先 tab2」或取 `[0]`。声明值从请求参数读取（参数名不区分大小写），
  重放从清单 `origin` 的规范指纹读回同一个值；信封与 `a1a2::rows` 共用同一个选择函数。
  选中块的 `tabkey`、选择依据（`declared_tabkey`/`unique_non_empty`/`only_block`）和各块
  row/page 数进 item 证据 `block_selection`，线上可直接区分「选错块」与「块本来就空」。
- **D2 代码列**（`src/official_security/a1a2.rs` `code_of`）：深市分支原先硬编码 `zqdm`；
  1110 上市行只有 `agdm`。现在按行接受 `zqdm` 或 `agdm`（两者都有且不等 → `source_invalid`，
  都没有 → `source_invalid`），沪市分支的 `A_STOCK_CODE`/`COMPANY_CODE` 优先级不变。
- **D3 形状守卫**（同文件 `parse_page`）：守卫原用过滤前 `row_count`，深市退市主板第 9 页
  10 行全是 B 股时被误判为「列表形状变了」。现在改用**过滤后 A 股行数**：有 A 股行但全部
  缺退市日才是形状变化；整页只有 B 股为「本页无事实」（非 A 前缀仍按 #398 fail-closed）；
  整页 0 行仍走既有零行策略。
  错误信息带本页总行数 / 过滤后行数 / B 股跳过数。
- **版本**：信封 `exchange-szse-page-v2` → `v3`（v2 保留为「`tab2` 优先」的旧重放语义，v1
  仍是第一个 block；同一份字节不会因升级被改写行数/页数）；业务 parser
  `exchange-a1a2-v3` → `v4`。
- **夹具**（`tests/fixtures/exchange-a1a2/`）：`szse_listing.json` 改为 1110 的真实
  4 block 形状（`tab1` 有数据、`tab2`/`tab3`/`tab4` 空，行用 `agdm`），新增
  `szse_delisting_main_b_only.json`（第 9 页 10 个 B 股代码）。计数按样本裁剪到自洽
  （1 页 6 行），不提交 146 页大文件；README 的 sha 表同提交更新。
- **测试（CI，不联网）**：`exchange_source::tests` 覆盖声明优先、唯一非空、多非空
  fail-closed、声明缺失 fail-closed、v2/v1 旧语义；`a1a2/tests.rs` 新增
  `the_current_szse_list_is_read_from_the_only_non_empty_block`（D1）、
  `the_szse_code_column_is_zqdm_or_agdm`（D2）、
  `a_page_of_only_b_shares_is_not_a_shape_change`（D3），以及
  `frozen_szse_bytes_replay_to_the_same_row_level_facts`：从来源库读回每个冻结页，
  用同一选块/行解析重算 `row_count/page_count` 与 `szse_listing`、`szse_delisting_main`、
  `szse_delisting_nm` 的**行级事实数**，并断言 `block_selection` 证据——信封级 replay
  正是三个缺陷都逃过的缺口。
- **文档**：`docs/exchange-source.md` §2/§5/§6 同步选块、代码列、形状守卫与版本。
- **未验证/未部署**：产品实跑、真实主表 5,565 行 / `listed_ever` 338 仍未验证（本任务
  不联网、不部署）；合并优先级、退市日语义（15 条沪市差异）与 338/277 口径不在本次范围。

验证：本机只跑 `cargo fmt`、`git diff --check` 与 `scripts/check_current_facts.py`；
Rust 由 PR CI（fmt/clippy `-D warnings`/nextest/export feature）证明。推送前逐字贴出
`scripts/check_current_facts.py` 与 `prepush_check.py --repo <worktree> --base origin/main`
的输出。不改 SLO 门禁、不部署、不动 `docs/STATUS.md` 的部署事实。

## 8. generation 保留与裁剪

### 8.1 现状

- 每代 generation 只有 3 个原始 artifact：`base.zip`、`gbbq.zip`、`hsjday.zip`（`src/generation/mod.rs:34`）。
- 退役时这 3 个文件**直接删除**（`retire_bytes`，`src/generation/mod.rs:557-578`）；自动策略只保留最新 2 个被取代代际的字节（`src/acquire/service.rs:41-43`）。
- 已被丢掉的、只存在于原始文件的行业事实：`base.zip!tdxhy.cfg`（行业归属）与 `gbbq.zip!zhb.zip!incon.dat`（行业名称）。`base.dbf`/`infoharbor`/`gbbq` 的其它事实已在 M1/M2 落库，`hsjday` 已在 M1 落库。
- 退役删的是 generation 目录里的 3 个 artifact；**不可再生的原始字节不能只靠 generation 保存**：行业成员与交易所来源响应必须先进入系统自有的来源库（§9.4），否则 fresh-init 部署（D-08：归档旧库 + 全新 init）后无法重建。
- owner 批准的临时措施（仓库外归档）只能支撑一次性回填与过渡，**不是**长期机制：它依赖人工在每次激活前运行，无法在退役路径上 fail-closed，也不在部署状态里（§9.4.4 导入一次后系统不再引用）。

### 8.2 裁剪前必须落地的派生事实

| 原始文件 | 只存在于原始文件的事实 | 派生事实（裁剪前必须登记） | 现状 |
|---|---|---|---|
| `base.zip` | `tdxhy.cfg` 行业归属 | `industry_snapshot` + `industry_assignments`（区间） | M3 新增 |
| `base.zip` | `base.dbf` 上市日/股本、`infoharbor_spec.cfg` 整理期 | `security_master` / `capital_events` | M2 已有 |
| `gbbq.zip` | `zhb.zip!incon.dat` 行业字典 | `industry_dictionary`（code→name + sha256） | M3 新增 |
| `gbbq.zip` | cat1 因子、cat2/3/5/7/8/9/10 股本 | `adjustment_events` / `capital_events` | M1/M2 已有 |
| `hsjday.zip` | 日线 OHLCV | `market_series` + `series_bars` | M1 已有 |

登记单元：`generation_facts(generation_manifest_sha256, role, fact_kind, fact_digest, evidence_sha256, parser_version, registered_at)`；`fact_digest` 是派生内容的摘要，`role` 是 `base.zip|gbbq.zip|hsjday.zip`，`evidence_sha256` 是该事实所依据的来源库 blob（不可再生事实必须非空，且登记时按 sha 读回校验）。这张表是**可重建索引**：`replay_source_store --scope generation_facts`（§9.4.2）能从来源库清单与派生表重建它；退役门禁不以它的存在为唯一依据。

### 8.3 退役门禁

```text
retire(generation) 前置：
  1) 现有检查：非 active、无 pin、confirm_token 匹配（不变）
  2) 来源库证据检查（权威，fail-closed）：该代每个 role 的不可再生原始成员
     （base.zip → tdxhy.cfg；gbbq.zip → zhb.zip!incon.dat）已在来源库：
       - 清单里有 generation_manifest_sha256 == 该代的记录
       - blob 存在，按 sha256 读回逐字节比对通过
     缺失/读不回 → 拒绝，reason_code=source_store_evidence_missing，列出 (role, source_id, sha256)
  3) 派生事实索引检查（加速）：generation_facts 中存在覆盖该代每个 role 的必需 fact_kind
     缺失 → 拒绝，reason_code=derived_facts_missing，列出 (role, missing kinds)
       （附提示：先跑 replay_source_store --scope generation_facts；该表可重建，不把它当唯一依据）
  4) 豁免：retire_exemptions 行（generation sha + 缺失项 + 理由 + 记录人 + 时间）
     豁免不是默认路径；无豁免记录一律拒绝；对第 2 步的豁免需 owner 单独批准
```

必需集合（初期）：`base.zip → {industry_snapshot, industry_dictionary}`；`gbbq.zip → {industry_dictionary, capital_events, adjustment_events}`；`hsjday.zip → {daily_series}`。其中 `industry_snapshot`/`industry_dictionary` 属「不可再生」，因此第 2 步要求字节在库；其余事实已在 M1/M2 落库，`fact_digest` 即可解释，但仍需登记索引。宁可拒绝一次退役，不可丢一类事实。

自动策略（保留最新 2 代）不变；它只决定「何时尝试退役」，不决定「能否退役」。手动 `retire_generation` 走同一门禁。这条把临时归档从「靠人记得」变成「**不从来源库读回就不能删**」，且 fresh-init 后门禁仍有依据（DB 索引可重建，字节在来源库）。

**PR-8 落地（2026-10-29，XDM-108）**：门禁实现在 acquire 进程（`src/generation_gate.rs`），手动 `retire_generation` 与激活后的自动保留都经过同一个 `RetireGate::check`，字节删除前没有旁路。读面不写市场库：acquire 以只读连接（`query_only`）读 `generation_facts`/`retire_exemptions`，WAL 读不占 data_service 的写租约（D-56）。来源库根沿用 `X_DATA_MCP_SOURCE_STORE`，默认 `<market db parent>/source-store`；acquire 服务需要显式 `--market-db`，未配置时退役一律 `retire_gate_unconfigured`，不会静默放行。证据检查对每个 role 取清单里**该代 manifest sha 的全部登记**逐一按 sha 读回，任一可读即视为证据在库（重复登记不误报）；全部失败或未登记才报 `source_store_evidence_missing`，并列出 `(role, source_id, sha256)`。派生事实按 `(role, fact_kind)` 集合判断，缺失时 `derived_facts_missing` 并提示 `replay --scope generation_facts`；索引可重建，不作为唯一依据。豁免由 `source_store exempt` 写入（必需非空 reason、recorded_by、时间；`source_store_evidence` 键另需 `--owner-approve-evidence`），退役成功时回复 `detail=exemptions: <keys>` 留痕，豁免行不随字节删除。

### 8.4 磁盘与性能估算（只估算，不否决正确性优先）

| 项 | 估算 | 依据 |
|---|---|---|
| 退市股日线入库 | +134MB DB + 首次 WAL ≤200MB | 1,251,129 bar × DB 实测 1.79GB/16.75M bar ≈ 107B/bar |
| 行业区间 + 快照索引 | <10MB（首年） | 5,586 行区间 + 7 份快照元数据 + 字典 67KB |
| 交易所快照（去重后） | <50MB/年 | 单次 ~1.7MB（沪 1.0MB + 深 0.55MB）；仅在内容变化时新增 |
| 原始 `base.zip` 宽限 | 32MB | 现有最新 2 代 × 16MB；不额外延长 |
| **合计首年** | **<0.5GB** | 硬限：剩余 ≥5GB；当前 `df` 剩余 11GB（2026-10-28） |
| 首次采集耗时 | ≈22s（338 实体） | M2 首采 5,227 实体 ≈340s → 约 65ms/实体（外推，需实测） |
| 每代行业变更集 | <0.2s | 150KB 解析 + 5.7k 行比较 |
| 每代不消失检查 | <50ms | 338 行主表 + 序列摘要查询（估算） |

存储不是阻塞项；若未来 ETF/北交所把成员数扩大一个量级，需要重新估算（见 §13 场景 1）。

### 8.5 过渡期

owner 归档在 M3 上线前继续按现规则运行（每次激活后、下一次激活前），并作为 §9.4.4 的一次性导入源；导入完成后归档降为**独立审计副本**，系统不再读它。M3 上线后「唯一副本」是来源库（原始字节 + 清单）+ 派生事实：fresh-init 后由 §9.4.2 重放重建 DB。回填只读来源库，不回头修改历史 generation。

---

## 9. 存储设计（schema 25）

### 9.1 为什么是 25

M2 的 023/024 已部署，不改已合并迁移；25 号迁移只追加 5 张表与必要索引（含 §9.4 要求的 `store_sha256`/`parser_version`/`evidence_sha256` 列）。迁移链是固定长度数组（`src/sqlite_tasks.rs:45-50`），要同时改 `COMPACT_SCHEMA_VERSION`、数组长度/顺序断言与 `scripts/check_current_facts.py:15`。

### 9.2 表

```sql
-- 1) 行业快照：一代 cfg 一份（按成员内容去重）
CREATE TABLE industry_snapshots(
  member_sha256 BLOB PRIMARY KEY,
  store_sha256 BLOB NOT NULL,                  -- 来源库 blob sha（按内容寻址读回的字节，§9.4.1）
  base_zip_sha256 BLOB,
  generation_manifest_sha256 BLOB,
  source_kind TEXT NOT NULL CHECK(source_kind IN ('generation','archive','repo_evidence')),
  publisher_last_modified TEXT,
  observed_at TEXT,
  ingested_at TEXT NOT NULL,
  evidence_grade TEXT NOT NULL CHECK(evidence_grade IN ('A','B','C')),
  parser_version TEXT NOT NULL,                -- 重放必须用同一解析器
  row_count INTEGER NOT NULL,
  scope_member_count INTEGER NOT NULL,
  dictionary_sha256 BLOB,
  notes TEXT
) STRICT;

-- 2) 行业区间（读面）
CREATE TABLE industry_assignments(
  exchange TEXT NOT NULL CHECK(exchange IN ('sh','sz')),
  code TEXT NOT NULL,
  scheme TEXT NOT NULL CHECK(scheme = 'tdx_research_industry'),
  level INTEGER NOT NULL CHECK(level BETWEEN 1 AND 3),
  industry_code TEXT NOT NULL,
  name TEXT,
  name_status TEXT NOT NULL CHECK(name_status IN ('resolved','unknown_dictionary','not_in_dictionary')),
  effective_from INTEGER NOT NULL,
  effective_to INTEGER,
  known_at TEXT NOT NULL,
  effective_from_basis TEXT NOT NULL
    CHECK(effective_from_basis IN ('publisher_last_modified','acquisition_time','evidence_commit_date')),
  evidence_grade TEXT NOT NULL CHECK(evidence_grade IN ('A','B','C')),
  t_code TEXT,
  snapshot_member_sha256 BLOB NOT NULL REFERENCES industry_snapshots(member_sha256),
  PRIMARY KEY(exchange, code, scheme, effective_from),
  CHECK(effective_to IS NULL OR effective_to > effective_from)
) STRICT;

-- 3) 交易所来源快照（5 个使用方共用）
CREATE TABLE exchange_source_snapshots(
  snapshot_id TEXT PRIMARY KEY,                -- 响应字节 sha256（hex）
  store_sha256 BLOB NOT NULL,                  -- 同一字节的来源库 blob sha（显式列，不从路径猜）
  store_kind TEXT NOT NULL CHECK(store_kind IN ('http_fetch','archive_import')),
  source_id TEXT NOT NULL,                     -- sse_listing / sse_delisting / sse_bulletin
                                               -- / sse_suspension / szse_listing / szse_delisting
                                               -- / szse_short_name
  url TEXT NOT NULL,
  request_fingerprint TEXT NOT NULL,
  fetched_at TEXT NOT NULL,
  http_etag TEXT,
  http_last_modified TEXT,
  as_of TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  row_count INTEGER NOT NULL,
  page_count INTEGER,
  parser_version TEXT NOT NULL,
  basis_status TEXT NOT NULL
    CHECK(basis_status IN ('approved_source','owner_risk_accepted','not_available')),
  notes TEXT,
  UNIQUE(source_id, request_fingerprint, snapshot_id)
) STRICT;

-- 4) generation 派生事实登记（退役门禁）
CREATE TABLE generation_facts(
  generation_manifest_sha256 BLOB NOT NULL,
  role TEXT NOT NULL CHECK(role IN ('base.zip','gbbq.zip','hsjday.zip')),
  fact_kind TEXT NOT NULL,
  fact_digest BLOB NOT NULL,
  evidence_sha256 BLOB,                        -- 事实依据的来源库 blob；不可再生事实必须非空
  parser_version TEXT,
  registered_at TEXT NOT NULL,
  PRIMARY KEY(generation_manifest_sha256, role, fact_kind)
) STRICT;

-- 5) 退役豁免（异常路径，必须留痕）
CREATE TABLE retire_exemptions(
  generation_manifest_sha256 BLOB NOT NULL,
  missing_kind TEXT NOT NULL,
  reason TEXT NOT NULL CHECK(length(reason) > 0),
  recorded_by TEXT NOT NULL,
  recorded_at TEXT NOT NULL,
  PRIMARY KEY(generation_manifest_sha256, missing_kind)
) STRICT;
```

说明：

- 区间键用 `(exchange, code)` 而不是 `entity` 字符串：退市股在 A-1/A-2 补齐前可能不在主表，但行业事实仍需可存；读面再 join 主表，join 不到的不返回（带 `orphan` 诊断）。
- `industry_assignments` 不存快照全量行；`industry_snapshots` 只存内容 sha 与统计，原文从**来源库** blob 重解析（审计与重放，§9.4）；`store_sha256` 读回校验失败即 `source_store_corrupt`，fail-closed。
- 交易所快照字节不进 DB（Q7 已裁决：内容寻址文件 + 表索引）：字节在来源库，表只存索引与 sha。
- `generation_facts` 是**可重建索引**：`replay_source_store --scope generation_facts` 可从来源库清单与派生表重建；退役门禁以来源库为准（§8.3）。
- 写入顺序固定：来源库 blob + 清单 fsync 成功后，才写上述索引行（§9.4.1）；不允许「DB 有行、blob 不可读」。
- 列名用 `store_sha256` 而不是 `blob_sha256`：它指「来源库中的 blob」，与 `member_sha256`（成员内容身份）区分语义角色；对 `tdxhy.cfg` 抽取物两者应相等，写入与重放时都断言。
- 索引：`industry_assignments(code, effective_from)`、`exchange_source_snapshots(source_id, as_of)`。
- `price_limit_rules.min_band_ticks INTEGER NULL`（owner 2026-10-29 裁决，见 decisions 的 2026-10-29 条目与
  仓库外规则研究报告 §4.3）：以**最小变动单位数**表达最小带宽下限（沪市风险警示股前收 ≤0.10 元按一 tick、
  深市通用一 tick 下限），避免再加一个低价阈值列。CHECK 为 `NULL OR >= 1`；025 用
  `ALTER TABLE price_limit_rules ADD COLUMN` 追加，**不重建 023 的表**。PR-1 只加列与存储往返
  （规则结构体加可空字段、读写往返、内容摘要计入该列），规则书全部保持 NULL，`limit-v1` 算法与
  规则书取值由紧接着的下一个 PR 改；`research-snapshot.v2` 的 `price_limit_rules.parquet` 列集合
  本 PR 不动（导出列属 PR-9 的 v3）。

### 9.3 迁移与回滚

- 只建新表与索引，不改 022/023/024 任何表；旧二进制读 schema 25 → 拒绝（现有 `SCHEMA_VERSION` 检查），回滚只能回到旧二进制 + 旧库（部署前归档）。
- 空表升级与全新 init 两条路径都要测；`pragma user_version=25`。
- **PR-1 落地（2026-10-29）**：迁移文件 `migrations/tasks/025_m3_index_tables.sql`，五张表 + 两个索引 + 上述
  `min_band_ticks` 列；`COMPACT_SCHEMA_VERSION` 24→25、迁移数组长度/顺序断言与
  `scripts/check_current_facts.py` 的期望 schema 同步；部署脚本仍从 release 读期望 schema
  （`scripts/deploy_phase_g.sh` 的 `expected_schema` 步骤），fresh init 走 25、旧库按现有版本检查拒绝。
  只有结构：五张表在两条 init 路径上都建为空表，PR-1 没有写路径，所以「blob/清单先于索引行」这条
  **由后续写入 PR（PR-2 交易所、PR-6 行业、PR-8 退役门禁）在各自写入器里断言**（每域一个事务）；
  `industry_assignments → industry_snapshots` 的外键只能保证索引内部的先后，不能替代来源库检查。
- **全新 init 后必须重放**：`task_sqlite init21` 建出的 25 号库中 M3 表为空，部署脚本在服务起来后、写部署报告前调用一次 `replay_source_store --scope all`（§9.4.2），否则「部署成功」只是「探活通过、历史为空」。
- **回滚不动来源库**：旧二进制不理解来源库，忽略即可；恢复旧库后来源库只增不改，再次前滚时重放幂等补齐（§9.4.5）。
- `industry_assignments.known_at NOT NULL`：没有知晓时间的行不允许入库（回填统一用回填日），防止把事件日冒充知晓时间。

### 9.4 持久性：fresh-init 部署与不可再生事实

**问题**。D-08 规定生产部署是「归档旧库 + 全新 init 重建」，不做原地迁移（`scripts/deploy_phase_g.sh:350-365`：归档旧库 → `task_sqlite init21` → 起服务 → 探活）。重建只能从**当前 active generation** 再生数据，而 M3 引入三类**不可再生**事实：

| 不可再生事实 | 为什么不可再生 | 按原设计（只改 DB）会怎样 |
|---|---|---|
| 历史行业快照（`tdxhy.cfg`）与行业字典（`incon.dat`） | 只存在于各自 generation 的 `base.zip`/`gbbq.zip` 里；退役即删字节（§8.1） | fresh-init 后 `industry_snapshots`/`industry_assignments`/`industry_dictionary` 为空，且旧代字节已删 → 无法重建 |
| 交易所来源原始响应 | 网站内容滚动变化（深交所简称表每月约 +15 行）；重抓得到的是**新**事实，不是当时冻结的那一份 | `exchange_source_snapshots` 只剩悬空索引；AC-M3-9 的「断网重放」失效 |
| 回填进来的归档快照 | 唯一副本在 owner 私有目录，不在部署状态里 | 换机、清理归档或下一次 fresh-init 即永久丢历史 |

§5.1/§9.2 原来写「原文从归档 blob 重解析」，但那个归档是 owner 的临时目录，不是系统的一部分；`generation_facts` 也只有 DB 行，fresh-init 后同样清空，退役门禁在部署后失去依据。结果就是把 M3 要防止的「丢历史」换个位置再发生一次。因此 M3 必须先有**系统自有的持久来源库**，DB 表只是它的索引。

#### 9.4.1 来源库（source store）

- **位置**：库外的内容寻址存储，位于部署状态目录下、与 generation 根同级（例如 `<deploy-state>/source-store/`）。具体路径由部署配置给出；本文档不写本机路径、不写绝对路径。
- **布局**：

```text
<source-store>/
  blobs/<sha256>        # 原始字节，内容寻址；只增不删、不覆盖
  manifest.jsonl        # 只追加清单，一行一条登记
  tmp/                  # 写入暂存（fsync 后 rename 进 blobs/）
```

- **存货范围**：`base.zip!tdxhy.cfg`、`gbbq.zip!zhb.zip!incon.dat`（每代一份抽取物）、全部交易所来源原始响应字节（A-1/A-2/ST/A-3/S-6 每次冻结）、以及回填导入的归档成员。**不存**整个 `base.zip`/`gbbq.zip`（每份 16–32MB，且 §8 只保留最新 2 代）；容器 `base.zip` 的 sha256 与 manifest 摘要作为清单字段留证。代价：只能按 sha 读回成员字节，不能逐字节重建容器——写进 §11 偏差，作为 AC-M3-9「重放边界」的明文定义。
- **清单字段**（`manifest.jsonl`，只追加；每行一条登记）：

| 字段 | 语义 |
|---|---|
| `sha256` | 字节内容 sha256（hex），即 blob 文件名 |
| `source_id` | 逻辑来源：`tdx.base.zip!tdxhy.cfg` / `tdx.gbbq.zip!zhb.zip!incon.dat` / `sse_listing` / `szse_short_name` … |
| `store_kind` | `generation_member` / `http_fetch` / `archive_import` / `repo_evidence` |
| `origin` | 证据出处：generation manifest sha / 请求指纹 / 归档批次 id + 条目路径 / 仓库路径 |
| `acquired_at` | 首次拿到该字节的时间（UTC 秒） |
| `publisher_last_modified` | 发布方 `last_modified`（可空） |
| `evidence_grade` / `basis_status` | 行业证据等级 A/B/C（§5.3）；交易所来源分档（§6） |
| `parser_version` | 解析该字节所用的解析器版本；重放必须用同一版本，未知版本 fail-closed |
| `size_bytes` / `container_sha256` | 字节数 / 容器 sha256（可空） |
| `import_batch` | 一次性导入批次 id（可空） |

- **写入顺序（强制）**：写 `tmp/` → `fsync(file)` → `rename` 到 `blobs/<sha256>` → `fsync(dir)` → 追加清单行 → `fsync(manifest)` → **最后**才写 DB 索引行（每域一个事务）。实现（PR-1a）在 `<store>/.lock` 上用 `flock` 排他锁覆盖整个登记（查重、发布、追加、两次 fsync），读清单取共享锁；清单行「JSON + 换行」一次 `write_all`；末行缺换行 → `manifest_torn_tail`（读/写均 fail-closed，不自动截断）。任何一步失败都不写 DB；「blob 在、清单在、DB 索引缺」是允许的中间态，由 §9.4.2 补齐；反过来「DB 有索引、blob 不可读」不可接受，读面与门禁一律 fail-closed。
- **权威关系**：blob + 清单是事实源；`industry_snapshots`、`exchange_source_snapshots`、`generation_facts` 是**索引**，可由来源库完整重建。DB 不复制整份清单（不造双真相）；重建后若发现清单里有、DB 没有，属于待补索引；DB 有、清单没有 → `source_store_index_orphan`，拒绝并列出。
- **不做自动 GC**：内容寻址去重后只增不删；删除只能走 owner 显式批准的 maint 命令并留痕（首期不提供该命令）。

#### 9.4.2 重放（replay）

一个 maint 入口，按域重放，幂等：

```text
maint replay_source_store --scope industry|exchange|generation_facts|risk_warnings|security_names|suspensions|all [--verify-only]
```

| scope | 从来源库重建什么 | 解析/校验 |
|---|---|---|
| `industry` | `industry_snapshots`（按 `store_sha256` 读回）、`industry_dictionary`、`industry_assignments`（按 §5.1/§5.2 的确定性规则重算区间） | 清单登记的 `parser_version` 解析；sha 读回不符 → `source_store_corrupt` |
| `exchange` | `exchange_source_snapshots` 索引行（清单 → 行，不重发 HTTP）；再按 `parser_version` 重建各使用方的派生表（主表/ST/停牌/简称区间） | 字节 sha 读回校验；解析失败保留索引、标记 `derived_unavailable`，不静默跳行 |
| `generation_facts` | `generation_facts`（按 `origin` 里的 generation sha + `role` + 派生表摘要重算 `fact_digest`/`evidence_sha256`） | 与 §8.3 门禁同源的校验函数 |
| `risk_warnings` | `risk_warning_intervals` + `derived_dataset_versions` 登记（冻结的深交所简称表 + 沪交所公告页，不重发 HTTP） | 字节 sha 读回校验；未知 `parser_version` fail-closed；空结果不写 0 行 |
| `security_names` | `security_name_history` + 其 `derived_dataset_versions` 登记（冻结的深交所 `SSGSGMXX`/`tab2` 表，不重发 HTTP，不读时钟） | 字节 sha 读回校验；未知 `parser_version` fail-closed；0 行变更表 → `empty_name_set`，不写库 |
| `suspensions` | `suspension_intervals` + 其 `derived_dataset_versions` 登记（冻结的 A-3 停复牌页 → facts；再用**该库**的 `security_master`、交易日历、bar_days 装配，见 §14.9） | 字节 sha 读回校验；未知 `parser_version` fail-closed；库缺主表/日历 → `suspension_replay_not_ready`，不写空表 |

- **确定性**：区间重算的排序键固定为 `(effective_from, ingested_at, sha256)`，同日多变更按 §5.2 处理；同一来源库 + 同一 `parser_version` 重放两次必须产出逐位相同的表内容（AC-M3-9 的仓内定义）。
- **摘要对账**：每个 scope 输出规范化摘要（按主键排序的固定字段 JSON 的 sha256）。`--verify-only` 只重算并与 DB 比较，返回 `match` 或 `drift` + 差异主键（前 20 条）与计数；不写 DB。
- **部署钩子**：`scripts/deploy_phase_g.sh` 在 fresh init 并起服务后、写部署报告前调用一次 `replay_source_store --scope all`（经 maint socket 或 PR-1a 选定的等价 CLI 入口）；来源库为空时返回 `store_empty` 并记入部署报告，不算失败（首次上线前先跑 §9.4.4 导入）。验收时比较重放前后的摘要，必须 `match`。
- **失败语义**：任一 blob 读不回 → 整个 scope 失败、不写半成品（每域一个事务）；只影响 M3 表，不阻塞 TDX 日更与已有行情（§10.1 的隔离规则不变）。
- **与日更的关系**：日更首个派生 item 可以顺带补本代缺失的索引，但**不能替代**部署后的显式重放——部署脚本不依赖日更是否成功。
- **条件可重放**：`suspensions` 的 confirmed 行来自来源库，但装配还依赖该库的 `security_master`、已提交交易日历和每实体 bar_days；后三者是行情数据，不属于来源库。因此它不进 `--scope all`（fresh-init、服务启动前的 all 没有这些输入，纳进去会让每次 fresh-init 都因 `suspension_replay_not_ready` 失败），必须在主表/日历就绪后显式运行，或直接跑日更 `derived:suspensions`。

**PR-6 落地（2026-10-29）**：`industry` scope 已实现（`exchange`/`generation_facts` 仍是
`rebuilt=skeleton_no_op`）。入口形式在 PR-1a 的 CLI 上加两个参数：
`source_store replay --store <dir> --scope industry [--db <market.sqlite>] [--verify-only]`
（`verify` 同形，恒为只读）。

- **`--db` 才是写**：不带 `--db` 只重算并报告 `summary`（`rebuilt=computed_no_db`），
  保证「读不建库、不写库」；带 `--db` 在**一个事务**里替换整个表集
  （`rebuilt=rebuilt`），写完立即回读比对，`match` 才算成功。
- **中文摘要口径**：`summary` = 按主键排序的规范化行（快照 14 字段、区间 12 字段）sha256；
  `--verify-only` 重算同一摘要并与 DB 比较，`drift` 时给出前 20 条主键。
- **空表不是 drift**：`store_empty` 与「库里为空但来源库有内容」都按状态区分，前者不算失败。
- **部署钩子顺序改了**：重放现在写库，所以它从「起服务之后」移到 `task_sqlite init21`
  之后、`systemctl start` 之前（重放要拿数据库服务租约，服务已起会失败而不是重建）。
  报告字段与 `store_empty` 语义不变。
- **区间重建与日更同源**：两条路径都调 `crate::industry::rebuild`/`apply_on`，
  测试断言「逐条追加 == 全量重建」，这是 AC-M3-9 的仓内可执行定义。

**PR-12 重放（2026-10-31）**：`security_names` scope 接入（`src/source_store/replay.rs`、`src/bin/source_store.rs`）。只选 `store_kind=http_fetch` 且 `source_id=szse_short_name` 的记录，并且只重放最近一次采集的页集（整市场快照，重放旧页集会把同一变更链重复一遍）；`--db` 时走 `derived:security_names` 的同一写入器，在一个事务里整表替换 `security_name_history`、登记 `derived_dataset_versions` 并绑定 generation；`--verify-only` 重算行 **与 coverage 窗口** 并与库内登记比较。

**as-of 的可重放选择**：写入端原先用 `Utc::now().date_naive()` 作为沪/北 `NOT_AVAILABLE` 窗口的 `to`，同一份冻结字节每重放一次就得到新证据。两种改法对比后选「`to` 固定为表内最后一个变更日」（即任务里提的更小改法）：

- 读清单 `acquired_at` 日期重放并不成立：去重键是 `(sha256, source_id, origin)`（§9.4.1），内容未变的后续采集**不追加清单行**，清单里的 `acquired_at` 永远是最早一次采集；日更写入端在无变更日仍会把 `to` 写成当天，重放却只能读到旧日期，反而制造 drift。新增可选字段也只在写入端能拿到当天的前提下有用，且必须穿过 PR-2 的 freeze 参数，改动面更大。
- 固定为表内最后变更日后，写入端与重放端共用同一个组装函数，coverage 完全由字节决定；采集日仍是清单事实，不属于数据集证据。报告的 `as_of` 就是这一天。

**两类「没有数据」**：清单里没有 `szse_short_name` 记录 → scope 状态 `store_empty`、`rebuilt=nothing_selected`，不碰数据库；有记录但冻结表没有任何 A 股变更行 → `security_names_rebuild_failed` + reason `empty_name_set`，不写库，上一版仍可读。

验证：`tests/source_store/security_names.rs` 六条端到端（清库重放证据摘要逐位相同、`--verify-only` `match`、篡改 blob → `source_store_corrupt`、未知 `parser_version` fail-closed、空库/无该来源记录、0 行变更表）；适配器单测钉住「写入端与重放端产出同一 coverage」。本机只跑 `cargo fmt`，Rust 由 PR CI 证明。

**PR-1a 落地（2026-10-28）**：入口选定为 CLI `source_store {replay,verify,import}`（release 包内二进制），不注册 maint MCP 工具：maint 入口是只读运维/取证面，来源库写入属于离线运维动作，且不改变三个静态 allowlist、capability revision 与工具数事实；部署钩子直接调用 release 二进制。PR-1a 只注册 `industry`/`exchange`/`generation_facts` 三个 scope 与按 sha 读回的校验路径（报告 `rebuilt=skeleton_no_op`），索引表与域写入器分别由 PR-1、PR-2/PR-6/PR-8 补齐。库格式、清单字段与导入索引格式见 [来源库说明](../source-store.md)。

#### 9.4.3 退役门禁

见 §8.3：必须是「按 sha 从来源库读回原始字节」才算证据；`generation_facts` 只是加速索引，可重建。这样 fresh-init 之后门禁仍有依据，且不依赖任何 owner 私有目录。

PR-8 的重放实现：`--scope generation_facts` 从清单里 `store_kind=generation_member` 的行业成员重建 `base.zip → industry_snapshot`/`industry_dictionary` 与 `gbbq.zip → industry_dictionary` 三类行（按 manifest sha 选每类最新登记，逐 sha 读回）；给了 `--db` 时再叠加 `source_bindings` 已绑定的 `capital_events`/`adjustment_events`/`daily_series` 派生摘要，没有绑定就不造行。`--verify-only` 重算摘要与库内行比较，`drift` 列出前 20 条 `(generation, role, kind)`。登记时（`generation_facts` 不可再生行）也先按 sha 读回，避免写入无法解释的 `evidence_sha256`。摘要不含 `registered_at`，所以清空表后重放得到同一 `summary`。

#### 9.4.4 一次性导入（owner 私有归档）

```text
maint import_source_store --index <archive-index.json> --blobs <dir> --tiers A,B [--kind industry|exchange] [--dry-run]
```

- **只导入通过校验的内容**：成员 sha256 与索引相符、容器 `base.zip` 的 sha256 与 manifest 相符（A 档）、`parser_version` 能完整解析（fail-closed）。不通过 → 进 `skipped_unverified` 清单（逐条原因：`sha_mismatch` / `manifest_mismatch` / `parse_failed` / `unknown_parser`），不静默丢。
- **幂等**：键为 `(sha256, source_id, origin)`（上文「不同 `origin` 追加一条记录」要求键里含 `origin`，否则两条要求互相矛盾；PR-1a 按含 `origin` 实现）；重复执行不新增 blob/清单行，只回报 `already_present`；同一字节不同 `origin` 追加一条清单记录（证据更全，blob 不重复）。
- **A/B 档**（Q2 已裁决）：A 档写 `evidence_grade='A'` + `publisher_last_modified`；B 档写 `B` + `acquired_at`，区间起点按 §5.2 取获取时间。C 档（仓库内 2026-09-16 文件）随仓库存在，不依赖 owner 归档；可选入库标 `repo_evidence`，不产生区间。
- **批次留痕**：导入写 `import_batch`，并在 `docs/STATUS.md` 记一行（批次 id、条目数、跳过数、最早/最晚 `last_modified`）。导入完成后系统**不再读** owner 私有目录；PR-6 的回填只以 `--source store` 读来源库，私有目录仅作为导入输入。
- **与 PR-1 的关系**：导入 + 重放框架是 PR-1a（先于 schema 25 合并）；schema 25 只负责索引列，不承担持久化职责。

#### 9.4.5 保留、磁盘、备份与回滚

- **保留策略**：只增不删，按 sha256 去重；同一内容不因来源重复而多存。
- **磁盘估算**（与 §8.4 合计预算 <0.5GB/年不冲突）：

| 项 | 估算 | 依据 |
|---|---|---|
| 行业抽取物 | 每代 <1MB（`tdxhy.cfg` 约 150KB + `incon.dat` 约 67KB）；按每日 1 代 <0.25MB/代 → 首年 <90MB（未压缩） | §5.6；历史 cfg 实测 |
| 交易所来源原始字节 | 每月数 MB；去重后 <50MB/年 | §8.4「单次 ~1.7MB、仅内容变化时新增」 |
| 合计首年 | **<0.15GB** | 含回填导入（9 份 `base.zip` 的成员抽取物，不含容器） |

- **备份**：部署前对来源库做一次快照（`rsync` 或同盘硬链接副本），与 DB 归档同一批次；因为除 `generation_facts` 外的派生表都能从来源库 + generation 重建，**备份只需覆盖来源库**。首期只有单机一份 + 部署快照，未做异地备份（列为 U-10）。
- **回滚**：回滚到旧二进制时来源库**不动**，旧二进制不认识它、忽略即可；旧库（部署前归档）恢复后与来源库各自独立。再次前滚由 §9.4.2 幂等补齐。「在旧库上直接跑新二进制」与「删来源库换新库」都属禁止操作。
- **损坏处理**：blob 读回 sha 不匹配 → `source_store_corrupt`，重放、门禁与读面全部 fail-closed，输出 `(sha256, source_id, expected, actual)` 清单，等待 owner 用备份或归档副本修复；不自动重抓（重抓得到的是新事实，不能冒充旧事实）。

## 10. 派生、日更、导出、v8 与契约

### 10.1 日更维护

| item | 写集 | 依赖 | 失败语义 |
|---|---|---|---|
| 逐实体日线 item（现有） | 不复权 + 前复权 + 因子/股本 | generation | 不变 |
| `derived:security_master`（现有） | 主表 + 简称 + 上市/退市 + 迁移 | TDX + A-1/A-2 冻结快照 | 失败 → item 失败；旧版继续可读 |
| `derived:industry_snapshot`（新） | 行业快照 + 区间 + 字典 | `base.zip!tdxhy.cfg`、`gbbq.zip!zhb.zip!incon.dat`、§6 共用入口（C 档回填不走日更） | 解析失败/字典缺失 → item 失败，字节已先落来源库；不丢上一版。实现（PR-6）：本代**不带** `tdxhy.cfg` → 成功且 `no_change`（`reason=member_absent`），因为「来源没有这类事实」不是解析失败；同一内容重复观测 → `no_change`，不重开区间 |
| `derived:suspensions`（现有） | `confirmed`（A-3）+ `unresolved_gap`（派生） | 冻结快照 + 日历 + 主表 | A-3 获取失败 → `partially_failed`，不写 `confirmed` |
| `derived:risk_warnings`（现有） | ST 区间 | #308 冻结快照 + GP29/行情锚定 | 同上；沪 2000 前/北交所 `unknown` |
| `derived:security_names`（新写入器） | 简称区间（仅深） | `SSGSGMXX/tab2` 冻结快照 | 沪/北 `unbounded`/`unknown` |
| `derived:delisted_series_check`（新） | 不写业务表；写检查结果与失败实体 | 主表 + 不复权日线序列（登记在实体 item 之前冻结） | 缺失/变更 → `partially_failed` + 实体清单；不自动重建。实现（PR-5）见 §4.5.1：允许的唯一变化是 append 尾巴（`last_day ≤ delisting_day`），退市日未知或代码复用歧义的实体写进 evidence 的 `skipped` |

- 退市股首次采集是**显式任务**（§4.4），不计入日更 plan；日更只做「新退市成员尾巴」与「不消失检查」。
- 内部 item 之间无事务耦合：行业失败不阻塞日线；主表失败不阻塞已有行情（沿用 M2）。
- 所有写 M3 表的 item 遵循 §9.4.1 顺序：先写来源库（blob + fsync + 清单），再写 DB 索引；中途失败只会留下可重放的证据，不会留下悬空索引。
- 交易所快照在 item 内获取：同一日多个 item 共用一个 run 级缓存（按 `source_id + request_fingerprint`），不重复下载。

### 10.2 `as_of` / `known_at` 语义

| 参数 | 位置 | 语义 | 默认 |
|---|---|---|---|
| `as_of` | `filter` | 区间数据：`effective_from ≤ as_of < effective_to`；股票池：成员时点；行业：同区间 | 缺省 = 今天（最后一个交易日） |
| `known_at` | 顶层 | 只返回 `known_at ≤ known_at` 的行；`NULL` 行排除并计数 | 缺省 = 不过滤 |

- `market.industry_assignments` 声明 `as_of + known_at`（coverage `Partial`：所有行都有 `known_at`，但回填行的 `known_at` 是回填日，无法早于它）。
- `market.daily` **不接受** `as_of`（M1 已冻结）；历史股票池走 `market.security_master` + `selection.kind=universe`。
- `known_at` 不来自文件 mtime；前向 = 首次入库时间，回填 = 回填入库时间。

### 10.3 v8 接线

| 变更 | 形状 | 兼容性 |
|---|---|---|
| `Dataset::IndustryAssignments` | `market.industry_assignments`，v2-only，`is_entity_bound=true` | 新增；v1 请求拒绝 |
| `Selection::Universe { scope? }` | 可选 `scope: CatalogScope`；只接受 `sh_sz_a_listed_ever` | v2 附加字段；不传时行为不变 |
| `Selection::Scope`（写面） | `start_collection` 的 `selection.kind=scope`，预算 1..500 | v2 新选择；v1 拒绝 |
| `CatalogScope::ShSzAListedEver` | 仅主表解析入口；`securities.catalog` 拒绝 | 新增值，不改变旧值语义 |
| capabilities | 多一个 dataset + `temporal` 声明 | 客户端按列表发现，无需重握手 revision（沿用 M2 §12.5） |

### 10.4 导出（`research-snapshot.v3`）

v2 的文件集不变，新增：

| 文件 | 内容 | 列（要点） |
|---|---|---|
| `industry_assignments.parquet` | 行业区间 | entity/exchange/code/scheme/level/code/name/name_status/effective_from/effective_to/known_at/basis/evidence_grade/source_snapshot_sha256 |
| `code_migrations.parquet` | 迁移去重表 | from/to/effective_from/certainty/source_kind/known_at（M2 已有表但未导出） |

- `daily_raw.parquet` 自动包含退市股（成员集取自主表，不取目录）；清单 `coverage` 加 `delisted_series` 计数与 `industry_coverage_start`。
- `snapshot_id` 规则不变（内容摘要）；同一输入同 `snapshot_id` 逐位重现；schema 升 v3、旧 v2 快照仍可读（只读目录）。
- `universe_intervals.parquet` 加 `canonical_entity`/`aliases` 列（v3 新列）。

**PR-9 落地（2026-10-30，XDM-115）**：`manifest.schema=x-data-mcp.research-snapshot.v3` 已实现；两个新文件由 `security` 数据集（即 `--dataset all` 的组成部分）固定导出，v3 即使为空也写真实空文件。列名与本节一致，仅把两处笔误落成唯一列名：`industry_code`（本节写作 `level/code`，同一文件不能有两列 `code`）与 `effective_from_basis`（§5.1 的 `basis`）。清单身份新增两块：`industry`（`dataset`/`version_id`/`snapshot_rows`/`assignment_rows`，`version_id` 即 §9.4.2 内容摘要，与 v8 `market.industry_assignments` 的 `version_ref` 同值）与 `coverage`（`delisted_series` = 导出股票中主表 `delisting_date` 非空的不同实体数，`industry_coverage_start` = 第一条区间日）；两者都是输入函数，所以逐位重现仍成立，而只差分行业行的两个库不可能复用同一 `snapshot_id`。`universe_intervals` 的 `aliases` 是逗号分隔的 STRING（与 `flags` 同形），只由 confirmed 迁移产生，candidate 不加别名。读取方：`scripts/export_smoke.py` 同时接受 v2/v3（v2 不要求新文件、`universe_intervals` 按 5 列校验），`scripts/smoke_release.py` 的清单断言同样接受两版；v1 读写路径不变（`market.daily` 之外的 `selection.scope` 与 v1 携带 scope 都被拒绝）。

### 10.5 契约影响

| 面 | 影响 | 处理 |
|---|---|---|
| v1 读 | `market.daily` 可读到更多实体（显式实体/批量选择）；行形状不变 | 兼容扩张；不把目录成员数当可回测成员数 |
| v2 读 | 新数据集与 `scope` | 客户端需按 capabilities 发现；未声明时拒绝 |
| 写面 | `start_collection` 新 `selection.kind=scope` | v2 only；预算 500；冻结选择写 evidence |
| 导出 | v3 | 下游需升解析；未升级可继续用 v2 快照 |
| `x` 下游 | 成员枚举必须改主表/股票池；单实体读取不变 | 下游任务，另立项 |
| 运维 | 退役门禁 + 来源快照 + 回填命令 | maint 入口；不前移到 ops |

---

## 11. 可行性逐项与必须写明的偏差

| 项 | 可行性 | 偏差 / 代价 |
|---|---|---|
| 退市股行情 | **可达** | 需要一次显式首采（338 项）；首次 WAL 峰值需分批；退市前最后 bar 依赖「上一版目录」延续规则 |
| 股票池 | **可达** | 退市日 `unknown` 仍按在册包含（`delisting_unknown`）；`strict` 可排除 |
| 代码变更 | **可达（1 例 confirmed）** | `600849→601607` 只有 candidate，不能去重；300114 与 302132 的 bar 历史重复（1 根不同），下游不得相加 |
| 上市日 | **可达（A-1/A-2 后）** | 交易所只有当前快照，历史修订不可重放；`600849` 依赖 A-2 |
| 退市日 | **可达（99.9%）** | 剩 2 例：300114（迁移）、600849（吸收合并）；深市名单本次未取到（沿用 M2） |
| 历史行业 | **部分** | 最早 2026-09-30（若批 C 档：2026-09-16）；之前 `unknown`；变更日最多晚一个发布周期；回填名称为 `unknown` |
| 行业字典 | **可达（前向）** | 回填期无同代字典 → 名称 `unknown`；代码/区间可用 |
| 不可再生字节的持久性 | **可达** | 靠系统自有来源库（§9.4）；只存成员抽取物，不存整个 `base.zip`，因此不能逐字节重建容器（重放边界 = 成员字节 + 清单） |
| fresh-init 重建 | **可达（需部署步骤）** | 依赖 `replay_source_store` 成为部署脚本的一步；首次上线需先导入（§9.4.4）；首期只有单机一份 + 部署快照，无异地备份（U-10） |
| 回填覆盖范围 | **有限** | 来源库首次导入只覆盖 owner 归档已有的 9 份 `base.zip`/7 份 `tdxhy.cfg`；更早历史仍 `unknown`，导入不创造新证据 |
| 交易所来源入口 | **可达** | 沪公告索引历史 2000-01 起；深市停牌 `confirmed` 首期没有；接口变更风险（参数全大写、分页怪癖） |
| 历史 ST | **部分（按裁决）** | 深市区间可达；沪 2000 起元数据可达但生效日 `OWNER_RISK_ACCEPTED`；沪 2000 前/北交所 `NOT_AVAILABLE` |
| generation 裁剪 | **可达** | 退役会变得更「容易失败」（门禁）；首次上线需要为存量活动代补登记 |
| ETF | **不在本设计** | 按 D-28 另拆；§13 场景 1 只写结构差异 |
| AC-M3-8 可复现快照 | **不在本设计承诺** | 需要完整快照规格（文件集、字节布局、重放命令），建议另拆 Issue |

**必须写进 `docs/data-catalog.md` §3.3 的新增偏差**（实现 PR 里做）：

1. 历史行业最早从 2026-09-30（或 owner 批准的 2026-09-16）开始；之前为 `unknown`，不使用当前值；变更生效日最多晚一个发布周期。
2. 回填区间的行业名称为 `unknown`（无同代 `incon.dat`）；代码与区间可用。
3. 退市股退市日仍有 2 例 `unknown`（300114 由迁移规则处理；600849 为吸收合并候选）；深市终止上市名单在 M3 实现前只有 M2 样本。
4. 代码迁移的 bar 历史重复：300114 的 3,421 根与 302132 前缀仅 1 根不同；股票池已去重，但下游按实体相加会双计。
5. 交易所来源只有当前快照：名单修订、历史 ST 公告完整性与停牌 `confirmed` 均无法重放；沪 2000 前与北交所 ST `NOT_AVAILABLE`。
6. 「0 行」不是证据：公告索引长区间会静默返回 0；来源入口按季度分段并校验，部分窗口会显式 `not_observed`。

## 12. 分离点落位

| # | 分离点 | 本设计的设计点 | 边界靠什么守住 | 过度设计防线 |
|---|---|---|---|---|
| 1 | 来源接入层 ↔ 领域层 | 交易所来源入口（§6）只产出 `ExchangeSnapshot`/公告事实/简称区间/停牌事实；领域层不认 URL、`sqlId`、`CATALOGID` | **依赖方向**：领域层 grep 不到来源名；**测试**：领域层单测不构造 HTTP 结构体 | 不建 crate；一个 source = 一个 parser + 一行注册 |
| 2 | 原始证据 → 解析事实 → 派生 | 原始：generation 字节 + 来源库冻结快照（两者都在库外内容寻址存储，DB 只是索引）；解析事实：行业区间、主表、ST/停牌/简称；派生：股票池、不消失检查、收尾清单 | **测试**：同一冻结快照重放逐位相同（AC-M3-9）；**部署**：fresh-init 后重放摘要必须 `match`（§9.4.2）；派生不进 `version_id` | 不做「来源 DSL」；不为每个来源建 trait 对象；不把清单镜像进 DB |
| 3 | 生效时间 ↔ 知晓时间 | 行业区间 `effective_from/to` + `known_at` + `basis`；ST 生效日与公告日分开 | **契约**：`temporal` 逐数据集；**测试**：`known_at` 排除 NULL 并计数；回填 `known_at` 不得早于入库日 | 不做秒级时间轴；不预建「多版本时钟」 |
| 4 | 存储模型 ↔ 研究读模型 | `src/research/industry.rs`、`universe`；表名/SQL 只在存储适配 | **依赖方向**：MCP/导出只调读模型；**测试**：换存储实现不改响应 schema | 不为每张表建 trait；读模型直吃只读连接 |
| 5 | 核心用例 ↔ 入口适配 | 核心：读模型 + 纯函数；入口：v8 dataset/scope、导出、maint（回填/门禁） | **契约**：三入口同一 `as_of` 结果逐字节相同 | 不复制查询逻辑；不做「一键回测」 |
| 6 | 交易规则 ↔ 数据 | M3 不新增规则语义；ST 区间喂给 M2 的规则表 | **测试**：ST 未知 → 涨跌停 `unknown`（M2 已测）；规则内容不硬编码 | 不把 ST 判断写进涨跌停函数 |
| 7 | 编排 ↔ 原子能力 | 原子：采集、读行业、读股票池、一次导出；编排由 AI 显式组合；回填/门禁是运维原子 | **契约**：工具不返回 `next_actions`；不新增「补退市股+回测」工具 | 不把 Skill 流程塞进工具 |

---

## 13. 扩展场景走查（6 个）

先读 `docs/design.md`「后续数据扩展要求」：本设计与之一致（新来源只改 Adapter；新类别独立模型；扩展契约要 scope 命名带类别）。需补的 4 处单列 §13.2。

| # | 场景 | 会改哪些层 / 分离点 | 新增 | 修改 | 能否「只改适配层 + 一处登记」 |
|---|---|---|---|---|---|
| 1 | 新增证券类型（ETF / 指数 / 北交所 / B 股） | 1、2、6；主表 `instrument_class`/`price_scale` | ETF 身份名单 + `price_scale=3` + cat11 | `universe` 类型过滤；量额单位（份/手）；gbbq 适用范围 | **ETF：不能**（身份名单、精度、cat11 是结构差异，D-28 另拆）。**指数：能**（已在 M1）。**B 股：不能**（币种/代码规则）。**北交所：不能只改适配层**（目录 scope、上市退市名单、ST 均未定；本设计不纳入） |
| 2 | 新增一个官方来源（如证监会行业分类） | 1、3 | 新 parser + `source_id` + 分档 + 映射表 | capabilities 的 `supported_sources`；双 `scheme` 读面 | **获取能**（共用入口）；**写面不能**（新口径需独立模型与映射，见 §5.7） |
| 3 | 新增字段或数据集（财务） | 2、4 | 独立模型 + 表 + 迁移 + 读模型 + 导出文件 | `derived_dataset_versions.dataset_kind`；清单 `files[]` | **不能**：`docs/design.md` 已裁定新类别独立模型 |
| 4 | 新增频率（分钟线） | 2、4 | 新 `period`/表 + 秒级时间 | `series_bars` 主键语义 | **不能** |
| 5 | 交易规则变更（涨跌停） | 6 | 规则表一行（M2） | 无代码改动 | **能**（ST 区间接入后自动生效） |
| 6 | 新增使用方/入口（x 之外的回测/导出格式） | 4、5 | 新入口适配或格式 writer | 入口预算表 + 封闭性测试 | **能**：入口枚举 + 静态表；代价是每个入口的 systemd/探针 |

### 13.1 达不到「只改适配层」的三处

- ETF（场景 1）：精度（厘）、身份名单、cat11 是结构差异，不是声明差异。
- 北交所：目录 scope、交易所名单与 ST 分档都未裁决，强行纳入会把 `unknown` 写成确定值。
- 证监会行业（场景 2 的写面）：口径不同，需独立模型 + 映射 + 双 scheme 契约。

不为达标强行抽象（复杂度预算）。

### 13.2 需要修改 `docs/design.md` 的地方（单列，不在本 PR 改）

这 4 处已由 PR-13a 落入 `docs/design.md`「后续数据扩展要求」（「来源快照（M3 起作为独立一类）」、
「扩展契约」的三条与「M3 模块落位与数据流（已合并，未部署）」），本节保留为映射记录。

1. 「后续数据扩展要求」补「**来源快照**」这一类：从网站获取的官方事实必须可冻结/重放/版本化，且不属于 generation artifact；冻结字节必须落在**系统自有的库外存储 + 只追加清单**里，并且 fresh-init 重建后可用（§9.4）。
2. 「扩展契约」补「**成员集与目录快照分离**」：目录是投影，成员集来自主表；scope 命名带类别。
3. 「事实行必须同时带生效时间与知晓时间」补「**回填行的 `known_at` 不得早于入库时间**」。
4. 「产品闭环与排除范围」补「**0 行不是证据**；范围查询必须分段并校验计数」。

## 14. 实现拆分

通用约束：本机不编译，Rust 正确性只由 PR 上的 CI 证明；每个 PR 跑 §15 的本机门禁。来源细节（ST/A-3/S-6 字段）不在本设计展开；它们共用 PR-2 的入口，各自一个写入器 PR。

截至 2026-10-30：PR-1a…PR-10 已合并（标注在各行 PR 号），PR-11/12 未做，PR-13 的文档半边由 PR-13a 交付；私有栈验收与部署仍未执行（清单见 §14.3）。

**顺序约束（§9.4）**：PR-1a（来源库 + 重放框架）必须先于 PR-1（schema 25）合并；PR-1 的迁移要加 `store_sha256`/`parser_version`/`evidence_sha256` 列，不能把持久化留到后续 PR，否则下一次 fresh-init 仍会清空 M3 表。

| PR | 范围 | AC | 测试计划（CI） | 依赖 |
|---|---|---|---|---|
| **PR-1a** 来源库基础设施（已合并 #453） | §9.4.1 布局 + 原子写/fsync + 只追加清单；`maint {replay,import,verify}_source_store` 骨架（scope 注册表，空实现 no-op）；部署脚本在 fresh init 后调用 `replay --scope all`（空库返回 `store_empty`）；一次性导入 A/B 档（幂等） | AC-M3-9 的存储面；AC-M3-3/5 的重建前提 | 夹具：写序失败（fsync 前/后）无悬空索引；重复导入不增行；sha 读回不符 → `source_store_corrupt`；重放两次摘要相同；空库部署报告 `store_empty` | 无 |
| **PR-1** schema 25（已合并 #455） | §9.2 五张表 + 索引（含 `store_sha256`/`store_kind`/`parser_version`/`evidence_sha256`）；常量/迁移数组/`check_current_facts.py` 同步；旧二进制拒绝 25；索引行必须在来源库写入后才写 | AC-M3-3/5/9 的存储面 | 全新 init + 空表升级两条路径；唯一键/CHECK/外键冲突；`user_version=25`；「blob 先于索引」断言 | PR-1a |
| **PR-2** 交易所来源入口（已合并 #458） | 获取/冻结/校验/注册；`exchange_source_snapshots` 写入；来源库 blob + 清单；`--scope exchange` 重放；分页与 ≤1 季度分段；0 行 `not_observed`；run 级缓存 | AC-M3-9/10 | 夹具：两页 SSE 列表、1 季度公告索引、0 行窗口 → 失败、HTTP 500 → `source_unavailable`、同指纹重复 → 不新增；不联网；索引可从来源库重建 | PR-1 |
| **PR-3** A-1/A-2 适配器 + 主表（已合并 #459） | SSE/SZSE 两个 parser；`ExchangeSnapshot` 装配；接 `exchange_source(Some(..))`；`code_migrations` confirmed 300114；`pending_source` 降为真实来源 | AC-M3-7（上市/退市/迁移） | 夹具：`600001`/`000003`/`600849`/`300114`/`302132`；B 股/科创板过滤；冲突行两侧值；与 `SSDATE` 零冲突回归 | PR-2 |
| **PR-4** 退市股首次采集（已合并 #461） | `CatalogScope::ShSzAListedEver`；`WriteSelection::Scope`（预算 500）；解析 = 主表 ∩ hsjday 成员；item 级发布 | AC-M3-1/2（行情面） | 夹具：5 个退市代码 + 2 个无 bar；重复提交幂等；catalog 数据集拒绝新 scope；预算边界 | PR-3 |
| **PR-5** 日更尾巴 + 不消失检查（已合并 #463） | eligible 集加「上一版目录成员」；`derived:delisted_series_check`；失败实体清单 | AC-M3-6 | 夹具：退市日当天最后 3 根 bar 不漏；活跃股不受影响；摘要变化 → 失败且不重写；缺序列 → 清单 | PR-4 |
| **PR-6** 行业落库 + 回填（已合并 #457） | `derived:industry_snapshot`；区间计算（相邻快照、同日/缺失 `last_modified` 规则）；字典 `incon.dat`；`--scope industry` 重放；maint 回填命令（A/B/C 档，只读来源库） | AC-M3-3/4 | 夹具：7 个历史 cfg（含同内容 3 个 blob）；成员增删/重分类；同日冲突；缺字典 → `unknown_dictionary`；回填幂等；早于起点 `unknown`；来源库重放后摘要 `match` | PR-1a；PR-1（不依赖 PR-2/3）；**已交付 2026-10-29（XDM-104）** |
| **PR-7** 行业读模型（已合并 #462） | `src/research/industry.rs`；`market.industry_assignments` 分支；`status=assigned/unknown`；`universe` 的 `canonical_entity/aliases` | AC-M3-3 | 10 个 `as_of` 夹具；`known_at` 排除 NULL 并计数；before_coverage/missing_in_snapshot；迁移前后只一个代码 | PR-6 |
| **PR-8** 退役门禁（已合并 #460；已交付 2026-10-29，XDM-108） | `generation_facts` 登记（各派生 item 写，含 `evidence_sha256`）；`--scope generation_facts` 重放；`retire_bytes` 前置检查（先来源库按 sha 读回，再查索引）；`source_store_evidence_missing`/`derived_facts_missing`；豁免表与 maint 命令 | AC-M3-5 | 缺任一 kind → 拒绝并给清单；来源库有字节但索引缺行 → 拒绝 + 提示重放；blob 读不回 → 拒绝；有豁免 → 成功且留痕；active/pin 检查不回归；自动退役走同一门禁 | PR-1a；PR-1；PR-3/5/6 登记源 |
| **PR-9** 导出 + v8（已合并 #464；已交付 2026-10-30，XDM-115） | `research-snapshot.v3`（`industry_assignments.parquet`、`code_migrations.parquet`、universe 新列）；`Dataset`/capabilities/`scope` 解码 | AC-M3-9（复现）；AC-M3-3（读面） | 清单确定性；旧 v2 快照可读；v1 拒新 dataset/scope；三入口同结果 | PR-7；**已交付 2026-10-30（XDM-115）** |
| **PR-10** 历史 ST 写入器（已合并 #467；未部署） | 深 `SSGSGMXX/tab2` ST 区间 + 沪公告索引状态事件；GP29/行情锚定标 `OWNER_RISK_ACCEPTED`；沪 2000 前/北交所 `NOT_AVAILABLE` | 归 #308（M3 只接 AC-M3-9） | 夹具：ST 实施/撤销区间、同日冲突、未知生效日；不把 0 行当无事件 | PR-2；#308 门禁 |
| **PR-11** 停复牌写入器（A-3） | 沪 `GW_PL_JYTS_TFPXX` 2008 起 `confirmed`；深首期无 `confirmed`（保留 `unresolved_gap`） | 归 A-3 | 夹具：停牌起止与撤销；深市不写 `confirmed`；与 `unresolved_gap` 不双计 | PR-2 |
| **PR-12** 简称写入器（S-6） | 深 `SSGSGMXX/tab2` 区间；沪/北 `unbounded`/`unknown` | 归 S-6 | 夹具：变更链、XD/XR 规范化、同日多变更、无 dated 源 | PR-2 |
| **PR-13** 文档与验收 | `docs/data-catalog.md` §3.1/§3.2/§3.3、`docs/STATUS.md`、`docs/design.md`（§13.2）、验收报告；fresh-init 后重放前后摘要对账、退役门禁演练 | AC-M3-1…10 | 文档事实检查脚本；私有栈验收证据贴回 #446（含 `store_empty` → import → replay `match`） | PR-3…12 |

### 14.1 独立 Issue（不计入实现 PR）

| ID | 任务 | 说明 |
|---|---|---|
| AC-M3-8 可复现快照 | 完整快照规格与跨机重放 | 需文件集/字节布局/命令；本设计只保证导出可重现，不承诺外部环境逐位重放 |
| 证监会行业分类评估 | §5.7 的五项前置 | 先做来源取证，不写实现 |
| ETF（D-28） | M3-2 | 身份名单、精度、cat11；与本设计的 scope 词汇不冲突 |

### 14.2 已裁决（owner 2026-10-28，全部按默认）

owner 2026-10-28 对 Q1–Q8 回复「按推荐」：下表默认值即裁决，实现 PR 不再回头询问；若实现中发现需要偏离，在对应 PR 写明理由并请 owner 重裁。

| Q | 问题 | 裁决（= 设计默认） |
|---|---|---|
| Q1 | 行业生效日取 `last_modified` 还是获取时间 | `last_modified` 优先，缺失取获取时间，偏差标 `basis` |
| Q2 | 归档快照与仓库内 2026-09-16 文件能否回填 | A/B 可回填；C 只交叉核对（除非另批） |
| Q3 | 退市股首采用 `start_collection` + `selection.kind=scope` | 是；预算 500 |
| Q4 | 派生事实缺失时退役「拒绝」还是告警 | 拒绝（可显式豁免） |
| Q5 | `CatalogScope` 新值只允许主表解析入口 | 是；catalog 拒绝 |
| Q6 | ETF 是否与 M3 同批 | 不同批，另拆 |
| Q7 | 冻结快照字节存文件还是 DB BLOB | 内容寻址文件 + 表索引 |
| Q8 | 五个使用方是否同批 | 共用入口先落地，五个写入器各自 PR |

§9.4 的来源库不是新增裁决项：它是 D-08（fresh-init 重建只能靠 active generation）+ Q7（内容寻址文件 + 表索引）的直接推论，本 PR 按默认设计给出。若 owner 要改为存整个 `base.zip` 容器、或要异地备份/删除策略，属新裁决，不影响 PR-1a 的布局。

### 14.2.1 PR-10 历史 ST 写入器落地记录

状态：**代码已合并（PR #467），未部署**。归属 #308，准入按 D-58 分档；本记录只写已交付代码与证据，不把合并写成已部署。

| 工作项 | 计划 | 证据/落点 | 状态 |
|---|---|---|---|
| 深市区间 | 用冻结的 `SSGSGMXX`/`tab2` 简称变更链，按每只代码的变更日期构造 `ST`/`*ST` 区间；只用「变更后简称」开启区间，非警示简称关闭区间；同日多次变更或无法闭合的链 fail-closed | 新适配器解析器版本 + `RiskWarningInterval` | 代码已合并/未部署 |
| 沪市事件级 | 用 2000-01 起公告索引的 `BULLETIN_TYPE_DESC`/标题、`SSEDATE`、代码与公告 URL 识别实施/撤销；生效日按公告日锚定并标 `OWNER_RISK_ACCEPTED`，不生成简称区间；同日冲突、缺生效日、孤立撤销 fail-closed | `source_kind=exchange_announcement`，证据记录事件计数与锚定方式 | 代码已合并/未部署 |
| 未观察 | 0 行不是「无事件」：冻结分段的空结果记 `not_observed`，写入集合带覆盖窗口；沪市 2000 年前与北交所不请求、不推断，保持 `NOT_AVAILABLE` | 集合级 coverage/evidence，读取端不返回 `normal` | 代码已合并/未部署 |
| 共享入口 | 两类来源都走 PR-2 的 `Entry`（fetch/freeze/validate/bind）与 source store；未配置 source store 时 item 记 `pending_source`，配置后抓取失败则 item 失败且保留上一版 | `src/official_security/risk_warnings.rs` | 代码已合并/未部署 |
| 写入器 | 整表替换 `risk_warning_intervals` 并在调用方事务内登记 `derived_dataset_versions`；空集合删除行与登记；版本绑定与 `security_master` 同一 generation | `src/sqlite_market/risk_warnings.rs` + `MarketBackend` | 代码已合并/未部署 |
| 日更接入 | `derived:risk_warnings` 调用适配器，成功时写库并返回行数/`not_observed`/绑定摘要；未准入时仍写 0 行且明确 `pending_source` | `src/data_supply/derived.rs` | 代码已合并/未部署 |
| 重放 | `source_store replay` 增加 ST 消费者 scope：只读来源库已冻结字节，重建 `risk_warning_intervals` 与登记；与首次写入摘要逐项 `match`，全程离线 | `src/source_store/replay.rs`、`src/bin/source_store.rs` | 代码已合并/未部署 |
| 夹具 | 深市实施/撤销区间、同日冲突、未知生效日、0 行 `not_observed`、沪事件级与深区间级共存；全部离线 | `tests/fixtures/exchange-risk-warnings/` + 单测 | 代码已合并/未部署 |
| 文档 | 数据目录缺口行与本文 PR-10 行只在合并且部署条件满足后改写；状态词只用固定词表（`代码已合并`/`未部署`），不宣称已部署 | `docs/data-catalog.md`、§14 表 | 代码已合并/未部署 |

验证：本机只跑 `cargo fmt` 与文档门禁；Rust 由 PR CI（fmt/clippy/nextest/export feature）证明。推送前逐字贴出 `python3 scripts/check_current_facts.py` 与 `prepush_check.py --repo <worktree> --base origin/main` 的输出。

### 14.2.2 PR-12 简称重放 scope 落地记录

状态：**代码待合并（任务 XDM-133，Refs #446），未部署**。归属 S-6；本记录只写本次交付的代码与证据，不把合并写成已部署。

| 工作项 | 计划 | 证据/落点 | 状态 |
|---|---|---|---|
| scope 接入 | 新增 `Scope::SecurityNames`（`security_names`）并接入 `Scope::ALL`、selects（只选 `source_id=szse_short_name` 的 `http_fetch` 记录）、`replay --scope all` dispatch、CLI usage/解析、报告常量 | `src/source_store/replay.rs`、`src/bin/source_store.rs` | 代码待合并/未部署 |
| 确定性 as-of | coverage 的沪/北 `NOT_AVAILABLE` 窗口结束日固定为冻结表最后一个变更日（不再用 `Utc::now()`）；写入端与重放端共用同一组装函数，报告的 `as_of` 与登记证据可重放 | `src/official_security/security_names.rs`（`assemble_table`） | 代码待合并/未部署 |
| 重建 | `--db` 走 `derived:security_names` 同一写入器（整表替换 + `derived_dataset_versions` 登记），写后回读比较；`--verify-only` 重算行与 coverage 并比较；无 `--db` 只算摘要与 `as_of` | `src/source_store/replay.rs` | 代码待合并/未部署 |
| 空数据语义 | 无 `szse_short_name` 记录 → `store_empty`/`rebuilt=nothing_selected`（不碰库）；0 行变更表 → `security_names_rebuild_failed` + `empty_name_set`（不写库） | 同上 + `EMPTY_TABLE_CODE` | 代码待合并/未部署 |
| 测试 | 清库重放摘要与登记 `evidence_digest` 逐位相同；`--verify-only` `match`；篡改 blob → `source_store_corrupt`；未知 `parser_version` fail-closed；空 store/无该来源；0 行表；覆盖窗口被重新盖章 → `drift`（单测） | `tests/source_store/security_names.rs`、`src/source_store/replay.rs` 单测 | CI（PR） |
| 文档 | 来源库说明、§9.4.2、本文与 `src/sqlite_market/security_names.rs` 的 `upcoming` 注释修正 | `docs/source-store.md`、本文、代码注释 | 代码待合并 |

验证：本机只跑 `cargo fmt`、`git diff --check` 与 `prepush_check.py`；Rust 由 PR CI（fmt/clippy/nextest）证明。

### 14.3 验收清单（私有栈，部署前）

本节是 PR-13 的验收半边：PR-13a 只交付文档（本表与数据目录/设计同步），**下表没有执行过任何一项**，状态一律「未执行」。执行前提：包含 PR-1a…PR-9 的 release 部署到私有栈，配置 `X_DATA_MCP_SOURCE_STORE`（指向部署状态目录下的来源库）与 `X_DATA_MCP_UNSAFE_NONLOCAL_FS=1`（仅本地验收）；生产实例与 `/opt` 不动。证据（命令输出、JSON、行数、sha256）逐条贴回 #446，不写进本仓库。

| AC | 私有栈动作 | 要贴回 #446 的证据 | 状态 |
|---|---|---|---|
| AC-M3-1 | fresh-init 后查询 `market.security_master`，`selection.kind=universe`、`filter.as_of=2015-01-05`；把返回成员与「2015 年以来退市且有 bar」清单逐条核对（退市股序列已首采） | 成员数与按代码清单、每条 `membership_basis`、`unknown` 逐条原因；与当日 catalog 快照的差集 | 未执行（依赖退市股首采） |
| AC-M3-2 | 用 `research-snapshot.v3` 的 `universe_intervals` + `daily_raw` 统计 10 年逐年样本数，与主表「当年在册数」逐年对比 | 逐年样本数/在册数表；每个偏差的代码与原因（退市日 `unknown`、代码复用、来源缺失） | 未执行 |
| AC-M3-3 | `query_data` 读 `market.industry_assignments`：`as_of` 取 2015-01-05、`industry_coverage_start` 与起点前一日；再用顶层 `known_at` 读一次 | 行内 `effective_from`/`effective_to`/`source_snapshot_sha256`/`evidence_grade`/`status`/`reason`；起点前 `status=unknown`；`known_at` 排除 NULL 的计数；没有当前行业回填 | 未执行 |
| AC-M3-4 | `source_store import --store … --index … --blobs … --tiers A,B --kind industry`（含 9 份 `base.zip`/7 个 `tdxhy.cfg`，重复执行一次）→ `source_store replay --store … --db … --scope industry` | 7 个不同内容各一条 `industry_snapshots` 记录与变更集；A/B 档与 `effective_from_basis`；C 档只作交叉核对；重复导入 `already_present`；两次重放 `summary` 相同且回读 `match` | 未执行 |
| AC-M3-5 | 对 superseded generation 调 `retire_generation` 四次：① 缺派生事实；② 来源库有字节但索引缺行；③ blob 不可读；④ owner 用 `source_store exempt … --owner-approve-evidence` 后重试 | 四次响应 JSON（`derived_facts_missing` 清单 / `source_store_evidence_missing` + 重放提示 / `source_store_corrupt` / 成功 + `exemptions:`）、豁免行、原字节仍在；active 与 pin 检查不回归 | 未执行 |
| AC-M3-6 | 连续两代 generation 各跑一次 `plan_market_update`/`run_market_update`，读两次 `derived:delisted_series_check` 的 item evidence | 两代 run/task id；登记的与核对的 `record_count`/`first_day`/`last_day`/`content_digest`；只允许尾追加；再造一次改写/消失 → 实体清单与 `partially_failed` | 未执行（依赖退市股首采） |
| AC-M3-7 | 绑定交易所快照后重跑 `derived:security_master`，统计 `listing_date`/`delisting_date` 的 `unknown` 与 `listing_conflict`/`delisting_conflict` | 部署前后 `unknown` 计数（退市日 ≤2 且逐条解释）、冲突行两侧值、与 `SSDATE` 零冲突回归 | 未执行 |
| AC-M3-8 | 不在本设计承诺（§14.1，建议另拆）；若 owner 另拆，按完整快照规格（文件集、字节布局、跨机重放命令）执行 | 另拆任务的规格与跨机重放结果 | 未执行（不在本轮承诺） |
| AC-M3-9 | fresh init 空库 `source_store replay --store … --db … --scope all`（期望 `store_empty`）→ `source_store import …` → `source_store replay --scope all --db`（期望 `match`）→ `source_store verify`；全程断网 | 三次 JSON（`store_empty`、导入统计、`summary` 与 `rebuilt`）、`manifest.jsonl` 的 sha256、重建表行数 | 未执行（本批覆盖 A-1/A-2、行业、`generation_facts`、风险警示与简称；停复牌的 `replay` scope 仍未接入，另开任务） |
| AC-M3-10 | 用交易所来源入口抓一个已知有数据的季度、一个 0 行窗口，并用一个 >1 季度窗口做对照；检查分段（≤1 季度）与分页计数 | 请求指纹、每个窗口的返回计数、`not_observed`/`unverified_zero`/失败分类；证明没有写出「无事件/无变更」 | 未执行 |

### 14.4 PR-11 落地记录（A-3 停复牌写入器）

状态：实现中（XDM-119）。本节先记计划，落地后逐条补证据；未执行的项写「未执行」。

**入口与请求**（§6.3，D-49/D-60）

- 来源：上交所结构化停复牌事件表 `GW_PL_JYTS_TFPXX`，端点
  `https://query.sse.com.cn/commonSoaQuery.do`；请求参数固定为
  `isPagination=true`、`sqlId=GW_PL_JYTS_TFPXX`、`pageHelp.pageSize=25`、
  `pageHelp.pageNo=<n>`、`productCode=`、`keyWords=`、`startStopDate`/`endStopDate`。
- 分页方言是 PR-2 的第三个值 `SseSoaPageNo`（清单 `parser_version`
  `exchange-sse-soa-page-v1`）：该端点只认 `pageHelp.pageNo`，`beginPage`/`endPage`
  被忽略（实测：发 `beginPage=2&endPage=2` 仍返回第 1 页且 `pageHelp.beginPage=1`），
  信封的页号也从 `pageHelp.pageNo` 读。
- 分段：`2008-01-01` 起、按自然季度对齐、下界固定，上界取「昨天」（日更运行时不查询
  尚未结束的当天窗口；当天事件在下一次运行落库）。每段一次请求集合，段内按页号翻页。
- 冻结/校验/绑定全部走 PR-2 的 `Entry`：字节先入来源库（`store_kind=http_fetch`、
  `source_id=sse_suspension`），信封计数（`pageHelp.total`/`pageCount`/行数）校验，
  分档 `approved_source`。来源库未配置（`X_DATA_MCP_SOURCE_STORE` 缺失）时保持
  `pending_source`，只写 `unresolved_gap`，日更不因此失败。

**行语义（D-49/D-60）**

- `type=LXTP`（连续停牌）且 `stopTime` 为空：区间 = `[startStopDate, endStopDate]`，
  两者都是日期级、闭区间；`endStopDate` 是复牌日前一自然日，直接作为 `end_day`，
  不做加减。`endStopDate` 为空（尚未复牌）= 未闭合：`end_day` 取本段窗口末日，
  并在证据里计数（下一次运行会把区间往后延）。
- `type=LSTP`（临时停牌）且 `stopTime=WH`（全天）且 `startStopDate=endStopDate`：
  单日全天停牌，写一行。
- `stopTime ∈ {AM, PM, 930}`（盘中临时停牌）与其它未知 `stopTime`：不写 `confirmed`
  （D-49 把盘中临时停牌记为 `NOT_AVAILABLE`），计入证据；未知 `type`/缺字段按
  `source_invalid` fail-closed。
- 身份：只取 6 位、以 `6` 开头的沪市 A 股代码；同一 `(exchange, code, start_day)`
  的重复行合并（去重计数）。其它产品（债券/优先股/基金）不写，计入证据。
- 主表里没有的实体（历史代码、B 股、非 A 股产品）不写 `confirmed`：`assemble_suspensions`
  对未知实体是 fail-closed，适配器在交给领域层之前按主表成员过滤，并把被过滤的实体
  计数与样例写进 item 证据，不静默丢行、也不让一行无关产品失败整次日更。
- `known_at = NULL`：事件表没有公告发布日，D-49 明确「公告日不得冒充 `known_at`」，
  因此 confirmed 行的 `known_at` 为空（点时序查询排除 NULL，fail-closed）。
- `reason=exchange_announcement`、`source_kind=exchange_suspension_event`、
  `source_record_id` = `<snapshot_id>:<productCode>:<type>:<startStopDate>`（可回到冻结字节）。

**失败语义**

- 任一段失败（网络/格式/计数不一致/0 行）：`confirmed` 一行不写，item 失败（本次运行
  `partially_failed`），上一版停牌数据集保持可读；0 行是 `unverified_zero`，不是
  「没有停牌」（BR-M3-10）。
- 与 `unresolved_gap` 不双计：confirmed 区间作为 `explained` 交给
  `derive_gaps`，被解释的交易日从缺口行里切掉；行身份 `(exchange, code, start_day)`
  冲突由 `validate_set` 拒绝（`Duplicate`）。

**重放**

- 冻结响应由 `--scope exchange` 选中并重建 `exchange_source_snapshots`（不重发 HTTP）；
  清单里的 parser 版本 `exchange-sse-soa-page-v1` 进 `known_parser_version`，
  否则重放会 `unknown_parser`。
- 适配器另提供「只读来源库」的再解析入口（同一 parser 读回同一批 sha256），测试断言
  两次解析得到的行集合逐位相同；不新增 scope。

**测试清单（CI，夹具，不联网）**

| 场景 | 断言 |
|---|---|
| 停牌起止 | LXTP 行 → 一行 `confirmed`，`start_day`/`end_day` 与事件表一致 |
| 撤销/复牌 | `endStopDate` 闭合后再出现同码新 `startStopDate` → 两行区间，不重叠 |
| 跨日停牌 | 跨越周末/多日的区间按交易日计 `missing_days`，非交易日不计 |
| 深市不写 `confirmed` | 深市不配置来源 → `pending_source`，`confirmed_rows=0`，缺口照常 |
| 不双计 | 缺口日被 confirmed 覆盖后 `gap_rows` 减少、`confirmed_rows` 增加，重叠行为 0 |
| 0 行 | 0 行窗口 → `not_observed`（证据里列窗口），不写 `confirmed`，item 成功；页数与行数自相矛盾才 `unverified_zero`/`truncated` fail-closed |
| 盘中临时停牌 | `stopTime=930/AM/PM` 行不产生 `confirmed`，计入证据 |
| 未复牌 | `endStopDate` 空 → `end_day` = 段末日，计入未闭合计数 |
| 非 A 股/主表外实体 | 债券、优先股、主表没有的代码不产生行，证据里有计数与样例 |
| 重放 | 冻结后断网再解析，事实逐位相同；`--scope exchange --verify-only` = `match` |

**实现状态**：本 PR 已实现，未部署。

- 适配器与解析：`src/official_security/a3.rs`（`plan()`、`acquire()`、`reparse()`、
  `parse_page()`、`merge_overlaps()`、`confirm()`），parser 版本 `exchange-a3-v1`。
- 分页方言：`PagingFormat::SseSoaPageNo`（`exchange-sse-soa-page-v1`）与
  `ZeroPolicy::NotObserved` 在 `src/exchange_source/{request,validate,fetch}.rs`。
- 写入：`src/data_supply/derived.rs` 的 `derived:suspensions` 分支走
  `a3::run_source` → `suspension_source(Some(snapshot))` → `assemble_suspensions`，
  结果证据增加 `exchange_source`；未配置来源库仍是 `pending_source`（深市口径不变）。
- 夹具：`tests/fixtures/exchange-a3/`（2008 Q1 空窗口、Q2 两页、2026-10 真实页）。

**文档**：`docs/exchange-source.md` 增加 A-3 消费者一节与第三个分页方言；
`docs/data-catalog.md` §3.3 偏差 6 与 `docs/daily-sync.md` 的 `derived:suspensions`
口径同步（沪市 2008 起 `confirmed`、深市只有 `unresolved_gap`）。

### 14.5 静默路径显式化（一）：strict 策略与四个 item fail-closed

状态：**未合并、未部署**（本分支实现，Rust 正确性待 PR CI；Refs #446）。

owner 决策（2026-10-30/31，设计报告 `silent-path-design.md` 方案 2a）：

1. 强制点=item 级：缺来源库时四个同源 item 各自 `success=false`，run
   `partially_failed`；日线实体 item 照常提交，上一版主表仍是 latest。
2. 范围=`derived:security_master`、`derived:suspensions`（A-3）、
   `derived:security_names`、`derived:risk_warnings`，它们读同一个
   `X_DATA_MCP_SOURCE_STORE`。
3. 开发豁免：`X_DATA_MCP_ALLOW_UNCONFIGURED_SOURCE_STORE=1` 时 Required 策略仍走
   `pending_source`，并在首次使用时打印 non-production override 提示。
4. 严格触发面=`data_service` 且存在 ops 入口：只有该构造点下传 `Policy::Required`；
   默认 `Policy::Optional`，stdio/测试与非 ops 入口不变。
5. 空 store 合法：strict 只检查「变量已配置且路径可读」，不要求 store 已有条目。
6. 不扩 `task_v2::REASON_CODES`：只保证 error 文本以稳定码
   `source_store_unconfigured` 开头。
7. legacy schema 18 不再作为「必须允许 Pending」的约束（其 plan-run 另有 compact-only
   问题，保持 Optional，不用本错误码掩盖）。

| 工作项 | 计划 | 证据/落点 | 状态 |
|---|---|---|---|
| 共享策略入口 | `source_store` 新增 `CONFIG_ENV`、`ALLOW_UNCONFIGURED_ENV`、`Policy`、`configured(policy)`、`Unconfigured`（稳定码 `source_store_unconfigured`）；Required 校验变量存在 + 目录/清单可读，空 store 合法 | `src/source_store/mod.rs` + `src/source_store/tests.rs` | 未部署 |
| 四个适配器 | `a1a2`/`a3`/`security_names`/`risk_warnings` 的 `run_source` 收 `Policy`；Required + 未配置 → `Invalid { code: source_store_unconfigured }`（不再是 Pending），Optional 保持原行为 | 四个 adapter 及其 tests（每个一例 Required，a1a2 另钉「上一版主表仍可读」） | 未部署 |
| 下传 | `derived::RunContext`（PR-9 `WholeMarket` 同形）收 run 级事实含 `Policy`，`run`/`execute` 不再堆位置参数；`worker::run_once` 收 `Policy`；`DataApi` 新增字段与 `with_source_store_policy`，默认 Optional；`Handler::Worker` 透传 | `src/data_supply/{derived,worker,mcp}.rs` | 未部署 |
| 服务入口 | `data_service` 在 backend 打开后判定 `has_ops && Compact` → Required；legacy 保持 Optional | `src/bin/data_service.rs` | 未部署 |
| 触发面钉法 | 纯函数 `source_store_policy(has_ops, backend)` 单测：只有 ops + compact 才是 Required，其余四种组合都是 Optional | `src/bin/data_service.rs` 测试模块 | 未部署 |
| 端到端钉法 | compact fixture 两代快照：先 Optional 日更提交主表，再 Required + 无 store 日更 → 四项失败各含稳定码、run `partially_failed`、实体日线已前进、主表版本/行不变 | `tests/mcp/plan_run.rs` | 未部署 |
| 文档 | 本节 + `docs/daily-sync.md` 的四项 pending 说明改为「Optional/测试仍 pending，生产 ops strict 下是配置失败」 | 两个文件 | 未部署 |

验证：本机只跑 `cargo fmt`、`git diff --check` 与 `scripts/check_current_facts.py`；Rust 由 PR CI
（fmt/clippy `-D warnings`/nextest/export feature）证明。推送前逐字贴出
`scripts/check_current_facts.py` 与 `prepush_check.py --repo <worktree> --base origin/main` 的输出。
不做启动告警（下一任务），不动 SLO 门禁与 `docs/STATUS.md` 部署事实。

### 14.6 静默路径显式化（二）：启动告警与文档/Skill 同步

状态：**未合并、未部署**（本分支实现，Rust 正确性待 PR CI；Refs #446）。

实现状态：本 PR 已实现启动检查、三种情形测试与文档/Skill 同步；未部署。

依据同一设计报告（`silent-path-design.md`）§3.3–3.4 与 §5.1 的 PR-2/PR-3；owner
2026-10-31 已决 3a：启动只告警、不拒绝服务。方案 2a（四个 item fail-closed）已由
§14.5 / PR #471 合并，本节只补「最早可见」信号与使用说明。

owner 决策/口径：

1. `data_service` 在 ops 入口 + compact 后端启动时检查 `X_DATA_MCP_SOURCE_STORE`：
   未配置且未开 dev 豁免 → stderr/journal 写一条以稳定码 `source_store_unconfigured`
   开头的告警，**继续 bind socket 并服务**（research/maint 读面不受影响）。
2. 已配置时启动日志记录来源库根路径（只记路径，不含任何密钥/凭证）；路径不可读也
   只告警，不拒绝启动（不新增 3b）。
3. dev 豁免 `X_DATA_MCP_ALLOW_UNCONFIGURED_SOURCE_STORE=1` 只限非生产；启动时打印
   `non-production override`，四个 item 回到 `pending_source`。部署单元不得设置它。
4. 空 store 合法；首次部署只要求变量指向的目录存在，不要求已有条目。
5. 部署时必须在 `x-data-mcp-data` 单元里设置 `X_DATA_MCP_SOURCE_STORE`；路径约定与
   PR-6 相同：默认 `<market db parent>/source-store`（部署状态目录下的
   `source-store/`）。首次部署 store 为空是合法状态。
6. 文档/Skill 只说明观测与上报：四个同源 item 报同一个稳定码时按一个部署配置问题
   报告 owner/operator；不 `retry_task`、不 `update_data` 补漏、不自行设置或绕过变量。
   日线实体 item 照常提交，run 收尾 `partially_failed`，上一版主表仍是 latest。
7. `docs/data-catalog.md` 缺口表只改原因说明，状态列仍只用固定取值（`已提供` /
   `部分提供` / `缺失·计划中` / `缺失·阻塞` / `待取证` / `不在范围`，以及「代码已合并，
   未部署」后缀），不引入新状态词。

| 工作项 | 计划 | 证据/落点 | 状态 |
|---|---|---|---|
| 启动检查 | ops+compact 调用 `source_store::configured(Required)`；未配置/不可读打印稳定码告警并继续；已配置打印路径；dev 豁免打印 override | `src/bin/data_service.rs` | 未部署 |
| 启动测试 | 三种情形：无变量 → 告警且服务 ready；有变量（空目录）→ 记录路径且无告警；dev 豁免 → override 且无告警 | `tests/service/local_runtime.rs` | 未部署 |
| 日更文档 | `docs/daily-sync.md` 补启动告警、四 item 失败/`partially_failed`/日线照常、稳定码聚合、dev 开关仅本地 | `docs/daily-sync.md` | 未部署 |
| Skill | 生产 strict 下四个 item 的失败路由；不设/不绕 `X_DATA_MCP_SOURCE_STORE`；回读接受「本次没有新 master 版本」 | `.devin/skills/x-data-daily-sync/SKILL.md` | 未部署 |
| 缺口表 | 6/7/9/11 行的「未配置仍 `pending_source`」改成区分默认与 ops+compact strict；状态列固定取值不变 | `docs/data-catalog.md` | 未部署 |
| 部署记录 | `x-data-mcp-data` 单元必须设置变量，路径默认 `<db parent>/source-store`，首次空 store 合法 | 本节 | 未部署 |

验证：本机只跑 `cargo fmt`、`git diff --check` 与文档检查脚本；Rust 由 PR CI
（fmt/clippy `-D warnings`/nextest/export feature）证明。推送前逐字贴出
`scripts/check_current_facts.py` 与 `prepush_check.py --repo <worktree> --base origin/main`
的输出。不改 SLO 门禁、不部署、不动 `docs/STATUS.md` 的部署事实。

### 14.7 日历覆盖率闸门修正：尾部未知与不回退（2026-10-31，XDM-132）

状态：**未合并、未部署**（本分支实现，Rust 正确性待 PR CI；Refs #446）。

实现状态：本 PR 已实现 `measure_coverage` 的三值判定、尾部字段、`calendar_regressed`
不回退保护、对应测试与文档；未部署。

依据：`calendar-suspensions-attribution.md` §A（XDM-131，只读归因）。失败轮
`derived:calendar` 报 15,712/17,873,567 = 0.08791% 超 0.01%，其中 15,651 根（99.6%）
是 2026-10-26/27/28 三个正常交易日，晚于该轮日历 `last_day=2026-10-23`；其余 61 根是
1991–1993 深市周六/周日真实早期 bar（0.00034%）。`measure_coverage` 当时只用
`calendar.contains(day)`，没有区间判断，把 `last_day` 之后的 `unknown` 计成 outside，
与 `src/trading_calendar.rs` 模块文档和 `day_status()` 的 `BeyondCalendar` 语义冲突。

口径（本次修订，不改阈值）：

1. 覆盖率自检改用 `TradingCalendar::day_status()`：`NonTrading`（区间内无行）计入
   `outside_bars` 并过 0.01% 闸门；`BeyondCalendar`（晚于 `last_day`）只记
   `stale_bars`，不进门禁。
2. `CalendarCoverage::total_bars` 口径明确为**区间内 bar**（`InCalendar + NonTrading`），
   是比率分母；尾部 bar 不进入分母，避免用未知天数稀释或放大比率。
3. 新增尾部字段 `stale_bars` / `stale_days` / `stale_samples` / `stock_last_day` /
   `calendar_last_day`（另有 `stale_days_truncated` / `stale_samples_truncated`），
   写入 `derived` evidence 的 `coverage` 与 item 成功 evidence；失败信息同时列出区间内
   外点与尾部两组计数，不再只按日期升序给出前 8 个 1991 年外点。
4. `commit_calendar` 读已存日历（`read_calendar`）：新日历 `last_day` 早于已存
   `last_day` 时返回稳定错误码 `calendar_regressed`（`Error::Integrity`，消息以该码
   开头），不整表替换；空库（`None`）照常提交；`last_day` 相等属正常重算，不拒绝；
   本 PR 不提供 force 开关。
5. 0.01% 阈值（`COVERAGE_LIMIT_NUMERATOR/DENOMINATOR`）不变；1991–1993 深市边界判定
   不变（61 根仍在区间内、仍计入 `outside`，0.00034% 通过）。

| 工作项 | 计划 | 证据/落点 | 状态 |
|---|---|---|---|
| 三值覆盖率 | `measure_coverage` 按 `day_status()` 分流：区间内无行 → `outside`；晚于 `last_day` → `stale`；`total_bars` 只含区间内 bar | `src/sqlite_market/calendar.rs` | 未部署 |
| 尾部字段 | `CalendarCoverage` 增 `stale_bars`/`stale_days`/`stale_samples`/`stock_last_day`/`calendar_last_day` 与截断标记；`ratio_text`/`require_within_limit` 注释与信息说明分母与两组 | `src/trading_calendar.rs` | 未部署 |
| 证据与信息 | evidence bundle 的 `coverage` 增 `total_bars_scope` 与全部尾部字段；item 成功 evidence 同时给出 `stale_bars`/`stale_days`/两个 `last_day`；失败信息同时给出区间内与尾部 | `src/sqlite_market/calendar.rs`、`src/data_supply/derived.rs` | 未部署 |
| 不回退保护 | `commit_calendar` 先 `read_calendar`，`last_day` 回退 → `calendar_regressed`（Integrity），不写行/不换版本；空库、相等照常 | `src/sqlite_market/calendar.rs` | 未部署 |
| 测试 | 尾部 3 天 + 日历到前一日 → 通过且 stale 字段正确；区间内非交易日超限 → 失败且消息同时给出两组；`last_day` 回退 → `calendar_regressed` 且已存行/版本不变；首/末个日历日不误计；空 bar 既有行为不变 | `src/sqlite_market/calendar/tests.rs` | 未部署 |
| 文档 | 本节 + `src/trading_calendar.rs`、`src/sqlite_market/calendar.rs` 注释；`docs/tasks/issue-383-m1-research-data.md` §5.2 口径澄清与不回退一句；不改阈值数值 | 三个文件 | 未部署 |

测试名（CI，不联网）：`bars_after_the_calendar_are_stale_and_do_not_gate_the_commit`、
`in_span_non_trading_bars_fail_and_the_message_names_the_tail_too`、
`a_calendar_that_ends_before_the_stored_one_is_refused`、
`bars_on_the_first_and_last_calendar_day_are_not_miscounted`；既有
`commit_writes_the_rows_the_registration_and_the_evidence`（空 bar）保持通过。

**为什么这不是放宽门槛**：闸门仍按 0.01% 拒绝区间内真实矛盾 bar（早期深市 61 根照计），
只是把日历自己声明为 `unknown` 的 `last_day` 之后 bar 从“日历外”改记为尾部信息，
并新增“日历不得回退”的独立拒绝；分母改为区间内 bar，比率不再被未知天数影响。

验证：本机只跑 `cargo fmt`、`git diff --check`、`scripts/check_current_facts.py` 与
`prepush_check.py`；Rust 由 PR CI（fmt/clippy `-D warnings`/nextest/export feature）证明。
不改 0.01% 阈值、不改早期深市边界、不联网、不部署、不动 `docs/STATUS.md` 的部署事实。

### 14.8 XDM-134：confirmed 区间未完全落在日历覆盖内（最小设计）

状态：**最小设计（先于代码）；实现未部署**（Refs #446；证据来源 XDM-131
`calendar-suspensions-attribution.md` §B）。

2026-10-31 协调者按推荐语义授权执行，owner 可在部署前否决：两种零交易日形态都不写行、
item 成功，计数与样例进证据；不写 `missing_days=0`，不放宽其它 fail-closed。

**问题**：`assemble_suspensions` 对每条 confirmed fact 用闭区间数已提交日历的交易日，
`== 0` 即 `Error::TradingDayRange`，整个 `derived:suspensions` 失败、一行不写。A-3 的
`end_day` 是「复牌日前一自然日」，且来源窗口末段可晚于日历 `last_day`，因此
（a）整段在 `last_day` 之后、（b）整段落在周末/假日块内都会让全市场停牌数据集失败。
另有一个更隐蔽的形态：区间**部分**越过 `last_day`（例如 `10-20..10-29`，日历到 `10-28`），
若按覆盖内交易日写行会把 `maybe-trading` 的 `10-29` 少算进 `missing_days`。

**判定（唯一口径，先看覆盖再看交易日）**：设日历覆盖 `[first_day, last_day]`。

1. 日历为空，或区间未完全落在 `[first_day, last_day]` 内（`start_day < first_day` 或
   `end_day > last_day`）→ `deferred_beyond_calendar`：整行不写、计数、最多 10 条样例；
   item 成功。**这条不看区间内有多少交易日**（部分越界的 `10-20..10-29` 即使覆盖内
   有 7 个交易日也整行 deferred）；**不 clamp `end_day`、不写部分 `missing_days`**；
   下一次运行日历前进后同一 fact 自然纳入。covered 子段
   `[max(start_day, first_day), min(end_day, last_day)]`（非空时）加入 `explained`，
   只用于向 gap 派生解释「这些天是已知 confirmed 停牌」，避免把缺 bar 误报成
   `unresolved_gap`；不写 suspension 行、不改 deferred 计数。整段在 `last_day` 之后
   或完全在 `first_day` 之前时无子段可加。
2. 区间完全落在 `[first_day, last_day]` 内且 `trading_days_within == 0` →
   `skipped_no_trading_day`：整行不写、计数、最多 10 条样例；item 成功。没有可交易日
   就没有可交易性影响，不能因为一个周末区间让全市场停牌数据失败。
3. 其余（覆盖完整且至少 1 个交易日）保持现状：`missing_days = trading_days_within`，
   正常写行。

**证据（不得静默）**：item `outcome_json` 与 `derived_dataset_versions.evidence` 各增加
`deferred_beyond_calendar`、`deferred_samples`、`skipped_no_trading_day`、
`skipped_samples`。样例顺序为输入 facts 顺序，字段 `entity` / `start_day` / `end_day` /
`source_record_id`；上限 10 条（计数是完整计数，样例只是截断）。稳定词逐字固定为
`deferred_beyond_calendar` / `skipped_no_trading_day`。

**不变式**：

- 表约束 `missing_days > 0`（`migrations/tasks/023_security_master.sql:218`）不放宽；
  跳过形态不产生行，因此不写 0。
- `Error::TradingDayRange` 保留，但不再用于上述两种形态；`UnknownEntity` / `Overlap` /
  `Duplicate` / `Range` / `MissingDays` / `Reason` / `SourceKind` 等仍 fail-closed。
- 与 `unresolved_gap` 不双计：`deferred_beyond_calendar` 的 covered 子段进
  `explained`（只解释缺 bar，不写行）；`skipped_no_trading_day` 不进 `explained`
  （区间内没有交易日），两种情况都不产生 gap 行。
- digest 只覆盖已写行：跳过不改变 `suspensions-v1` 的输入，也不改 `algorithm_version`。

**测试清单（CI，不联网）**

| 场景 | 断言 |
|---|---|
| 纯周末区间（覆盖内） | `Ok`、`skipped_no_trading_day == 1`、无行、样例含 entity/起止/来源 id |
| 整段在 `last_day` 之后（`10-29`，日历到 `10-28`） | `Ok`、`deferred_beyond_calendar == 1`、无行 |
| 部分越界（`10-20..10-29`，日历到 `10-28`） | `deferred_beyond_calendar == 1`、无行、`missing_days` 不出现部分计数 |
| 跨交易日区间（覆盖完整） | 正常行、`missing_days` = 区间内交易日数 |
| 既有 fail-closed | `UnknownEntity`/`Overlap`/`Duplicate`/`Range` 回归不变 |
| deferred 的 covered 子段 | 覆盖段内缺 bar → 不产生 gap 行；日历前进后同一区间写出正常 confirmed 行且 `missing_days` 完整 |
| digest / gap | 跳过不改变 digest；skipped 区间不产生 explained/gap 行 |
| 写入器 | evidence 落库含 4 个字段，计数与样例一致，行数不变 |

**落点**：`src/security_master/suspensions.rs`（计数与样例、覆盖边界、分支）、
`src/data_supply/derived.rs`（item outcome）、`src/sqlite_market/suspensions.rs`
（evidence）、两处测试文件。不改表结构、不改读面、不改 `docs/STATUS.md` 部署事实。

**落地记录（未部署）**

实现状态：代码与测试已提交；本机禁止 Rust 编译（只跑 `cargo fmt` 与文档门禁），Rust 正确性
由 PR CI（`cargo clippy --all-targets --all-features -- -D warnings`、`cargo nextest run`）证明。

- 域层：`SuspensionSet` 新增 `deferred_beyond_calendar` / `deferred_samples` /
  `skipped_no_trading_day` / `skipped_samples`；`assemble_suspensions` 先判覆盖
  （`calendar_covers`，空日历返回 false）再数交易日；样例上限 `SKIP_SAMPLE_LIMIT = 10`。
  `Error::TradingDayRange` 保留但不再由本函数返回。
- gap 交互：`deferred_beyond_calendar` 区间的 covered 子段进 `explained`
  （不写行、不改计数），覆盖段内的缺 bar 不再产生 `unresolved_gap`；`skipped` 区间不进。
- 证据：item `outcome_json` 与 `derived_dataset_versions` evidence 各 4 个字段（样例
  `entity`/`start_day`/`end_day`/`source_record_id`）；写入器新增 H3 同形校验，拒绝
  「样例数 > 计数」的集合（行计数器校验不变）。
- 边界：数据集最终无任何行（无 gap、全部 deferred/skipped）时仍按既有规则不登记
  `derived_dataset_versions`（空集不造空登记），跳过计数只出现在 item outcome。
- 测试：`src/security_master/suspensions/tests.rs`
  `a_weekend_only_interval_is_skipped_with_a_sample`、
  `an_interval_past_the_calendar_last_day_is_deferred_whole`（含部分越界 `10-20..10-29`）、
  `a_covered_interval_keeps_its_trading_day_count`、
  `a_skipped_interval_neither_splits_a_gap_nor_adds_a_row`、
  `a_deferred_interval_explains_its_covered_days_and_writes_no_gap`、
  `a_skipped_interval_does_not_enter_the_digest`、
  `skip_counts_stay_complete_when_samples_are_truncated`、
  `a_confirmed_fact_for_an_unknown_entity_is_refused`（回归）；
  `src/sqlite_market/suspensions/tests.rs`
  `skipped_confirmed_intervals_are_counted_in_evidence_and_write_no_row`、
  `a_set_whose_skip_samples_outnumber_their_counters_is_refused`。
- 文档：`docs/data-catalog.md` 第 11 行（停牌）补零交易日口径，状态列不变。

### 14.9 XDM-138：停牌 confirmed 的来源库重放（条件可重放，未部署）

状态：**设计已确认，代码与测试已提交；未部署**（Refs #446）。

**结论：条件可重放，不是无条件可重放。** 来源库只持有 A-3 冻结页，能重解析出
`ConfirmedSuspensionFact`；但整表装配还需要三样**行情数据**：`security_master` 区间、
已提交交易日历、每实体 bar_days。fresh-init 后仅凭来源库无法重建 confirmed 行，
因为这三个输入不在来源库里。可选路径只有两条：先重建主表/日历（日更
`derived:security_master`/`derived:calendar`，或未来的重放路径），再显式
`replay --scope suspensions`；或者直接跑日更 `derived:suspensions` 重新抓取/复用冻结页。

**scope 行为**（`src/source_store/replay.rs`、`src/bin/source_store.rs`）：

- 只选 `store_kind=http_fetch` 且 `source_id=sse_suspension` 的登记；清单 → 索引行
  由 `exchange_source::rebuild` 重算（不依赖库里已有的 `exchange_source_snapshots`）。
- **无 `--db`**：只解析冻结页并报告 `facts`（merged confirmed intervals）计数，
  `rebuilt=computed_no_db`；没有主表就无从放置/裁剪 member window，不装配、不写库。
- **有 `--db`**：先 `read_inputs_if_ready` 读主表注册、交易日历、bar_days；缺主表或
  日历为空 → 状态/问题码 `suspension_replay_not_ready`，整表不动，绝不写空表冒充成功。
  就绪后与日更走**同一个** `assemble_suspensions`，再用**同一个**
  `commit_suspensions`（绑定到主表同一个 generation binding）整表替换
  `suspension_intervals` 与 `derived_dataset_versions`，写后回读比较。
- `--verify-only` 重算行并与库内比较：`match` 或 `drift` + 前 20 条差异键，不写库。
- **确定性**：摘要只覆盖已写行（`suspensions-v1`）；同一来源库 + 同一库状态重放两次
  得到同一 `summary`。三样库输入单独哈希为 `inputs_digest` 并写进登记证据（日更与重放
  都写），所以库状态前进导致重算行变化时，`--verify-only` 报 `drift_cause=inputs_changed`
  （报告同时给出新的 `inputs_digest`），而不是 `source_store_corrupt`；若输入未变而行
  不同，则报 `drift_cause=rows_changed`。
- **不进入 `--scope all`**：all 在 fresh-init、服务启动前运行，那时主表/日历必然不存在；
  把条件 scope 放进 all 会让每次 fresh-init 部署都因 `suspension_replay_not_ready` 失败。
  文档与 usage 都明确写「先主表/日历，再 replay suspensions，或直接跑日更」。

**测试（CI，不联网）**：冻结页 → facts 计数；`--db` 装配的行与日更写入逐位一致；
`--verify-only` `match` → 改库输入后 `drift`/`inputs_changed`；库缺日历 →
`suspension_replay_not_ready` 且不写；未知 `parser_version` → fail-closed；空库 /
无 `sse_suspension` 记录 → `store_empty`/`nothing_selected` 不碰库。

**落点**：`src/source_store/replay.rs`、`src/bin/source_store.rs`、
`src/official_security/a3.rs`（共享 reparse 与无主表计数）、
`src/security_master/suspensions.rs`（`inputs_digest`）、
`src/sqlite_market/suspensions.rs`（ready/missing/empty 三态、replay 输入摘要证据）、
`src/data_supply/derived.rs`（日更也登记 `inputs_digest`）、`docs/source-store.md`。
不改表结构、不改读面、不改部署脚本与 `docs/STATUS.md` 部署事实。

## 15. 本机与 CI 验证方式

- 本机**禁止** `cargo build/test/check/clippy/nextest/run`；只允许 `cargo fmt`。本文档 PR 只跑仓库自带的文档门禁（见下一条），不使用仓库外的本机私有工具。
- 本机能跑：`git diff --check`、`cargo fmt --check`、`python3 scripts/check_current_facts.py`、`python3 scripts/check_daily_sync_eval.py`、`python3 scripts/check_deploy_facts.py`、`python3 scripts/check_public_hygiene.py`，以及 CI 里对应的 `python3 -B -m unittest`（`test_runtime_peer.py` 需 sudo，跳过并注明）。
- Rust 正确性只能由 PR 上的 GitHub Actions 证明；没有 CI 结果时一律写「未运行」。
- 每个实现 PR 的 CI 验证方式（写进 PR 正文）：
  1. `cargo fmt --check`、`cargo clippy --all-targets --all-features --locked -- -D warnings`；
  2. `cargo nextest run --locked --no-fail-fast` + `cargo test --doc`；
  3. `cargo build --locked`；
  4. 迁移与 schema 断言（`schema_upgrade` 类 + `check_current_facts`）；
  5. **不联网测试**：来源入口用固定小夹具（JSON/XLSX/季度响应/0 行）；真实抓取只在私有栈验收，结果贴回 #446。
  6. **来源库**：写序夹具（fsync/rename 任一步失败 → 无悬空 DB 索引）、重放两次摘要一致、blob 读回不符 → `source_store_corrupt`、导入幂等（第二次 `already_present`）。
- 私有栈验收（不走 CI）：fresh-init → 空库 `replay` 返回 `store_empty` → `import_source_store` → `replay --scope all` 摘要 `match`；7 份行业快照回填 → 6 个边界；338 个退市代码首采；退役门禁在真实 generation 上拒绝/放行（含“来源库有字节、索引缺行”与“blob 不可读”两种拒绝）；退市股序列跨代摘要不变；耗时/磁盘与估算对账。

---

## 16. 实测、未验证与待确认

### 16.1 本次新增实测（可复现）

全部在 `work/` 与 `work/external/`（仓库外），内存限 `150M`：

| # | 结论 | 证据 |
|---|---|---|
| L-1 | 目录 5,226 / 主表 5,503 / 退市日 153；338 个不在目录 | `delisted_codes.json`、只读 DB |
| L-2 | 338 个代码 / 1,251,129 bar / 254 个末根 ≥2015；62 个不在主表且全为 `SSDATE='0'` | `delisted_codes.json`、`base_dbf.json` |
| L-3 | 9 个 `base.zip`、7 个 `tdxhy.cfg`、6 个变更集、0 行重分类；仓库 2026-09-16 文件与最早快照差 8 增/2 重分类 | `industry_snapshots.json`、`repo_cfg_vs_early.json` |
| L-4 | `300114`/`302132` 重叠 3,421 根 bar 中仅 1 根不同 | `hsjday` 逐 bar；本任务记录 |
| E-1/E-2 | SSE 上市 1,845 行 / 终止上市 145 行，`pageSize` 被忽略 | `external/sse_*.json`（sha256 见清单） |
| E-3 | 深交所 1110 当前在册 2,906 行 | `external/szse_1110.xlsx` |
| E-4 | 深交所终止上市本次 HTTP 500，无新样本 | `external/szse_terminated_p1.json` |

### 16.2 未验证（如实列出）

| # | 项 | 为什么未验证 | 取证方法 |
|---|---|---|---|
| U-1 | 沪公告索引 ≤1 季度分段 | 本任务未拿到可区分的非空响应；通用参数两次都 `data:[]` | #308/A-3 实现时用已知有公告的季度验证 |
| U-2 | 深交所终止上市 188 个 A 股 | 本次 HTTP 500；沿用 M2 样本 | PR-3 上线前重抓并写冻结快照 |
| U-3 | 归档快照的真实生效日 | 官方无生效日 | 只能按 §5.2 取上界，偏差已知 |
| U-4 | 回填行 `known_at` 对回测的影响 | 需要下游回测验证 | PR-7 后私有栈跑 10 年回测 |
| U-5 | 首次采 338 实体的耗时/WAL 峰值 | 未实现 | PR-4 私有栈实测 |
| U-6 | 退役门禁对存量活动代的影响 | 未实现 | PR-8 首代回填登记后演练 |
| U-7 | `x` 下游改造 | 不在本仓 | 下游任务 |
| U-8 | 沪 2000 前 ST 与北交所 ST | owner 已裁 `NOT_AVAILABLE` | 不取证；写 `unknown` |
| U-9 | fresh-init 后重放摘要真的一致 | 未实现 | PR-1a/13 私有栈：init → import → replay → 比对摘要（预期先 `store_empty` 再 `match`） |
| U-10 | 来源库备份/损坏演练与容量 | 未实现；首期只有单机一份 + 部署快照 | PR-1a 后用真实归档做一次恢复演练，并与 §9.4.5 估算对账 |

### 16.3 待确认

无。§14.2 的 Q1–Q8 已由 owner 2026-10-28 全部按默认裁决。§9.4 的来源库属 D-08 + Q7 的直接推论，若 business 上要改「存整个容器 / 异地备份 / 删除策略」再另开裁决；不阻塞 PR-1a。

---

## 17. Also noticed

1. **`CatalogScope` 是个空枚举**（`src/data_supply/wire.rs:366-369`）：目前没有任何代码消费它，`securities.catalog` 的选择靠 `catalog_head`；扩值前要先补「scope 如何解析」的测试，否则新值可能被静默忽略。
2. **`market.daily` 的 universe 忽略 `as_of`**（`src/sqlite_supply.rs:342-344`），只读当前目录；历史股票池只能走 `market.security_master`。下游若用 daily+universe 做回测，会永远拿不到退市股。
3. **`base.dbf` 的 `SSDATE='0'` 不是「无上市日」而是「未锚定」**：M3 接入 A-1/A-2 后这 62 个代码才进主表；在此之前它们的历史 bar 无法解释。
4. **同一 `tdxhy.cfg` 可对应 3 个不同 `base.zip`**（10-13/10-14/10-15）：快照去重必须按成员 sha256，不能按 `base.zip` sha 或 `last_modified`。
5. **归档索引里同一内容有 26 条记录、部分 `last_modified` 为 `NULL`**：回填必须取「最早的非空值」并保留 `observed_at`，否则会把已知时间降级成 unknown。
6. **行业名称字典也在 generation 里**（`gbbq.zip!zhb.zip!incon.dat`），此前只讨论 `tdxhy.cfg` 会漏掉它；没有字典的区间只能给代码，不能给名称。
7. **`research-snapshot.v2` 已包含 7 个 M2 文件但不含 `code_migrations`**（`src/research/mod.rs:610-622`）；迁移去重信息现在只能从 `security_master` 的区间变化反推，v3 应显式导出。
8. **生成退役是「删 3 个文件」，但 manifest/一致性证据保留**（`src/generation/mod.rs:34,557-578`）：只保留「证据」不保留「可复现输入」，所以门禁必须查派生事实，而不是查 manifest。
9. **`pageHelp.pageSize` 被上交所忽略**：实测 `pageSize=3` 返回 1,845 行；依赖 `pageSize` 做分页的实现会一次拿全量并可能触发超时/截断。
10. **深交所 `ShowReport` 的 `pageSize` 不被采纳**（M2 已记）且本次 `1793_ssgs` 重试 500：来源入口需要「单次失败不影响已冻结快照」的隔离。
11. **`security_master` 已有 `listing_conflict`/`delisting_conflict` 列**（`migrations/tasks/023_security_master.sql:96-98`），M3 不需要新列即可记录两侧值；不要另造冲突表。
12. **AC-M3-8（跨机逐位复现）与 AC-M3-9（断网重放）不是同一件事**：前者要求快照格式规范，后者只要求同一仓内从冻结快照重建；本设计只交付后者。
13. **fresh-init 部署没有派生/重放步骤**（`scripts/deploy_phase_g.sh:350-365`：归档旧库 → `init21` → 起服务 → 探活）：新库的 M3 表在重放前全空，而探活仍然通过。M3 必须把 `replay_source_store` 写成部署脚本的一步并把摘要进部署报告，否则「部署成功」与「历史仍在」没有任何关系。
