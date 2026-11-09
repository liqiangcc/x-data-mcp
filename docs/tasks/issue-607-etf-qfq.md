# ETF 前复权的实现设计（[#607](https://github.com/liqiangcc/x-data-mcp/issues/607) 阶段 2）

- **状态**：设计已由 [D-89](../decisions.md)、[D-91](../decisions.md)、[D-92](../decisions.md)、[D-93](../decisions.md) 裁定，
  **实现进度：PR-A（#618）、PR-B（#615）、PR-F（#614）与 `convert`（#616）、每周 `verify`（#617）已合并；
  PR-C（写入侧：存储与日更）正在实现，尚未开 PR**（§11）。阶段 1/2 的取证结果见 §9.2（VM2-21/22）、§9.3（VM2-23 全量公告）、
  §9.4（D-93 事件日语义）；§9 的 P 表已按事件表回填，未取得项仍明确标出。
- **依据**：[D-89](../decisions.md)（owner 2026-11-08 批准交付 ETF 前复权）、
  [M3-2 设计](issue-509-m3-2-etf.md) §8（§8.1 现状、§8.2 为什么不首期交付、§8.3 交付条件与方法）、
  [SLO](../SLO.md) 第 1/2 行、[STATUS](../STATUS.md)。
- **代码基线**：分支起点 `origin/main` = `de1f84a`（preview.47 版本号，schema 27，
  `generic-data-v11`）。文中引用的行号都基于这个提交。
- **证据分级**：§1 的定位只做「读代码 + 读设计文档」，没有实测、没有连生产；
  §9.2–§9.4 的数字来自仓库外的取证报告与事件表（VM2-21/22/23，vm2 生产只读）。
  标「推断」的是从代码或设计里推出的结论，不是门禁依据。**§9 的 P8、P10 已由 vm2 只读预检（报告 `607-p8p10`，仓库外）回填（§9.5）**，不再标「待取证」。

---

## 0. 结论摘要

1. **拒绝位置只有一处语义源**：ETF 的 `qfq` 与「未指定口径」都由
   `SeriesShape` 的解析函数 `resolve_read_shape` 拒绝
   （`src/sqlite_market/series.rs:583-596`），错误码在 MCP 边界映射成
   `adjustment_required_for_etf` / `adjustment_unavailable`（`src/data_supply/mcp.rs:835-838`）。
   让 ETF `qfq` 从「必然报错」变成「有条件可读」，改动集中在
   `resolve_read_shape` 的 ETF 分支与一条新的「哪些实体可算」判定。
2. **算法大部分可复用**：`src/qfq.rs` 的 `forward_adjust_exact`（`src/qfq.rs:219-267`）
   对事件集合是**盲的**——它不筛 category（`src/qfq.rs:99-102` 的注释明确写「caller supplies」），
   只按 `event_date > bar_date` 严格大于、oldest→newest 复合、最后一次 HALF_UP。
   所以 cat11 不需要在 `qfq.rs` 里加分支，只需要**在调用方**把 cat11 的事件以
   `/ c3` 的仿射形式喂进去。真正的缺口是：`exact_compose_chain` 只认
   `(10+C3+C4)/10` 那一种变换（`src/gbbq.rs:361-386`），cat11 要用**另一种**
   per-event 变换，并且 `c3` 不能被 2dp 截断（ETF 因子是 f32 最短往返，
   `gbbq-etf-factors-f32-shortest-v1`，`src/gbbq.rs:141-157`）。
3. **必须新写的三块**：(a) 精确链的「cat11 变换 + 任意位小数因子」；
   (b) 同日 cat1+cat11 的次序规则；(c) 按实体的可算性判定（fail-closed，D-89 第 6 条）。
4. **按实体 fail-closed 推荐方案 (a)**：任一阻断事件存在时，整只 ETF 的 `qfq`
   返回 `adjustment_unavailable` + 原因码。方案 (b)（只给最后一个阻断事件之后的 bar）
   让同一只 ETF 的序列起点随因子修订漂移，回测里最难发现，见 §3。
5. **存储**：`SeriesShape::etf(Forward)` 已在 `src/sqlite_market/series.rs:76-85` 定义好
   （`price_scale=6`），只需新增序列行，不需迁移。序列数 +1 条/只可算 ETF，
   bar 数按 ETF 不复权 bar 数等比增长；库体积增量**未取得**（§9 P8），见 §4。
6. **日更**：复用 A 股前复权的 Full/rewrite 语义，但触发条件不同（ETF 的 `qfq` 完全由
   不复权 bar + 因子事实派生，没有独立的来源绑定）。见 §5。
7. **契约要升版**（推荐 `generic-data-v12`），即使只是把一个错误码换成一种新的可读形态：
   客户端的行为分支变了。见 §6。
8. **研究导出**推荐 `daily_qfq` 收录 ETF 行、**不升**快照 schema（列集合不变，见 §7）。
9. **核对结论的落地方式决定了部署代价**：`(a)` 扩枚举和 `(b)` 新建表**都是 schema 迁移**
   （27 → 28），在这个仓库「fresh init only」的模型下等于每次部署都要重建一次行情库
   （约 2–3 小时）；推荐 `(c)` 不改 schema，把核对结论按内容寻址登记进来源库，
   读面按 `evidence_digest` 查阻断集合。见 §3.2.1/§3.2.2 与 §10.0。
10. 待 owner 决策的问题共 9 个，在 §10。

---

## 1. 现状定位

### 1.1 ETF 现在在哪里被拒绝 `qfq`

| 位置 | 代码 | 行为 |
|---|---|---|
| 形状解析 | `src/sqlite_market/series.rs:588-596` | `master_class == INSTRUMENT_CLASS_ETF` 时：`requested=None` → `adjustment_required_for_etf`；`Some(Forward)` → `adjustment_unavailable`；`Some(Raw)` → `SeriesShape::etf(Raw)`。**这是唯一的语义源** |
| 年线 | `src/sqlite_market/series.rs:576-587` | ETF 年线一律 `Ok(None)`（absent），D-70 Q6 不变 |
| wire 映射 | `src/data_supply/mcp.rs:835-838` | 把 `Error::Invalid` 的前缀映射成稳定错误码 `adjustment_required_for_etf` / `adjustment_unavailable` |
| 读面入口 | `src/sqlite_supply.rs:540-551` | `dataset_shape` → `resolve_read_shape`；`dataset_head`/`dataset_invisible`（`src/sqlite_supply.rs:555-600`）共用同一个解析，因此「版本列举」和「读行」不会一个能读一个不能读 |
| 公共读路径 | `src/sqlite_market/series.rs:634-646` | `read_public_series` 写死 `requested = Some(Adjustment::Forward)`：**ETF 走这条路径必然撞到 `adjustment_unavailable`**。这是「ETF 有前复权之后仍然读不到」的风险点，实现时必须改成按请求口径解析 |
| 研究导出 | `src/research/read.rs:329-400` | 显式点名 ETF 的 `--adjustment qfq` 导出直接报 `adjustment_unavailable`（`src/research/read.rs:387-396`） |
| 契约测试 | `src/data_supply/collect/tests.rs:906-922`、`src/data_supply/mcp/tests.rs:4110-4160`、`src/data_supply/mcp/tests.rs:4330-4345` | 这些断言写死了「ETF qfq = 不可用」，实现 PR 要按新语义改写，不能只删测试 |

要点：仓库里「ETF 前复权 = 不可用」的断言分散在上面最后一行列的 3 个测试文件里，共 13 处（`grep` 计数，含断言与期望字符串）。
实现 PR 的验收必须逐个交代，否则会出现「代码允许、测试仍假设拒绝」的漂移。

### 1.2 `SeriesShape` 与 ETF 不复权的映射

`SeriesShape`（`src/sqlite_market/series.rs:53-58`）是三个身份维度的组合：
`adjustment`、`instrument_class`、`price_scale`。schema 22 起这三项折进序列摘要
的 seed（文件头 `src/sqlite_market/series.rs:9-15`），所以「同样的整数、不同的口径」
是不同版本，不会碰撞。

| 形状 | 定义 | `instrument_class` | `price_scale` |
|---|---|---|---|
| `stock(Raw)` | `series.rs:74-82` | 0 | `PRICE_SCALE_CENTS` = 2（`src/compact_market.rs:19`） |
| `stock(Forward)` | 同上 | 0 | `PRICE_SCALE_QFQ` = 6（`src/compact_market.rs:28`） |
| `etf(Raw)` | `series.rs:91-99` | `INSTRUMENT_CLASS_ETF` = 2 | `PRICE_SCALE_LI` = 3（`src/compact_market.rs:23`） |
| `etf(Forward)` | 同上，**已被 D-89 启用** | 2 | `PRICE_SCALE_QFQ` = 6 |
| `INDEX_UNITS` | `series.rs:101-107` | 1 | 2，且恒 `Raw` |

结论：**存储层不需要任何改动**。`SeriesShape::etf(Forward)` 早就写好了，
注释里那句「a future 前复权 series uses the same 1e-6 元 precision as stocks」
（`series.rs:86-88`）就是 D-89 要用的口径；当时的「PR-B writes only the raw shape」
是交付边界，不是 schema 限制。

`resolve_read_shape` 里还有两条与 ETF 相关的完整性约束，实现时不能绕开：
- 有 ETF 序列行但主表没有对应 `security_master` 行 → integrity 错（`series.rs:529-542`）；
- 主表类别与存储行的 `instrument_class` 不一致 → integrity 错（`series.rs:560-572`）。

### 1.3 `dataset_shape` 与 `read_public_series` 怎么按实体解析品种

调用链（全部走一个解析函数，这是 M3-2 PR-C 的设计要求）：

```text
query_data / versions / availability
  └─ src/sqlite_supply.rs:540-551  dataset_shape(dataset, entity, requested)
       └─ src/sqlite_market/series.rs:511-600  resolve_read_shape(c, entity, kind, requested)
            ├─ read_series_kind(dataset)  → Daily / Yearly / Index（src/sqlite_supply.rs:529-537）
            ├─ master_instrument_class(c, entity)  → security_master（series.rs:472-505）
            └─ SeriesShape::{stock,etf,INDEX_UNITS}
  └─ read_visible_shaped(c, entity, period, shape)  （series.rs:652-672）
       └─ read_visible_series(c, entity, period, adjustment, instrument_class)
```

品种判定**只看 `security_master.instrument_class`**，不看代码前缀：设计 §0 第 1 条
明确写了「前缀只是发现口径，实现必须用名单成员集」。所以「这只 ETF 能不能算前复权」
也必须落到主表 + 因子事实表上，不能靠 `sh51*` / `sz159*` 之类的前缀判断——
这在 §3 的按实体 fail-closed 里是硬要求。

### 1.4 `src/qfq.rs` 的 `forward_adjust_exact` 及其调用方

| 层 | 位置 | 事实 |
|---|---|---|
| 合同 | `src/qfq.rs:219-267` | `forward_adjust_exact(bars, events, scale)`：事件按 `date_raw` 排序；bar 的链是**严格大于** bar 日期的那一段（`src/qfq.rs:236-241`）；`event_date > bar_date` 才生效；每个 bar 只做一次 HALF_UP（`ExactUnits::units`，`src/gbbq.rs:329-346`） |
| 链构造 | `src/gbbq.rs:384-397` | `exact_compose_chain` 对每个事件调 `exact_event_transform`（`src/gbbq.rs:361-386`）后 `then` 复合，全程 BigInt，只做一次归约 |
| per-event 变换 | `src/gbbq.rs:361-386` | `T(x) = (x − C1/10 + C4·C2/10) / ((10+C3+C4)/10)`，**用 2dp 整数写死**（`two_decimal_units`，`src/gbbq.rs:397-402`） |
| 调用方 | `src/official_market.rs:1587-1625`（`qfq_bars`） | 唯一的非测试调用方；`scale = PRICE_SCALE_QFQ` |
| 事件从哪来 | `src/official_market.rs:927-948`（`security_events`） | **只收 `event.category == 1`**，其余计入 `excluded` 计数 |

**「只取 cat1」的那一处过滤**就是 `src/official_market.rs:932-940`：

```rust
for event in self.security_all_events(market, code)? {
    if event.category == 1 { admitted.push(event); } else { excluded += 1; }
}
```

ETF 走的是另一条构造：`src/official_market.rs:1123-1143`（`etf_factor_set`）
收 `category in {1, 11}`，precision 用 `EtfF32Shortest`。也就是说
**ETF 的 cat11 事实已经在库里了，只是没有任何代码把它送进 `exact_compose_chain`**。

要点：`forward_adjust_exact` 本身对 category 是盲的（`src/qfq.rs:99-102` 明确
「this function deliberately does not filter by `category`」）。所以「ETF 前复权」
不是给 `qfq.rs` 加分支，而是**给它一个 ETF 版的事件→变换适配层**。

### 1.5 ETF 因子事实行

| 项 | 位置 | 事实 |
|---|---|---|
| 表结构 | `migrations/tasks/027_etf_factor_events.sql:16-34` | `adjustment_events.category CHECK(category IN (1,11))`；`basis_status CHECK IN ('verified','pending')`，默认 `verified` |
| 事实写入 | `src/official_market.rs:1146-1191`（`parsed_factor_set`） | `category==1` → `Verified`；其它（含 11）→ `Pending`（`src/official_market.rs:1172-1176`）。**这是现状，即 cat11 一律 pending** |
| 精度标签 | `src/market_data.rs:324-338` | `FactorPrecision::{Stock2dp, EtfF32Shortest}`，后者序列化为 `f32-shortest` |
| 币种语义 | `src/market_data.rs:340-392` | `AdjustmentEvent.category` 注释：1 = 每 10 份派现、11 = 份额折算；`c_bits` 是 f32 原样位 |
| 读数入口 | `src/research/read.rs:809-830` | `for_each_factor`：`SELECT ordinal,effective_day,known_at,category,c1,c2,c3,c4 ... ORDER BY ordinal`。**注意它不返回 `basis_status`**（见 §7 Q7） |
| 算法版本 | 设计 §8.1（`docs/tasks/issue-509-m3-2-etf.md:452-462`） | ETF 集合的 `algorithm_version` = `gbbq-etf-factors-f32-shortest-v1`；A 股仍是 `gbbq-cat1-2dp-v1`，两者不混用 |

`basis_status` 现在是**两值枚举**。D-89 的交付门槛要求「不能核对/不能归因的事件
不进 QFQ」，这意味着实现要能区分「已核对通过」与「未核对」。现有两值不够用
（`pending` 同时表示「cat11 全部」和「等取证」），处理方案见 §3.2 的 Q2。

---

## 2. 计算口径

### 2.1 公式（D-89 第 1 条，与设计 §8.3 一致）

```text
raw_price   = raw_day / 1000                  # ETF .day 整数是「厘」，raw_day: u32
per event, oldest→newest, over events with event_date > bar_date:
  category 11 (份额折算):  p ← p / c3          # c3 = 份额倍数，价格因子 1/c3
  category 1  (每 10 份派现): p ← p − c1/10    # c1 = 每 10 份派现（元）
result      = round_half_up_away_from_zero(p × 10^6) / 10^6   # 只舍入一次
```

与 A 股骨架的差别只有两点：**cat11 变换**和**因子精度**（ETF 用 f32 最短往返，
不能先截到 2dp，否则 `c3=0.93584` 会变成 `0.94`，`1/c3` 从 1.0685… 变成 1.0638…）。

### 2.2 能复用 / 要新写

| 部分 | 位置 | 复用方式 |
|---|---|---|
| bar→链的映射、严格大于、只舍入一次、后缀记忆化 | `src/qfq.rs:219-267`（`forward_adjust_exact`） | **原样复用**。它不看 category，只看喂进来的事件数组 |
| 精确仿射复合 `then`（BigInt 无中间舍入） | `src/gbbq.rs:265-274` | **原样复用**。`ExactAffine` 是通用的 `a·x+b` 有理数三元组 |
| 归约与 4096 位预算 | `src/gbbq.rs:290-315` | **原样复用**。ETF 的事件数远少于股票，预算不是约束 |
| 定点舍入 `units(scale)` | `src/gbbq.rs:329-346` | **原样复用**。`scale=6` 就是 `PRICE_SCALE_QFQ` |
| per-event 变换 = `(10+C3+C4)/10` 分母 | `src/gbbq.rs:361-386` | **不能复用**：它硬编码了派现/送转耦合式，且要求因子是 2dp（`two_decimal_units`，`src/gbbq.rs:397-402`） |
| 2dp 因子归一 | `src/gbbq.rs:162-169`（`normalize_factor`） | **不能用于 ETF**；ETF 用 `shortest_factor`（`src/gbbq.rs:154-158`） |

新写的东西，建议放在 `src/qfq.rs` 旁边（不塞进 `gbbq.rs`，因为 `gbbq.rs` 的口径是
A 股已冻结合同 `#54`，加分支会动到 A 股的可复核性）：

1. `etf_event_transform(category, factors) -> ExactAffine`：
   - `category == 11`：`T(x) = x / c3`，即 `a = 1/c3`、`b = 0`。
   - `category == 1`：`T(x) = x − c1/10`，即 `a = 1`、`b = −c1/10`。
   - 其他 category：`Err`（fail-closed，绝不猜）。
2. `etf_exact_compose_chain(events) -> ExactAffine`：按 `(effective_day, ordinal)` 排序后复合。
3. `etf_forward_adjust_exact(bars, events, scale) -> Vec<QfqBar>`：与 `forward_adjust_exact`
   同形，唯一差别是链用 (2) 构造。**为了不让两条链的口径漂移**，`forward_adjust_exact`
   应当改成薄壳：把「事件→变换」抽成一个 trait/函数指针参数，A 股传入现有的
   `gbbq::exact_event_transform`，ETF 传入 `etf_event_transform`；数值路径共用一份。

因子用「最短往返十进制 → 精确有理数」而不是 `Decimal`：因为 `c3=0.93584` 这类值在
`ExactAffine` 里必须是一个分数（`93584/100000` 归约后 `5849/6250`），
用 `Decimal` 会引入 28 位有效数字的近似。建议签名：

```rust
// 建议签名（拟，实现 PR 按此落地并在 PR 描述里贴实测）
pub fn etf_event_transform(category: u8, factors: &[Decimal; 4]) -> Result<gbbq::ExactAffine, gbbq::Error>;
pub fn etf_exact_compose_chain<'a, I>(events: I) -> Result<gbbq::ExactAffine, gbbq::Error>
    where I: IntoIterator<Item = (u32 /*effective yyyymmdd*/, u8 /*category*/, &'a [Decimal; 4])>;
pub fn etf_forward_adjust_exact(
    bars: &[crate::day_file::RawDayRecord],
    events: &[EtfEvent],          // { effective_day, ordinal, category, factors }
    scale: u32,                   // PRICE_SCALE_QFQ
) -> Result<Vec<crate::qfq::QfqBar>, gbbq::Error>;
```

`Decimal` 转精确分数的辅助函数（`Decimal::mantissa()` / `scale()`）也要新写，
并带「分母不超过 1e28」的上界检查——`gbbq::EXACT_CHAIN_MAX_BITS` 已有 4096 位预算
（`src/gbbq.rs:242`），复用同一个错误即可。

#### 2.1.1 事件日 E 直接进链（D-93 取代 D-91 第 (1) 条，2026-11-08）

**现行口径（D-93）**：ETF 的 gbbq 事件日 **E 就是除权除息日**，与 A 股一致。前复权**不做任何日期映射**，
直接把 E 交给现有的精确链：链的语义不变——事件只作用于**日期严格早于 E** 的 bar
（`src/qfq.rs:219-267` 的 `event_date > bar_date`，`src/official_market.rs:1584-1586` 的注释
「Output depends only on each record and the events strictly after its date」），**E 当天及以后**的 bar 不调整。
E 落在周末或节假日时也不改动。同一个 E 上有多个事件时按**先 cat11 后 cat1** 合并（§2.3）。
于是 A 股与 ETF 共用同一条链、同一个严格大于判定，`etf_event_transform`（§2.2）是唯一的新增部分。

**D-93 的判定用例（单测 T7）**：`sz.159922` cat11 在 E=2024-12-01、cat1 在 E=2024-12-02，
2024-11-29 那根 bar 6.159 → `6.159 / 2.5 − 0.1292 = 2.334400`（对官方 `qss` 2.334）；
`sz.159919` E=2025-03-31（cat1），3-28 那根 4.111 → **4.044900**，3-31 当天的 bar 不调整。
反次序 `(6.159 − 0.1292) / 2.5 = 2.4119` 必须不通过，这条守「合并次序」。

> **以下为已由 D-93 取代的原 R→X 映射（D-91 第 (1) 条），原文保留作反例，不再作为判据。**
>
> ETF 的 gbbq 事件日 **R 是权益登记日**，事件从 **X = R 之后的第一个交易日** 起生效。
> 现有 A 股链只作用于**严格晚于 bar 日期**的事件。直接喂入 R 会让 **R 当天**那根 bar 漏调，
> 而按 D-91，R 当天仍属「除权前」，必须被调整。所以：
>
> 1. 先把事件的生效日从 R 改成 X（X = 该实体 bar 序列里第一个 `trade_day > R` 的交易日；
>    R 是周末/节假日时语义不变），再进 `forward_adjust_exact` 的严格大于判定。
> 2. 同一个 X 上的多个事件合并成一次调整，次序固定**先 cat11 后 cat1**。
> 3. X 的计算需要该实体的交易日历；日历缺失或 X 落在序列之外时按 §3.1 的阻断规则处理，不猜。
>
> 取代原因见 §9.4：`adjustment_events.effective_day` 实测就是除权除息日（`sz.159901` 三次 cat11 的前收
> 恰好是 `3.966/5`、`0.566/0.2`、`7.411/2`；`sz.159830` 公告除权日 2024-12-20、`sz.159901` 2025-11-17 公告除息日、
> `sz.159919` 2025-03-31 前收 `4.111 − 0.0661 = 4.045`）。「事件日 = 登记日」是把 VM2-19 的**事件日**解码早一天
> （同样的 `1969-12-31` 问题）造成的误判；VM2-22 只修了日线日期。PR-F（#614）在生产数据上用 R→X 时，
> 在 `sz.159901` 出现重复调整导致的假跳空，由此暴露了这个问题。
> 官方 154/154 与公告 985/1,001 的核对结论不受影响，因为「旧事件日之后的第一根 bar」恰好就是 E。

### 2.3 同日 cat1 + cat11 的次序

D-89 只规定「先 cat11 后 cat1」，**没有定义同日两类事件**。设计 §8.3 说
「事件同日同时有 cat1/cat11 时，以官方前收核对决定次序」，并指出本代有 34 只
ETF 同时带两类事件（`docs/tasks/issue-509-m3-2-etf.md:490`）。

两种次序对结果**不同**（`x/c3 − c1/10` 与 `(x − c1/10)/c3`），差值 `= c1/10·(1/c3 − 1)`。
所以这不是洁癖问题，是数值问题。规则建议：

- 默认按 D-89 的「先 cat11 后 cat1」；
- 对**同日同实体**出现的组合，用官方前收做锚定核对；核对结果与默认次序不符的，
  该事件组进入阻断集合（§3），**不按默认次序静默计算**；
- 不能核对的组合（沪市无历史日线等）同样进阻断集合。

同日组合到底有多少个：**旧的「同日组合 0 条」已作废**（§9.1 的日期解码错误）。按 `events-v2.csv` 重算，
同一 `(entity, day)` 上同时有 cat1 与 cat11 的事件仍为 **0 条**（`Counter((r["entity"], r["day"]))` 无重复项），
但按**生效日 X** 归并后出现 **2 组**共享同一个 X 的事件（`Counter((r["entity"], r["X_day"]))` 计到 2 个重复 key）：
`sz.159922` 的 2024-12-02（cat11 `c3=2.5` + cat1 `c1=1.292`，官方 `qss` 2.334 可锚定），
以及 `sh.511880` 的 2013-04-18（两个 cat11，`c3` 均为 0.01，`official_verdict=no_bar`，不影响序列）。

- **已裁定**：同一个生效日上的事件按**先 cat11 后 cat1** 合并（D-91 第 (1) 条的后半句由 **D-93** 沿用），
  `sz.159922` 就是锚定样本（`6.159 / 2.5 − 0.1292 = 2.3344` 对官方 2.334）。本节第 2 条的「不符即阻断」在没有反例时保持为防御性检查。
- 事件日 E 直接进链与同 E 合并都在 §2.1.1（D-93 后不再有 R→X 映射）；PR-A 必须包含这两件事（§11）。

### 2.4 单元测试清单

纯函数测试，不依赖数据库，放在 `src/qfq.rs` 的 `mod tests`（现有测试同文件）。

| # | 测试 | 断言 |
|---|---|---|
| T1 | `raw/1000` 换算 | `ohlc_raw = 4_123` → `4.123`；`scale=6` 时 6dp |
| T2 | cat1 单事件 | `T(x)=x−c1/10`；空事件集合是恒等（与 `forward_adjust_exact` 一致的边界） |
| T3 | cat11 单事件 | `T(x)=x/c3`；`c3=0.93584` 时结果不是 `x/0.94` |
| T4 | 严格大于（D-93 后不映射） | 事件只作用于**严格早于 E** 的 bar（`E > bar_date`）；**E 当天及以后**的 bar 不受该事件影响；`E` 落在周末/节假日时同样只作用于 E 之前的 bar。断言 `sz.159919` E=2025-03-31 时 3-31 当天的 bar 不调整（§2.1.1） |
| T5 | **`sz159919` 锚定例（D-93 断言）** | E=2025-03-31 的 cat1 事件：2025-03-28 那根 bar 的收盘 4.111 → **4.044900**（`4.111 − c1/10`，`c1/10` 使结果与官方前收 4.045 在 ±0.001 元内）；**2025-03-31 当天的 bar 不调整**。同一 bar 的 6dp 值只由 (bar, events, scale, version) 决定 |
| T6 | **`sz.159922` 跨 E 双因子例（D-93 断言）** | cat11 在 E=2024-12-01（`c3=2.5`）、cat1 在 E=2024-12-02（`c1=1.292`）：2024-11-29 那根 bar 6.159 → **2.334400**（对官方 `qss` 2.334，±0.001）；2024-12-01 与 12-02 当天的 bar 不套用各自及之后的事件。旧的「cat1 记成 2.5:1 拆分、应判阻断」结论**撤回** |
| T7 | 合并次序 | `sz.159922` 用例：输入 6.159，`c3=2.5`、`c1=1.292`，先 cat11 后 cat1 得 **2.3344**；反次序 `(6.159 − 0.1292) / 2.5 = 2.4119` 必须不通过 |
| T8 | 事件后缀记忆化 | `etf_forward_adjust_exact` 与逐 bar 直接计算的参考实现逐位相等（对应 `forward_adjust_exact_reference`，`src/qfq.rs:269-304`） |
| T9 | 非 1/11 类别 | 传 category=2 的 ETF 事件 → `Err`，不猜 |
| T10 | 空序列 / 单 bar | 与 A 股同构行为：空 bar → 空输出；单 bar → 只受其后的链影响 |
T5/T6/T7 的**具体官方数字**取自 `events-v3.csv` / `events-v2.csv`（列 `prev_close` / `X_day` / `official_qss` / `X_open`，见 §9.2、§9.3），不取自 VM2-19 的 `events.csv`（该表事件日与日线日期都解码早一天，见 §9.1）。
2026-11-08 再更正（D-93）：`sz.159922` 的两条事件按 **E** 生效——cat11 在 E=2024-12-01、cat1 在 E=2024-12-02，
2024-11-29 那根 bar（6.159）同时受两条影响，先 cat11 后 cat1 得 **2.334400**，与官方 `qss` 2.334 一致；`.day` 的 X 开盘 2.461（官方 `ks` 亦 2.461）落在 E 之后，按 D-93 不调整。

---

## 3. 按实体 fail-closed（D-89 第 6 条）

D-89 原文：「核对不了或不能归因的事件**不得静默进入前复权**：实现时按实体 fail-closed，
该 ETF 的 `qfq` 返回 `adjustment_unavailable` 并带上原因。」

### 3.1 哪些事件状态会阻断

| # | 阻断条件 | 判定依据（数据侧） | 现状可得吗 |
|---|---|---|---|
| B1 | cat11 事件既核对不了官方 `qss`，也核对不了公告 | 「X 当天」（= E 当天；E 非交易日时取 E 之后第一根 bar）无官方 `qss` **且**该事件没有逐条公告核对记录（`.day` 内部一致性只作旁证，不算核对） | 需全量公告取证落库后才可判；待核对 1,001 条 / 260 只（§9 P7） |
| B2 | cat1 事件既核对不了官方 `qss`，也核对不了公告 | 同上（核对项：公告的金额与除息日 E；登记日只作旁证，D-93 后链不按登记日） | 同上 |
| B3 | 事件类别为 1/11 之外（ETF 不应出现） | `adjustment_events.category NOT IN (1,11)` | 是（DDL 已挡，防御性检查） |
| B4 | 同一 `(entity, E)` 上多个事件的合并结果无法锚定 | 该组用「X 当天」（= E 当天；E 非交易日时取 E 之后第一根 bar）的官方 `qss`（深市）或公告（其余）核对的结果，与「先 cat11 后 cat1」合并值不符或缺失 | 已有锚定样本 `sz.159922`（§2.1.1）；`sh.511880` 2013-04-18 那组两条均在首根 bar 之前（`no_bar`），不影响序列 |
| B5 | 因子不可解析成精确分数（非有限、位数超预算、`c3 = 0`） | 运行时 `etf_event_transform` 返回 `Err` | 是（`src/gbbq.rs` 已有同类错误） |
| B6 | 事件生效日 E 无从确定（bar 序列/D 文件缺失，链无法定位「严格早于 E」的 bar） | 交易日历/序列边界 | 运行时判定 |

**判定口径（D-91 第 3 条，2026-11-08 更正）**：**只有「官方 `qss` 和公告都核对不了」才阻断**。
下文与 §9 里沿用 D-91/D-92 原文的「X 当天」，按 **D-93** 读作**「E 当天；E 不是交易日时取 E 之后的第一根 bar」**
（E 是除权除息日，链不做 R→X 映射；核对实现见 #616/#617）。
深市优先用「X 当天」的官方 `qss`（±0.001 元）；没有 `qss` 的事件走公告核对，核对项是金额（`c1` 元/10 份、`c3` 折算比）
以及登记日 R 与除息日 X。沪市没有历史日线接口（831 条），**全部走公告核对**，不再因为「无官方 `qss`」而直接阻断——
旧表里沪市的 `NOT_AVAILABLE` 只说明「没有 `qss`」，不构成阻断依据。

**核对口径（2026-11-08 审核更正）**：`basis_v2=verified_internal_only` 表示 `.day` 内部一致性通过，这一项**只作旁证、不作放行依据**；
在 D-91 下这些事件仍必须由 X 当天的官方 `qss` 或基金公告核对（`official_verdict == "official_unavailable"` 的 1,001 条即待核对集合，§9 P7）。

**不阻断**的：**首根 bar 之前的事件**和**未来日期的事件**不影响序列，按 D-91 第 3 条**不纳入核对**，自然不阻断。
按 `events-v2.csv` 计数：`attribution_v2=event_before_first_bar` **75 条 / 74 只**，
`no_bar_on_R_future_or_untraded` **3 条**（`sh.515450`、`sh.530880`、`sz.159118`，均为 2026-11-09），
合计 78 条与 `official_verdict=no_bar` 的 78 条一致。但「上市后有没有发生过 0.01 折算」是 #607 的 AC 之一：
`c3=0.01` 的 **21 条 / 20 只** cat11 事件（`r["cat"]=="11" and r["c3"]=="0.01"`）**全部落在首根 bar 之前**，上市后 0 条；
`c3=0.1` 的 2 条（`sh.510020` 2012-12-13、`sh.511030` 2025-03-18）在首根之后，属沪市，走公告核对。

### 3.2 原因码与证据落点

现有 wire 只有一个码 `adjustment_unavailable`，文本固定
（`src/sqlite_market/series.rs:593-595`）。建议：**保留错误码不变，扩展 detail**，
因为改码会让所有现有客户端分支失效（现在它们只认这一个码）。

```json
{ "code": "adjustment_unavailable",
  "message": "adjustment_unavailable: ETF sh.510050 has no verified 前复权 series",
  "details": { "reason": "etf_factor_unverified",
               "blocking_events": 12,
               "categories": [1, 11] } }
```

建议的原因码（`details.reason`）：

| 原因码 | 含义 |
|---|---|
| `etf_factor_unverified` | 有事件未通过核对（B1/B2/B6） |
| `etf_event_order_unverified` | 同日 cat1+cat11 次序未锚定（B4） |
| `etf_event_category_unexpected` | 出现 1/11 之外的类别（B3） |
| `etf_factor_unparsable` | 因子本身不可解析（B5） |
| `etf_qfq_not_built` | 该 ETF 还没有构建过前复权序列（实现初期/新上市实体） |

`blocking_events` 只给条数，不给事件明细——明细走研究导出或运维查询，
不要把可变长数组塞进错误信封（`docs/contracts/generic-data-v11.md` 的响应预算）。

**核对结论存哪里（Q2）**：现在的 `adjustment_events.basis_status` 是
`verified|pending`（`migrations/tasks/027_etf_factor_events.sql:29`）。三个做法，
每个都先看它对**部署**意味着什么（见 §3.2.1）：

- **(a) 扩 `basis_status` 枚举** 成 `verified | pending | unverifiable | owner_accepted`：
  SQLite 不能放宽 CHECK，只能重建表（同 027 的做法）。语义最直接，但把核对结论写进事实表；
- **(b) 在行情库里新建证据表** `etf_factor_checks(entity, ordinal, check_status, basis_ref, checked_at)`：
  事实表不动，但**仍然是 schema 迁移**（27 → 28）。**这不是「无需迁移」**——
  在本仓库「fresh init only」的部署模型下，新建一张表和在旧表上加一列对部署的代价完全相同；
- **(c) 不改 schema**，把核对结论当成**证据**按内容寻址登记进来源库
  （`source_store`），运行时加载成「阻断集合」（§3.2.2）。

#### 3.2.1 先看 schema 变更现在怎么部署

这三条路径是当前部署模型的事实，决定了 Q2 各选项的真实代价：

1. **行情库不做原地升级。** `deploy_phase_g.sh` 的文件头写明
   「the existing market database is NEVER upgraded in place and NEVER deleted —
   it is renamed aside and `task_sqlite init21` creates a fresh compact database」
   （`scripts/deploy_phase_g.sh:19-22`）；`COMPACT_SCHEMA_VERSION` 的实现注释同样写
   「Every step stays fresh-init only, never in-place: a file built by another schema
   revision is refused, not upgraded (D-08)」（`src/sqlite_tasks.rs:49-54`）。
2. **只换二进制不行。** 读路径有两处硬版本检查会在旧库上 fail-closed：
   `src/research/read.rs:73`（导出侧：`version != COMPACT_SCHEMA_VERSION` 直接拒绝）和
   `src/sqlite_market/generation_facts.rs:225`（generation facts 装载侧：同）。所以 schema 一旦 +1，
   **`switch_binary.sh` 的纯换二进制路径不再可用**——
   该脚本的不变量是「数据库文件、WAL/SHM 永不删除、移动或截断」
   （`scripts/switch_binary.sh:9-12`），它不会替你做 fresh init。
3. **唯一的原地升级是历史特例。** `upgrade_compact`（`src/sqlite_tasks.rs:245-274`）
   只接受 **schema 18 → compact**，且要求所有业务表为空（`require_business_tables_empty`）；
   它不是通用的 N→N+1 升级路径。
4. **所以改 schema = 一次完整重建。** 顺序是 runbook §4
   （`docs/new-host-runbook.md:184-215`）：部署 → `source_store replay --scope all`
   （在 `init21` 之后、服务启动之前，`scripts/deploy_phase_g.sh:454-462`）→
   `full` 刷新 → 目录首采 → A 股日线首采（53 批）→ 主表发布 →
   `sh_sz_a_listed_ever` 首采 → 年线 plan-run → ETF 日线首采（`sh_sz_etf_current` 三批 500/500/500/131
   + `sh_sz_etf_listed_ever` 128 只）→ 读回验收。

**一次重建的实测代价（都注明口径与出处）**：

| 项 | 读数 | 出处 |
|---|---|---|
| 从克隆完成到验收全部通过 | 约 2 小时 15 分 – 2 小时 40 分 | `docs/new-host-runbook.md:282-284`（上次迁移实测） |
| 其中 `full` 刷新 | 约 73 分钟（原有 7 来源 42.5 min + ETF 4 来源 30.5 min） | `docs/new-host-runbook.md:194` |
| A 股日线首采 | 53 批，约 3.3 分钟（199 s） | `docs/new-host-runbook.md:196` |
| 年线 plan-run（生产单任务形态） | 89.6 s / 127.9 s（vm2 生产日更） | `docs/SLO.md:19` 第 3 行 |
| 同代全量 `update_data` write-loop | 573.4 s（XDM-84 独占实测） | `docs/SLO.md:17` 第 1 行 |
| 部署窗口（服务停摆） | 47 s（历史 preview.41 退役）/ 20 s（preview.42 部署窗口） | `docs/STATUS.md:149`、`docs/STATUS.md:62-63` |
| 首采后 DB 体积上限 | ≤2.5 GiB（hard）；当前 2.31 GB | `docs/SLO.md:16` 第 4 行；`docs/STATUS.md:539` |

要点：**schema 变更的代价不只在写代码那一次**，而是每次部署都要付一次约 2–3 小时的
重建，而且这期间生产数据是空的（存量库被归档、新库要靠首采重建）。

#### 3.2.2 (c) 的具体做法：证据入来源库

来源库（`source_store`）已经是「fresh init 无法重新生成的事实」的唯一持久副本
（`scripts/deploy_phase_g.sh:445-450` 的注释），并且已经服务多个写入方
（`replay --scope all`、`--scope industry`、`--scope generation_facts`）。
把 ETF 事件核对结论登记进去，与既有模式一致：

- **写入**：核对完成后生成一份 `etf-factor-checks` 记录（每个 `(entity, ordinal)`
  一条：`check_status`、官方前收来源与 sha、比对结果、核对日期），按内容寻址
  （sha256 即 id），进来源库。版本演进 = 新增一条记录，旧记录不删。
- **读取**：这是 (c) 的核心约束——**当时序关系必须与库一致**。(c) 不能在启动时
  把来源库整表读进内存，因为**来源库的新旧与行情库的新旧不一定同步**：
  - 重放（`deploy_phase_g.sh` 的 `replay`）会写库，但**不会**把 ETF 核对结论写进
    `adjustment_events`（那不是重放范围）；
  - 一次日更可能带来新的因子事件（新折算/新分红），而来源库里还没有对应核对结论。
  所以阻断集合的解析时机必须是**读面按需查询 + 内容摘要绑定**，而不是启动时快照：
  - 每次 `qfq` 解析该 ETF 时，取该实体**当前**因子事实的内容摘要
    （`adjustment_events.evidence_digest`，`migrations/tasks/027_etf_factor_events.sql:32`
    已经有这一列），用 `(entity, evidence_digest)` 去来源库索引里查核对记录；
  - 没有匹配记录 → `etf_factor_unverified`（缺省即阻断，不需要"已核对白名单"）；
  - 有匹配记录且全部通过 → 放行；有未通过项 → 带原因码阻断。
  这样「库里多了一条未核对的事件」会**自动**变成阻断，不依赖任何离线同步动作。
- **缓存**：把 `(entity, evidence_digest) → 判定` 缓存在进程内，key 含摘要，
  所以因子一变缓存自然失效；不需要失效通知。
- **replay 与复现**：
  - `replay --scope all` 的语义不变（不碰 ETF 核对结论），所以 (c) 不改变 replay 的
    任何已有 scope；要新增一个 `--scope etf_factor_checks`（只读证据、不写库），
    或干脆不进 DB（见下一条）。
  - **不落库的代价**：核对结论不在行情库里，所以 `query_data` 的
    `inputs`（版本号 + 算法版本）**不足以**复现一次响应——
    同样的 `market.daily` 版本 + 同样的算法版本，换一个来源库可能给出不同的
    阻断判定。这是 (c) 最大的语义代价。缓解办法：在响应的 `details` 里带上
    `etf_checks_digest`（核对记录的内容摘要），并在 `get_data_capabilities` 的
    算法版本列表里登记 `etf-factor-checks-v1`；`daily_qfq` 导出的 manifest 也要记这个摘要。
  - **导出/验签**：`source_store replay --verify-only` 已经能校验来源库自身，
    核对记录跟着它一起被校验；不受影响。

**写面复杂度**：需要一个小的来源库 scope + 一个只读装载器 +
「摘要 → 判定」的缓存。规模是几百到几千条记录（P1/P2 的取证规模），
不需要索引结构以外的存储。

### 3.3 给 owner 的两个方案

| | **(a) 整只阻断** | **(b) 截断序列** |
|---|---|---|
| 语义 | 任一阻断事件 → 该 ETF 的 `qfq` 恒 `adjustment_unavailable` | 只返回**最后一个阻断事件之后**的 bar 的前复权序列；更早的历史不可读 |
| 序列形态 | 该实体没有 `Forward` 序列行 | 有 `Forward` 序列行，但 `first_day > ` 不复权的 `first_day` |
| 读面表现 | 一个稳定错误 + 原因码 | 一段**看起来完整、其实被截短**的序列；客户端只能靠 `first_day` 与 `availability` 的覆盖声明发现 |
| 实现复杂度 | 低：判定 + 报错 | 高：要定义「阻断事件之后」的边界、要让 `availability`/`versions`/导出一致、要防「截断点随因子修订漂移」 |
| 回测风险 | 低：拿不到就是拿不到 | 中：序列起点会随每次因子修订变化，**同一份代码跑两次可能拿到不同长度的历史**，这是最难发现的偏差 |
| 与 D-89 的关系 | 直接兑现「不得静默进入前复权」 | 「部分前复权 + 部分缺失」不是 D-89 写的那种「fail-closed」，需要 owner 另行裁定 |
| 数据新鲜度 | 一只 ETF 一条坏事件 → 整只不可用（可能比 (b) 更伤人） | 保住近期可用性 |

**推荐 (a)**。理由：
1. D-89 明写「该 ETF 的 `qfq` 返回 `adjustment_unavailable`」，是整只语义；
2. 截断序列的边界一旦随因子修订移动，就是**静默的**偏差，而这正是 D-89 要防的；
3. (b) 的真实收益要看 B1/B2 最终影响多少只 ETF。**已取得（§9.3）**：待核对 1,001 条涉及的 16 条未通过项在 D-92 第 1 条的
   复权价容差下全部判为一致，**D-92 下阻断实体数为 0**（337 只全部可出前复权），所以 (b) 目前没有收益，D-92 第 2 条按 (a) 裁定；
   若以后出现新的阻断集合（Q10 的新事件未核对），(b) 仍可作为**独立 Issue** 再议，而不是塞进本期。

---

## 4. 存储

### 4.1 序列形态

设计 §338（`docs/tasks/issue-509-m3-2-etf.md` §5.1）已经写明 `SeriesShape::etf(qfq)` → 6，
代码里这条形状也已存在（`src/sqlite_market/series.rs:91-99`）。所以：

| 维度 | 不复权（现状） | 前复权（本期新增） |
|---|---|---|
| `market_series.instrument_class` | 2（ETF） | 2（ETF） |
| `market_series.adjustment` | 1（raw，`Adjustment::Raw`） | 2（forward，`Adjustment::Forward`） |
| `market_series.price_scale` | 3（厘） | 6（1e-6 元） |
| `series_bars.*_units` 含义 | `price × 10^3` | `price × 10^6` |
| `volume` / `amount` | 份 / 元（`volume_quantum=100`） | **与不复权逐字节相同**（`qfq.rs` 文件头 `src/qfq.rs:36-38`：量额不参与复权） |
| 内容摘要 seed | `series-fold-v2`，含上面三项 | 同上，因 shape 不同而天然不同 |

**不需要 schema 迁移**：`market_series` 的 CHECK 已经允许 `instrument_class=2`、
`price_scale ≤ 6`（schema 26 引入，见设计 §0 第 2 条与
`migrations/tasks/027_etf_factor_events.sql` 的注释）。

### 4.2 序列数与 bar 数增量

基数（**都标出处，不新造数字**）：

| 量 | 值 | 出处 |
|---|---|---|
| ETF 主表行数 | 1,846 | `docs/STATUS.md:345`（11-05 读数，标明「历史」） |
| 有日线的 ETF 批次数 | 1,632（`task-c8fee63e…`） | `docs/STATUS.md:380` |
| 现有 `market_series` 总行数 | 18,118 | `docs/STATUS.md:539`（2026-11-07 只读快照） |
| 现有 `series_bars` 总行数 | 37,831,020（11-05）/ 36,145,465（另一时点） | `docs/STATUS.md:341`、`docs/STATUS.md:442` |
| 无日线的 ETF | 86 只 | `docs/STATUS.md:345` |
| 带事件的 ETF 只数 | 331 | 设计 §8.1（`issue-509-m3-2-etf.md:452`） |
| 当前库体积 | 2,313,494,528 B | `docs/STATUS.md:539`（XDM-198） |

增量估算（**推断，不是实测**）：

- **序列数**：每只「有日线且通过核对」的 ETF +1 条 `Forward` 行。
  上限是 1,632 条（若全部通过核对）；按方案 (a) 的阻断，实际是
  `1,632 − 阻断实体数`。旧按 D-90 A/C/D 得出的 **29 只**已作废（D-90 的事实依据不成立，`sz.159830`、`sh.512890` 两条撤回，
  `sz.159922` 由阻断改为合并用例）。**D-92 下阻断实体数为 0**（§9.3：待核对的 1,001 条里 987 条由公告证实、16 条在
  D-92 第 1 条的复权价容差下判为一致，`entity-summary.json` 的 `fail_closed_list` 的 **11 只**不再阻断），
  所以序列增量按上限 1,632 条计；实际条数仍要等实现后统计（有日线的 ETF 只数会变）。
  `market_series` 从 18,118 涨到至多约 19,750（推断）。
- **bar 数**：前复权 bar 与不复权 bar 一一对应（`forward_adjust_exact` 保序保数，
  调用方 `src/official_market.rs:1593-1600` 显式检查了 `adjusted.len() == raw_bars.len()`）。
  所以增量 = ETF 不复权 bar 总数。**该数字已由 vm2 只读预检（报告 `607-p8p10`，仓库外）取得，见 §9.5 P8**：
  ETF 不复权日线 **1,761 条序列 / 1,645,415 根 bar**（`instrument_class=2 AND adjustment=0 AND period=1`，
  `record_count` 合计与 `series_bars` 实际行数逐序列完全一致）。
- **库体积增量**：设计 §0 第 7 条给过一条非直接依据：ETF 加入把同代全量
  `update_data` 的 item 数从 5,238 推到约 6,869（设计 §11）。这只说明 item 数，
  不等于 bar 数。体积增量 = `ETF bar 数 × per-row 成本`，而 per-row 成本
  `series_bars` 已有可测口径（SLO 备注里的「净增长 ≈ 3.4KiB/item」是**周期增量**，
  不是稳态存储成本，不能直接相乘）。

  口径要写清：**前复权是在不复权之外新增的一整条序列，不是就地改写**，所以库增量
  **约等于这条序列本身的体积**，不能用「复权比不复权每行多几字节」的差额去算。
  （vm2 只读预检报告 `607-p8p10` 初稿里「相对不复权 +29.3 MB」的写法是错的，已按此改正。）

  | 口径 | 每 bar 成本 | ETF 前复权体积（1,645,415 bar） | 出处 |
  |---|---|---|---|
  | 按页估算（`dbstat` 整表 1,860,857,856 B / 37,768,593 bar，按 `price_scale` 6 vs 2 的载荷比例拆分） | 约 58.2 B/bar | **约 96 MB** | §9.5 P8 |
  | PR-C 写库基准实测 | **41.8 B/bar** | **约 69 MB** | §9.5 P8 |

  两个口径都要留着：按页估算是「表数据页」的理论上限，41.8 B/bar 是写库基准的实测值，
  更贴近真实增量。**生产库现在 2,313,494,528 B（约 2.32 GB），SLO 第 4 行的上限是 2.5 GiB**，
  按 41.8 B/bar 的 69 MB 计预计到约 2.39 GB，**仍在门禁内但余量已不大**——
  部署前后各要采一次 `state/preview.sqlite` 的字节数（§8.2）。

**给取证的精确 SQL（只读，生产侧执行）**：

```sql
-- ETF 不复权 bar 总数与实体系数
SELECT count(*) AS series, sum(record_count) AS bars
FROM market_series WHERE instrument_class = 2 AND adjustment = 1;
-- 带事件但事件全部早于首根 bar 的实体（不阻断集合的候选）
SELECT e.entity, count(*) FILTER (WHERE e.effective_day < s.first_day) AS before_first
FROM adjustment_events e JOIN market_series s
  ON s.entity = e.entity AND s.instrument_class = 2 AND s.adjustment = 1
GROUP BY e.entity;
```

### 4.3 写入路径

- 首采/重建：ETF 不复权 collect 之后补一次派生构建（见 §5.2），或按 `update_data`
  的 Full 语义整条重写。
- 日更：见 §5。
- **幂等**：前复权的 `version_id` 由内容摘要决定（`series-fold-v2`），
  同样的 bar 列表 + 同样的 shape 得到同一个版本号，重跑不会造新版本。
  这一点和 A 股前复权一致，SLO 第 1 行的 `data_versions +0` 因此仍可比。

---

## 5. 日更 plan/run

### 5.1 现状：ETF 日更只有一条不复权路径

| 环节 | 位置 | 事实 |
|---|---|---|
| plan | `src/data_supply/plan.rs:705-773`（`resolve_etf_daily_on`） | 注释明写「ETF has one 不复权 series only (no forward/raw pair, no yearly)」；每个 item 带 `PlanSeries::EtfRaw`（`src/data_supply/plan.rs:770`） |
| 计划项路由 | `src/data_supply/plan.rs:93-108` | `PlanSeries::EtfRaw` 是唯一的 ETF 标记 |
| run | `src/market_update/runner.rs:1006-1100`（`acquire_and_commit_etf`） | 只接受 `update_data`，frozen marker 必须是 `EtfRaw`，非前缀重写是 item 失败（`etf_history_rewrite`） |
| 提交 | `src/sqlite_market/compact_repo.rs:1372-1460`（`commit_etf_raw_typed`） | 校验基版本是「3dp raw ETF 形状」；`Append` / `Metadata` / `NoChange` 三态；同时刷新 cat1/cat11 因子事实 |

也就是说：**ETF 日更的输入同时包含 bar 和因子**，一次提交两样都更新
（`commit_etf_raw_typed` 的 `factors` 参数）。前复权要在这个既有提交之后派生，
不需要新的来源绑定。

### 5.2 怎么产出前复权

推荐：**在同一次 `update_data` 的 item 内，不复权提交成功后增量产出/重写前复权**。

| 场景 | 触发条件 | 动作 |
|---|---|---|
| 不复权有新 bar，因子未变 | `TypedDecision::Append` 且因子事实摘要未变 | 按 `bar` 的**后缀**复用：新 bar 的前复权值只依赖其后的空链（最新 bar 的链恒为恒等），所以**只追加**新 bar 的前复权行；已有行不动。这正是 `forward_adjust_exact` 的「proven-identical prefix never needs re-adjustment」（`src/official_market.rs:1581-1586` 注释） |
| 不复权有新 bar，因子也变了 | 因子事实摘要变了 | **整条重写**该 ETF 的前复权序列（Full/rewrite） |
| 不复权无变化，因子变了 | `TypedDecision::Metadata`（`compact_repo.rs:1441-1447`） | 整条重写；`data_versions` 应 +1（内容变了） |
| 不复权与因子都无变化 | `TypedDecision::NoChange` | 不动；SLO 第 1 行要求的 `data_versions +0` 得以保持 |
| 该 ETF 之前没有前复权序列 | 首次构建 / 新上市 | 全量构建 |

「因子摘要」已经在库里可用：`adjustment_events` 有 `evidence_digest` 与
`source_binding_digest`（`migrations/tasks/027_etf_factor_events.sql:31-32`），
用它做「因子是否变化」的判定，不要另算一次哈希。

### 5.3 是否复用 A 股前复权的 Full/rewrite 语义

**复用「语义」，不复用「触发源」**：

- 复用：Full/rewrite 的提交路径、`RevisionUnverified` 的 fail-closed 规则、
  摘要与版本链语义（`docs/tasks/issue-509-m3-2-etf.md` 与
  `docs/latest-only-storage.md` 里对 A 股前复权的描述），以及
  「同代无变化 → `no_change` → `data_versions +0`」这条性质。
- 不复用：A 股前复权的数据来源是 `.day` + `.gbbq` 的**类型化 acquire 决策**
  （`TypedDecision::Append`，`src/market_update/runner.rs:1090-1096` 与设计 §
  「跨代日更」段），
  带 generation/source/coverage metadata；ETF 前复权是**纯派生**，
  没有独立的来源绑定，`coverage` 应继承不复权序列的 `coverage`。
- **不要**给 ETF 前复权开一条独立的 plan item 类型（比如 `PlanSeries::EtfQfq`）：
  它会让 plan 的 item 数翻倍（SLO 第 1 行的基数从 5,238 再涨），
  而派生完全可以挂在既有 item 的提交事务里。这符合设计把 ETF 写成
  「一个 item 一条序列」的既有选择。

### 5.4 对 SLO 第 1、2 行的影响（推断）

| 行 | 现状 | 影响 | 判断依据 |
|---|---|---|---|
| 第 1 行（同代无变化全市场 `update_data`，write-loop <600s，净增长 ≤4KiB/item，`data_versions +0`） | 5,238 item / 573.4s（XDM-84）；生产近期 154.3–175.3s（跨代） | **item 数不变**（派生挂在既有 ETF item 内），所以 write-loop 的 item 级成本基本不变；但每个 ETF item 多做一次「因子摘要比较」，无变化时不写 bar。**推断：影响在噪声内，需实测** | `src/market_update/runner.rs:1006-1100` 已有 ETF 路径，新增的是事务内的一次比较 |
| 第 2 行（跨代有变化 `<600s`） | 175.3s / 154.3s（vm2-126）；设计 §0 第 7 条外推「ETF 加入后全量约 752s」（**前提是 item 数涨到 6,869**） | 若按本文方案（item 数不变），第 2 行不受设计 §0 第 7 条那条外推的影响；真正的增量是**因子变化时的整条重写**成本，取决于每年发生多少次 ETF 份额折算/分红。**推断：低频（每年数十到数百个事件，数量待取证 VM2-19）**，不足以推动单次日更越过门槛 | 设计 §8.1 给的 cat1 974 / cat11 250 是本代事件总数（累计），不是年度事件率 |
| 首次/重建 | 首采 ≤20min，DB ≤2.5GiB（SLO 第 4 行） | 重建要额外跑一遍全量 ETF 前复权构建。**推断：与 ETF 不复权 collect 同量级或更小**（无 IO，纯计算），但需要实测；DB 当前 2.31GB 已经贴近 2.5GiB 门槛，ETF 前复权的体积增量因此是**硬约束**（§4.2 待取证） | `docs/STATUS.md:539`；`docs/SLO.md` 第 4 行 |

实现 PR 的验收必须给出：一次同代无变化全量 + 一次含 ETF 因子变化的定向
`update_data` 的 write-loop 与库增量，并注明主机、并发、代码版本
（AGENTS.md 要求「不因采用 Rust 就宣称高性能」，SLO 的每一行都要口径）。

---

## 6. 读面与契约

### 6.1 capability revision 要不要升

**推荐升 `generic-data-v12`。** 依据是 D-70 Q10 的判例：「新 scope enum / `adjustment`
参数是 wire 形状变化，不是 D-32 的『精度不变』」。`adjustment=qfq` 的**结果**从
「必然 `adjustment_unavailable`」变成「可能返回 6dp 数据或带原因码的错误」，
客户端的分支行为变了，属于同一类变化。

具体改动点（都在读面，不改工具签名）：

| 项 | 位置 | 改什么 |
|---|---|---|
| capability 常量 | `src/data_supply/mcp.rs:454` | `"generic-data-v11"` → `"generic-data-v12"` |
| series 能力表 | `src/data_supply/registry.rs:361-421`（`series_capabilities`） | 新增一行 `(Daily, "etf", "qfq", 6, "yuan", "share", Some(100), false)`；`default=false`，因为 ETF 仍要求显式口径 |
| 错误映射 | `src/data_supply/mcp.rs:835-838` | 错误码不变；新增 `details.reason`（§3.2）。**注意**：`ApiError.details` 目前只在 `Error::Task` 分支填（`src/data_supply/mcp.rs:819-825`），ETF 分支要新增一条 |
| 契约文档 | `docs/contracts/generic-data-v12.md`（新）+ `docs/contracts/generic-data-v11.md` 状态行 | 新增一节「ETF 前复权」；v11 标为历史 |
| 事实核对脚本 | `scripts/check_current_facts.py:40-60` | `EXPECTED_DATA_CAPABILITY_REVISION` / `EXPECTED_DEPLOYED_*` / `EXPECTED_SKILL_CAPABILITY_REVISION` 一起改；**部署差异期**要注意脚本对「代码 vs 部署」的区分规则（同文件中已有 `code_data_tools=` 的先例） |
| STATUS 与客户端文档 | `docs/STATUS.md:125`、`docs/client-connect.md:139-140`、`docs/mcp-tools.md:352` | 现在都写「ETF `qfq` → `adjustment_unavailable`」，要改成新语义 |

### 6.2 客户端看到的变化

**升级前（现状）**：

```text
market.daily + ETF + adjustment=qfq
  → error { code: "adjustment_unavailable",
            message: "adjustment_unavailable: ETF 前复权 is not delivered; use adjustment=raw" }
market.daily + ETF + 无 adjustment
  → error { code: "adjustment_required_for_etf", ... }
```

**升级后**：

```text
market.daily + ETF + adjustment=qfq + 该 ETF 通过核对
  → 正常 rows：price_scale=6，volume 单位「份」（volume_quantum=100），
    amount 与不复权逐字节相同
market.daily + ETF + adjustment=qfq + 存在阻断事件
  → error { code: "adjustment_unavailable",
            message: "...ETF sh.510050 has no verified 前复权 series",
            details: { reason: "etf_factor_unverified", blocking_events: N } }
market.daily + ETF + 无 adjustment
  → 仍然是 adjustment_required_for_etf（D-70 Q4 的行为不变，**推荐不改**）
```

「无 `adjustment` 仍然报错」这一条要**明确保留**：现在 ETF 只有 raw 与 qfq 两种口径，
缺省到底该取哪个没有共识（设计 Q4 的理由是「同一 dataset 不同品种不同口径会误导回测」）。
一旦缺省取 qfq，「阻断实体」与「正常实体」在同一个请求里就会得到两种结果，
客户端的错误分支更难写。

### 6.3 fail-closed 的响应形态

- **HTTP/JSON-RPC 层**：沿用 `isError` + `code`，不返回空 `rows`。
  返回空页会被回测当成「这只 ETF 那天没有数据」，是 D-89 第 6 条要防的静默失败。
- **`versions` / `availability` 面**：该实体查询 `adjustment=qfq` 时同样报同一个错，
  不能只在读行时拦。这两条路径共用 `resolve_read_shape`
  （`src/sqlite_supply.rs:555-600`），所以只要在那个函数里拦，三处同时生效——
  这是当前代码结构的**优点**，实现时要守住，不要在新代码里另开一条解析。
- **`get_data_capabilities`**：`series` 里 ETF 的 `qfq` 行**照实列出**（能力是存在的），
  阻断是**实体级**的事实，不是数据集级的能力缺失。可选的补充是
  `availability` 的 `reasons` 里加 `etf_factor_unverified`。

**减法前复权的 ≤0 限制要写进契约**（[D-94](../decisions.md)）：v12 的契约文本与交付说明必须写明
**减法前复权保留的是价差、不是比例，早期价格可能 ≤0，不能直接拿来算收益率**；
**A 股与 ETF 都要写**（A 股一直有这个问题，见 §9.5 的 747 只、777,958 根 bar）。
这是**说明性限制，不是交付门槛**——负价照实返回，不阻断、不截断、不改值。
由 **PR-D** 落地。

### 6.4 给 owner 的选项

| 选项 | 内容 | 影响 |
|---|---|---|
| (a) | 升 `generic-data-v12`，`series` 加 ETF qfq 行 | 客户端可发现新能力；契约文档要新增 |
| (b) | 不升版本，只改 release notes | 违反 D-70 Q10 的判例；客户端无法从 revision 判断能否请求 ETF qfq |
| (c) | 升版本 + 同时把 ETF 缺省口径改成 qfq | 破坏「ETF 必须显式口径」，§6.2 的理由反过来 |

**推荐 (a)**。

---

## 7. 研究导出

### 7.1 `daily_qfq` 要不要收录 ETF 行

现状：`write_daily` 的过滤是 `instrument_class in {Stock, Etf} && adjustment == 目标`
（`src/research/parquet.rs:465-470`），而 `read.rs` 的 ETF 分支**只收 raw**
（`src/research/read.rs:361-372` 显式拒绝非 raw 的 ETF 序列行）。所以只要
ETF 前复权序列行进库，`write_daily(Forward)` **已经会自动收录它**，
不需要改过滤条件；要改的是 `read.rs` 那条 `integrity` 断言和 `plan.series` 的构造。

| 选项 | 内容 | 后果 |
|---|---|---|
| (a) | `daily_qfq` 收录 ETF 行 | 与「`market.daily` 能读 ETF qfq」一致；但导出的 ETF 行要带上阻断信息（被阻断的实体干脆不出现），且 `export_smoke.py:581` 的断言「`daily_qfq` 无非股票行」必须改成新语义 |
| (b) | `daily_qfq` 继续只收股票 | 导出口径与读面不一致：MCP 读得到、导出拿不到。这违反「同输入版本同算法版本给出同结果」的既有要求 |
| (c) | 新增 `daily_etf_qfq.parquet` | 行数少、单独文件更清楚；但多一个文件要维护 manifest、schema、sum 校验 |

**推荐 (a)**，并把 `scripts/export_smoke.py:577-581` 的断言改成：
「`daily_qfq` 的 ETF 行必须 `price_scale=6`，且每个 ETF 实体在 `market_series` 里
确有 `Forward` 行」（防止把 raw 混进来）。

### 7.2 快照 schema 要不要升版

`DAILY_SCHEMA`（`src/research/parquet.rs`）的列集合不变——ETF 前复权行只是
`instrument_class=2` + `price_scale=6` 的既有列组合。所以：

| 选项 | 内容 | 后果 |
|---|---|---|
| (a) | 不升，`instrument_class` 已有 0/2 两值 | 逐列兼容；老消费者把 ETF 行当「class 2 且 6dp」即可 |
| (b) | 升 schema 版号 | 会让所有消费者必须跟着升，收益只是「明显区分」 |

**推荐 (a)**。但要在 `docs/research-data.md`（或等价的导出说明）里写清
「`daily_qfq` 自本期起含 class 2 行、`price_scale=6`、量额与 `daily_raw` 逐字节相同」。

### 7.3 `factors.parquet` 的 `basis_status`

`write_factors` 的列集合是 `ordinal,effective_day,known_at,category,c1..c4`
（`src/research/parquet.rs:516-540`，来源 `src/research/read.rs:809-830`），
**不含 `basis_status`**。ETF 交付前复权后，「哪些事件进过链」变成研究侧必须知道的
事实（否则用户无法解释某个实体为什么被阻断）。

**推荐**：`factors.parquet` **加一列 `basis_status`**（或按 §3.2 (b) 的
`etf_factor_checks` 另出一个文件）。这是列集合变化 → **要升 schema 版号**，
与 §7.2 的结论不冲突（那是 `daily` 文件，这是 `factors` 文件，两者独立版本化）。

---

## 8. 验收计划

### 8.1 D-89 第 4 条的 10 只样本连续性检查

样本（设计 §8.3 第 4 条，`docs/tasks/issue-509-m3-2-etf.md:502-503`）：
`159901`、`159338`、`510050`、`588000`、`159919`、`159922`、`159830`、`510880`、
`159001`、`513330`。

判据：事件日前后 1 日的**复权收益**不出现 ±2× / ±5× / ±10× 的假跳空。

生产只读脚本的思路（不写文件、不改库；建议落成
`scripts/check_etf_qfq_continuity.py`，与 `check_daily_run.py` 同风格）：

```text
1. 只读打开 state/preview.sqlite（deploy 目录），PRAGMA query_only=1。
2. 对每个样本实体：
   a. 取 adjustment=0（Forward, class=2）序列的 series_bars：trade_day, close_units。
   b. 取 adjustment_events 的 effective_day 集合 E。
   c. 对每个 e ∈ E，找 e 之前最后一个 bar 与 e 之后第一个 bar（"事件日前后 1 日"），
      计算 r = close_after / close_before（两值都已复权，故 r 应接近 1，除除权外没有跳变）。
   d. 断言 r ∉ [2,∞) ∪ (0, 0.5] 之类的区间；更严格地断言 |ln r| < ln(1.35)
      （ETF 单日涨跌停通常 ±10%，跨事件日不该出现量级跳变）。
3. 同一脚本对 adjustment=1（raw）跑一次作对照：raw 上出现的大跳变，
   在 qfq 上必须消失。这一条才是"复权真的生效"的证据。
4. 输出：每只样本一行（事件数、最大 |ln r|、判定），退出码 0/1。
```

要点：
- 用 `close_units`（整数）做除法，不要用导出的浮点，避免 6dp 舍入引入假跳空。
- 「事件日前后 1 日」取的是**交易日**，不是自然日；用序列自身的前后相邻 bar 即可。
- 对 §3 里被阻断的实体，脚本应当断言「`qfq` 读不到 + 原因码正确」，而不是断言连续性。

### 8.2 部署前后各要采的读数

**部署前（在 vm2-126 上只读，作为基线）**：

| # | 读数 | 来源 |
|---|---|---|
| 1 | `market_series` 总数与 ETF 分项（`instrument_class=2`，按 `adjustment` 分组） | SQL |
| 2 | ETF 不复权 `series_bars` 行数、`record_count` 合计 | SQL（§4.2 的取证 SQL） |
| 3 | `adjustment_events` 里 ETF 实体的行数，按 `category`、`basis_status` 分组 | SQL |
| 4 | 库体积（`preview.sqlite` 字节数）与 `/opt/x-data-mcp-preview` 全树 | 文件系统 |
| 5 | 一次同代无变化全量 `update_data` 的 write-loop（若可安排） | `tasks` 表 |
| 6 | `sh.510050` / `sz.159919` / `sz.159922` 的 `qfq` 读面错误码（现状） | MCP 只读调用 |
| 7 | 导出快照的 `daily_qfq` 行数与 sha | 现有导出 |

**部署后（同样只读）**：

| # | 读数 | 期望 |
|---|---|---|
| 1' | 同上 1–4 | 序列数 +（通过核对的 ETF 数），库体积增量可归因 |
| 2' | §8.1 的连续性脚本 | 10/10 通过；raw 上的假跳空在 qfq 上消失 |
| 3' | 通过核对的 ETF 的 `qfq` 读面 | 返回 `price_scale=6` 的行；阻断实体返回 `details.reason` |
| 4' | `get_data_capabilities` | `capability_revision = generic-data-v12`，`series` 含 ETF qfq 行 |
| 5' | 「A 股字节不变」：A 股 `market_series` 的 `version_id`、导出的 `daily_qfq` A 股行 sha | 与部署前**逐条相同**（沿用设计 AC-M3-2-2 的检查方式） |
| 6' | 一次跨代与一次同代日更的 write-loop | 不劣于部署前基线（SLO 第 1/2 行） |

**A 股字节不变是最容易被忽略的一条**：§2.2 建议把 `forward_adjust_exact` 改成带
「事件→变换」参数的薄壳。A 股的变换函数不动，但**重构本身**必须证明：
同样输入下 A 股前复权的每个 6dp 值逐位相同。测试方式：
用现有前复权夹具跑一遍 `forward_adjust_exact_reference` 与重构后的实现，
逐 bar 全字比较（现有 `tests/algorithms/qfq_exact.rs` 已有同类结构）。

---

## 9. 待取证占位（VM2-19）

下面每一项都**必须**等阶段 1 的取证结果，本文不预填数字。
阶段 1 的产物应当是一份可复算的表（事件级），落在仓库外的证据目录，
并在本文件回填时给出「表的位置 + 复算命令」。

| # | 待填项 | 影响的小节 | 取证口径 |
|---|---|---|---|
> **2026-11-08 更正：下表 P1–P12 已按 `events-v2.csv`（VM2-22 离线重算，1,233 条）重新计数，旧 `events.csv` 的数字作废。**
> 每个数的计数方法写在行内；凡与 #611（旧表）不同的一律写成「旧 → 新」。表为空的一格是「待全量公告取证」。
> 通用口径：`rows = list(csv.DictReader(open("events-v2.csv")))`；事件数 = `sum(1 for r in rows if <条件>)`；
> 实体数 = `len({r["entity"] for r in rows if <条件>})`。列名是 `entity/market/cat/day/X_day/has_bar_R/c1/c3/expected/
> official_qss/official_verdict/attribution_v2/basis_v2/internal_ratio`。表与报告都在**仓库外**。

| # | 待填项 | 影响的小节 | 取证口径（2026-11-08 更正后） |
|---|---|---|---|
| P1 | cat1 事件总数、可核对条数、不一致条数 | §2.3、§3.1 B2、§3.3 | **已取得**：cat1 共 **979 条 / 150 只**（沪 689 / 105 只，深 290 / 45 只；`r["cat"]=="1"`）。深市官方可核对（`official_verdict` 为 `official_match` 或 `official_mismatch`）**120 条**（旧 101 → 新 120），其中一致 **119**（旧 15 → 新 119）、不一致 **1**（旧 86 → 新 1）。深市 `official_unavailable` **169 条**（旧 0 → 新 169，主要是 X 当天没有官方行或 raw 缺该日）、`no_bar` **1 条**。沪市 689 条 `official_unavailable` 687 条 + `no_bar` 2 条，**待全量公告取证**。不一致的唯一条目是 `sz.159922`（同 X 双因子，见 §2.1.1 与 P5） |
| P2 | cat11 事件总数、可核对条数、不一致条数 | §3.1 B1 | **已取得**：cat11 共 **254 条 / 220 只**（沪 142 / 123 只，深 112 / 97 只）。深市官方可核对 **34 条**（旧 19 → 新 34）：一致 **33**（旧 0 → 新 33）、不一致 **1**（旧 19 → 新 1）；`official_unavailable` **47 条**（旧 0 → 新 47）、`no_bar` **31 条**（旧 33 → 新 31）。沪市 `official_unavailable` 98 条 + `no_bar` 44 条，**待全量公告取证**。「官方 `qss` 不随份额折算调整」的旧判断**撤回**（它只在错误的日期基准下成立） |
| P3 | `sz.159830` 2024-12-19 `c3=0.9358405` 的归因 | §3.1、§3.3 | **已取得，结论撤回**：R = 2024-12-19 是**权益登记日**（当日收 5.722，未调整），X = **2024-12-20**，`expected = 5.722 / 0.9358405 = 6.1143`，12-20 实收 **6.120**（差 0.0057，属取整）。旧「日期错位 → fail-closed」**撤回**（旧结论来自数据库文件日期解码偏一天）；`official_verdict = official_unavailable`，原因是 `official_missing_ex_day`（raw 缺 2024-12-20），**待全量公告取证** |
| P4 | `sz.159922` 2024-12-02 的同 X 双因子归因 | §2.4 T6/T7 | **已取得**：2024-11-30（周六）cat11 `c3=2.5` 与 2024-12-01（周日）cat1 `c1=1.292` 共享 X = 2024-12-02。`prev_close = 6.159`，先 cat11 后 cat1 得 **2.3344**，等于官方 `qss` **2.334**（反次序 2.4119 不等）。`.day` 的 X 开盘 2.461 与官方 `ks` 2.461 完全相等。旧「2.5:1 拆分记在 cat1、应判阻断」**撤回**，改为「同一 X 合并 + 先 cat11 后 cat1」的锚定用例 |
| P5 | 同一 X 上的多事件组数量 | §2.3、§3.1 B4 | **已取得**：同一 `(entity, day)` 同时有 cat1 与 cat11 仍为 **0 条**（`Counter((r["entity"], r["day"]))` 无重复 key，与旧一致）。按**生效日 X** 归并后 `Counter((r["entity"], r["X_day"]))` 有 **2 组**重复 key：`sz.159922` 2024-12-02（cat11 + cat1，官方 `qss` 2.334 可锚定）、`sh.511880` 2013-04-18（两条 cat11 `c3=0.01`，`official_verdict=no_bar`，不影响序列）。旧「0 条」的分母是同一天，没有看 X，所以旧结论对 B4 不充分 |
| P6 | `c3=0.01` 折算是否发生在上市后 | §3.1 的「不阻断」段 | **已取得**：`c3=0.01` 的 cat11 事件 **21 条 / 20 只**（`r["cat"]=="11" and r["c3"]=="0.01"`），`has_bar_R` **全部为 False**，即都在该实体首根 bar 之前，上市后 **0 条**。`c3=0.1` 的 **2 条**在首根之后（`sh.510020` 2012-12-13、`sh.511030` 2025-03-18），沪市无官方 `qss`，**待全量公告取证**。数字与旧表一致（旧报告正文写 22 条、csv 计数 21 条，仍以 csv 的 21 条为准） |
| P7 | 待核对的基数与按实体的阻断数 | §3.3、§4.2、§11 | **待全量公告取证**（这就是 **VM2-23** 的范围）。**待核对基数 = `official_verdict == "official_unavailable"` 的事件：1,001 条**（`sum(1 for r in rows if r["official_verdict"]=="official_unavailable")`）：沪市 cat1 **687** + cat11 **98**，深市 cat1 **169** + cat11 **47**。按 `entity` 去重涉及 **260 只** ETF（`len({r["entity"] for r in rows if <同一条件>})`；其中沪市 180 只、深市 80 只，两市有交集，**不相加**）。**D-91 只认两种核对**：X 当天官方 `qss`，或基金公告；`basis_v2` 里的 `verified_internal_only`（**901 条 / 206 只**）只作旁证，**不算核对**（那是已取代的 D-90 B 口径）。已由官方 `qss` 核对的 **152 条 / 58 只**（`official_verdict == "official_match"`）不在待核对范围内；`no_bar` 的 **78 条**不纳入核对（§3.1）。旧口径「`basis_v2=pending` → 83 只 / 102 条」**作废**：它漏掉了 `verified_internal_only` 那 901 条。**2026-11-08 已由 VM2-23 取得（§9.3）**：1,001 条中 **987 条**由公告证实、**14 条** `ann_mismatch`，另有 `sz.159922` **2 条**（`out_of_selection`）合计 **16 条**未通过；按 D-92 第 1 条的复权价容差（≤0.001 元）这 16 条全部判为一致，因此 **D-92 下阻断实体数为 0**（`entity-summary.json` 的 `fail_closed_list` 是 D-92 之前的口径：**11 只**，计数 `len(json["fail_closed_list"])`）。按实体汇总取 `entity-summary.csv`（337 行）：`can_qfq == "yes"` **326 只**（`sum(1 for r in rows if r["can_qfq"]=="yes")`）、`can_qfq == "no"` **11 只**、`n_events == no_effect`（全部事件不影响序列）**46 只**（`sum(1 for r in rows if r["n_events"]==r["no_effect"])`），实体总数 `len(rows)` = **337** |
事件 **102 条**（`sum(...)`），涉及实体 **83 只**（`len({r["entity"] …})`；两市实体集合**不相加**，沪市含该条件的实体与深市有交集）；
沪市按 `official_verdict != "official_match"` 计另有 `official_unavailable` 785 条 + `no_bar` 46 条，全部等公告。深市已由官方 `qss` 判定的不一致实体是 **1 只**（`sz.159922`，仍需公告复核）。旧 D-90 的 29 只（27 + 2）**作废** |
| P8 | ETF 不复权 `series_bars` 行数与 `record_count` 合计 | §4.2 | **已取得（vm2 预检，§9.5）**：**1,761 条序列 / 1,645,415 根 bar**，`record_count` 合计 1,645,415，与实际行数逐序列完全一致。bar 数 min/中位/p95/max = 1 / 732 / 2,769 / 5,275。体积增量按 41.8 B/bar（PR-C 写库基准）约 **69 MB**，按页约 96 MB |
| P9 | 沪市无法用官方 `qss` 核对的实体数 | §3.1 B6、§3.3 | **已取得**：沪市 cat1 **689 条 / 105 只**、cat11 **142 条 / 123 只**，两类实体并集 **203 只**、交集 25 只（与旧表一致，`len(set(cat1 实体) | set(cat11 实体))`）。按 `official_verdict`：沪市 `official_unavailable` **785 条**（cat1 687 + cat11 98）、`no_bar` **46 条**（cat1 2 + cat11 44）。这 831 条**全部走公告核对**（D-91），**待全量公告取证**。沪市内部的 `.day` 内部一致性（`verified_internal_only`）只作旁证，不算核对 |
| P10 | 年度事件率 | §5.4 | **已取得（vm2 预检，§9.5）**：ETF 事件 1,233 条 / 337 只；2021–2026 年为 64 / 47 / 56 / 141 / 351 / 387 条，**稳态约 2.7 只 ETF/周**有新事件（2025 年 2.82、2026 年至 11-08 为 2.72） |
| P11 | 事件日无 bar 的事件数 | §3.1 | **已取得**：`official_verdict=no_bar` **78 条 / 77 只**（旧 80 条 → 新 78 条；旧的口径含日期解码偏移）。按 `attribution_v2` 拆：`event_before_first_bar` **75 条 / 74 只**、`no_bar_on_R_future_or_untraded` **3 条**（`sh.515450`、`sh.530880`、`sz.159118`，均为 2026-11-09 的未来事件）。按 D-91 这两类**不纳入核对**。`has_bar_R == "False"` 共 **419 条**（含首根 bar 之前的历史折算），与「无 bar」不是同一口径 |
| P12 | **沪市**事件的只数与事件数（按 cat 分） | §3.1 B6、§10 Q9 | **已取得**：沪市 **cat1 689 条 / 105 只**、**cat11 142 条 / 123 只**，事件合计 831 条；两类并集 **203 只**、交集 **25 只**（与旧表相同，旧 171 → 203 的修正保持不变）。按 D-91 沪市不再因为「无官方 `qss`」而整批阻断，改为**通过公告核对**，结果**待全量公告取证**（沪市待核对 **785 条**，见 P7） |

**证据位置与计数方法**（本节所有「已取得/部分取得」的数字都可由下面这一段原样复算）：

- 逐事件表 `events.csv`（1,233 行 = cat1 979 + cat11 254，30 列），本机副本 `/root/xdm-briefs/607-evidence/events.csv`，
  原件在主控机 `/root/.herdr-projects/reports/x-data-mcp/607-evidence/`；报告本机副本 `/root/xdm-briefs/607-evidence/report.md`。
  两者都在**仓库外**，不进版本库。表里的 `verdict`（`official_match`/`official_mismatch`/`official_unavailable`/`not_requested`/`no_bar`）、
  `attribution`、`has_bar`、`c3`、`day`、`entity`、`market`（`sh`/`sz`）、`category`（`1`/`11`）是本节用到的列。
- 通用口径：`rows = list(csv.DictReader(open("events.csv")))`；
  **事件数** = `sum(1 for r in rows if <条件>)`；**实体数** = `len({r["entity"] for r in rows if <条件>})`；
  **多类实体并集** = `len(set(A) | set(B))`（A/B 是各自条件筛出的实体集合），交集同理用 `&`。
  「只数」一律指 `entity` 去重后的个数，**不是**各子集只数相加。
- 报告给的是聚合表（§1 盘点、§3.1 计数表、§3.2 归因档），本轮对其中可以由 csv 复算的部分逐项重算，结果一致；
  唯一差异是 P6 的 `c3=0.01`（报告 §4.4 写 22 条 / 21 只，csv 计 21 条 / 20 只，差 1 条，已在该行标待复核）。
  本轮修正的计数错误只有 P12/P9 的沪市并集：**171 → 203**（交集 25 只）。其余各行的数字本轮重算后未变化。

**回填规则**：本文件回填时，(1) 每个 P 项给出数字与出处（SQL + 执行时间 + 主机）；
(2) 正文里凡是依赖它的句子从「待取证（VM2-19）」改成具体结论；
(3) **不允许**在没取证的情况下给出估算值并标「推断」——§4.2 已给的「推断」
只允许存在于「基数 × 已知口径」的算术，不允许拍脑袋。

### 9.1 阶段 1 取证结论（2026-11-08）——**已作废**

> **本节已作废（2026-11-08 晚）。** 它的取证脚本按 `1969-12-31` 解码 `.day` 的 epoch day，
> 而生产库按 `1970-01-01`（`src/compact_market.rs:868`），所以表里全部 `.day` 日期早了一天；
> 脚本又取 gbbq 事件日当天的官方 `qss`，而事件日是权益登记日 R，正式生效在 R 之后的第一个交易日 X。
> 两项错误同时作用，本节「深市 cat1 86 条不一致」「cat11 的 `qss` 不随份额折算调整」
> 「`sz159830` 日期错位」「`sh512890` 内部一致性 6.2% 偏离」等结论**全部撤回**，
> 下表与下方的原文只作反例保留，不再作为任何判据。现行结论见 §9.2 与 [D-91](../decisions.md)。
> 本节的逐事件表 `events.csv` 同样保留不动。

执行者 VM2-19，vm2-126 生产只读，SZSE `1815_stock`（`TABKEY=tab2`）5,838 次请求。
**D-89 的两条交付门槛都不满足，实现暂停**；六类裁决见 [D-90](../decisions.md)（`docs/decisions.md` 的 D-90 行）。

- 盘点（报告 §1；条数与只数已用 `events.csv` 重算，方法见本节末尾）：ETF 实体 **1,847**（沪 1,029 / 深 818，含已退市，
  报告 §1 的数）；cat1 **979 条 / 150 只**（沪 689 / 105 只，深 290 / 45 只）；cat11 **254 条 / 220 只**
  （沪 142 / 123 只，深 112 / 97 只）；两类实体并集 **337 只**；同一只 ETF 同一天两类事件 **0 条**（`(entity, day)` 去重后无重复）。
- cat1：深市官方可核对 **101 条**，15 条一致（`official_match`）、**86 条不一致**（`official_mismatch`），不一致涉及 **27 只**（`len({r["entity"] ...})`）。归因指向**公式口径**
  （9 条百分比、2 条每份、2 条当天无除权、9 条 `c1` 量级不符、64 条 ≤0.05 元小差口径未知），
  没有一条是日期错或拆分。**D-90 A**：`c1` 语义定案前这些实体按实体 fail-closed。
- cat11：SZSE 的 `qss`（前收）**不随份额折算调整**（可核对 19/19 在不复权价位），官方前收核不了 `1/c3`。
  **D-90 B**：「可核对」改为 `.day` 内部一致性（175/177）加官方开盘/收盘交叉核对，2 条例外 fail-closed。
- 沪市：**831 条**事件（cat1 689 + cat11 142，涉及 **203 只**，两类交集 25 只）无官方历史日线，全部 `NOT_AVAILABLE`。
  官方基金表不是全量快照，深市 402 个（事件日, ETF）对只取到 **120 个**。**D-90 E**：接受官方可核对子集口径，
  交付说明写明覆盖率（深市 cat1 101/290）。
- `sz159830` 2024-12-19 的 `c3=0.9358405` 是**日期错位**（当天价格没动，折算在下一交易日）；
  `sh512890` 2021-10-24 的 `.day` 内部一致性偏离 6.2%。**D-90 C/D**：两只都按实体 fail-closed。
- 货币 ETF 的 `c3=0.01` 折算 **21 条全部在首根 bar 之前**（P6 的 csv 计数），首根之后 0 条。
- **下一步**：基金收益分配与份额折算**公告试探取证**（约 20 个事件），用它定 `c1` 的权威语义并争取覆盖沪市；
  结论出来之前 §11 的实现批次不开工。
- **本节数字的计数方法**：除「ETF 实体 1,847」与报告给的比例（175/177、402→120、5,838 次请求）外，
  其余条数与只数都由 `events.csv` 重算：事件数 `sum(1 for r in rows if r["market"]==m and r["category"]==c)`，
  只数 `len({r["entity"] for r in rows if <同上条件>})`，只数不能相加（沪市 cat1 105 + cat11 123 ≠ 并集）；
  并集 `len(set(cat1 实体) | set(cat11 实体))`。逐项方法与差异见 §9 的「证据位置与计数方法」。

---

### 9.2 更正后的取证结论（VM2-21 / VM2-22）

裁决见 [D-91](../decisions.md)（取代 D-90）。数字全部取自仓库外的两份报告与 `events-v2.csv`（1,234 行 = 表头 + **1,233 条**）。

- **语义**：gbbq 事件日 = 权益登记日 **R**；生效日 **X = R 之后的第一个交易日**。前复权对**日期早于 X** 的 bar
  （含 R 当天）套用该事件；同一个 X 上的多个事件按**先 cat11 后 cat1** 合并。`c1` = 元/10 份，`c3` = 折算比（`新价 = 旧价 / c3`）。
- **公告（VM2-21，39 次请求）**：`c1` 与公告 **18/18** 逐位一致；`c3` 与公告的折算/拆分比例一致；深市 cat1 事件日 = 权益登记日 **14/14**。
- **重算（VM2-22，0 次网络请求）**：深市 cat1 可核对 **120 条 / 119 条一致**、cat11 可核对 **34 条 / 33 条一致**。
  唯一的两条不一致是 `sz.159922` 在同一个 X（2024-12-02）上的两个事件：按先 cat11 后 cat1 得
  `6.159 / 2.5 − 0.1292 = 2.3344`，等于官方 `qss` 2.334（反次序得 2.4119）。深市官方可核对 **154 条全部一致**，最大偏差 0.0005 元。
- **撤回**：`sz.159830` 不是日期错位（R = 2024-12-19、X = 2024-12-20，`5.722 / 0.9358405 = 6.114` 对 12-20 收 6.120）；
  `sh.512890` 的 6.2% 是 X 当天开盘跳空。两条都不再按实体 fail-closed。
- **仍不能核对**：沪市 **831 条**无官方 `qss`；深市 **216 条**在 X 当天没有官方行或 raw 缺该日；**78 条**没有 bar
  （其中 **75 条**在首根 bar 之前，不影响序列）。这些交给公告核对（§3.1 的 B1/B2/B6）。
- **计数口径**（`events-v2.csv`）：事件数 = `sum(1 for r in rows if <条件>)`，实体数 = `len({r["entity"] for r in rows if <条件>})`。
- **核对口径**（D-91 第 3 条）：只认 **X 当天官方 `qss`** 和**基金公告**两种核对。`events-v2.csv` 的 `basis_v2=verified_internal_only`
  （901 条 / 206 只）是 `.day` 内部一致性的结果，**只作旁证，不算核对**（它是已被取代的 D-90 B 口径）。
  因此待核对基数是 `official_verdict == "official_unavailable"` 的 **1,001 条 / 260 只**（§9 P7），不是 `basis_v2=pending` 的 180 条。

### 9.3 全量公告取证结果（VM2-23）与 D-92

裁决见 [D-92](../decisions.md)。数字取自仓库外的 VM2-23 报告与 `events-v3.csv`、`entity-summary.csv`、`unverified.csv`
（2026-11-08 12:00Z–13:05Z，vm2-126 生产只读；网络请求 **2,017/2,500**，PDF 1,224 个 182.7 MB，被引用的 1,144 个重算 sha256 全部一致）。

- **覆盖**：待核对（`official_verdict == "official_unavailable"`）**1,001 条**（沪 cat1 687 / cat11 98，深 cat1 169 / cat11 47；
  `sum(1 for r in rows if r["official_verdict"]=="official_unavailable")`），其中 **987 条**由公告逐项证实
  （`sum(1 for r in rows if r["ann_verdict"]=="ann_match")`，与 `final_status=="verified_announcement"` 的 987 条同一集合），
  **14 条** `ann_mismatch`。报告 §1 的「985 条（98.4%）」是同一批里扣掉 2 条的保守记法，本文一律用**事件表计数**。
- **16 条未通过** = 14 条 `ann_mismatch`（沪 13 / 深 1，涉及 **10 只**：`sh.510180`、`510390`、`510500`、`510590`、`511020`、
  `511030`、`511880`、`512390`、`512930`、`sz.159943`）+ 2 条 `sz.159922`（`ann_reason` 以 `out_of_selection:official_mismatch` 开头，
  在 `unverified.csv` 里，`ann_verdict` 为空）。
  按 `final_status` 计全表：`verified_official` **152**、`verified_announcement` **987**、`no_effect` **78**、`unverified` **16**（合计 1,233）。
- **容差（D-92 第 1 条）**：改为按复权价衡量（事件前收的复权价相差 ≤ 0.001 元）。16 条全部判为一致：
  10 条 gbbq `c1` 只有 2 位小数、1 条全角小数点解析错、1 条存储精度（`sz.159943`）、1 条拆分比例写法（`sh.510180`，`c3=10`）、
  1 条 `sh.510500` 复权价差 0.0009 元、2 条 `sz.159922` 同 X 复合事件（D-91 合并后与官方一致）。
- **阻断实体数（D-92 下）**：**0**。按实体汇总取 `entity-summary.csv`：**337 只**（沪 203 / 深 134），
  其中 `can_qfq == "yes"` **326 只**（`sum(1 for r in rows if r["can_qfq"]=="yes")`）、`can_qfq == "no"` **11 只**；
  这 11 只正是上面 16 条未通过项所在的实体（`entity-summary.json` 的 `fail_closed_list`），D-92 把 16 条判一致后它们不再阻断。
  **46 只**满足 `n_events == no_effect`（全部事件都不影响序列，`sum(1 for r in rows if r["n_events"]==r["no_effect"])`）。
- **限制说明**：gbbq 的派现金额只保留 2 位小数（元/10 份），部分基金公告精确到 3–4 位，交付说明须写明。


### 9.4 事件日语义的再更正（D-93，2026-11-08）

裁决见 [D-93](../decisions.md)（取代 D-91 第 (1) 条，D-91 第 (2)–(5) 与 D-92 不变）。
gbbq 事件日 **E 就是除权除息日**，前复权把 E 直接交给现有精确链，只作用于**严格早于 E** 的 bar，**不做日期映射**。

- **实测（生产库 `adjustment_events.effective_day`，按 1970-01-01 解码）**：`sz.159901` 三次 cat11 在 2010-11-22、
  2014-09-01、2021-04-12（都是周一），官方前收分别 0.793 = 3.966/5、2.83 = 0.566/0.2、3.706 ≈ 7.411/2；
  `sz.159830` 在 2024-12-20，公告写明除权日 12-20；`sz.159901` 2025-11-17 的 cat1，公告除息日 11-17、登记日 11-14；
  `sz.159919` 在 2025-03-31，官方前收 4.045 = 4.111 − 0.0661。四条都说明 E 是除权除息日，不是登记日。
- **误判来源**：VM2-19 的 `events.csv` 把**事件日**也解码早了一天（同样的 `1969-12-31` 问题），VM2-22 只修了日线日期，
  于是「事件日」看起来比真实除权日早一天、被读成登记日。官方 154/154、公告 985/1,001 的核对结论**不受影响**，
  因为「旧事件日之后的第一根 bar」恰好就是 E。PR-F（#614）在生产数据上用 R→X 时，在 `sz.159901` 出现重复调整的假跳空。
- **受影响段落**：§2.1.1（改为 E 直接进链，原 R→X 段落以引用块保留作反例）、§2.3（去掉 R→X 映射的说法）、
  §2.4 的 T4/T5/T6/T7（改为 E 语义与 `sz.159919`/`sz.159922` 的断言）、§11 的 PR-A（去掉「R→X 映射」）。
  §3.1 的 B1/B2/B4 仍在「X 当天」用官方 `qss` 核对：D-93 后这里的「X 当天」读作**「E 当天；E 不是交易日时取 E 之后的第一根 bar」**，
  不能说成「X 与 E 是同一个日期」——E 可能落在非交易日，例如 `sz.159922` cat11 的 E 是 2024-12-01（周日），第一根 bar 在 12-02。核对口径本身不变。

### 9.5 P8/P10 与部署前因子预检（vm2 只读预检，报告 `607-p8p10`，仓库外，2026-11-08）

取证口径：生产库 `file:/opt/x-data-mcp-preview/state/preview.sqlite?mode=ro`（不加 `immutable=1`，
不复制、不写入、不联网），日期按 1970-01-01 起算天数解码（`src/compact_market.rs:868`），
命令前缀 `nice -n 19 ionice -c3`。全部 ETF、不抽样。报告与 JSON 在**仓库外**（`607-p8p10`）。

#### P8（回填 §4.2）：ETF 不复权日线规模

SQL：`market_series` 过滤 `instrument_class=2 AND adjustment=0 AND period=1`。

| 量 | 值 |
|---|---|
| 序列数（全部有 bar） | **1,761** |
| `series_bars` 总行数 | **1,645,415** |
| `record_count` 合计 | 1,645,415（与实际行数逐序列**完全一致**，无漂移） |
| bar 数 min / 中位 / p95 / max / 均值 | 1 / 732 / 2,769 / 5,275 / 934.4 |

min=1 是只上市一天、只有一根 bar 的 ETF；这类序列没有前一根可比对。

**库体积增量口径（重要）**：前复权是在不复权**之外新增**的整条序列，不是就地改写，
所以**增量 ≈ 这条序列本身的体积**，不能拿「复权比不复权每行多几字节」的差额去算。

| 口径 | 每 bar | ETF 前复权体积 | 备注 |
|---|---|---|---|
| 按页估算 | 约 58.2 B/bar | **约 96 MB** | `dbstat` 整表 1,860,857,856 B / 37,768,593 bar = 49.27 B/bar（叶页抽样 5,000 页，填充率 93.6%），再按 A 股 raw(`price_scale`=2) 与 qfq(6) 的载荷比 34.32 → 49.48 B 拆分 |
| PR-C 写库基准实测 | **41.8 B/bar** | **约 69 MB** | PR-C 基准；更贴近真实增量，采纳这个 |

生产库现在 **2,313,494,528 B（约 2.32 GB）**（`docs/STATUS.md` XDM-198 读数），
**SLO 第 4 行上限 2.5 GiB**（2,684,354,560 B）。按 41.8 B/bar 的 69 MB 计预计到约 **2.39 GB**，
**仍在门禁内，但余量只剩约 300 MB**；部署前后各采一次库字节数（§8.2）。

> 标定依据：同一批 A 股实体的 raw 与 qfq 两侧 bar 数完全相同（5,562 对全部相等），
> 差异只在 `price_scale`（2 vs 6，多 4 位小数），所以载荷比可作为前复权的成本上限。

#### P10（回填 §5.4）：年度事件率

ETF 事件共 **1,233 条 / 337 只**（`effective_day` 全部可解码），占全库 60,622 条事件的 2.0%。

| 年份 | 事件数 | 涉及 ETF | cat1 | cat11 | 每周平均有新事件的 ETF |
|---|---|---|---|---|---|
| 2021 | 64 | 52 | 33 | 31 | 1.00 |
| 2022 | 47 | 35 | 29 | 18 | 0.67 |
| 2023 | 56 | 38 | 47 | 9 | 0.73 |
| 2024 | 141 | 72 | 128 | 13 | 1.38 |
| 2025 | 351 | 147 | 300 | 51 | 2.82 |
| 2026（至 11-08） | 387 | 142 | 322 | 65 | 2.72 |

「每周平均」= 当年涉及 ETF 只数 / 52.14 周。**结论：稳态约 2.7 只/周**（2025–2026），
即每周约 2–3 只 ETF 需要整条前复权重写；增长主要由 cat1 现金分红驱动。
§5.4 里「每年数十到数百个事件」的推断，实测落在 47–387 条区间，量级成立；
但 2025–2026 已是历史最高，**不能按「低频」继续外推**。

#### 部署前因子预检（全量）

**（1）因子合法性**（1,233 条 ETF 事件，0 条违规）：
事件类别只有 1 和 11；cat11 的 `c3 > 0`；cat1 的 `c1 ≥ 0`；`effective_day` 全部可解码；
`c1`/`c3` 全部可解析为 Decimal。**四项均为 0 条违例。**
（`basis_status` 列的 254 条 `pending` 是 schema 27 遗留标记，按 **D-92 Q2(c)** 不作门槛，不阻断。）

**（2）连续性检查**（`scripts/check_etf_qfq_continuity.py` 的参照实现，1,761 只全量）：

| 指标 | 结果 |
|---|---|
| 计算出错的实体 | **0** |
| 假跳空（D-89 第 4 条 ±2×/±5×/±10×） | **0** |
| 复权后价格 ≤ 0 的 bar | **601**（`sh.510050` 108 / `sh.510880` 41，按收盘价计） |

**（3）负价与根因**：`sh.510050` 最低 **−0.097 元**（2005-03-24…2005-12-20），
`sh.510880` 最低 **−0.286 元**（2008-09-18…2009-01-14）。根因是**累计 cat1 现金分红超过早期低价**：

| 实体 | 序列最低 raw low | 该 bar 生效的累计 cat1 | 前复权 low |
|---|---|---|---|
| `sh.510050` | 0.700（2005-06-03） | 0.797 | **−0.097** |
| `sh.510880` | 1.290（2008-11-04） | 1.576 | **−0.286** |

逐条算式 `low/c3 − Σc1/10` 与观测最小值完全一致（两只 cat11 折算日都在上述日期之后，divisor 取 1）。
**运算本身正确，是减法语义在低价起点上不收敛**，不是数据不合法。

**（4）A 股对照**：已落库的 A 股前复权（`instrument_class=0 AND adjustment=1 AND period=1`）中
**747/5,562 只、777,958 根** bar 的 low 或 close ≤0（close ≤0 为 759,861 根），
最低 `sh.600519` **−314.917 元**（2003-09-23），首次出现早至 1992-08-14。**程度远重于 ETF。**

**（5）等比前复权试算**（`cat1` 取 `(C_prev − c1/10)/C_prev`，`cat11` 仍 `1/c3`，按 E 连乘）：
`sh.510050` −0.097 → **+0.483502**、`sh.510880` −0.286 → **+0.711284**，两只负价归零。
291 只有可见事件的 ETF 首根 bar 两法相对差：**中位 0.0087%、p95 17.8%、最大 7.67×**，
影响集中在高分红老 ETF 的长尾。

> 关于锚定：**「末根 bar 两种方法相同」证明不了锚定**——两种方法在末根 bar 上都不做调整，
> 相同是必然的，不能据此说口径选择不影响历史值。真正的依据是：**单个事件时，除权日前一根 bar
> 在两种方法下都等于 `C_prev − c1/10`**（等比因子的定义就是让 `C_prev × factor = C_prev − c1/10`），
> 所以 **D-92 的核对证据在两种口径下都适用**，改口径不推翻已完成的 154/154 与 985/1,001 核对结论。

#### D-94 的结论

因此 **ETF 前复权沿用 D-89 第 1 条的减法口径**，与 A 股 `gbbq-cat1-2dp-v1` 同一条链；
**≤0 的价格照实交付**，不阻断、不截断、不改值。减法前复权保留的是**价差、不是比例**，
早期价格可能 ≤0，**不能直接拿来算收益率**——这条限制要写进 v12 契约与交付说明（A 股与 ETF 都写）。
等比前复权另行评估，与 A 股一起跟踪 [#621](https://github.com/liqiangcc/x-data-mcp/issues/621)。

---

## 10. owner 选项表

格式同 [issue-433-tradability-read.md](issue-433-tradability-read.md) §8.2。
「推荐」是本设计的建议，owner 未批不写进 `docs/decisions.md`。
「裁定」一列标出每一项由哪一条决策定案：[D-89](../decisions.md)（交付条件）、[D-91](../decisions.md)（语义、R→X、同 X 次序、核对方式）
或 [D-92](../decisions.md)（核对容差与 Q1/Q2/Q4–Q10）。2026-11-08 的全量公告取证（VM2-23，§9.3）已完成，
D-91 第 5 条的「等取证完成再实现」条件已满足，§11 按 D-92 恢复实现。

| # | 问题 | 选项 | 推荐 | 理由 | 阶段 1 后状态（2026-11-08） |
|---|---|---|---|---|---|
| **Q1** | 按实体 fail-closed 的粒度 | (a) 任一阻断事件 → 整只 ETF `qfq` 不可用；(b) 只提供最后一个阻断事件之后的 bar（序列截断） | **(a)** | D-89 第 6 条的措辞就是整只语义；(b) 的截断点会随因子修订漂移，产生**静默**的长度变化，是回测最难发现的偏差；实现复杂度也低。详见 §3.3 | **已裁定 = (a)**：D-89 第 6 条给出整只语义，**D-92 第 2 条**复述为 (a)。D-92 下阻断实体数为 **0**（§9.3），所以 (a) 目前不排除任何 ETF，它作为「未通过核对」时的行为保留 |
| **Q2** | 核对结论存哪（三选项，**每一项都涉及部署代价**，见 §3.2.1） | (a) 扩 `adjustment_events.basis_status` 枚举；(b) 行情库新建 `etf_factor_checks` 表；(c) 不改 schema，核对结论按内容寻址进来源库，读面按 `evidence_digest` 查询阻断集合 | **(c)** | 前两项都是 schema 迁移（27→28），在本仓库「fresh init only」模型下等于**每次部署都要付一次约 2–3 小时的重建**（`docs/new-host-runbook.md:282-284`；`scripts/deploy_phase_g.sh:19-22`、`src/sqlite_tasks.rs:49-54`），而 `switch_binary.sh` 的纯换二进制路径在 schema 变化后不再可用（`src/research/read.rs:73`、`src/sqlite_market/generation_facts.rs:225` 两处硬版本检查）。(c) 的代价是复现不完全（`inputs` 不含核对摘要，需在 `details`/manifest 里补 `etf_checks_digest`），但远小于重建代价。§3.2 | **已裁定 = (c)**：**D-92 第 2 条**。不改 schema（27 不变），核对结论按内容寻址进来源库，读面按 `evidence_digest` 查询阻断集合；§3.2.2 的 `etf_checks_digest` 随 PR-B 一起做 |
| **Q3** | 同日 cat1+cat11 的次序 | (a) 默认「先 cat11 后 cat1」，锚定不符或不可锚定即阻断；(b) 一律按默认次序算，不阻断 | **(a)** | 两种次序的数值差 `c1/10·(1/c3−1)` 是真实差异，不是舍入噪声；D-89 要求逐条核对。真实收益取决于 P5 的组合数量（**待取证**） | **已由 D-91 裁定**（取代 D-90 F）：同一个 **X** 上的事件按**先 cat11 后 cat1** 合并，前复权对日期早于 X 的 bar（含 R 当天）套用。锚定样本 `sz.159922`（6.159 → **2.3344**，官方 `qss` 2.334）；按 X 归并后共享同一 X 的事件组共 **2 组**（另一组 `sh.511880` 两条都在首根 bar 之前）。详见 §2.1.1 / §9 P4、P5 |
| **Q4** | 前复权的构建时机 | (a) 挂在既有 ETF `update_data` item 的事务里（不新增 item 类型）；(b) 新增独立 `PlanSeries::EtfQfq` item | **(a)** | (b) 会把 SLO 第 1 行的 item 基数再推高，而派生不需要独立来源绑定。§5.3 | **已裁定 = (a)**：**D-92 第 2 条**。不新增 item 类型，前复权构建挂在现有 ETF `update_data` item 的事务里 |
| **Q5** | 追加 vs 整条重写 | (a) bar 追加且因子未变 → 只追新 bar 的前复权行；因子变 → 整条重写；(b) 每次日更都整条重写 | **(a)** | 复用 `forward_adjust_exact` 的「proven-identical prefix 不复权」性质（`src/official_market.rs:1581-1586`），(b) 会让每只 ETF 每天都写一遍全历史。§5.2 | **已裁定 = (a)**：**D-92 第 2 条**。因子没变只追加新 bar 的前复权行，因子变了整条重写 |
| **Q6** | capability revision | (a) 升 `generic-data-v12`；(b) 不升只改 release notes；(c) 升版并把 ETF 缺省口径改成 qfq | **(a)** | 客户端分支行为变了，属 D-70 Q10 的同类；缺省仍要求显式口径（§6.2）。§6.1 | **已裁定 = (a)**：**D-92 第 2 条**。capability revision 升到 `generic-data-v12`，即使只是把拒绝换成新的可读形态 |
| **Q7** | fail-closed 的响应细节 | (a) 错误码不变（`adjustment_unavailable`）+ `details.reason`；(b) 新错误码 `etf_factor_unverified` 等 | **(a)** | 新码会让现有客户端分支全失效；`details` 是既有信封的扩展位（`src/data_supply/mcp.rs:819-825` 已在 `Error::Task` 用）。§3.2/§6.3 | **已裁定 = (a)**：**D-92 第 2 条**。错误码沿用 `adjustment_unavailable`，只加 `details.reason`，现有客户端分支不变 |
| **Q8** | 研究导出 | (a) `daily_qfq` 收录 ETF 行 + `factors.parquet` 加 `basis_status` 列（`factors` 升 schema 版）；(b) `daily_qfq` 不收录 ETF；(c) 另出 `daily_etf_qfq.parquet` | **(a)** | 导出口径必须与读面一致；(b) 会造出「MCP 读得到、导出拿不到」；(c) 多一个文件要维护 manifest 与校验，收益不抵成本。§7 | **已裁定 = (a)**：**D-92 第 2 条**。`daily_qfq` 收录 ETF 行，`factors` 加 `basis_status` 列（`factors` 升 schema 版） |
| **Q9** | 沪市无法用官方 `qss` 核对的实体 | (a) 一律阻断（fail-closed）；(b) owner 显式接受风险、标 `OWNER_RISK_ACCEPTED` 放行；(c) 先只交付深市 ETF 的 qfq | **(b) 的替代：通过公告核对**（D-91 第 3 条） | 沪市没有历史日线接口，但有基金公告（SSE `commonQuery.do`，`sqlId=COMMON_PL_JJXX_JJGG_L`，VM2-21 已实测可取原文），能核 `c1`/`c3` 的数值与登记日/除息日，所以不必在 (a)/(b)/(c) 之间选：核对过的事件正常进链，**官方 `qss` 和公告都核对不了**的事件才按实体 fail-closed。没做公告核对之前不得放行 | **已由 D-91 改为「通过公告核对」，D-92 后不再一律阻断**：沪市 831 条（cat1 689 条 / 105 只、cat11 142 条 / 123 只，并集 203 只、交集 25 只，§9 P9/P12）全部走公告核对；VM2-23 已核完（§9.3），沪市 13 条 `ann_mismatch` 在 D-92 第 1 条的容差下判为一致，沪市不再一律阻断 |
| **Q10** | 快照之后出现的新事件怎么核对 | (a) 每周 `full` 时批量核对新出现的 cat1/cat11 事件（深市在「X 当天」用官方 `qss`——D-93 后读作「E 当天；E 不是交易日时取 E 之后的第一根 bar」——或用公告），核对前相关 ETF 按实体 fail-closed；(b) 不核，新事件直接进前复权链 | **(a)** | 全量公告取证（§9.3）核对的是**当时快照**：1,001 条待核对事件加上已由官方 `qss` 核实的 152 条。快照之后新登记的因子事件没有任何核对记录，如果不先把它们纳入核对就放行，等于让未核对事件静默进入前复权链，违反 D-89 第 6 条 | **已裁定 = (a)**：**D-92 第 3 条**。核对之前相关 ETF 按实体 fail-closed，不出前复权；批量核对已在 PR-B 的 `convert`（#616）与每周 `verify`（#617）里落地，每周对生产运行属运维步骤（§11）；读面的阻断由 PR-D 执行 |

### 10.0 Q2 的三选项代价对照

| 选项 | 是否改 schema | 部署方式与代价 | 主要缺点 |
|---|---|---|---|
| **(a) 扩 `basis_status` 枚举** | **是**（27 → 28；`COMPACT_SCHEMA_VERSION` +1，`src/sqlite_tasks.rs:54`，尾部追加一条 `028_*.sql` 到 `COMPACT_MIGRATIONS`，`src/sqlite_tasks.rs:59-69`） | 发布后必须走 fresh init 重建：库被归档、`init21` 建新库、`source_store replay --scope all`、`full` 刷新（约 73 分钟）、目录/A 股/ETF 首采、年线 plan-run。整体约 **2 小时 15 分 – 2 小时 40 分**（`docs/new-host-runbook.md:282-284`）。服务停摆窗口本身只有 20–47 s（`docs/STATUS.md:149`、`docs/STATUS.md:62-63`），但数据重建期间读面是空库。**不能只换二进制**。注意：核对结论的**更新**本身不需要再重建——`basis_status` 是库内列，重算并重发布该实体序列即可 | 核对结论写进因子事实表，后来换核对来源（比如沪市以后有了官方历史日线）要改事实表结构；扩枚举还会牵动 `FactorBasisStatus` 的解析与写入校验（`src/market_data.rs:297-320`，`FactorBasisStatus` 的枚举、`as_str` 与 `parse`） |
| **(b) 新建证据表** | **是**（同上，27 → 28）。新建一张表和在旧表上加一列，对部署的代价**完全相同**——这是审核意见指出的关键一点：本仓库没有 N→N+1 的原地升级路径（`src/sqlite_tasks.rs:49-54`、`245-274`） | 同上（一次约 2–3 小时的重建） | 来源库和库内两份结论要同步；核对结论每次更新都要写库并重新发布 |
| **(c) 来源库证据 + 读面按摘要查询** | **否**（仍是 27；`switch_binary.sh` 的纯换二进制路径保持可用） | 部署代价 ≈ 0：ETF 核对结论只需随其它 scope 一起 `replay`（服务停着做，`docs/new-host-runbook.md:190-191` 的同一操作）+ 换二进制。核对结论的后续更新同样只走一次导入，不重建行情库 | **复现不完全**：`query_data` 的 `inputs`（版本号 + 算法版本）不足以复现判定，必须补 `etf_checks_digest`（§3.2.2）。另外读路径多一次来源库查询（可缓存，key 含摘要） |

（重建的其它实测参照：A 股日线首采 53 批约 3.3 分钟、`docs/new-host-runbook.md:196`；
年线 plan-run 89.6–127.9 s、`docs/SLO.md:19`；同代全量 write-loop 573.4 s、
`docs/SLO.md:17`；首采后 DB ≤2.5 GiB 是 hard 门禁、`docs/SLO.md:16`，
而当前库已 2.31 GB、`docs/STATUS.md:539`——(a)/(b) 每重建一次都要重新验证体积。）

### 10.1 与已批准决策的关系

- Q1、Q7 直接落实 D-89 第 6 条，不需要新增决策；Q1 的阻断范围按 D-91 第 3 条的判定口径收窄
  （只有「官方 `qss` 和公告都核对不了」才阻断）。
- Q3 已由 D-91 裁定（取代 D-90 F）；Q9 按 D-91 第 3 条改为通过公告核对。
- Q1、Q2、Q4–Q8、Q10 已由 **D-92** 裁定（Q1/Q7 落实 D-89 第 6 条的整只语义与错误码形态，Q2 选 (c) 不改 schema）；
  事件日语义由 **D-93** 裁定（E 直接进链，不做 R→X 映射）。
- Q6 的判例来自 D-70 Q10（已批准），本设计只是沿用。
- Q2 的 (a)/(b) **都是 schema 迁移**（27 → 28），差别只在表结构长什么样，
  部署代价一样。按 AGENTS.md 的约束，schema 变更不在本任务的授权范围内，
  所以本文推荐不改 schema 的 (c)。若 owner 认为复现完整性优先于部署代价，
  (a) 是可以自洽的选择——但那时要单独开一个 schema 任务，并在部署计划里
  预留一次完整重建。
- Q3、Q4、Q5、Q8 属于新裁定，已由 **D-91**（Q3）与 **D-92**（Q4/Q5/Q8）写入 `docs/decisions.md`。
- Q9 的取舍按 D-91 第 3 条改为**通过公告核对**（取代 D-90 E 的「一律 `NOT_AVAILABLE`」）；
  沪市 831 条已全部核完（§9.3），在 D-92 第 1 条的容差下无阻断项，沪市不再一律阻断。

### 10.2 本文没做的事

- 没有改任何代码，没有跑 `cargo test`，没有连生产或 tailnet 主机。
- 没有给出任何 ETF bar 数、库体积增量、阻断实体数——这些在 §9 等取证。
  （2026-11-08 部分回填：事件级数字按 `events-v2.csv` 重新计数，见 §9 与 §9.2；全量公告取证见 §9.3，D-92 下阻断实体数为 0；
  ETF bar 数与库体积增量仍未取得，见 P8。）
- 没有为 ETF 年线做任何设计（D-70 Q6 不变）。

---

## 11. 后续拆分（供 owner 排期参考）

> **实现进度（2026-11-08，D-92/D-93 之后）：PR-A [#618](https://github.com/liqiangcc/x-data-mcp/pull/618)、
> PR-B [#615](https://github.com/liqiangcc/x-data-mcp/pull/615)、PR-F [#614](https://github.com/liqiangcc/x-data-mcp/pull/614)
> 与 `convert` [#616](https://github.com/liqiangcc/x-data-mcp/pull/616)、每周 `verify` [#617](https://github.com/liqiangcc/x-data-mcp/pull/617)
> 已合并；PR-C 正在实现，尚未开 PR。** D-92 已确定阻断集合为空（§9.3），D-91 第 5 条的「取证完成后再实现」条件已满足。
> 事件日语义按 **D-93** 执行：E 直接进链，严格早于 E 的 bar 才套用，**不做 R→X 映射**（§2.1.1、§2.4、§9.4）。
> PR-C 只做**写入侧**；`resolve_read_shape` 放行、`read_public_series` 按口径解析，以及按 PR-B 门槛做**读面阻断**，
> 都放在 **PR-D**，与 `generic-data-v12` 一起上线——这样核对门槛与读面在同一次发布里生效。

| PR | 内容 | 前置 / 状态 |
|---|---|---|
| PR-A | 精确链的「事件→变换」参数化 + `etf_event_transform` + **事件日 E 直接进链**（§2.1.1、D-93）+ **同 E 先 cat11 后 cat1 合并**（§2.3） + 单测 T1–T10（T4/T6/T7 按 §2.4 的更正断言，`sz.159922` 夹具输入 6.159、期望 2.334400）；A 股逐位不变 | 已合并 [#618](https://github.com/liqiangcc/x-data-mcp/pull/618) |
| PR-B | **两件事**：① 来源库证据 scope（Q2 = (c)，D-92 第 2 条：核对结论存入来源库，不改 schema）+ 阻断判定纯函数 + 单测；② **每周批量核对新事件的流程**（Q10 = (a)，D-92 第 3 条）——子命令与判定已由 `convert` [#616](https://github.com/liqiangcc/x-data-mcp/pull/616) 与每周 `verify` [#617](https://github.com/liqiangcc/x-data-mcp/pull/617) 落地，**每周对生产运行属于运维步骤**；核对之前相关 ETF 按实体 fail-closed 由 PR-D 的读面阻断执行 | 已合并 [#615](https://github.com/liqiangcc/x-data-mcp/pull/615)（+ #616/#617） |
| PR-C | **只做写入侧**：`SeriesShape::etf(Forward)` 写入、追加/重写规则（Q5 = (a)）、前复权构建挂在现有 ETF `update_data` item 的事务里（Q4 = (a)）。**不碰读面** | 进行中（未开 PR） |
| PR-D | 读面与契约：`generic-data-v12`（Q6 = (a)）、`series_capabilities` 新行、`details.reason`（Q7 = (a)）、**`resolve_read_shape` 放行**、**`read_public_series` 按请求口径解析**、**按 PR-B 门槛做读面阻断**、契约文档、`check_current_facts.py` | PR-C |
| PR-E | 研究导出：`daily_qfq` 收录 ETF、`factors` 加 `basis_status` 列（Q8 = (a)）、`export_smoke.py` 断言 | PR-D |
| PR-F | 验收脚本 `scripts/check_etf_qfq_continuity.py` + 部署前/后读数记录 | 已合并 [#614](https://github.com/liqiangcc/x-data-mcp/pull/614)（部署前/后读数随 PR-D 上线补记） |
