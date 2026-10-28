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
| 11 | 存储 | **schema 25**：行业快照/行业区间、交易所来源快照、generation 派生事实登记、退役豁免；沿用 022/023 的表不重建 | 设计 |
| 12 | 契约 | 不改已冻结行为：新增 v2-only `market.industry_assignments`、`CatalogScope` 新值与 `selection.kind=scope`、导出 `research-snapshot.v3`；`x` 下游改用主表/股票池取成员 | 设计 |
| 13 | 可行性 | 退市股行情、股票池、行业区间、来源入口、裁剪门禁均可达；历史行业**早于 2026-09-30 不可达**（标 `unknown`）；北交所与沪 2000 前 ST **NOT_AVAILABLE**；偏差逐条列在 §11 | 实测 + 设计 |

**三个最容易被做错的点**（本设计专门守住）：

1. **「不在目录」不等于「已退市」**。目录是当前快照；把目录当成员集会永久丢掉 338 个退市代码，把主表当「当前名单」又会把已退市证券当成仍在册。两者职责分开：目录回答「今天叫什么、属于什么行业」，主表 + 股票池回答「某天在不在册」。
2. **缺快照不是「没有变化」**。行业只有观测到的快照；两个快照之间的变更日不可知，早于最早快照只能 `unknown`。用当前行业回填历史，或用「0 行」证明无变更，都是伪造历史。
3. **0 行不是证据**。交易所公告索引在区间过长时会静默返回 `total=0`；来源入口必须分段并校验计数，否则会把「查询失败」写成「没有停牌/没有 ST」。

---

## 1. 范围

### 1.1 本次包含

1. 退市股行情：成员集、`CatalogScope` 扩展、与主表/上市退市日的关系、首次采集、日更与跨代不消失检查、对 `x`/v8 的影响。
2. 历史行业：快照/区间模型、生效日取法与偏差、证据等级与归档回填、早于起点 `unknown`、读面（数据集名/wire 形状/`as_of`/`known_at`）、行业字典。
3. 证监会行业分类候选：只写评估结论与依赖，不在本设计内接入。
4. 交易所来源共用入口：A-1/A-2、历史 ST（#308）、停复牌（A-3）、简称历史（S-6）的获取、冻结、保留、版本、与 generation 的关系、失败语义；沪公告索引的 ≤1 季度分段与计数校验。
5. generation 保留与裁剪：裁剪前必须落哪些派生事实、退役门禁、磁盘估算。
6. 存储（schema 25）、派生、日更维护、导出列、契约影响、可行性逐项偏差。
7. 分离点落位、6 个扩展场景走查、实现拆分（含 ST/A-3/S-6 各一个写入器 PR）与 owner 裁决项。

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
             交易所来源快照（沪深名单/公告索引/简称变更/停复牌，按 sha256 冻结，跨代保留）
                 │  每个来源一个 Adapter（分离点 1）
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
                 └── 运维（maint：来源快照、退役门禁、回填）
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

- **区间由相邻快照计算，不存快照全量行**（全量行只用于重算与审计：只存 `member_sha256` + 统计；如需复查，从归档 blob 重解析）。
- 同一代码出现在相邻快照且 `(t_code, x_code)` 不变 → 不产生新行，只延长区间；变化 → 旧区间 `effective_to = 新起点`，新行开启。
- 从某快照消失的代码 → 旧区间在**该快照起点**结束（我们只能证明「从这次观测起不在」，不能证明具体日期）。
- 区间不重叠、不空洞（在观测窗口内）；同一 `(entity, effective_from)` 唯一，冲突 fail-closed。
- 一个快照 5,666–5,669 行；区间表初始约 5,586 行（scope S 内）、每年增量估计几百行。

### 5.2 生效日取法与偏差

官方 `tdxhy.cfg` **没有生效日**，`base.zip`/HTTP 只有 `last_modified`。规则（待 owner Q1 确认）：

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

回填流程（一次性 maint 命令，不是日更）：

1. 读归档索引，按 `tdxhy.cfg` sha256 去重；同一内容多条记录取**最早的非空 `last_modified`**与最早 `observed_at`。
2. 校验每个 blob 的 sha256 与成员名；校验 `tdxhy.cfg` 能被现有解析器完整解析（fail-closed）。
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

- 每代同时落 `industry_dictionary` 派生事实：`incon.dat` 的 sha256 + `#TDXRSHY` 的 `(code, name)` 行 + `parser_version`。
- 归档回填没有同代 `incon.dat`：`name=null`、`name_status='unknown_dictionary'`，代码与区间仍然可用；不得用 2026-10 的字典给 2026-09 的区间「补名字」而不标偏差。
- 仓库内 2026-09-16 的 `incon.dat` 可作为 C 档字典作交叉核对，不自动使用。

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

- 原始字节内容寻址存（`blobs/<sha256>`，与行业归档同形状）； `exchange_source_snapshots` 表存索引（`snapshot_id` = sha256，`source_id`、`request_fingerprint`、`url`、`fetched_at`、`http_etag`、`http_last_modified`、`as_of`、`row_count`、`page_count`、`parser_version`、`basis_status`）。
- 冻结发生在解析前：解析失败也保留字节，供诊断与复现。
- 不做「只存解析结果」：原始响应是 AC-M3-9 断网重放的唯一依据。

**保留与版本**

- 按 sha256 去重，**永不自动删除**（全部为小文件：沪深名单日更下估计每秒不到 2MB，但去重后一年 <50MB）。
- 每个 consumer 派生数据集登记 `source_binding_digest`，绑定「用了哪些 snapshot_id + parser_version」；同一绑定重跑必须逐位相同。
- 与 generation 的关系：交易所快照**不是** generation 的第 4 个 artifact（`ARTIFACTS` 保持 3 个，`src/generation/mod.rs:34`），而是在派生 item 运行时按需获取并冻结；`generation_manifest_sha256` 记在快照的「使用记录」上，便于回答「这一代主表/ST 用的是哪次响应」。
- 退役：交易所快照不随 generation 退役删除（它们不在 generation 目录里，且体积小）；退役门禁只要求「派生事实已登记」（§8）。

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

## 8. generation 保留与裁剪

### 8.1 现状

- 每代 generation 只有 3 个原始 artifact：`base.zip`、`gbbq.zip`、`hsjday.zip`（`src/generation/mod.rs:34`）。
- 退役时这 3 个文件**直接删除**（`retire_bytes`，`src/generation/mod.rs:557-578`）；自动策略只保留最新 2 个被取代代际的字节（`src/acquire/service.rs:41-43`）。
- 已被丢掉的、只存在于原始文件的行业事实：`base.zip!tdxhy.cfg`（行业归属）与 `gbbq.zip!zhb.zip!incon.dat`（行业名称）。`base.dbf`/`infoharbor`/`gbbq` 的其它事实已在 M1/M2 落库，`hsjday` 已在 M1 落库。
- owner 批准的临时措施（仓库外归档）只能支撑回填与过渡，**不是**长期机制：它依赖人工在每次激活前运行，无法在退役路径上 fail-closed。

### 8.2 裁剪前必须落地的派生事实

| 原始文件 | 只存在于原始文件的事实 | 派生事实（裁剪前必须登记） | 现状 |
|---|---|---|---|
| `base.zip` | `tdxhy.cfg` 行业归属 | `industry_snapshot` + `industry_assignments`（区间） | M3 新增 |
| `base.zip` | `base.dbf` 上市日/股本、`infoharbor_spec.cfg` 整理期 | `security_master` / `capital_events` | M2 已有 |
| `gbbq.zip` | `zhb.zip!incon.dat` 行业字典 | `industry_dictionary`（code→name + sha256） | M3 新增 |
| `gbbq.zip` | cat1 因子、cat2/3/5/7/8/9/10 股本 | `adjustment_events` / `capital_events` | M1/M2 已有 |
| `hsjday.zip` | 日线 OHLCV | `market_series` + `series_bars` | M1 已有 |

登记单元：`generation_facts(generation_manifest_sha256, role, fact_kind, fact_digest, registered_at)`；`fact_digest` 是派生内容的摘要，`role` 是 `base.zip|gbbq.zip|hsjday.zip`。

### 8.3 退役门禁

```text
retire(generation) 前置：
  1) 现有检查：非 active、无 pin、confirm_token 匹配（不变）
  2) 新检查：generation_facts 中存在覆盖该代每个 role 的必需 fact_kind
     缺失 → 拒绝，reason_code=derived_facts_missing，列出 (role, missing kinds)
  3) 豁免：retire_exemptions 行（generation sha + 缺失项 + 理由 + 记录人 + 时间）
     豁免不是默认路径；无豁免记录一律拒绝
```

必需集合（初期）：`base.zip → {industry_snapshot, industry_dictionary}`；`gbbq.zip → {industry_dictionary, capital_events, adjustment_events}`；`hsjday.zip → {daily_series}`。宁可拒绝一次退役，不可丢一类事实。

自动策略（保留最新 2 代）不变；它只决定「何时尝试退役」，不决定「能否退役」。手动 `retire_generation` 走同一门禁。这条把临时归档从「靠人记得」变成「不登记就不能删」。

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

owner 归档在 M3 上线前继续按现规则运行（每次激活后、下一次激活前）；M3 上线后归档仍保留作为独立审计副本，但**不再是唯一副本**：派生事实 + 冻结的行业快照已能支撑复现。回填只读归档，不回头修改历史 generation。

---

## 9. 存储设计（schema 25）

### 9.1 为什么是 25

M2 的 023/024 已部署，不改已合并迁移；25 号迁移只追加 4 张表与必要索引。迁移链是固定长度数组（`src/sqlite_tasks.rs:45-50`），要同时改 `COMPACT_SCHEMA_VERSION`、数组长度/顺序断言与 `scripts/check_current_facts.py:15`。

### 9.2 表

```sql
-- 1) 行业快照：一代 cfg 一份（按成员内容去重）
CREATE TABLE industry_snapshots(
  member_sha256 BLOB PRIMARY KEY,
  base_zip_sha256 BLOB,
  generation_manifest_sha256 BLOB,
  source_kind TEXT NOT NULL CHECK(source_kind IN ('generation','archive','repo_evidence')),
  publisher_last_modified TEXT,
  observed_at TEXT,
  ingested_at TEXT NOT NULL,
  evidence_grade TEXT NOT NULL CHECK(evidence_grade IN ('A','B','C')),
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
- `industry_assignments` 不存快照全量行；`industry_snapshots` 只存内容 sha 与统计，原文从归档 blob 重解析（审计）。
- 交易所快照字节不进 DB（Q7 待定）；表只存索引与 sha；字节在内容寻址目录。
- 索引：`industry_assignments(code, effective_from)`、`exchange_source_snapshots(source_id, as_of)`。

### 9.3 迁移与回滚

- 只建新表与索引，不改 022/023/024 任何表；旧二进制读 schema 25 → 拒绝（现有 `SCHEMA_VERSION` 检查），回滚只能回到旧二进制 + 旧库（部署前归档）。
- 空表升级与全新 init 两条路径都要测；`pragma user_version=25`。
- `industry_assignments.known_at NOT NULL`：没有知晓时间的行不允许入库（回填统一用回填日），防止把事件日冒充知晓时间。

## 10. 派生、日更、导出、v8 与契约

### 10.1 日更维护

| item | 写集 | 依赖 | 失败语义 |
|---|---|---|---|
| 逐实体日线 item（现有） | 不复权 + 前复权 + 因子/股本 | generation | 不变 |
| `derived:security_master`（现有） | 主表 + 简称 + 上市/退市 + 迁移 | TDX + A-1/A-2 冻结快照 | 失败 → item 失败；旧版继续可读 |
| `derived:industry_snapshot`（新） | 行业快照 + 区间 + 字典 | `base.zip!tdxhy.cfg`、`gbbq.zip!zhb.zip!incon.dat`、§6 共用入口（C 档回填不走日更） | 解析失败/字典缺失 → item 失败并保留字节；不丢上一版 |
| `derived:suspensions`（现有） | `confirmed`（A-3）+ `unresolved_gap`（派生） | 冻结快照 + 日历 + 主表 | A-3 获取失败 → `partially_failed`，不写 `confirmed` |
| `derived:risk_warnings`（现有） | ST 区间 | #308 冻结快照 + GP29/行情锚定 | 同上；沪 2000 前/北交所 `unknown` |
| `derived:security_names`（新写入器） | 简称区间（仅深） | `SSGSGMXX/tab2` 冻结快照 | 沪/北 `unbounded`/`unknown` |
| `derived:delisted_series_check`（新） | 不写业务表；写检查结果与失败实体 | 主表 + 序列 | 缺失/变更 → `partially_failed` + 实体清单；不自动重建 |

- 退市股首次采集是**显式任务**（§4.4），不计入日更 plan；日更只做「新退市成员尾巴」与「不消失检查」。
- 内部 item 之间无事务耦合：行业失败不阻塞日线；主表失败不阻塞已有行情（沿用 M2）。
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
| 2 | 原始证据 → 解析事实 → 派生 | 原始：generation 字节 + 冻结快照；解析事实：行业区间、主表、ST/停牌/简称；派生：股票池、不消失检查、收尾清单 | **测试**：同一冻结快照重放逐位相同（AC-M3-9）；派生不进 `version_id` | 不做「来源 DSL」；不为每个来源建 trait 对象 |
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

1. 「后续数据扩展要求」补「**来源快照**」这一类：从网站获取的官方事实必须可冻结/重放/版本化，且不属于 generation artifact。
2. 「扩展契约」补「**成员集与目录快照分离**」：目录是投影，成员集来自主表；scope 命名带类别。
3. 「事实行必须同时带生效时间与知晓时间」补「**回填行的 `known_at` 不得早于入库时间**」。
4. 「产品闭环与排除范围」补「**0 行不是证据**；范围查询必须分段并校验计数」。

## 14. 实现拆分

通用约束：本机不编译，Rust 正确性只由 PR 上的 CI 证明；每个 PR 跑 §15 的本机门禁。来源细节（ST/A-3/S-6 字段）不在本设计展开；它们共用 PR-2 的入口，各自一个写入器 PR。

| PR | 范围 | AC | 测试计划（CI） | 依赖 |
|---|---|---|---|---|
| **PR-1** schema 25 | §9.2 五张表 + 索引；常量/迁移数组/`check_current_facts.py` 同步；旧二进制拒绝 25 | AC-M3-3/5/9 的存储面 | 全新 init + 空表升级两条路径；唯一键/CHECK/外键冲突；`user_version=25` | 无 |
| **PR-2** 交易所来源入口 | 获取/冻结/校验/注册；`exchange_source_snapshots` 写入；内容寻址 blob；分页与 ≤1 季度分段；0 行 `not_observed`；run 级缓存 | AC-M3-9/10 | 夹具：两页 SSE 列表、1 季度公告索引、0 行窗口 → 失败、HTTP 500 → `source_unavailable`、同指纹重复 → 不新增；不联网 | PR-1 |
| **PR-3** A-1/A-2 适配器 + 主表 | SSE/SZSE 两个 parser；`ExchangeSnapshot` 装配；接 `exchange_source(Some(..))`；`code_migrations` confirmed 300114；`pending_source` 降为真实来源 | AC-M3-7（上市/退市/迁移） | 夹具：`600001`/`000003`/`600849`/`300114`/`302132`；B 股/科创板过滤；冲突行两侧值；与 `SSDATE` 零冲突回归 | PR-2 |
| **PR-4** 退市股首次采集 | `CatalogScope::ShSzAListedEver`；`WriteSelection::Scope`（预算 500）；解析 = 主表 ∩ hsjday 成员；item 级发布 | AC-M3-1/2（行情面） | 夹具：5 个退市代码 + 2 个无 bar；重复提交幂等；catalog 数据集拒绝新 scope；预算边界 | PR-3 |
| **PR-5** 日更尾巴 + 不消失检查 | eligible 集加「上一版目录成员」；`derived:delisted_series_check`；失败实体清单 | AC-M3-6 | 夹具：退市日当天最后 3 根 bar 不漏；活跃股不受影响；摘要变化 → 失败且不重写；缺序列 → 清单 | PR-4 |
| **PR-6** 行业落库 + 回填 | `derived:industry_snapshot`；区间计算（相邻快照、同日/缺失 `last_modified` 规则）；字典 `incon.dat`；maint 回填命令（A/B/C 档） | AC-M3-3/4 | 夹具：7 个历史 cfg（含同内容 3 个 blob）；成员增删/重分类；同日冲突；缺字典 → `unknown_dictionary`；回填幂等；早于起点 `unknown` | PR-1（不依赖 PR-2/3） |
| **PR-7** 行业读模型 | `src/research/industry.rs`；`market.industry_assignments` 分支；`status=assigned/unknown`；`universe` 的 `canonical_entity/aliases` | AC-M3-3 | 10 个 `as_of` 夹具；`known_at` 排除 NULL 并计数；before_coverage/missing_in_snapshot；迁移前后只一个代码 | PR-6 |
| **PR-8** 退役门禁 | `generation_facts` 登记（各派生 item 写）；`retire_bytes` 前置检查；`derived_facts_missing`；豁免表与 maint 命令 | AC-M3-5 | 缺任一 kind → 拒绝并给清单；有豁免 → 成功且留痕；active/pin 检查不回归；自动退役走同一门禁 | PR-1；PR-3/5/6 登记源 |
| **PR-9** 导出 + v8 | `research-snapshot.v3`（`industry_assignments.parquet`、`code_migrations.parquet`、universe 新列）；`Dataset`/capabilities/`scope` 解码 | AC-M3-9（复现）；AC-M3-3（读面） | 清单确定性；旧 v2 快照可读；v1 拒新 dataset/scope；三入口同结果 | PR-7 |
| **PR-10** 历史 ST 写入器（#308） | 深 `SSGSGMXX/tab2` ST 区间 + 沪公告索引状态事件；GP29/行情锚定标 `OWNER_RISK_ACCEPTED`；沪 2000 前/北交所 `NOT_AVAILABLE` | 归 #308（M3 只接 AC-M3-9） | 夹具：ST 实施/撤销区间、同日冲突、未知生效日；不把 0 行当无事件 | PR-2；#308 门禁 |
| **PR-11** 停复牌写入器（A-3） | 沪 `GW_PL_JYTS_TFPXX` 2008 起 `confirmed`；深首期无 `confirmed`（保留 `unresolved_gap`） | 归 A-3 | 夹具：停牌起止与撤销；深市不写 `confirmed`；与 `unresolved_gap` 不双计 | PR-2 |
| **PR-12** 简称写入器（S-6） | 深 `SSGSGMXX/tab2` 区间；沪/北 `unbounded`/`unknown` | 归 S-6 | 夹具：变更链、XD/XR 规范化、同日多变更、无 dated 源 | PR-2 |
| **PR-13** 文档与验收 | `docs/data-catalog.md` §3.1/§3.2/§3.3、`docs/STATUS.md`、`docs/design.md`（§13.2）、验收报告 | AC-M3-1…10 | 文档事实检查脚本；私有栈验收证据贴回 #446 | PR-3…12 |

### 14.1 独立 Issue（不计入实现 PR）

| ID | 任务 | 说明 |
|---|---|---|
| AC-M3-8 可复现快照 | 完整快照规格与跨机重放 | 需文件集/字节布局/命令；本设计只保证导出可重现，不承诺外部环境逐位重放 |
| 证监会行业分类评估 | §5.7 的五项前置 | 先做来源取证，不写实现 |
| ETF（D-28） | M3-2 | 身份名单、精度、cat11；与本设计的 scope 词汇不冲突 |

### 14.2 需 owner 裁决

| Q | 问题 | 本设计的默认 |
|---|---|---|
| Q1 | 行业生效日取 `last_modified` 还是获取时间 | `last_modified` 优先，缺失取获取时间，偏差标 `basis` |
| Q2 | 归档快照与仓库内 2026-09-16 文件能否回填 | A/B 可回填；C 只交叉核对（除非另批） |
| Q3 | 退市股首采用 `start_collection` + `selection.kind=scope` | 是；预算 500 |
| Q4 | 派生事实缺失时退役「拒绝」还是告警 | 拒绝（可显式豁免） |
| Q5 | `CatalogScope` 新值只允许主表解析入口 | 是；catalog 拒绝 |
| Q6 | ETF 是否与 M3 同批 | 不同批，另拆 |
| Q7 | 冻结快照字节存文件还是 DB BLOB | 内容寻址文件 + 表索引 |
| Q8 | 五个使用方是否同批 | 共用入口先落地，五个写入器各自 PR |

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
- 私有栈验收（不走 CI）：7 份行业快照回填 → 6 个边界；338 个退市代码首采；退役门禁在真实 generation 上拒绝/放行；退市股序列跨代摘要不变；耗时/磁盘与估算对账。

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

### 16.3 待确认

见 §14.2（Q1–Q8）。审核时只需确认或否决默认方案。

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
