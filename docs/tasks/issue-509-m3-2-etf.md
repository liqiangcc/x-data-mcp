# M3-2：ETF 日线收录设计（严谨回测级）

- **状态**：设计（docs-only）。实现按 §12 拆分，逐 PR 提交；本文件不含实现代码。
- **Issue**：[#509](https://github.com/liqiangcc/x-data-mcp/issues/509)。
- **依据**：D-28（ETF 是 `market.daily` 的证券类型、精度按序列、只交付不复权 + cat1/cat11）、
  D-62 Q6（ETF 另拆 M3-2）、D-20/D-25（一手官方来源、逐个准入）、D-30（严谨回测级、偏差如实写）、
  [#246](https://github.com/liqiangcc/x-data-mcp/issues/246)（数据范围）、
  [M3 设计](issue-446-m3-delisted-industry.md) §6（共用交易所来源入口）、§9.4（来源库）、§13（ETF 结构差异）、
  [数据目录](../data-catalog.md) §3.2 #13 与「ETF 的取证结论」段、XDM-15 取证报告（仓库外，基于已退役代 `c184e9c4`）。
- **输入 pin（本文件所有数字的输入，2026-11-03 只读复核）**：
  active generation `5ac3ef2b3ddc710562eff9e9640496fa5c4be89455ee0aa5ee1d5882b77178ba`
  （`active-generation.json`，activated 2026-11-02T08:39:19Z，previous `f2fd6fcb…`）；
  `snapshot-manifest.json` 三个官方文件：`base.zip` 16,612,587 B / `2e740d65…de4c`、
  `gbbq.zip` 6,152,987 B / `a8c63e77…78b6`、`hsjday.zip` 551,187,688 B / `623c1e39…60da`。
  交易所原始响应共 16 份（17 次请求，1 次被对端重置），全部 200，URL/时间/sha256 见仓库外证据清单
  `manifest-20261103.json`；两个名单响应的 sha256 前缀：SSE `FUND_LIST` `ee9e4c2e…`（694,715 B）、
  SZSE `1945` `7e265cc3…`（35,959 B）；SSE `ebs` 报价锚是 `72224d4c…`（73,121 B）。
- **证据分级**：标「实测」的都有命令或上面的 sha 可复算；标「推断」的不是结论，不得当作门禁依据。

---

## 0. 结论摘要

1. **身份**只能来自交易所 ETF 名录（现行 1,717 只 = 沪 947 + 深 770）。**SSE `FUND_LIST` 与 SZSE `1945` 已于
   2026-11-03 由 owner 批准为身份来源（D-70 Q1）；`ebs` 只是报价锚，不是身份来源**。代码段不能当身份：
   本代 `gbbq` 里 `sh519xxx`（61 只 LOF/开放式）与 `sh550xxx`（11 只 1990 年代基金）都有 cat1/cat11 事件，
   但它们不在 ETF 名单里；前缀只是本文件的**发现口径**，实现必须用名单成员集。
2. **`.day` 单位**已在当前代复核：沪 861/861 收盘 `raw/1000` = 官方前收，深 558/558 OHLC
   `raw/1000` 精确相等，`raw/100` 命中 0；ETF 量是**份**，3,819 根「手日」bar（61 只）按标记
   `0xC36400xx` + 量额一致性全部识别，按「分」口径 0 根通过。价格精度与量纲的改动点集中在
   `official_market.rs` 与 `SeriesShape`，**schema 26 已经支持** `instrument_class=2`、
   `price_scale=3`、`volume_quantum`。PR-B 实施时发现因子事实要同时存 cat11 与逐行
   `basis_status`，而 schema 26 的 `adjustment_events.category` 只允许 1，因此追加了
   `027_etf_factor_events.sql`（fresh compact schema 27），见 §10.1 的实施注记。
3. **缺口**：`hsjday` 有、现名单无的 **128** 只（候选退市，全部有 `base.dbf` 行）；名单有、`.day` 无的
   **86** 只（全沪市 526/551/589 段，而 `hsjday` 这三段**各 0 成员**，同期 2026 年上市的 306 只都在，
   所以不是「新上市滞后」，是源覆盖缺段）。这两类都不能静默处理（§4）。
4. **首期交付建议**：只交付**不复权 + cat1/cat11 因子事实行**；ETF 前复权延后（§8）。
   依据：cat11 只有 3 个事件用官方前收锚定；cat1 内部一致性 925/967，且至少 1 个事件
   （`sz159922` 2024-12-02）表现为 2.5:1 拆分却记在 cat1 —— 直接套现有 QFQ 链会造出错误历史。
5. **读取**：ETF 序列的可见性不是"新 dataset"，但现有读面把 `market.daily` 写死成
   (前复权, 股票)（`src/sqlite_supply.rs:486-505`），必须改成按**实体**解析品种；同时 `market.daily`
   目前没有口径参数，ETF 只有不复权序列，因此需要 owner 在 Q4 决定读取口径的表达方式；
   **ETF 在未指定口径时必须显式报错（fail-closed）**，不能静默返回不复权或空结果。
6. **A 股字节不变**是可执行的验收项：改动前后 16,354 条 `market_series` 的 `version_id`、
   5,561+5,561+9+5,223 条序列的 digest、以及 A 股导出文件 sha256 必须逐一相同（§13 AC-M3-2-2）；
   2026-11-05 线上对账的结论见该行（11,116/11,122 逐条相同，6 条由 11-05 除权事件解释）。
7. **SLO**：ETF 加入会把同代全量 `update_data` 的 item 数从 5,238 推到约 6,869。按 preview.42 同代
   实测的 109.5 ms/item 外推 ≈ **752s**，超过第 1 行 600s hard 门禁（**推断**，未实测）。
   设计不把外推写成结论：§11 给 A/B 测量方法，Q8 决定「重新基线 / ETF 独立批次预算 / 先优化」。
8. **退市 ETF 的官方身份**由 D-71 的两个公告索引提供（候选 ∩ 公告命中，128/128），精确退市日由
   D-72 的公告 PDF 一次性提取结果提供；更早的沪市公告（2006-08 前）、深市"退市且不在 `hsjday`"的
   成员仍未验证，必须写进数据目录 §3.3。

推荐裁决（owner 逐项确认，未批准不写进 `docs/decisions.md`）：**Q1 先准入两份交易所 ETF 名单（SSE `FUND_LIST` 作身份/上市日，
SZSE `1945` 作身份；`ebs` 只作报价锚）**；Q2 退市名单来源先取证、期间标 `unknown`；Q3 只不复权；
Q4 增 `adjustment` 参数（缺省 qfq，ETF 未指定时显式报错）；Q5 不进 `securities.catalog`；Q6 首期不出年线；
Q7 导出 v4 加 `instrument_class` 列（可后置）；Q8 先实测再定 SLO；Q9 ETF 写 `volume_quantum=100`、A 股保持现状；
Q10 升 `generic-data-v10`；Q11 类型/指数先随快照留证、不建数据集；Q12 夹具合成、不提交交易所原始字节。

---

## 1. 范围与非范围

### 1.1 本设计包含

- 沪深 ETF 名单的**来源准入证据包**、与 M3 §6 共用入口/来源库的接线方式（不另起抓取链路）。
- ETF 身份写入 `security_master`（`instrument_class=2`）、上市日来源、退市日的 `unknown` 语义。
- `.day` → `market.daily` 的不复权序列：价格精度、成交量/成交额单位、手日还原。
- 成员集与读取：`CatalogScope` 新值（主表解析）、对 A 股目录/`universe`/读面的影响。
- cat1/cat11 事实行与证据分级；前复权是否/何时交付的判断条件。
- 年线、研究快照导出、MCP 契约与能力修订的取舍。
- schema/部署（D-08）、首采入口与预算、日更与 SLO 的实测方案、PR 拆分与验收。

### 1.2 不包含

- ETF 成分股、跟踪指数全量映射、行业归属（ETF 无 TDX 研究行业；`tdxhy.cfg` 0 行、`block*.dat` 0 成员）。
- LOF、封闭式基金、REITs（这些不在 ETF 名单里，见 §3.5）。
- 基金净值/份额/规模数据的生产化（`base.dbf` 有 `ZGB`/`TZMGJZ`，但本批不做数据集）。
- 北交所、B 股、分钟线、财务数据。
- 生产部署与 release（按 D-08 另走部署窗口）；本文件只给上线/回滚点。
- 交易所 ETF 类型/跟踪指数的**生产数据集**（名录快照里已有 `subClass`/`INDEX_CODE`，但落库模型另议，Q11）。

---

## 2. 现状事实

### 2.1 只读实测（active generation `5ac3ef2b…`）

| 项 | 值 | 来源/命令 |
|---|---|---|
| 交易所现行 ETF | **1,717**（SSE 947 + SZSE 770） | SSE `ebs` 名单 `date=20261102`、`total=947`；SZSE `1945` xlsx 770 行（sha 见文件头） |
| `hsjday` 成员 | **12,233** 个 `.day`（12,232 个六位数字代码 + 1 个 `sz200b07`），**29,777,317** 条记录；其中 ETF 段 1,759 个成员 1,638,888 条、其余 28,137,516 条、`sz200b07` 913 条。生产库 `series_bars` 36,155,908 行是「股票两口径 ×2 + 指数 + 年线」的口径，不能与 `.day` 记录数直接相减 | `zipfile` 中央目录 + 记录数 |
| ETF 段成员（发现口径 `sh 51/52/53/55/56/58` + `sz 158/159`） | **1,759** | 同上 |
| 名单 ∩ `.day` | **1,631**（沪 861 / 深 770） | 名单 × 成员对账 |
| `.day` 有、名单无 | **128**（`sz159` 46、`sh510` 18、`sh512` 18、`sh515` 14、`sh511` 10、`sh517` 7、`sh516` 5、`sh560` 5、`sh513` 2、`sh561` 2、`sh562` 1） | 同上 |
| 名单有、`.day` 无 | **86**（`sh526` 7、`sh551` 12、`sh589` 67） | 同上 |
| `hsjday` 在这三段的成员数 | 526=0、551=0、589=0（对照 588=84、520=47、530=14） | 中央目录 |
| `hsjday` 覆盖的 2026 年上市 ETF | **306**（最新 `sz158065`/`sz158079`，`base.dbf` `SSDATE=20261102`，`.day` 有 11-02 bar） | 名单 × `base.dbf` × `.day` |
| 上市日（`base.dbf` `SSDATE`）与 `.day` 首根 bar | 1,624 相同、7 例 `SSDATE` 晚于首 bar、0 缺失 | 同上；7 例：`sh511600/511800/511910/511920/511960/511970/511980` |
| 官方上市日（SSE 名录） | `FUND_LIST.listingDate` **947/947** 有值（2005-02-23…2026-10-16），与 `base.dbf` 的 `SSDATE` **947/947 相同**；与 `.day` 首根 bar：854 相同、7 例上市日晚于首 bar、86 只无 `.day` | 实测（名录原始响应已冻结） |
| 价格缩放（沪） | **861/861** 收盘 `raw/1000` = SSE 名单 `prev_close`（2026-10-30 收盘）；`raw/100` 命中 **0** | 名单 `prev_close`（日期 20261102，前收即 10-30） |
| 价格缩放（深） | **558/558** OHLC `raw/1000` 与 SZSE 2025-10-31 官方日行情精确相等；`raw/100` 命中 **0** | SZSE `1815_stock` 2025-10-31（558 只 ETF 段成员） |
| 成交额 | **557/558** 与官方「万元×1e4」一致（±1000 元）；1 例 `sz159770` 与成交量同源不一致 | 同上 |
| 成交量 | 557/558 与官方「万份×1e4」一致（±100 份）；1 例 `sz159770` 差 5,000,000 份（1.68%，未解释） | 同上 |
| 手日 bar | ETF 段 1,638,888 根中 **3,819 根 / 61 只** 带 `0xC36400xx`；3,819/3,819 通过量额一致性（按 `raw/1000`）；按 `raw/100` 判定 **0** 根通过 | 逐 bar 扫描（`low×1000 ≤ amount/(vol×100) ≤ high×1000`） |
| 手日阈值 | 「份」bar 最大 4,289,510,575；「手」bar 隐含份数最小 4,290,271,400（区间内，精确常量未定） | 同上 |
| A 股既有手日 | 非 ETF 成员 24 根 marker；其中 **19 根**与「分」口径一致（`000725` 11、`600157` 2、`601988` 2、`600010/600221/601288/601868` 各 1，与 XDM-15 一致）；另 5 根属 `sz162411`（LOF，非目录成员，现行实现 fail-closed） | 同上 |
| gbbq（ETF 名单成员） | 331 只带事件：cat1 974 条 / 148 只，cat11 250 条 / 217 只；另有 13 条 cat11 落在无 `.day` 的成员（551 段等） | 解码 `gbbq.zip!gbbq`（207,536 条；密钥表取 `src/gbbq.rs`，锚 `sh.600000` 2008-04-24 = `[1.6,0,3,0]`） |
| cat11 内部一致性 | 176 个事件有当日 bar，**175** 满足 `前收/开盘 ≈ 1/c3`（±6%），1 例外 `sz159830` 2024-12-20（`c3=0.93584`，价格未变） | 同上 |
| cat11 事件位置 | 60 条在首根 bar 之前（货币 ETF 的 `0.01` 折算等）、13 条成员无 `.day`、1 条事件日无 bar | 同上 |
| cat1 内部一致性 | 967 个事件有当日 bar，**925** 满足 `开盘 ≈ 前收 − c1/10`（±1.5%），42 例外（部分为 >1.5% 的正常跳空，部分未解释，见 §8.3） | 同上 |

**样本对照（官方前收，2026-11-03 复核）**：

| 事件 | gbbq | `.day` 前一根收盘 | 官方前收 | 判定 |
|---|---|---|---|---|
| `sz159919` 2025-03-31 cat1 | `c1=0.661` | 4.111（2025-03-28） | 4.045（SZSE 1815） | 4.111 − 0.661/10 = 4.0449 → `c1`=每 10 份派现（元） |
| `sz159901` 2021-04-12 cat11 | `c3=2.0` | 7.411（2021-04-09） | 3.706 | 7.411/2 = 3.7055 → 价格因子 `1/c3` |
| `sz159901` 2010-11-22 cat11 | `c3=5.0` | 3.966（2010-11-18） | 0.793 | 3.966/5 = 0.7932 |
| `sz159338` 2025-09-18 手日 | 无事件 | 1.158（2025-09-17） | 量 429,379.50 万份 | `.day` 42,937,950 × 100 = 4,293,795,000 份，`r7=0xC3640053` |

### 2.2 生产库与容量基线（只读，`mode=ro`）

| 项 | 值 |
|---|---|
| 库文件 | `state/preview.sqlite` **2,017,660,928 B**（schema 26，`user_version=26`，WAL 已 checkpoint） |
| `series_bars` | **36,155,908** 行；`dbstat` **1,782,435,840 B** → **49.30 B/bar**（含索引） |
| `market_series` | **16,354** 行（日线不复权 5,561 + 日线前复权 5,561 + 指数 9 + 年线 5,223）；**4,804,608 B** → **293.79 B/series** |
| 目录/主表 | `catalog_members` 5,225、`security_master` 5,565 |
| 部署目录 | `/opt/x-data-mcp-preview` 全树 **6,300,018,421 B**（SLO 第 5 行 ≤8GiB，余量约 1.35GiB） |

### 2.3 代码事实（`file:line`，当前 worktree）

| 位置 | 现状 | 对 ETF 的含义 |
|---|---|---|
| `migrations/tasks/022_research_data.sql:29-37` | `instrument_class IN (0,1,2)`；`price_scale BETWEEN 0 AND 6` | ETF 类与 3dp **已允许**，无需 migration |
| `migrations/tasks/022_research_data.sql:70-90` | `series_bars.*_units`（无单位名）+ `volume_quantum` | 列名已中性；手日精度列已存在 |
| `migrations/tasks/022_research_data.sql:106-108` | `yearly_series_lineage.base_instrument_class IN (0,1,2)` | ETF 年线在存储层允许（但读/派生仍限股票，见下） |
| `migrations/tasks/004_catalog.sql:5-11` | `catalog_members` **没有** `instrument_class`，`catalog_head` 单例 | ETF 若进目录需要加列 + 迁移；不推荐（§5.3） |
| `src/compact_market.rs:15-22` | `MAX_PRICE_SCALE=6`、`PRICE_SCALE_CENTS=2`、`PRICE_SCALE_QFQ=6` | ETF 不复权需要 `PRICE_SCALE_LI=3` 一类的显式常量 |
| `src/sqlite_market/series.rs:60-95` | `SeriesShape::stock()` 映射不复权→2、前复权→6；`INDEX_UNITS` | 需要 `SeriesShape::etf()`（不复权→3） |
| `src/sqlite_market/series.rs:116-121` | 解码允许 class 0/1/2 | ETF 类通过解码，属预留落点 |
| `src/official_market.rs:108-118` | `normalized_volume`：marker + 一致性；不一致 → `None` | ETF 手日会因下一行口径不符而整体 fail-closed |
| `src/official_market.rs:173-190` | `hands_consistent` 用 `raw/100` 判 `[low, high]` | ETF 需 `raw/1000`；否则 3,819 根手日全部判不一致 |
| `src/official_market.rs:1247-1259` | `raw_bars` 把 OHLC 写成 `Decimal::new(raw, 2)` | ETF 必须写 3dp；否则是**静默 10× 错价**（2dp 在 scale=3 下仍可表示，`units()` 不会拒绝） |
| `src/official_market.rs:1375-1399` | 单位证据写死「fen integers」「volume is u32 shares」 | ETF 需按品种给证据（厘/份 + 手日 100 份精度），否则年线闸门会拿错依据 |
| `src/qfq.rs:81-86` | `raw_price=/100`、`cent()` 2dp（legacy baseline） | 不可用于 ETF；生产路径是 `forward_adjust_exact(…, 6)`（`src/qfq.rs:306-310`） |
| `src/sqlite_supply.rs:486-505` | `dataset_shape`：`market.daily/yearly` = (前复权, 股票) | ETF 序列对 `market.daily` **不可见**；plan/run、版本列表、availability 共用 |
| `src/sqlite_supply.rs:511-540` | `dataset_head` 按 dataset shape 取 head | 同上 |
| `src/sqlite_supply.rs:541-550` | `is_invisible_version` 按 dataset shape 判定 | ETF 版本会被当成"不可见" |
| `src/sqlite_supply.rs:2145-2146`、`:2763-2780` | availability/`series_stats` 校验 shape | 写入后回读会报 mismatch |
| `src/sqlite_market/series.rs:400-416` | `read_public_series` = (前复权, 股票) | `query_data`/`list_data_versions` 的唯一可见性谓词 |
| `src/sqlite_market/series.rs:418-440` | `read_visible_series` 已参数化 | 可复用为 ETF 读取 |
| `src/sqlite_market/series.rs:1157-1175` | `yearly_state` 要求 `base_adjustment=Forward ∧ base_instrument_class=Stock` | ETF 年线即使存在也会被判 integrity 错 |
| `src/research/read.rs:199-230`、`:983-1000` | 导出/退市计数只取 `instrument_class=Stock` | ETF 不进导出（除非改） |
| `src/research/parquet.rs:421`、`:892` | 日线/指数过滤 | 同上 |
| `src/research/mod.rs:546-558` | `DAILY_COLUMNS` 无 `instrument_class` | 同一文件混入 ETF 后消费者无法区分（§9.3） |
| `src/data_supply/collect.rs:320-336` | `candidates` 只收 `instrument_class==0` | ETF 首采需要 class=2 分支 |
| `src/data_supply/collect.rs:145-155`、`:296-306` | catalog 只接受 `current_sh_sz_a`；listed-ever 只接受 `sh_sz_a_listed_ever` | 新 scope 值需要显式分支 |
| `src/data_supply/delisted_series.rs:250-258` | 退市跟踪只对 class=0 | ETF 退市跟踪需另设（本批不做，见 §4） |
| `src/data_supply/wire.rs:409-429` | `CatalogScope` 两个值 + schemars 枚举 | 新值会进 `tools/list` schema（§9.4） |
| `src/data_supply/query.rs:265-290` | `universe_scope` 只接 `sh_sz_a_listed_ever` | ETF universe 需要扩这一处 |
| `src/sqlite_market/calendar.rs:135-150`、`:230-245` | 日历来源只读 (股票, 不复权) | ETF bar 不会进交易日历推导（可接受，需写明） |
| `src/security_master.rs:1252-1256` | `instrument_class ≤ 2` | ETF=2 已通过校验 |
| `src/data_supply/columnar.rs:29` | `DAILY_FIELDS` = open/high/low/close/volume/amount | 读面不回 `price_scale`；值本身是 decimal，无需 scale 才能读（§9.2） |

> 数据目录 §3.2 #13 与 XDM-15 报告引用的是 `compact_market.rs:812-822` 的 `cents()`；该函数在当前代码里
> 已不存在（schema 22 起改为 `*_units` + `price_scale`）。本文件按当前代码重列，§16 记录这条陈旧引用。

---

## 3. 身份与名单（新来源准入证据包）

### 3.1 入口与字段（2026-11-03 实测）

| 来源 | 请求（一次拿全量） | 行数 | 字段 | 分页 |
|---|---|---|---|---|
| **上交所 ETF 名录（推荐作身份来源）** | `query.sse.com.cn/commonSoaQuery.do?isPagination=true&pageHelp.pageSize=2000&pageHelp.pageNo=1&sqlId=FUND_LIST&fundType=00&subClass=01,02,03,04,05,06,07,08,09,30,31,32,33,34,35,36,37,38` | **947**（`pageHelp.total=947`，`pageCount=1`） | `fundCode`、`secNameFull`、`fundAbbr`、`companyName`、`fundManager`、**`listingDate`**、**`subClass`**、`INDEX_CODE`、`INDEX_NAME`、`fundType` 等 | `pageHelp.pageNo/beginPage/endPage/pageSize`；页面默认 25/页（≈38 页），`pageSize=2000` 一次取完（实测）；超出页大小边界未验证 |
| 上交所 ETF 报价快照（**只作价格锚，不作身份**） | `yunhq.sse.com.cn:32041/v1/sh1/list/exchange/ebs?select=code,name,open,high,low,last,prev_close,volume,amount&begin=0&end=999` | 947（`total=947`，`date=20261102`，`time=162900`） | 代码、简称、开/高/低/现价/前收、成交量、成交额 | `begin/end` 单请求取全量；`end` 大于 `total` 时不报错，按 `total` 截断 |
| 深交所 ETF 列表（推荐作身份来源） | `www.szse.cn/api/report/ShowReport?SHOWTYPE=xlsx&CATALOGID=1945` | 770 | 证券代码、证券简称、**拟合指数**、当前规模（份）、基金管理人 | 单 xlsx，无分页参数；**需要 `Referer` + 浏览器 UA**（不带 `Referer` 时连接被重置，实测一次） |
| 上交所单只基金基本信息（历史规模，非首期） | `query.sse.com.cn/commonQuery.do?sqlId=COMMON_SSE_ZQPZ_ETFZL_ETFJBXX_JJGM_MOREN_L&SEC_CODE=<code>` | 1 行/代码 | `ETF_TYPE`（单市/跨市/跨境）、`STAT_DATE`、`SEC_NAME`、`TOT_VOL`（万份）、`FUND_EXPANSION_ABBR` | **逐只请求 947 次**；类型/指数已由名录查询一次给出，本接口只用于历史规模，**不作首期依赖**（Q11） |

- **`ebs` 不是基金名录**：它是 `yunhq` **实时行情快照接口**的"分类列表"（返回当日/前一交易日 OHLC 与前收），
  `ebs` 的类别口径**没有官方说明**（接口文档与页面都没有给出类别定义）。本次只用它做价格/成交量锚
  （§2.1 的 861/861 前收比对），**不得作为 ETF 身份依据**。
- **名录查询才是身份依据**：ETF 列表页 HTML（2026-11-03T06:07:34Z 实测，26,816 B）引用
  `/xhtml/home/2021public/querySearch/search_fund_2021.js`；该页 JS 的 `etfList` 参数（保存副本，XDM-15 2026-10-26）
  指向 `commonSoaQuery.do?sqlId=FUND_LIST`。实测该查询返回 **947 行**、`pageCount=1`：
  `listingDate` **947/947**、`subClass` **947/947**、`INDEX_CODE`/`INDEX_NAME` **915/947**
  （缺的正好是 24 只交易型货币 ETF 与 8 只黄金 ETF，符合"没有跟踪指数"）。
- **成员集一致**：`FUND_LIST` 与 `ebs` 的代码集**完全相同**（947=947，双向差集为空）→ 用 `FUND_LIST`
  替换 `ebs` 作身份来源**不改变成员集**，但多了官方上市日期、ETF 类型与跟踪指数。
- **上市日**：沪市取 `FUND_LIST.listingDate`（与 `base.dbf SSDATE` 947/947 相同）；深市 `1945` 没有上市日，
  仍取 `base.dbf SSDATE`（D-47）。
- **类型字段**：`subClass` 的取值→中文名映射来自页面 JS（01 单市场股票、02 单市场债券、03 跨市场股票、
  05 交易型货币、06 黄金、08 跨市场股票(沪深港京)、09 单市场科创板、31 跨市场(含科创板)、32 跨市场债券(沪深)、
  33 跨境、37 现金申赎类债券），**页面映射不是独立发布的分类文档**，入库要标来源与版本。
- **更新频率/历史**：两所页面都没有声明频率，也没有版本号/历史快照。快照语义按「`fetched_at` 时的当前成员集」
  记录（同 M3 §5.2 的 `effective_from=last_modified 或获取时间`，这里没有 `last_modified`，取获取时间并标
  `basis=fetch_time`）。日更只做"当前名单刷新"，历史成员靠快照积累（§4.2）。

### 3.2 与 M3 §6 共用入口和来源库的接线（不另起抓取链路）

```text
source_refresh（已有 R2 出站最小间隔 1s、冻结、清单）
  → source_id = sse_etf_list / szse_etf_list
  → freeze（原始字节 → sha256 → source-store/blobs + manifest.jsonl）
  → validate（HTTP、xlsx/JSON 形状、SSE total==rows、SZSE 行数下界、代码唯一）
  → parse（parser_version = etf-list-v1）
  → etf identity writer（security_master class=2 + 名单成员事实）
```

- **不新增抓取链路**：两个身份来源 URL（`FUND_LIST`/`1945`）注册进现有交易所来源入口（`refresh_exchange_sources` 的 `sources` 集合），
  复用同一重试/分页/冻结/错误语义；`exchange_source_snapshots` 只加索引行，不存原始字节。
- **来源分档：已准入（D-70 Q1，2026-11-03）**。owner 2026-11-03 批准 SSE `FUND_LIST` 与 SZSE `1945`
  为 ETF 身份来源（`APPROVED_SOURCE`；本地个人研究，再分发 `NOT_AVAILABLE`，D-20/#71）。准入边界：
  `ebs` **只作报价锚、不是身份来源**；Q2 的"终止上市基金名单"**只批准取证路线，不等于准入该来源**
  （取证结果另报 owner）。证据包状态见下表，逐项可按 sha256 复算。
- **证据包状态**（2026-11-03 实测；原始字节已按 §3.2 的 freeze 流程冻结进本任务证据清单）：

| 来源 | 成员集 | 官方字段 | 分页 | 历史/更新频率 | 冻结证据 | 再分发 |
|---|---|---|---|---|---|---|
| SSE `FUND_LIST` | 947（= ebs，无差集） | `fundCode/secNameFull/fundAbbr/companyName/fundManager/listingDate/subClass/INDEX_CODE/INDEX_NAME` | `pageHelp`；`pageSize=2000` 一次取完（默认 25/页） | 无历史版本、无 `last_modified`、未声明频率 | 原始 JSON 694,715 B，sha256 见证据清单（`sse_fund_list_20261103.raw.json`） | `NOT_AVAILABLE`（本地个人研究，D-20/#71） |
| SZSE `1945` | 770 | 证券代码/简称/拟合指数/当前规模（份）/基金管理人 | 单 xlsx，无分页 | 同上 | 原始 xlsx 35,959 B，sha256 见证据清单 | 同上 |
| SSE `ebs`（仅报价锚） | 947 | code/name/OHLC/prev_close/volume/amount | `begin/end` | 实时快照；类别口径无官方说明 | 原始 JSON 73,121 B，sha256 见证据清单 | 同上 |

- **已批准来源组合**（D-70 Q1）：SSE 身份用 `FUND_LIST`（`source_id=sse_etf_list`），SZSE 身份用 `1945`
  （`source_id=szse_etf_list`）；`ebs` 只作价格/成交量锚（不注册为身份来源，或注册为 `sse_etf_quote`
  但只进证据、不进成员集）。
- **上市日来源**：沪市 ETF 用 `FUND_LIST.listingDate`（`listing_source=ExchangeList`）；深市 ETF 用
  `base.dbf SSDATE`（`listing_source=TdxSsdate`，D-47）。「当前成员」是快照事实，不是"历史成员"；
  再分发 `NOT_AVAILABLE`，公开仓库不提交原始字节（§12 测试夹具同此约束）。
- **重放**：ETF 身份写入器必须是"冻结字节 → 行"的纯函数（输入 = 快照 sha256 + `parser_version`），
  接入 `replay --scope exchange` 同一事务；`--verify-only` 用同一摘要对账。这样 D-08 的
  fresh-init 不需要重新抓取（交易所名单会变，重抓得到的是新事实）。
- **失败语义**：形状/计数不符 → `source_invalid`（不写 0 行）；网络失败 → 该 item 失败、旧身份仍可读；
  名单为空 → 失败（0 行不是证据）。

### 3.3 成员集如何进入主表（`security_master`）

- 每个名单成员写一行：`exchange/code`、`instrument_class=2`、`listing_date=base.dbf SSDATE`、
  `listing_source=TdxSsdate`（枚举见 `src/security_master.rs:223-227`）、`delisting_date=NULL`
  （无官方退市日，按 `unknown` 语义，不猜）、`issuer_key` 按基金代码建、`issuer_resolution=Unique`。
- `effective_from` = 该成员第一次出现在已冻结名单快照的 `fetched_at`；`known_at` = 同一时间
  （快照事实的知晓时间；不早于入库时间，符合 M3 §13.2 第 3 条）。
  > **已由 D-80（owner 2026-11-06）修订 `effective_from`**：有 `listing_date` 时 `effective_from` 取
  > 上市日，`known_at` 仍是首次观察日；没有 `listing_date` 时保持上面的取值。原因：首次观察日
  > 把成员区间起点顶后（vm2 preview.45 上 1,846 个 ETF 行有 1,845 行 `effective_from` 晚于
  > `listing_date`，1,718 行是 2026-11-05），`universe_intervals` 的 ETF 成员在此日期之前基本为空。
  > 上市日晚于区间终点时（矛盾输入）该行保持首次观察日，不让整张主表写入失败。取值由
  > `assembly::member_effective_from` 一处给出，日更与 `source_store replay` 共用。
- 名单移除 ≠ 退市：快照只证明"某日不在名单"，不证明退市日；`delisting_date` 保持 `NULL`，
  该成员保留 class=2 但不再出现在 `current` 成员集里（§4）。
- 统计口径：`security_master` 行数会从 5,565 增加到约 5,565 + 1,717（若只入当前名单；退市未知的不入）。
  A 股行本身不变（AC-M3-2-2 只约束 A 股序列与导出，不约束主表行数）。

### 3.4 上市/退市日

- 上市日：沪市取 `FUND_LIST.listingDate`（947/947，`listing_source=ExchangeList`），与 `base.dbf SSDATE`
  947/947 一致；深市 `1945` 没有上市日，取 `base.dbf SSDATE`（`listing_source=TdxSsdate`，D-47）。
  `.day` 首根 bar 早于官方上市日的 7 例（`sh511600/511800/511910/511920/511960/511970/511980`，均早 10–18 天）
  保留官方上市日并进审计清单，不用首 bar 覆盖；86 只名单有、`.day` 无的成员照写上市日、序列记 `absent`。
- 退市日（现名单内成员）：`NULL`（`unknown`）。交易所有"终止上市基金"公告/名单，但本任务未取证，
  不能假设 A-2 的股票终止名单覆盖基金（A-2 明确过滤 `STOCK_TYPE`，D-46）。

### 3.5 与 LOF、封闭式基金、REITs 的区分

- 规则只有一条：**在 `ebs` 或 `1945` 名单内 = ETF**；其余场内基金即使代码段相邻也不是。
- 反例（实测，本代 gbbq）：`sh519xxx` 61 只（LOF/开放式）与 `sh550xxx` 11 只（1990 年代老基金）
  都有 cat1/cat11 事件、都不在 ETF 名单；`sz16x`=LOF、`sz18x`=封闭式、`sh501/502/505/506`=LOF、
  `sh508`=REIT、`sh500`=封闭式，全部不在名单内（名单前缀实测只有
  510/511/512/513/515/516/517/518/520/526/530/551/560/561/562/563/588/589 与 158/159）。
- 因此：`instrument_class=2` 只由名单成员产生；不得用前缀/名称/行业码推断。数据目录 §3.3 需写明
  "LOF/封闭式/REIT 未收录、代码段相邻不构成身份"。

---

## 4. 退市 ETF 与两个缺口

### 4.1 `hsjday` 有、现名单无（128 只；D-71/D-72 已准入）

- 事实：128 只全部有 `base.dbf` 行（`SSDATE` 齐全），末根 bar 分布 2015–2026（2026 年 5 只，最新
  `sh512390` 2026-10-26）；**没有一只**在本代最后一根 bar（2026-11-02）上有成交。
- 但"不在现名单 + 末根 bar 较早"只是**候选**，不是官方退市事实；身份仍然只能由交易所披露锚定。
- **D-71（owner 2026-11-03 批准）**准入两个**公告索引**作为退市 ETF 身份 + 公告日来源，但它们
  **不是结构化终止上市名单**：
  1. 沪 `query.sse.com.cn/commonQuery.do?sqlId=COMMON_PL_JJXX_JJGG_L`，`TITLE=终止上市`，
     按 `START_DATE/END_DATE` 分段；分页参数无效（回显但不改变行集，`pageHelp.total=0`、
     `pageCount=null`），所以不能靠信封判断完整性，一次响应即该窗口全量；
  2. 深 `www.szse.cn/api/disc/announcement/annList`（POST，`channelCode=["etfNotice_disc"]`，
     `stock` 数组，`pageSize` 实际上限 50）；公告日取 `publishTime`，`attachPath` 里的日期常早一天、
     不使用；同一公告按 URL 去重。
- **身份规则**：候选 = `hsjday` ETF 代码段中不在现行名单的成员；`class=2` = 候选 ∩ 公告命中
  （沪：标题含"终止上市"且含"交易型"；深：`etfNotice_disc` 频道命中）。**代码段或"hsjday 有、
  名单无"单独不构成身份**（§3.5 的 519/550 反例）。2026-11-03 实测 128/128 命中（沪 82、深 46）。
- **D-72（owner 2026-11-03 批准）**：精确退市日走方案 B——终止上市公告 PDF 原样冻结进来源库，
  一次性提取结果作为派生事实（每条带 `doc_sha256`、页码、原文片段、提取器版本、置信度）；
  **生产代码不解析 PDF**；新增公告人工确认后按同一格式补登。128/128 有官方终止上市日。
  - 主公告选择：按 URL 去重 → 去掉标题含"提示性"的 → 取公告日最新一份；只有提示性公告时回退用它
    （实测 1 只：`sz159986`；正式与提示性两版的终止上市日 7/7 相同）。
  - 校验：`公告日 ≤ 终止上市日 ≤ 公告日 + 9 天`，且终止上市日晚于该代码 `.day` 末根 bar；
    违反即 fail-closed。提取结果与 PDF 只进来源库，不提交公开仓库。
  - 落库：日更与 replay 共用 `delisted_row_values`；`effective_to=终止上市日`（排他上界，与 A 股
    一致）、`delisting_reason_class=other`。零提前量（公告日=终止上市日）时
    `effective_from=终止上市日-1`，保证区间非空且不改变退市上界。同一代码同时有名单历史事实
    与提取事实时，首次观察日（`known_at`）取最小值（起点不被往后挪）；`effective_from` 在此之后按 D-80 取上市日；有提取结果时精确终止
    上市日覆盖名单的 `first_missing`，没有提取结果时才取名单边界的最小值。
- **单调身份**：来源库里**所有已冻结的名单快照 + 退市公告取并集**，首次出现定 `known_at`（无上市日时也定 `effective_from`，D-80 修订）；
  移出名单不删行。`sh_sz_etf_listed_ever` = 并集，`sh_sz_etf_current` = 最新快照成员。这样已采集的
  ETF 序列不会因为一次名单刷新失去主表行。
- **公告日 ≠ 生效日**：`delisting_date` 只在 D-72 提取结果存在时写精确日；只有公告索引时
  `delisting_date=NULL`（unknown），公告日作为证据另存。公告里没有"最后交易日"，不编造。
- **尾部零成交 K 线（D-72）**：原始 bar 照原样保存、不删；另记"最后有成交日"（最后一根 `vol>0` 的
  bar）。零成交 bar 在读面/导出标 `no_trade`（由 `vol=0` 且 `amount=0` 推导，不加存储列）；
  回测可交易窗口上界 = 最后有成交日，证券存续上界 = 终止上市日。
- 剩余缺口如实写进[数据目录](../data-catalog.md) §3.3：沪公告索引最早 2006-08（更早的沪市终止公告
  不可得）、深"退市且不在 `hsjday`"的成员未验证、没有 D-72 提取结果的成员退市日仍 `unknown`。

### 4.2 名单有、`.day` 无（86 只）

- 事实：全部沪市，`sh526` 7 / `sh551` 12 / `sh589` 67；`hsjday` 在这三个代码段**各 0 个成员**；
  同期 `hsjday` 覆盖 306 只 2026 年上市的 ETF（含 `SSDATE=20261102` 的两只），所以不是"上市太新"。
- 结论（实测部分）：这是**源覆盖缺段**（TDX `.day` 不覆盖这三段），不是逐个成员的偶发缺失。
  为什么 TDX 不覆盖（产品口径/延迟/白名单）**未验证**，不写成结论。
- 处理：主表照写（身份由名单给出）、`market.daily` 无序列 → 读面 `absent` 且 `reason=source_member_absent`；
  首采报告里 86 只计 `absent`（预期），不得当失败、也不得静默丢；数据目录 §3.3 写"86 只现行 ETF 无日线"。
- 不能用交易所行情接口补齐：SSE `ebs` 只有当日快照（`/v1/sh1/day` 只返回当日、`line?period=day` 为空，
  XDM-15 已实测），补历史日线是另一个来源准入问题。

---

## 5. 主表与目录

### 5.1 品种与精度落点

- `market_series.instrument_class=2`（DDL 已允许）；ETF 不复权 `price_scale=3`。
- `SeriesShape` 增加 `etf(adjustment)`：不复权 → 3；前复权（若交付）→ 6（与股票前复权一致，
  理由：链式结果需要比报价更高精度，读取端按 decimal 输出，不引入新的舍入口径）。
- `series_bars.*_units` 语义由所属 series 的 `price_scale` 决定，不需要改列名/迁移。
- `volume` 单位：**份**（§7）；手日另写 `volume_quantum=100`（列已存在）。

### 5.2 上市日/退市日

见 §3.4。`SSDATE` 7 例晚于首 bar 的成员保留原值 + 审计清单；`delisting_date` 对 ETF 一律 `NULL`。

### 5.3 目录（`securities.catalog`）与 scope 词汇

- **不把 ETF 加进 `catalog_members`**（推荐，Q5）：
  - `catalog_members` 没有 `instrument_class` 列（`migrations/tasks/004_catalog.sql:5-11`），
    加列 = 新 migration（027）；`catalog_head` 是单例，无法同时挂"A 股目录"和"ETF 目录"两个 head；
  - 把 ETF 混进同一目录会让默认 `universe`（目录版本）突然包含 ETF，A 股读面的成员集随之改变
    （与"AC-M3-2-2 A 股输出不变"冲突）。
- 采用与 M3 `sh_sz_a_listed_ever` 相同的模式（D-62 Q5）：新增 `CatalogScope` 值
  `sh_sz_etf_current` / `sh_sz_etf_listed_ever`，**只**由主表解析，且只在 `market.daily` 的
  `start_collection`/`universe` 接受；`securities.catalog` 一律拒绝（`src/data_supply/query.rs:319-329`、
  `src/data_supply/collect.rs:145-155` 的同一守卫）。
- 语义：
  - `sh_sz_etf_current`：`security_master` 中 `instrument_class=2` 且当前名单快照含该代码；
  - `sh_sz_etf_listed_ever`：class=2 且**曾**出现在已冻结快照或已发布序列里（供未来退市身份准入后使用；
    首期没有官方退市身份时，其成员集 = 当前名单 ∪ 已有 ETF 序列）。
- 影响面：
  - 默认 `universe`（不填 `scope`）**不变**：仍是目录版本 = A 股；
  - `market.daily` 的 `single`/`batch` 选择按 entity，不需要目录成员资格（`canonical_entity` 只校验
    `sh|sz` + 6 位数字，`src/compact_market.rs:841-848`），ETF 实体可直接读；
  - `securities.catalog`、`list_catalog_facets`、行业相关读面不受影响（ETF 不进目录）。
- scope 命名词汇沿 M3 §13.2 第 2 条「scope 命名带类别」；不引入 `market.*` 之外的 dataset 身份（D-28）。

---

## 6. 价格精度：逐处清单与改法

> 原则：**值 = 整数 / 10^price_scale**（schema 22 起），读取端已按 series 的 scale 解码；
> 剩余问题都是"写入/校验/可见性/导出"里把 2 写死的地方。A 股路径必须字节不变。

| # | 位置（`file:line`） | 现状 | ETF 改法 |
|---|---|---|---|
| P1 | `src/compact_market.rs:15-22` | `PRICE_SCALE_CENTS=2`，无 3dp 常量 | 增 `pub const PRICE_SCALE_LI: u32 = 3;`（名称与文档同义：厘）；`MAX_PRICE_SCALE=6` 不变 |
| P2 | `src/sqlite_market/series.rs:60-95` | `SeriesShape::stock(adj)` 只产 (stock, 2/6) | 增 `SeriesShape::etf(adj)`：(class=2, raw→3, forward→6)；写入路径显式命名 shape，禁止默认套用 |
| P3 | `src/official_market.rs:1247-1259` | `raw_bars` 固定 `Decimal::new(raw, 2)` | `raw_bars(records, price_scale)`：股票 2、ETF 3。**注意这是静默 10× 错价，不是报错**：`decimal(4178, 2)=41.78` 在 `price_scale=3` 下仍可精确表示，`units()` 不会拒绝；只有超过 scale 的小数才会被拒 |
| P4 | `src/official_market.rs:1088-1096` | 指数 OHLC `Decimal::new(raw, 2)`（点） | 不变（指数仍 2dp）；只是提醒不要把它当通用模板 |
| P5 | `src/official_market.rs:173-190` | `hands_consistent` 用 `raw/100` | 参数化 `price_divisor`（股票 100、ETF 1000），否则 ETF 手日全 fail-closed |
| P6 | `src/official_market.rs:1375-1399` | 单位证据文本「fen integers」「volume is u32 shares」 | 按品种给证据：ETF `price_unit=yuan`（3dp，厘为最小变动）、`volume_unit=share`（手日 `volume_quantum=100`）；不复制"fen"文本 |
| P7 | `src/qfq.rs:81-86` | legacy `raw_price=/100`、`cent()` 2dp | 不用于 ETF；生产路径 `forward_adjust_exact(…, PRICE_SCALE_QFQ)`（`src/qfq.rs:306-310`）若用于 ETF，scale 取 6 且加 cat11（§8） |
| P8 | `src/sqlite_supply.rs:486-505` | `dataset_shape` 按 dataset 定 (前复权, 股票) | 改为「dataset + entity → shape」：ETF 实体查 `security_master.instrument_class`；`market.index` 分支不变 |
| P9 | `src/sqlite_supply.rs:511-550`、`:2145-2146`、`:2763-2780` | head/可见性/availability 都用 dataset shape | 跟随 P8 的解析函数，一处改、全路径一致（禁止各自硬编码） |
| P10 | `src/sqlite_market/series.rs:400-416` | `read_public_series` = (前复权, 股票) | 改为按 P8 解析后的 shape 调用 `read_visible_series`；A 股仍走 (前复权, 股票) |
| P11 | `src/sqlite_market/series.rs:1157-1175` | 年线 lineage 必须是 Forward+Stock | 若 Q6 = 出 ETF 年线：允许 `base_instrument_class=2` 且口径与 base 一致（首期不出，见 §9.1） |
| P12 | `src/research/read.rs:199-230`、`src/research/parquet.rs:421`、`:892` | 导出只取 Stock / Index | 若 Q7 = 导出 ETF：按 `instrument_class` 过滤扩展；列集加 `instrument_class`（v4） |
| P13 | `src/research/market_cap.rs:443-470` | `price_cents(units, scale)` 已通用，字段名/注释写"分" | ETF 市值不在范围；该函数不得被 ETF 调用（加品种守卫或注释），不改为 ETF 专用 |
| P14 | `src/sqlite_market/calendar.rs:135-150`、`:230-245` | 日历来源 = (股票, 不复权) | 保持：ETF bar 不进日历推导；在文档写明（否则会以为 ETF 也参与 `cal-union-v1`） |
| P15 | `src/data_supply/columnar.rs:29` | 读面列集无 `price_scale` | 值本身是 decimal（`to_bar` 已按 scale 解码），读不需要 scale；建议版本元数据**加** `price_scale`（可加性字段，§9.2） |
| P16 | `src/data_supply/collect.rs:320-336` | 首采候选只收 class=0 | 新 scope 分支收 class=2；不得放宽成"所有 class" |
| P17 | `src/data_supply/delisted_series.rs:250-258` | 退市检查只对 class=0 | 本批不改（ETF 退市身份未知）；写明"ETF 不参与该检查" |
| P18 | `migrations/tasks/022_research_data.sql:29-37/70-90/106-108` | class 0/1/2、scale 0..6、`volume_quantum` | 价格/量纲列**无需 migration**；PR-B 的 cat11 + 逐行 `basis_status` 另加 027（§10.1） |

**为什么现在才发现**：M1 已把价格单位从"分"改成"按序列 scale"（schema 22），XDM-15 的 `cents()`
清单（2dp 拒绝 3dp）已过期；真正剩余的是 `.day` 适配器的写入/校验、读面可见性与导出的品种过滤。

**A 股不变的可执行定义**（写成测试，AC-M3-2-2；2026-11-05 线上对账结果见 §13 该行）：
1. A 股/指数/年线 16,354 条序列中，可与 aws-node 冻结库逐条对照的 11,122 条（A 股 raw 5,561 +
   qfq 5,561）里有 11,116 条的行数与内容摘要（截断到 2026-11-04）相同；其余 6 条差异全部由 11-05
   日更新增的除权事件解释（`sh.603309`、`sh.603596`、`sz.000777`、`sz.002152`、`sz.300259`、
   `sz.301529` 的 qfq 序列内容变化）。`version_id` 只在无新增 bar 的 690 条上相同：`content_digest`
   覆盖整条序列，日更追加 bar 后必然改变，所以「16,354 条 `version_id` 逐一相同」不是可满足的判据；
2. 5,561 条不复权 + 5,561 条前复权 + 9 条指数 + 5,223 条年线的 `record_count/first_day/last_day/digest` 一致；
3. A 股导出文件（`daily_raw`/`daily_qfq`/`universe`/`calendar` 等）sha256 一致（ETF 未导出时）；
4. 抽样 `query_data` 的 A 股 K 线 JSON 逐字节一致。

---

## 7. 成交量单位（份 / 手）

### 7.1 规则

`.day` 第 7 字段对 ETF 默认是**份**；当份数达到约 **4.29e9** 时 TDX 改写为**手**，同时第 8 字段 `r7`
从 `0x10000` 变为 `0xC36400xx`（高 24 位固定，低字节逐条变化）。归一化规则（沿用 M3 已实现的
"标记 + 量额一致性"双证据，`src/official_market.rs:108-118`）：

| r7 标记 | 量额一致（`low/div ≤ amount/(vol×100) ≤ high/div`） | 结论 |
|---|---|---|
| 是 | 过 | 手日：`volume = vol × 100` 份，`volume_quantum = 100` |
| 是 | 不过 | **fail-closed**（不猜；item 失败，旧版本仍可读） |
| 否 | 过 | **fail-closed**（自相矛盾；现有实现即如此） |
| 否 | 不过 | 份：`volume = vol` |

- `div` = 10^price_scale（股票 100、ETF 1000）。本代实测：ETF 3,819 根手日按 1000 全过、按 100 全不过；
  股票 19 根按 100 全过。
- 阈值常量：实测只把阈值夹在 **[4,289,510,575, 4,290,271,400]** 份之间（`sh513330` 2026-08-11 与
  `sh513130` 2023-11-29）；`4.29e9` 是推断值。**规则不依赖阈值**：只用标记 + 量额一致性，所以常量不确定
  不影响正确性（阈值只用于解释/监控）。
- 不整除的取整：159338 的 4,293,795,000 恰好被 100 整除；是否存在非整除丢余数**未验证**（XDM-15 U-4）。
  验收时对每根手日同时检查 `volume×100` 与官方量（±100 份）并在证据里记 `volume_quantum`。
- 已知 1 例量额与官方不一致（`sz159770` 2025-10-31，差 5,000,000 份）：运行时不拿官方行情做门禁
  （`.day` 是唯一生产输入），但验收/抽查要能解释或列偏差。

### 7.2 与其他单位的关系

- 成交额：**元**（f32），558/558 与官方"万元×1e4"一致；不需要换算。
- 指数：量是**手**（既有 `index_batch` 原样保留，`src/official_market.rs:1062-1096`），不套用证券规则。
- 读面：`volume` 返回归一后的**份**；`volume_quantum=100` 只在导出/逐 bar 证据里表达精度（D-34）。

---

## 8. 复权（cat1 / cat11）

### 8.1 语义锚定现状

- 市场字节：`sh=1`、`sz=0`（另有 `2`/`99`，ETF 不用）。
- 事件集合：ETF 只有 cat1 与 cat11（本代 331 只名单成员带事件；cat1 974 条、cat11 250 条），
  没有 cat2/3/5/9 等股票类事件。
- cat1：`c1` = **每 10 份派现（元）**，价格变换 `price − c1/10`。用官方前收锚定 1 个事件
  （159919 2025-03-31，§2.1）；内部一致性 925/967（±1.5%）。
- cat11：`c3` = **份额倍数**，价格因子 `1/c3`。用官方前收锚定 3 个事件（159901 2021-04-12 `c3=2`、
  2010-11-22 `c3=5`；另 XDM-15 的 159901 之外 1 个）；内部一致性 175/176（±6%）。
- **ETF 因子存储口径（PR-B）**：cat1/cat11 的 `c1..c4` 按源 f32 的**最短往返十进制**存储
  （`0.661`、`0.93584`、`2`），`c_bits_*` 原样保留；不套用 A 股的 2dp HALF_UP
  （`gbbq-cat1-2dp-v1` 仍是 A 股口径）。ETF 集合的 `algorithm_version` 为
  `gbbq-etf-factors-f32-shortest-v1`，后续前复权直接使用这些事实值。
- cat11 的 `c3` 分布（名单成员）：`2.0` 82、`0.01` 25（货币 ETF，多在首根 bar 之前）、`3.0` 25、
  `5.0` 7、`4.0` 7、`10.0` 5、`2.5` 4、`1.5` 3、`0.1` 2，其余为单个非整数
  （0.19–0.95 / 1.1–11.8 等，多为分级/折算类历史基金）。

### 8.2 首期是否交付前复权

**推荐：不交付。** 只交付不复权序列 + cat1/cat11 事实行（`market.adjustment_events` 语义扩展或 ETF 专用
因子数据集，见 §12 PR-B）。

理由（可执行证据）：

1. cat11 只锚定 3 个事件；1 个未解释（`sz159830` 2024-12-20 `c3=0.93584`，前收/开盘 = 0.999825，价格未按 `1/c3` 变化）；
2. cat1 42/967 不一致，其中至少 1 个不是分红：
   `sz159922` 2024-12-02 `c1=1.292`，前收 6.159 → 开盘 2.461（比率 2.5026），是 2.5:1 拆分却记在 cat1；
   直接套现有"只取 cat1"的 QFQ 链会把它当成分红（价格差 0.13 元而不是 2.5 倍）；
3. 现有 QFQ 实现只取 cat1（`src/qfq.rs` 的 `forward_adjust_exact` 调用方传 `category==1` 集合），
   对 ETF 会漏掉 217 只的 cat11；
4. 货币 ETF 的 `0.01` 折算 60 条在首根 bar 之前（不影响序列），但同一机制若发生在上市后尚未验证。

### 8.3 若 owner 要求交付前复权（条件与核对方法）

公式（与股票骨架同构，参数不同）：

```text
raw_price = raw_day / 1000                      # ETF 厘 → 元
cat11: p ← p / c3                               # 份额倍数 c3（价格因子 1/c3）
cat1 : p ← p − c1/10                            # 每 10 份派现（元）
forward(t) = 对 t 之后所有事件按“先 cat11 后 cat1”的次序做仿射复合，最后一次 HALF_UP 舍入到 6dp
```

（事件同日同时有 cat1/cat11 时，以官方前收核对决定次序；本代数据里 34 只同时有 cat1/cat11，
需逐只确认是否有同日组合。）

核对方法（逐条，不抽样）：

1. 对每个 cat1/cat11 事件，取事件日 `前收` = 官方日行情（深 `1815_stock`、沪 `ebs` 的前收/当日快照；
   沪所无历史日线接口，只能覆盖到有官方快照的日期）与 `.day` 前一根 bar 收盘，计算
   `expected = prev_close − c1/10`（cat1）或 `prev_close / c3`（cat11），与官方前收比较（±1 tick）。
2. 不能核对的日期（沪市历史日线不可得、事件日无 bar、成员无 `.day`）→ 该事件标 `basis_status=OWNER_RISK_ACCEPTED`
   或 `NOT_AVAILABLE`，不得静默采用。
3. 交付前至少要求：cat11 全部 250 条中，可核对事件 100% 一致 + 不可核对事件逐条列出；
   cat1 42 条不一致全部归因（分红/拆分/日期错/市场跳空），不能归因的不进 QFQ。
4. 复权后连续性验收：对 10 只样本（含 `159901`、`159338`、`510050`、`588000`、`159919`、`159922`、
   `159830`、`510880`、`159001`、`513330`）计算事件日前后 1 日的复权收益，确认没有 ±2×/±5×/±10× 的假跳空。
5. 实现设计（存储形态、按实体 fail-closed、日更派生、契约与导出）见
   [issue-607-etf-qfq.md](issue-607-etf-qfq.md)；owner 选项表在该文件 §10。

---

## 9. 年线、研究快照导出、MCP 契约

### 9.1 年线

**推荐首期不出 ETF 年线（Q6）**，原因：

- 年线派生只接受「前复权 + 股票」的日线（`src/sqlite_market/series.rs:1157-1175`），ETF 没有前复权序列；
- 若用不复权日出年线，`yearly.rs:51-80` 的口径/单位证据会要求"前复权"确认（现有 `aggregate()` 明确
  `Adjustment::Forward.matches_evidence`），要另开口径；
- 首期只交付不复权时，年线聚合会把分红/份额折算当成价格跳空，正好是 QFQ 未解决的部分。

若 owner 要求出 ETF 年线：必须在前复权交付之后，且 `yearly_state` 放宽为"base class 与 base 口径匹配即可"，
并补 A 股年线不变的回归。

### 9.2 MCP 读面

- ETF 序列的值按 `price_scale` 解码，客户端拿到的是 decimal（3dp），**不需要新字段也能正确解析**；
  但需要：
  1. `market.daily` 的可见性/head/availability 按实体品种解析（§6 P8/P10）；
  2. 口径：ETF 首期只有不复权，而 `market.daily` 现在固定返回前复权（股票）。两种选择（Q4）：
     - **推荐**：请求加可选 `adjustment: "raw"|"qfq"`（缺省 `qfq`，A 股行为不变），ETF 用 `raw`；
     - 备选：`market.daily` 对 ETF 隐式返回不复权（同 dataset 不同品种不同口径，容易误读，不推荐）。
  3. **缺省行为必须 fail-closed**：请求**未带** `adjustment` 时缺省为 `qfq`；若实体是 ETF（首期没有前复权序列），
     必须返回**显式错误**（稳定错误码，例如 `adjustment_required_for_etf` / `no_forward_series`），
     **不得静默返回不复权序列，也不得返回空结果**（空结果会被误读成"该 ETF 没有数据"）。
     这条写进 AC-M3-2-7 与 PR-C 的测试；若 owner 选"隐式不复权"备选，也必须在契约里显式声明口径并同样拒绝静默降级。
  4. 版本元数据**增加** `price_scale`（可加性字段；现状见 `src/data_supply/columnar.rs:29` 与
     `sqlite_supply.rs` 的 `units`/`adjustment` 元数据），让客户端能区分 2dp/3dp；
  5. `market.daily` 的 `comparison`/`expected_head` 等写路径参数不因 ETF 改变。
- `universe`：新 scope 只对 `market.daily`；默认（目录版本）仍是 A 股（§5.3）。
- `tools/list` 体积：新增 2 个 scope 字符串（`sh_sz_etf_current`/`sh_sz_etf_listed_ever`）出现在
  `market.daily` 的 selection schema 与 universe scope 上，估算 +200–400 B；若 Q4 采纳 `adjustment`，
  再加一个 2 值枚举（约 +150 B）。当前 ops `tools/list` **30,159 B**、批准上限 **30,800 B**
  （[v9 契约](../contracts/generic-data-v9.md) §2）。**推断**总增量 350–550 B，可能贴线；
  超过时按同一节的「已批准上限 + 已记录面增长」规则申请，不静默超限。

### 9.3 研究快照导出

- `DAILY_COLUMNS`（`src/research/mod.rs:546-558`）没有 `instrument_class`；ETF 行若并入
  `daily_raw`，消费者无法区分股票/ETF（只能靠代码段，违反本文档自己的身份规则）。
- **推荐（Q7）**：`research-snapshot.v4` 在日线文件加一列 `instrument_class`（INT32，0/1/2），
  ETF 不复权行并入 `daily_raw`；`daily_qfq` 在 ETF 前复权交付前不含 ETF。
  - 兼容性：v3 文件的列集不变（v3 仍然可读）；v4 是新增列，属 schema 变更 → 版本号必须升；
  - 若 owner 不想在本批改导出：可先只交付 daily 读面（ETF 不导出），把导出放到 Q7 的后续批次，
    数据目录写明"导出暂不含 ETF"。
- ETF 不进 `index` 文件、不进 `universe`（目录）文件；若未来导出 ETF，另加 `etf_membership` 或复用
  `security_master`/`code_migrations` 的行（`instrument_class=2` 已在主表）。
- **PR-D 实现记录**（契约见 [v10 §1.6](../contracts/generic-data-v10.md)）：
  - 版本仍记 `research-snapshot.v4`：XDM-157 的 v4 在代码里已合并但从未部署（线上导出是 v3），
    ETF 列并入 v4，不升 v5；
  - D-72 追加：`daily_*` 再加可空 `no_trade`；`universe_intervals` 给 ETF 同时写 `last_trade_date`
    与 `delisting_date`（A 股行为 NULL）。上面「ETF 不进 universe 文件」由此改为：ETF 区间进
    `universe_intervals`（A 股块在前），但仍不进 `universe(as_of)` 读模型与 `index` 文件；
  - 「A 股导出文件 sha256 不变」按文件分开落实：不该有 ETF 的 11 个文件字节相同；`daily_raw` 与
    `universe_intervals` 因新增列整体 sha256 必然变，改为 A 股行是文件前缀且 v3 各列值逐一相同。

### 9.4 能力修订与 wire 枚举

- 新 scope 值是 **wire 形状变化**（`tools/list` schema 的 enum），且 `market.daily` 的可用成员类型变了：
  按 v8/v9 先例升 `capability_revision` → **`generic-data-v10`**（Q10）。D-32 的"精度不算契约升版"不适用：
  这里变的是可选值与行为，不是数值精度。
- `docs/contracts/generic-start-collection-v1.json:49` 的 `scope` 是 `const`（v1 只允许
  `current_sh_sz_a`）；ETF 的 scope 只出现在 v2（与 `sh_sz_a_listed_ever` 同一分支），
  不需要改 v1 快照；但 `generic-data-v10.md`/JSON 需要记录新 enum 与 `adjustment`（若 Q4 采纳）。
- 工具数不变（无新工具）；`tools/list` 体积按 §9.2 测量；`get_data_capabilities` 的
  `datasets[].adjustments`/`units` 需要能表达 ETF（不复权 + 厘 + 份 + 手日精度）。

---

## 10. schema 与部署

### 10.1 是否需要新 migration

- **不需要**：`instrument_class IN (0,1,2)`、`price_scale BETWEEN 0 AND 6`、`series_bars.*_units`、
  `volume_quantum`、`yearly_series_lineage.base_instrument_class` 都已存在（§2.3）。
- 价格/量纲列不需要 migration。**PR-B 实施注记（偏离本节原结论）**：因子事实要求 cat1/cat11
  都落行且逐行带 `basis_status`，而 022 的 `adjustment_events.category CHECK(category=1)` 不允许
  cat11，因此 PR-B 增加 `027_etf_factor_events.sql`（重建该表，category 允许 1/11，新增
  `basis_status`），fresh compact schema 变为 27；仍按 D-08 全新 init，不做原地升级。
  Q5 推荐的"不进 catalog"不变，不需要 `catalog_members.instrument_class`。
- 来源快照使用既有 `exchange_source_snapshots` + 来源库（M3 §9.4），不需要新表；ETF 名单新增
  `source_id` 两个值，属数据不是 schema。

### 10.2 上线（D-08：归档旧库 + 全新 init）

1. 发布新二进制（schema 仍 26）；部署脚本按现有流程归档旧库、`task_sqlite init21` 建新库、
   `replay --scope all`（含新的 ETF 身份 writer）。
2. 来源库需先有 ETF 名单快照：首次部署前用 `refresh_exchange_sources`（或离线抓取 + freeze）冻结两份名单；
   `replay` 从快照重建身份（不重抓）。
3. 首采：`start_collection` v2，`selection={"kind":"scope","scope":"sh_sz_etf_current"}`，预算 500/批；
   1,631 个有序列成员 → 4 批（500/500/500/131）；86 只无 `.day` 成员进 `absent`。
   分批由 ETF scope 自身实现：每次调用取按代码排序的前 500 个未采成员，`remaining_count` 记剩余，重复调用至「nothing to collect」（语义见 [契约 v10](../contracts/generic-data-v10.md) §1.1，XDM-193）。
4. 首采预算与门槛：SLO 第 4 行 ≤20min、首采后 DB ≤2.5GiB；按 §11 外推首采 ≈726s、DB ≈2.09GB（**推断**）。
5. 读面自检：`market.daily` 单只 ETF（如 `sh.510300`/`sz.159919`）返回 3dp；A 股对照（`sh.600000`）不变；
   `tools/list` 与 `get_data_capabilities` 记入部署报告。

### 10.3 回滚点

- 回滚 = 恢复部署前归档库 + 回退二进制；来源库**不动**（旧二进制忽略它）。
- 旧二进制（v9）的读面把 `market.daily` 视为 (前复权, 股票)，ETF 序列对它不可见（不会被误读）；
  新库里的 ETF 序列在回滚后不参与 A 股读面，不会污染 A 股结果。
- 前滚：再次部署新二进制 + `replay`（幂等）+ 首采（幂等跳过已有 head）。

---

## 11. 日更与 SLO

### 11.1 规模与容量（**推断**，以 §2.2 实测基线外推）

| 项 | 值 | 依据 |
|---|---|---|
| ETF 不复权 bar（现行 1,631 只） | 1,530,437 | `.day` 记录数（实测） |
| 库增量（bar） | **75.4 MB** | 1,530,437 × 49.30 B/bar（实测 B/bar）**推断** |
| 库增量（series 行） | **0.48 MB** | 1,631 × 293.79 B/series **推断** |
| 库增量（ETF 年线，若交付） | 7,392 年 bar ≈ 0.36 MB + lineage | 实测 member-year 数 |
| 前复权若同时交付 | 再加 **75.4 MB**（bar）+ 0.48 MB | 两倍序列 |
| 128 只候选退市若日后准入 | +5.35 MB（108,451 bar） | 实测 |
| 首期后 DB | ≈ **2.093 GB** | 2,017,660,928 B + 76 MB **推断** |
| 部署目录 | 6.30 GB → ≈6.38 GB（SLO 第 5 行 ≤8GiB） | 实测目录 + 推断增量 |

### 11.2 日更耗时（**推断**，不得当结论）

| SLO 行 | 参考实测 | 每 item | +1,631 item 外推 | 判定 |
|---|---|---|---|---|
| 1 同代无变化全市场 `update_data`（hard <600s） | 573.4s / 5,238 item（XDM-84，preview.37 代码 `0f3fe25`） | 109.5 ms | **≈752s** | **推断会超 600s**；需实测/裁决（Q8） |
| 2 G1 跨代日更（hard <600s，D-52 豁免中） | 734.0s / 5,238 item（XDM-84） | 140.1 ms | ≈963s | 已豁免；仍变差 |
| 3 全市场年线（hard <600s，D-69 豁免中） | 791.89s / 5,223 item（preview.42 生产单任务） | 151.6 ms | ≈1,039s | 只在 ETF 出年线时相关 |
| 4 首次全市场采集（hard ≤20min） | 553.5s / 5,223 item（preview.42 重建） | 106.0 ms | ≈726s | 仍在 20min 内 |
| 6 `get_task_progress`（回归小门） | 0.108s 中位 | — | 任务数 +1 | 不受影响 |

### 11.3 实测方法（SLO §2 口径）

1. **A/B 同二进制**：同一 release 二进制、同一 active generation，先在 ETF 不入库时跑一次
   `plan_market_update`+`run_market_update`（同代、显式 `end_date` 造全量，n=1），记录 `tasks` 数与
   `created_at`→`last_progress_at` 的 write-loop、`data_versions +0`、checkpoint 后库增长；
   再用 ETF 首采后的库重复同样负载（ETF 与 A 股同任务/分任务两种形态各一次，记录 item 数）。
2. **记录**：主机、代码 sha、二进制 digest、rustc、DB 大小与页缓存冷热、MemAvailable 最小值、
   task/每 task 项数（SLO 第 2 节 2026-11-02 的新要求：单任务形态与 53 批形态不可横比）。
3. **门禁判定**：由 owner 按原始数据判定；本设计只给外推，不写 PASS/FAIL。
4. **停止线**：MemAvailable <300MB 或磁盘可用 <5GB 立即停止；本机 1.9GB RAM，测量期间不并行跑其他重任务。

---

## 12. PR 拆分

约束：每个 PR 只改本层，Rust 正确性由 CI 证明（本机只 `cargo fmt`）；夹具不得提交交易所原始字节
（D-47 再分发 `NOT_AVAILABLE`），用**合成记录**复现下面的锚点值，并在测试注释/文档里记录真实字节的 sha256
作为 provenance（Q12）。

| PR | 范围 | 关键 AC / 测试 | 依赖 |
|---|---|---|---|
| **PR-A 身份与来源** | SSE 用 `FUND_LIST`、SZSE 用 `1945`（`ebs` 只作报价锚）；`source_id` `sse_etf_list`/`szse_etf_list` + parser `etf-list-v1`；freeze/validate/replay；`security_master` class=2 写入器（沪 `listing_source=ExchangeList`、深 `TdxSsdate`）；新 `CatalogScope` 值 + 主表解析 + catalog 拒绝 | 名单 sha256 冻结；1,717 成员；86 只 `absent`；128 只不入 class=2；`FUND_LIST` 的 947 代码集与 `ebs` 双向差集为空（每次刷新都断言）；`listingDate` 与 `base.dbf SSDATE` 一致；`replay --verify-only` match；`securities.catalog` 拒新 scope；A 股 `security_master` 行不变 | M3 §6 已合并（共用入口/来源库）；Q1 准入 |
| **PR-B 精度与量纲** | `SeriesShape::etf`、`PRICE_SCALE_LI`、`.day` 适配器按品种 scale、`normalized_volume/hands_consistent` 参数化、单位证据按品种；ETF 不复权采集（新 scope 分支、预算 500） | 合成夹具：`sh510050` 3dp 往返、`sz159338` 手日 ×100 + `volume_quantum=100`、`sz159919` cat1 锚点、`sz159901` cat11 锚点、`sh510070`（退市候选）不入库；A 股 16,354 条 version_id 不变；1 个不一致手日 fail-closed | PR-A |
| **PR-C 读面与 scope 语义** | `dataset_shape`/`read_public_series`/head/availability 按实体解析；`market.daily` ETF 不复权可读；元数据加 `price_scale`；（Q4 采纳时）`adjustment` 参数 | `query_data` 单只 ETF 返回 3dp、`adjustment=raw`；A 股 `sh.600000` 输出逐字节不变；`universe` 新 scope 只对 daily 生效；`tools/list` 增量实测 | PR-B |
| **PR-C2 日更** | `plan_market_update`/`run_market_update` 的可选 `scope`（`sh_sz_etf_current`、`sh_sz_a_and_etf_current`）；ETF item 走 raw typed 路径（no_change/metadata/追加，非前缀改写 fail-closed `etf_history_rewrite`）；ETF-only 任务不追加派生 item；年线与退市序列检查不含 ETF | 不传 `scope` 时 A 股 `plan_id`/item 字节不变；ETF 同代 no_change、跨代追加 3dp 读回、手日 `volume_quantum=100`、矛盾 bar 失败且序列不变、非前缀指定错误码；混合批 ETF 失败不影响 A 股；yearly 不含 ETF；§11.3 的参数形态见 v10 契约 §1.7 | PR-C |
| **PR-D 导出（可选，Q6）** | `research-snapshot.v4`：日线文件加 `instrument_class`，ETF 行并入 `daily_raw` | v3 仍可读；v4 列集与行数；A 股导出文件 sha256 不变；manifest 版本 | PR-C |
| **PR-E 年线/前复权（可选，Q3/Q6）** | cat11 进 QFQ 链、事件逐条核对清单、ETF 年线放宽 `yearly_state` | §8.3 的逐条核对表；复权连续性 10 只样本；年线 A 股不变 | PR-B/Q 裁决 |

顺序：A → B → C →（D/E 视裁决）。每个 PR 的 CI 必须绿；docs-only 设计 PR（本文件）先行。

**测试夹具（真实记录 sha256，作为 provenance；内容用合成 32B 记录复现）**：

| 成员 | 解压字节 sha256 | 记录数 | 用途 |
|---|---|---|---|
| `sh\lday\sh510050.day` | `02f0e51778efbe2c51ed245347d1952edf0e6787433074ba8b767594428e2e16` | 5,271 | 3dp 价格往返、老 ETF 基准 |
| `sz\lday\sz159919.day` | `faba834e5178a0cc8f2035dd99a1eebb2d56d9a94eb9fc1293026688c802d9b3` | 3,504 | cat1 锚点（2025-03-31） |
| `sz\lday\sz159901.day` | `b75467e27c2975d031cae1715e5151b59ec561b7c42b42a26b61823185c9a696` | 4,984 | cat11 锚点（2021-04-12 / 2010-11-22） |
| `sz\lday\sz159338.day` | `edb7ddae3907cd3513ec0dbd724d609f354fbb1d403919eaa30c8268b58b017d` | 498 | 手日（2025-09-18，`r7=0xC3640053`） |
| `sh\lday\sh513330.day` | `8acd21643b538abd6e8030fe0b5a8a1a6c13df8b639a32d8d77b4d00f027a2e5` | 1,386 | 手日阈值上界（2026-08-11） |
| `sh\lday\sh510070.day` | `366c34b2f7a79b45b89342e099b825ea4e72b4c58dca554ed070d4f2d16ff6f0` | 2,383 | 退市候选（不入 class=2） |
| `gbbq.zip!gbbq` | `d6cb92c8eaa39d47cfa0e109d6b12e0756dda1e9f0823e75a10eecf8bde6714a` | 207,536 | 事件解码；`sh.600000` 2008-04-24 锚 `[1.6,0,3,0]` |
| 事件原始记录 `sz159919:20250331:cat1` | `fc6c277f4e29162d9b41b2e79aac9a5df0412de8f3cb22c7b2915956b652cfbc` | 1（29B） | `c1=0.661` |
| 事件原始记录 `sz159901:20210412:cat11` | `7e0606cf050a3f434b168aa5b2e3454e204e66d86512e6e0c89188c9987eb911` | 1 | `c3=2.0` |
| 事件原始记录 `sz159901:20101122:cat11` | `8d761dd318897339be76a5ef0ca02bbbe160f107442658eb30f27df0d8d67861` | 1 | `c3=5.0` |

---

## 13. 验收清单（AC-M3-2）

| AC | 判据（可执行） |
|---|---|
| AC-M3-2-1 身份 | 1,717 只现行 class=2；沪市上市日来自 `FUND_LIST.listingDate`（947/947 与 `base.dbf SSDATE` 一致）、深市来自 `SSDATE`；86 只 `absent`（带原因）；**128 只有公告的退市 ETF 也是 class=2（沪 82/深 46），`delisting_date` 在无 D-72 提取结果时为 `unknown`，有提取结果时为精确终止上市日**；`sh_sz_etf_listed_ever` 含这 128 只，`sh_sz_etf_current` 不含；名单移出不删 class=2 行；`FUND_LIST` 与 `ebs` 代码集差集为空；`replay --verify-only` = match；名单/公告快照 sha256/URL/时间入清单 |
| AC-M3-2-2 精度 + A 股不变 | ETF OHLC 3dp 精确往返；A 股/指数/年线 16,354 条序列中，可与 aws-node 冻结库逐条对照的 11,122 条里有 11,116 条的行数与内容摘要（截断到 2026-11-04）相同，其余 6 条差异全部由 11-05 日更新增的除权事件解释（qfq 序列内容变化）；`version_id` 只在无新增 bar 的 690 条上相同（`content_digest` 覆盖整条序列，日更追加 bar 后「逐一相同」不可满足）。A 股导出行、前缀与文件 sha256 相同；`daily_raw`/`universe_intervals` 因新增 ETF 列整体 sha 变化，见 [v10 §1.6](../contracts/generic-data-v10.md) |
| AC-M3-2-3 量纲 | 3,819 根手日夹具还原 ×100 + `volume_quantum=100`；`r7` 标记与量额一致性表 4 个分支各 1 测试；不一致 → item 失败（不部分写） |
| AC-M3-2-4 成员集 | 新 scope 只在 `market.daily`；`securities.catalog` 拒绝；默认 `universe` 成员数不变；`market.daily` 单只 ETF 可读 |
| AC-M3-2-5 采集 | 4 批（500/500/500/131）首采；considered/succeeded/absent/failed 逐项报告；86 只 absent；不静默丢 |
| AC-M3-2-6 复权 | 首期只交付不复权 + 因子事实；因子行带 `basis_status` 与事件证据；若交付 QFQ，§8.3 的逐条核对表齐全 |
| AC-M3-2-7 契约 | capability 升 v10；`tools/list` 增量实测并记录；`price_scale` 在版本元数据可读；`get_data_capabilities` 能表达 ETF 口径/单位；**ETF 未指定 `adjustment` 时返回显式错误（fail-closed），不得静默返回不复权或空结果** |
| AC-M3-2-8 来源/重放 | 原始字节只在来源库；`replay --scope exchange` 幂等；公开仓库无交易所原始字节；再分发 `NOT_AVAILABLE` 写明 |
| AC-M3-2-9 性能 | 按 §11.3 记录 A/B；task 数/项数、write-loop、库增长、MemAvailable；未实测不写 PASS、不改门槛 |
| AC-M3-2-10 偏差 | 数据目录 §3.3 新增：128 只候选退市的幸存者偏差、86 只无日线、ETF 无行业、ETF 暂无年线/前复权（若延后）、`SSDATE` 7 例晚于首 bar |

---

## 14. owner 待决项（已批准：D-70，2026-11-03）

owner 2026-11-03 回复「按推荐批准」：Q1–Q12 全部按推荐值批准，已记入 [D-70](../decisions.md)。下表「推荐」列
即裁决值，实现不再回头询问；若实现中发现需要偏离，在对应 PR 写明理由并请 owner 重裁。

| Q | 问题 | 选项 | 推荐 | 理由 |
|---|---|---|---|---|
| **Q1**（已批准 D-70，2026-11-03） | **是否准入 SSE `FUND_LIST` 与 SZSE `1945` 作为 ETF 身份来源**（证据包状态见 §3.2 表） | (a) 两份都准入为身份来源：SSE `FUND_LIST`（身份 + `listingDate` + `subClass` + `INDEX_CODE`），SZSE `1945`（身份 + 拟合指数/规模/管理人），`ebs` 仅作报价锚；(b) 只准入 SZSE `1945`，SSE 继续用 `ebs`（则无官方上市日/类型）；(c) 都不准入（M3-2 无法交付身份） | **(a)** | 证据包已齐：SSE 947 行、`pageCount=1`、`listingDate` 947/947、`subClass` 947/947、`INDEX_CODE` 915/947；SZSE 770 行；两份成员集与 `ebs` **完全一致**（947=947，无差集）。`ebs` 是行情快照的分类列表、类别口径无官方说明，不能当身份。D-25 要求逐个准入，**准入前不得进入生产路径** |
| **Q2**（已批准 D-70，2026-11-03；2026-11-03 追加 D-71/D-72 落地） | 128 只候选退市 ETF 的官方身份怎么拿 | (a) 取证交易所"终止上市基金/历史基金名单"并按 #308 准入；(b) 从今天起积累快照、历史标 `unknown`；(c) 不收录 ETF；**(d) 追加：沪 `COMMON_PL_JJXX_JJGG_L` 与深 `etfNotice_disc` 两个公告索引作身份/公告日来源，精确退市日取公告 PDF 一次性提取结果** | **(a) 已完成取证；(d) 已按 D-71/D-72 批准实施** | 两所都没有结构化终止上市基金名单，但公告索引对 128/128 命中，足以只由"候选 + 公告命中"产生身份；公告索引本身不提供生效日，所以 D-71 保持 `delisting_date=unknown`（公告日另存），D-72 只把一次性 PDF 提取结果作为派生事实，生产代码不解析 PDF。这样既消掉 128 只的幸存者偏差，也不把公告日冒充生效日 |
| **Q3**（已批准 D-70，2026-11-03） | 首期复权范围 | (a) 只不复权 + 因子事实；(b) 不复权 + 前复权（需补 cat11/cat1 逐条核对） | **(a)** | cat11 仅 3 个事件锚定、cat1 含 1 例疑似拆分（159922）；先交付事实行，QFQ 作为独立批次（§8.3 给核对方法） |
| **Q4**（已批准 D-70，2026-11-03） | `market.daily` 如何表达口径 | (a) 请求加 `adjustment`（取值 raw 或 qfq；缺省 qfq；**ETF 未指定 `adjustment` 时显式报错 fail-closed**）；(b) ETF 隐式返回不复权；(c) 首期只导出不进 MCP | **(a)** | (a) 显式、A 股缺省不变，且 ETF 缺省 qfq 不会静默返回 raw 或空结果；(b) 同一 dataset 不同品种不同口径会误导回测；(c) 不满足"ETF 是 market.daily 的证券类型" |
| **Q5**（已批准 D-70，2026-11-03） | ETF 是否进 `securities.catalog` | (a) 不进，用主表 scope；(b) 进目录（需 migration 027 + `catalog_members.instrument_class` + 单例 head 改造） | **(a)** | (b) 改动 schema/head/默认 universe，破坏"A 股输出不变"；(a) 复用 M3 的 scope 模式（D-62 Q5） |
| **Q6**（已批准 D-70，2026-11-03） | ETF 是否出年线 | (a) 首期不出；(b) 随前复权一起出；(c) 用不复权硬出 | **(a)** | 年线派生要求前复权股票口径；不复权年线会把分红/折算当跳空 |
| **Q7**（已批准 D-70，2026-11-03） | 研究快照导出 | (a) v4 日线文件加 `instrument_class`、ETF 行并入 `daily_raw`；(b) 新 `etf_daily` 文件；(c) 本批不导出 | **(a)，可后置一批** | (a) 不新增文件语义、消费者能区分；(b) 文件面扩大；(c) 与"可回测"目标冲突，可短期接受 |
| **Q8**（已批准 D-70，2026-11-03） | 第 1 行 SLO（同代全量 <600s）怎么办 | (a) 先实测再定；(b) 直接重新基线；(c) ETF 独立批次、不计入"全市场" | **(a)，若确认 >600s 再在 (b)/(c) 中选** | 外推 752s 是推断；实测前改门槛或无依据排除都不合规（D-30/AGENTS.md） |
| **Q9**（已批准 D-70，2026-11-03） | 手日精度列与 A 股既有 19 根 | (a) ETF 写 `volume_quantum=100`，A 股保持现状 NULL；(b) A 股也回填 100（改 A 股导出字节） | **(a)** | (b) 违反 AC-M3-2-2；A 股 19 根的精度标注可另案 |
| **Q10**（已批准 D-70，2026-11-03） | 能力修订与工具面 | (a) 升 `generic-data-v10`；(b) 不升版只更 release notes | **(a)** | 新 scope enum/`adjustment` 是 wire 形状变化，不是 D-32 的"精度不变" |
| **Q11**（已批准 D-70，2026-11-03） | ETF 类型/跟踪指数是否落库 | (a) 首期只入身份/名称/上市日，类型/指数随名录快照冻结留证、不建数据集；(b) 新建 ETF 元数据数据集（类型/指数/规模/管理人）；(c) 写进 `security_master` flags | **(a)**，后续可做 (b) | 名录查询已一次给出 `subClass` 与 `INDEX_CODE`/`INDEX_NAME`（915/947），**不再需要 947 次逐只请求**；但落库要新表/迁移，首期先留证不扩 schema |
| **Q12**（已批准 D-70，2026-11-03） | 测试夹具与再分发 | (a) 合成夹具 + 记录真实字节 sha256 作 provenance；(b) 直接提交交易所原始字节 | **(a)** | (b) 属对外再分发（D-47 `NOT_AVAILABLE`）；(a) 同样能钉住数值与格式 |

---

## 15. 未验证 / 待确认

1. **手日阈值精确常量**未定（只夹在 [4,289,510,575, 4,290,271,400]）；规则不依赖它。
2. **非整除手日的取整**未验证（只有 159338 的整除样本）。
3. **`sz159830` 2024-12-20 的 cat11**（`c3=0.93584`）未解释：价格未按 `1/c3` 变化。
4. **`sz159922` 2024-12-02 的 cat1**（`c1=1.292`、比率 2.5026）疑似拆分记错类别，未逐条核对官方公告。
5. **86 只缺段的原因**（TDX 为什么不含 526/551/589）未验证；只证明"整段缺失、不是新上市滞后"。
6. **128 只候选退市的官方身份/退市日**未取得（Q2）。
7. **ETF 名称历史**：名单只给当前简称，历史简称没有日期化来源（与 S-6 同类问题，本批不做）。
8. **ETF 规模/份额/净值**未纳入（`base.dbf` 有 `ZGB`/`TZMGJZ`，但未锚定到 ETF 口径）。
9. **`fetch_time` 作为 `effective_from`** 的上界性：名单无 `last_modified`，只能取抓取时间（偏差同 M3 §5.2）。D-80 起只对没有上市日的成员适用；有上市日的成员 `effective_from` 是上市日，抓取时间只在 `known_at`。
10. **SLO 外推**（§11）全部为推断，未实测；未验证 ETF item 的 per-item 成本与 A 股是否相同。
11. **导出 v4** 的兼容性未验证（只有设计取舍）。
12. **沪市 ETF 逐日 OHLC** 只有单日官方锚（`prev_close`）+ 全历史量额一致性；沪所无历史日线接口
    （XDM-15 已验证），逐日锚定强度弱于深市（5 日 × 13 只 → 本次 558 只单日）——这是来源限制，不是实现缺陷。
13. **SSE `FUND_LIST` 的更新频率/历史版本未验证**；`pageSize` 上限（>2000）与未知 `subClass` 值的行为未验证；
    `subClass` 的中文映射来自页面 JS，不是独立发布的分类文档，入库要标来源与版本。
14. **`FUND_LIST` 与 `ebs` 成员集一致只在本日快照成立**（947=947，双向差集为空）；写入器必须在每次刷新时
    重新做双向比对，出现差异要显式列出而不是取其一（本文件不改运行时行为）。
15. **SSE `FUND_LIST` 的 `INDEX_CODE`/`INDEX_NAME` 覆盖 915/947**（缺 24 只货币 + 8 只黄金，符合无跟踪指数），
    但指数代码与中证/国证代码体系的对齐未验证（Q11 只留证，不落库）。

---

## 16. Also noticed

- `docs/data-catalog.md` §3.2 #13 与 XDM-15 报告引用的 `src/compact_market.rs:812-822` `cents()`
  在当前代码已不存在（schema 22 起为 `*_units` + `price_scale`）；#13 的状态行仍写"未满足 → M3"，
  本设计批准后应改为"M3-2 设计中"并更新代码引用。
- `docs/STATUS.md`「数据」表的 active generation 仍是 `f2fd6fcb…`、库大小 1,932,939,264 B；
  实际 active 为 `5ac3ef2b…`（activated 2026-11-02T08:39:19Z）、库 2,017,660,928 B（2026-11-03 只读）。
- `docs/STATUS.md` 的「最新集中行情日 2026-10-30」与本代 `hsjday` 已含 2026-11-02 bar 不一致
  （`sh510050` 等成员末根 bar 为 2026-11-02）；属 STATUS 未随 11-02 日更更新。
- SLO 第 3 行的年线记录（791.89s）与 `derived:risk_warnings` 的 D-68 修复都引用 preview.42/43；
  ETF 上线前应先确认这两项的状态，避免把"已知超时+豁免"与"ETF 新增负载"混在一次测量里。
- 交易所 1815 日行情接口在本环境对 2026 年日期返回空表（2025-10-31 及更早正常），本次锚定使用
  2025-10-31 与各事件日；这是沙箱/镜像的数据边界，不是接口废弃（未验证）。
