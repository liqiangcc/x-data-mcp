# ETF 前复权的实现设计（[#607](https://github.com/liqiangcc/x-data-mcp/issues/607) 阶段 2）

- **状态**：设计初稿（docs-only）。阶段 1 的取证（VM2-19，vm2 生产只读）**尚未完成**，
  依赖取证结果的地方一律标「待取证（VM2-19）」并在 §9 汇总，本文不预填数字。
- **依据**：[D-89](../decisions.md)（owner 2026-11-08 批准交付 ETF 前复权）、
  [M3-2 设计](issue-509-m3-2-etf.md) §8（§8.1 现状、§8.2 为什么不首期交付、§8.3 交付条件与方法）、
  [SLO](../SLO.md) 第 1/2 行、[STATUS](../STATUS.md)。
- **代码基线**：分支起点 `origin/main` = `de1f84a`（preview.47 版本号，schema 27，
  `generic-data-v11`）。文中引用的行号都基于这个提交。
- **证据分级**：本文只做「读代码 + 读设计文档」的定位，没有实测、没有连生产。
  标「推断」的是从代码或设计里推出的结论，不是门禁依据；标「待取证（VM2-19）」的是
  必须等阶段 1 结果才能写的数字或结论。

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

同日组合到底有多少个、锚定结果如何：**已取证（2026-11-08）——同日组合 0 条**（§9 P5），
「先 cat11 后 cat1」因此没有官方样本可核；D-90 F 把它定为构造性约定，本节第 1 条的阻断条件在没有同日组合时不触发。

### 2.4 单元测试清单

纯函数测试，不依赖数据库，放在 `src/qfq.rs` 的 `mod tests`（现有测试同文件）。

| # | 测试 | 断言 |
|---|---|---|
| T1 | `raw/1000` 换算 | `ohlc_raw = 4_123` → `4.123`；`scale=6` 时 6dp |
| T2 | cat1 单事件 | `T(x)=x−c1/10`；空事件集合是恒等（与 `forward_adjust_exact` 一致的边界） |
| T3 | cat11 单事件 | `T(x)=x/c3`；`c3=0.93584` 时结果不是 `x/0.94` |
| T4 | 严格大于 | 事件日当天的 bar 不受该事件影响（`event_date > bar_date`） |
| T5 | **`sz159919` 锚定例** | 2025-03-31 cat1（设计 §2.1 的官方前收锚点）。断言：事件前一交易日收盘 − `c1/10` 与官方前收在 ±1 tick 内相等；同一 bar 的 6dp 值只由 (bar, events, scale, version) 决定 |
| T6 | **`sz159922` 拆分例** | 2024-12-02 `c1=1.292` 的 cat1 实际是 2.5:1 拆分（设计 §8.2 第 2 条、§10 第 4 条）。**期望行为**：数学上按 cat1 应得 `6.159 − 0.1292 = 6.0298`，与实际开盘 `2.461` 差 2.5 倍 → 该事件是「因子类别错误」，测试断言这组事件**被判为阻断**，而不是断言一个复权价。这条测试守的是 fail-closed，不是数值 |
| T7 | 同日 cat1+cat11 次序 | 用合成事件验证 `x/c3 − c1/10`，并用一个「已锚定次序」的用例验证读取锚定结论的路径 |
| T8 | 事件后缀记忆化 | `etf_forward_adjust_exact` 与逐 bar 直接计算的参考实现逐位相等（对应 `forward_adjust_exact_reference`，`src/qfq.rs:269-304`） |
| T9 | 非 1/11 类别 | 传 category=2 的 ETF 事件 → `Err`，不猜 |
| T10 | 空序列 / 单 bar | 与 A 股同构行为：空 bar → 空输出；单 bar → 只受其后的链影响 |

T5/T6 的**具体官方前收数字**不从本文抄：设计里给了 `6.159 → 2.461` 这一对，
其余锚点要以 VM2-19 的取证表为准（`events.csv` 的 `official_qss`/`official_open` 列，见 §9 的「证据位置与计数方法」）。
2026-11-08 回填：`159922` 的 2.5:1 拆分**未取得官方核对**（§9 P4 标「未取得」），T6 的夹具暂不能按官方锚定落地。

---

## 3. 按实体 fail-closed（D-89 第 6 条）

D-89 原文：「核对不了或不能归因的事件**不得静默进入前复权**：实现时按实体 fail-closed，
该 ETF 的 `qfq` 返回 `adjustment_unavailable` 并带上原因。」

### 3.1 哪些事件状态会阻断

| # | 阻断条件 | 判定依据（数据侧） | 现状可得吗 |
|---|---|---|---|
| B1 | cat11 事件未经价格锚定核对 | `basis_status != 通过`（现有枚举没有这个值，见 §3.2） | 需要取证结论落库后才可判 |
| B2 | cat1 事件与官方前收不一致且未归因 | 同上 | 同上 |
| B3 | 事件类别为 1/11 之外（ETF 不应出现） | `adjustment_events.category NOT IN (1,11)` | 是（DDL 已挡，防御性检查） |
| B4 | 同一 `(entity, effective_day)` 同时有 cat1 和 cat11，且次序未锚定 | 同日组合表 | 需取证（`sz159922` 这类） |
| B5 | 因子不可解析成精确分数（非有限、位数超预算、`c3 = 0`） | 运行时 `etf_event_transform` 返回 `Err` | 是（`src/gbbq.rs` 已有同类错误） |
| B6 | 事件日的 bar 缺失且官方前收不可得，导致该事件无法核对（典型是沪市：无历史日线接口） | 取证表里 `NOT_AVAILABLE` | 需取证；**沪市的波及面见 §9 P12** |

**不阻断**的：事件日早于该 ETF 第一根 bar（如货币 ETF 的 `c3=0.01` 折算，
设计 §8.1 说 60 条在首根 bar 之前）——这类事件对任何 bar 都不生效
（`event_date > bar_date` 严格大于），不进入任何链，自然不阻断。但「上市后
有没有发生过 0.01 折算」是 #607 的 AC 之一，**已取证（2026-11-08）**：`c3=0.01` 的 21 条
cat11 事件（csv 计数，§9 P6）全部在首根 bar 之前，上市后为 0 条；`c3=0.1` 的 2 条（`sh510020` 2012-12-13、
`sh511030` 2025-03-18）在首根之后，按 B4/B1 类的普通事件判，沪市官方不可得 → `NOT_AVAILABLE`。

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
3. (b) 的真实收益要看 B1/B2 最终影响多少只 ETF——**待取证（VM2-19）**。
   如果影响面很小（推断：cat1 不一致 42 条分布在少数实体上），(a) 的代价可忽略；
   如果影响面大到几十只，(b) 可以作为一个**独立 Issue** 再议，而不是塞进本期。

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
  `1,632 − 阻断实体数`。阻断实体数**已部分取证（2026-11-08）**：按 D-90 A/C/D 合计 **29 只**（§9 P7），
  但报告没有给出完整的阻断实体分布表，实际少多少条 `Forward` 序列要等实现后统计。
  `market_series` 从 18,118 涨到至多约 19,750（推断）。
- **bar 数**：前复权 bar 与不复权 bar 一一对应（`forward_adjust_exact` 保序保数，
  调用方 `src/official_market.rs:1593-1600` 显式检查了 `adjusted.len() == raw_bars.len()`）。
  所以增量 = ETF 不复权 bar 总数。**ETF 不复权 bar 总数未取得**（§9 P8：取证批次没跑这条 SQL）
  ——`docs/STATUS.md:341` 只给了全库总量，没有 ETF 分项。
- **库体积增量**：设计 §0 第 7 条给过一条非直接依据：ETF 加入把同代全量
  `update_data` 的 item 数从 5,238 推到约 6,869（设计 §11）。这只说明 item 数，
  不等于 bar 数。体积增量 = `ETF bar 数 × per-row 成本`，而 per-row 成本
  `series_bars` 已有可测口径（SLO 备注里的「净增长 ≈ 3.4KiB/item」是**周期增量**，
  不是稳态存储成本，不能直接相乘）。**体积增量待取证（VM2-19）**，
  取证时请一并给出：ETF 不复权 `series_bars` 行数、`market_series` 里
  `instrument_class=2 AND adjustment=1` 的汇总 `record_count`。

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
| P1 | cat1 事件总数、其中不一致的条数、逐条归因分布（分红/拆分/日期错/市场跳空） | §2.3、§3.1 B2、§3.3 | **已取得**（报告 §1、§3.1、§3.2）：cat1 共 **979 条**（沪 689 / 深 290，涉及 150 只）；官方前收只覆盖深市，可核对 **101 条**：15 条一致、**86 条不一致**，归因分布 9 `formula_percent` + 2 `formula_per_share` + 2 `no_exrights_day` + 64 `small_gap_unresolved` + 9 `c1_magnitude_mismatch`，**没有一条**是日期错或拆分。三个分母的口径：974/967 是设计按「现行名单成员」计的 cat1 条数，42 是设计 ±1.5% 内部判据下的不一致条数（沪深混合），本次 979/101/86 是按 `security_master.instrument_class=2` 全部实体实测（含 19 只已退市成员，多出 5 条）。沪市 689 条 `NOT_AVAILABLE`，未逐条归因 |
| P2 | cat11 事件总数、能核对/不能核对的条数、可核对里不一致的条数 | §3.1 B1 | **已取得**（报告 §1、§3.1、§3.3）：cat11 共 **254 条**（沪 142 / 深 112，涉及 220 只）；无 bar **80 条**（沪 98+44、深 79+33，见 §1.1 的 177 有 bar / 77 无 bar）；深市官方可核对 **19 条，0 条一致、19 条不一致**——19/19 的官方 `qss` 都落在不复权价位，说明官方前收不随份额折算调整，不能用它核 `1/c3`；`.day` 内部一致性 **175/177**（±6%），例外 2 条（`sz159830`、`sh512890`） |
| P3 | `sz159830` 2024-12-19 `c3=0.93584` 的归因结论（设计原文写作 2024-12-20） | §3.1、§3.3 | **已取得**（报告 §4.2）：官方当天开 5.723 / 收 5.722 / -0.81%，**当天价格没动**；`.day` 前收 5.722 / 开盘 5.723 与官方一致；下一根 bar（2024-12-22）开盘 6.170，与 `5.722/0.9358405 = 6.114` 同量级 → 折算是**下一个交易日**发生，`gbbq` 把日期记到了 2024-12-19，属**日期错位**（`date_shifted`）。按 D-90 C 该实体 fail-closed |
| P4 | `sz159922` 2024-12-02 的归因结论（确认是 2.5:1 拆分记在 cat1） | §2.4 T6 | **未取得**：报告 §1.1 只记录 `sz159922` 2024-11-30 的事件日落在周末（`.day` 无该日 bar），取证批次没有对 2024-12-02 的 `c1=1.292` 做单价核对，也没有 2.5:1 的官方锚定；报告 §3.2 明确 86 条不一致里没有一条满足 `split_recorded_as_cat1`，但这条不在可核对子集里（`not_requested`）。需在公告试探里补 |
| P5 | **同日 cat1+cat11 的事件组数量与逐组锚定次序** | §2.3、§3.1 B4 | **已取得**（报告 §1、§4.3）：按 `(entity, day)` 计数，同一天同时有 cat1 与 cat11 的事件 **0 条**；33 只 ETF 的两类事件都落在**不同**日期。因此「先 cat11 后 cat1」**没有可核对样本**，D-90 F 把它写成构造性约定 |
| P6 | 货币 ETF 的 `c3=0.01` 折算是否发生在上市后 | §3.1 的「不阻断」段 | **已取得**（报告 §4.4）：`c3=0.01` 的 cat11 事件共 **21 条 / 20 只**（沪 10 / 深 11），按 `events.csv` 的 `has_bar` 计数**全部 21 条 `has_bar=False`**（都在该实体首根 `.day` bar 之前）；首根之后为 **0 条**。另 `c3=0.1` 的 **2 条在首根之后**（`sh510020` 2012-12-13、`sh511030` 2025-03-18），沪市官方不可得，只能靠 `.day` 内部一致性。**注**：报告 §4.4 正文写作「22 条（21 只）」，与 `events.csv` 逐行计数（21 条 / 20 只）差 1，列在这里待复核 |
| P7 | 按实体汇总的**阻断实体数**与阻断原因分布 | §3.3（决定方案 (a) 的代价）、§4.2（序列增量） | **部分取得**（`events.csv` 按 `entity` 去重计数，方法见本节末）：按 D-90 A 阻断 **27 只**（深市 cat1 `official_mismatch` 全部实体：159209/159221/159307/159331/159332/159333/159336/159351/159355/159357/159390/159399/159545/159547/159576/159581/159589/159596/159691/159816/159901/159902/159905/159920/159925/159960/159972，其中 8 只另有 `official_match` 事件）；按 D-90 C/D 再加 **2 只**（159830、512890）；三集合无交集 → **合计 29 只**。沪市 689 + 142 条事件全 `NOT_AVAILABLE`，D-90 E 接受子集口径后**不因此阻断**。**未取得**：报告没有给阻断实体的分布表，`qfq` 实际少多少条 `Forward` 序列要等实现后统计 |
| P8 | ETF 不复权 `series_bars` 行数与 `record_count` 合计 | §4.2（bar 数与库体积增量） | **未取得**：阶段 1 取证只读了 `adjustment_events` 与 `.day` 上下文，**没有执行** §4.2 末尾的 `SELECT count(*), sum(record_count) FROM market_series WHERE instrument_class=2 AND adjustment=1`；报告 §6 第 7 条把「未核对 `price_scale`/`volume_quantum`」列为未验证项。§4.2 的 bar 数与体积增量仍待补测 |
| P9 | 沪市 ETF 中「完全无法核对」的实体数（无历史日线接口） | §3.1 B6、§3.3 | **已取得**（`events.csv` 按 `entity` 去重）：沪市 cat1 涉及 **105 只**（689 条）、cat11 涉及 **123 只**（142 条），两类合计 **171 只**；官方可核对 0 条（沪市无历史日线接口，报告 §2.2、§6 第 1 条）。按 D-90 E 这些标 `NOT_AVAILABLE`，不阻断 |
| P10 | 年度事件率（每年新增 cat1/cat11 事件数），用于 §5.4 的重写成本推断 | §5.4 | **未取得**：报告与 `events.csv` 都没有按年份的直方图（`events.csv` 只有 `day` 列，取证批次未给年度汇总）。需要按 `effective_day` 年份重算，方法：对 `events.csv` 的 `day[:4]` 分 market × category 计数 |
| P11 | 001/其他类别的 `partition_point` 边界：事件数与首个 bar 日期的关系 | §3.1 | **部分取得**（报告 §1.1、§6）：事件日无 `.day` bar 共 **80 条**（cat1 3 + cat11 77）；其中 15 条落在周末、**75 条在该实体首根 bar 之前**，真正「上市后有事件日却无 bar」的只有 **5 条**（3 条是 2026-11-09 未来日，2 条 `sz159922` 2024-11-30、`sz159932` 2015-06-28），这 5 条要单独列出。**未取得**：`partition_point` 的逐实体边界表（哪些实体的事件**全部**早于首根 bar）不在报告里 |
| P12 | **沪市**有 cat1/cat11 事件的 ETF **只数**与**事件数**（按 category 分） | §3.1 B6、§10 Q9 | **已取得**（`events.csv` 按 `market` + `category` 计数，`entity` 去重）：沪市 **cat1 689 条 / 105 只**、**cat11 142 条 / 123 只**，合计 831 条事件；两类的实体集合有交集，合并后 **171 只**。这些实体按 D-90 E 一律 `NOT_AVAILABLE`，不受 Q9 的 (a)/(b) 影响 |

**证据位置与计数方法**（本节的「已取得/部分取得」数字都可用下面的命令复算）：

- 逐事件表 `events.csv`（1,233 行 = cat1 979 + cat11 254，30 列），本机副本 `/root/xdm-briefs/607-evidence/events.csv`，
  原件在主控机 `/root/.herdr-projects/reports/x-data-mcp/607-evidence/`；报告本机副本 `/root/xdm-briefs/607-evidence/report.md`。
  两者都在**仓库外**，不进版本库。
- 表中各列：`verdict`（`official_match`/`official_mismatch`/`official_unavailable`/`not_requested`/`no_bar`）、
  `attribution`、`has_bar`、`c3`、`basis_suggested`、`day`；计数一律用 `csv.DictReader` 读原表后 `Counter`：
  事件数 = 行数，实体数 = `len({row["entity"]})`，按类别与市场 = `Counter((row["market"], row["category"]))`。
- 报告里的聚合数（如 §3.1 的计数表、§3.2 的归因档）与 `events.csv` 逐行计数**只在 P6 的 `c3=0.01` 上差 1 条**
  （报告 §4.4 写 22 条 / 21 只，csv 计 21 条 / 20 只），已在该行标注待复核；其余各项两处一致。

**回填规则**：本文件回填时，(1) 每个 P 项给出数字与出处（SQL + 执行时间 + 主机）；
(2) 正文里凡是依赖它的句子从「待取证（VM2-19）」改成具体结论；
(3) **不允许**在没取证的情况下给出估算值并标「推断」——§4.2 已给的「推断」
只允许存在于「基数 × 已知口径」的算术，不允许拍脑袋。

### 9.1 阶段 1 取证结论（2026-11-08）

执行者 VM2-19，vm2-126 生产只读，SZSE `1815_stock`（`TABKEY=tab2`）5,838 次请求。
**D-89 的两条交付门槛都不满足，实现暂停**；六类裁决见 [D-90](../decisions.md)（`docs/decisions.md` 的 D-90 行）。

- 盘点：ETF 实体 **1,847**（沪 1,029 / 深 818，含已退市）；cat1 **979 条 / 150 只**（沪 689 / 深 290）；
  cat11 **254 条 / 220 只**（沪 142 / 深 112）；同一只 ETF 同一天两类事件 **0 条**。
- cat1：深市官方可核对 **101 条**，15 条一致、**86 条不一致**（27 只）。归因指向**公式口径**
  （9 条百分比、2 条每份、2 条当天无除权、9 条 `c1` 量级不符、64 条 ≤0.05 元小差口径未知），
  没有一条是日期错或拆分。**D-90 A**：`c1` 语义定案前这些实体按实体 fail-closed。
- cat11：SZSE 的 `qss`（前收）**不随份额折算调整**（可核对 19/19 在不复权价位），官方前收核不了 `1/c3`。
  **D-90 B**：「可核对」改为 `.day` 内部一致性（175/177）加官方开盘/收盘交叉核对，2 条例外 fail-closed。
- 沪市：**831 条**事件无官方历史日线，全部 `NOT_AVAILABLE`。官方基金表不是全量快照，
  深市 402 个（事件日, ETF）对只取到 **120 个**。**D-90 E**：接受官方可核对子集口径，
  交付说明写明覆盖率（深市 cat1 101/290）。
- `sz159830` 2024-12-19 的 `c3=0.9358405` 是**日期错位**（当天价格没动，折算在下一交易日）；
  `sh512890` 2021-10-24 的 `.day` 内部一致性偏离 6.2%。**D-90 C/D**：两只都按实体 fail-closed。
- 货币 ETF 的 `c3=0.01` 折算 **21 条全部在首根 bar 之前**（P6 的 csv 计数），首根之后 0 条。
- **下一步**：基金收益分配与份额折算**公告试探取证**（约 20 个事件），用它定 `c1` 的权威语义并争取覆盖沪市；
  结论出来之前 §11 的实现批次不开工。

---

## 10. owner 选项表

格式同 [issue-433-tradability-read.md](issue-433-tradability-read.md) §8.2。
「推荐」是本设计的建议，owner 未批不写进 `docs/decisions.md`。
末尾一列是本文件 2026-11-08 的补充：哪些问题已由 [D-90](../decisions.md)（或 D-89 既有条目）裁定，
哪些仍等公告试探取证（§9.1 的下一步）的结果。

| # | 问题 | 选项 | 推荐 | 理由 | 阶段 1 后状态（2026-11-08） |
|---|---|---|---|---|---|
| **Q1** | 按实体 fail-closed 的粒度 | (a) 任一阻断事件 → 整只 ETF `qfq` 不可用；(b) 只提供最后一个阻断事件之后的 bar（序列截断） | **(a)** | D-89 第 6 条的措辞就是整只语义；(b) 的截断点会随因子修订漂移，产生**静默**的长度变化，是回测最难发现的偏差；实现复杂度也低。详见 §3.3 | **已裁定**：D-89 第 6 条即整只语义；D-90 A/C/D 的三类阻断都按 (a) 执行 |
| **Q2** | 核对结论存哪（三选项，**每一项都涉及部署代价**，见 §3.2.1） | (a) 扩 `adjustment_events.basis_status` 枚举；(b) 行情库新建 `etf_factor_checks` 表；(c) 不改 schema，核对结论按内容寻址进来源库，读面按 `evidence_digest` 查询阻断集合 | **(c)** | 前两项都是 schema 迁移（27→28），在本仓库「fresh init only」模型下等于**每次部署都要付一次约 2–3 小时的重建**（`docs/new-host-runbook.md:282-284`；`scripts/deploy_phase_g.sh:19-22`、`src/sqlite_tasks.rs:49-54`），而 `switch_binary.sh` 的纯换二进制路径在 schema 变化后不再可用（`src/research/read.rs:73`、`src/sqlite_market/generation_facts.rs:225` 两处硬版本检查）。(c) 的代价是复现不完全（`inputs` 不含核对摘要，需在 `details`/manifest 里补 `etf_checks_digest`），但远小于重建代价。§3.2 | **未裁定**：与取证结论无关，等实现前定；若 (c) 仍是推荐则 §3.2.2 的 `etf_checks_digest` 要一起做 |
| **Q3** | 同日 cat1+cat11 的次序 | (a) 默认「先 cat11 后 cat1」，锚定不符或不可锚定即阻断；(b) 一律按默认次序算，不阻断 | **(a)** | 两种次序的数值差 `c1/10·(1/c3−1)` 是真实差异，不是舍入噪声；D-89 要求逐条核对。真实收益取决于 P5 的组合数量（**待取证**） | **已裁定**（D-90 F）：固定先 cat11 后 cat1，并注明是构造性约定；P5 实测同日组合 0 条 |
| **Q4** | 前复权的构建时机 | (a) 挂在既有 ETF `update_data` item 的事务里（不新增 item 类型）；(b) 新增独立 `PlanSeries::EtfQfq` item | **(a)** | (b) 会把 SLO 第 1 行的 item 基数再推高，而派生不需要独立来源绑定。§5.3 | **未裁定**：实现细节，等实现开工 |
| **Q5** | 追加 vs 整条重写 | (a) bar 追加且因子未变 → 只追新 bar 的前复权行；因子变 → 整条重写；(b) 每次日更都整条重写 | **(a)** | 复用 `forward_adjust_exact` 的「proven-identical prefix 不复权」性质（`src/official_market.rs:1581-1586`），(b) 会让每只 ETF 每天都写一遍全历史。§5.2 | **未裁定**：实现细节，等实现开工 |
| **Q6** | capability revision | (a) 升 `generic-data-v12`；(b) 不升只改 release notes；(c) 升版并把 ETF 缺省口径改成 qfq | **(a)** | 客户端分支行为变了，属 D-70 Q10 的同类；缺省仍要求显式口径（§6.2）。§6.1 | **未裁定**：读面契约，随实现一起定 |
| **Q7** | fail-closed 的响应细节 | (a) 错误码不变（`adjustment_unavailable`）+ `details.reason`；(b) 新错误码 `etf_factor_unverified` 等 | **(a)** | 新码会让现有客户端分支全失效；`details` 是既有信封的扩展位（`src/data_supply/mcp.rs:819-825` 已在 `Error::Task` 用）。§3.2/§6.3 | **未裁定**：读面契约，随实现一起定 |
| **Q8** | 研究导出 | (a) `daily_qfq` 收录 ETF 行 + `factors.parquet` 加 `basis_status` 列（`factors` 升 schema 版）；(b) `daily_qfq` 不收录 ETF；(c) 另出 `daily_etf_qfq.parquet` | **(a)** | 导出口径必须与读面一致；(b) 会造出「MCP 读得到、导出拿不到」；(c) 多一个文件要维护 manifest 与校验，收益不抵成本。§7 | **未裁定**：导出批次，随实现一起定 |
| **Q9** | 沪市无法核对的实体 | (a) 一律阻断（fail-closed）；(b) owner 显式接受风险、以 `.day` 内部一致性作证据等级并标 `OWNER_RISK_ACCEPTED` 放行；(c) 先只交付深市 ETF 的 qfq | **(a)**，待 P12 取证数字出来后复议 | (a) 的代价完全取决于「沪市有事件的 ETF 有多少只、多少条事件」（§9 **P12，待取证 VM2-19**）：若只波及少数实体，(a) 就是最省事且最符合 D-89 第 6 条的选择；若波及面很大，(b) 才值得讨论。(b) **不是静默放行**：它要求 owner **显式**接受风险，证据等级取 `.day` 内部一致性（前收与除权日开盘的比率），并逐行标 `basis_status=OWNER_RISK_ACCEPTED`——D-89 第 6 条末句「owner 对某一类事件另有裁定的除外」正是这条路径。(c) 会让沪深混合请求出现部分可读，实现上要显式支持，代价不小 | **已被 D-90 E 取代**：沪市 831 条（cat1 689 条 / 105 只、cat11 142 条 / 123 只，合计 171 只）一律 `NOT_AVAILABLE`，不阻断，也不再需要在 (a)/(b)/(c) 之间选 |

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

- Q1、Q7 直接落实 D-89 第 6 条，不需要新增决策。
- Q6 的判例来自 D-70 Q10（已批准），本设计只是沿用。
- Q2 的 (a)/(b) **都是 schema 迁移**（27 → 28），差别只在表结构长什么样，
  部署代价一样。按 AGENTS.md 的约束，schema 变更不在本任务的授权范围内，
  所以本文推荐不改 schema 的 (c)。若 owner 认为复现完整性优先于部署代价，
  (a) 是可以自洽的选择——但那时要单独开一个 schema 任务，并在部署计划里
  预留一次完整重建。
- Q3、Q4、Q5、Q8 属于新裁定，写进 `docs/decisions.md` 需 owner 批准（Q3 已由 D-90 F 裁定）。
- Q9 的取舍已被 D-90 E 取代：沪市一律 `NOT_AVAILABLE`，不再需要在 (a)/(b)/(c) 之间选。

### 10.2 本文没做的事

- 没有改任何代码，没有跑 `cargo test`，没有连生产或 tailnet 主机。
- 没有给出任何 ETF bar 数、库体积增量、阻断实体数——这些在 §9 等取证。
  （2026-11-08 部分回填：阻断实体数与事件级数字见 §9 / §9.1；ETF bar 数与库体积增量仍未取得，见 P8。）
- 没有为 ETF 年线做任何设计（D-70 Q6 不变）。

---

## 11. 后续拆分（供 owner 排期参考，需阶段 1 完成后确认）

> **实现暂停，待公告试探（[#607](https://github.com/liqiangcc/x-data-mcp/issues/607)）结论。**
> 2026-11-08 的阶段 1 取证（§9.1）表明 D-89 的两条交付门槛都不满足，D-90 对 A–F 六类做了裁决；
> 其中 A 的下一步是基金收益分配与份额折算**公告试探取证**。那一轮结论出来之前，下面六个 PR 都不开工。
> PR-A 与 PR-B 里的单测夹具、阻断判定的**形状**会受公告结论影响（`c1` 的语义、`etf_checks_digest` 要记什么），
> 提前开工等于把会删掉的东西写两遍。

| PR | 内容 | 前置 |
|---|---|---|
| PR-A | 精确链的「事件→变换」参数化 + `etf_event_transform` + 单测 T1–T10（T5/T6 用取证夹具）；A 股逐位不变 | 本文批准 |
| PR-B | 来源库证据 scope（Q2 推荐 (c)；若 owner 选 (a)/(b) 则改成一条 schema 任务 + 完整重建计划）+ 阻断判定纯函数 + 单测 | 取证结论落库 |
| PR-C | 存储与日更：`SeriesShape::etf(Forward)` 写入、追加/重写规则、`resolve_read_shape` 放行、`read_public_series` 按请求口径解析 | PR-A、PR-B |
| PR-D | 读面与契约：`generic-data-v12`、`series_capabilities` 新行、`details.reason`、契约文档、`check_current_facts.py` | PR-C |
| PR-E | 研究导出：`daily_qfq` 收录 ETF、`factors` 加列、`export_smoke.py` 断言 | PR-D |
| PR-F | 验收脚本 `scripts/check_etf_qfq_continuity.py` + 部署前/后读数记录 | PR-C |
