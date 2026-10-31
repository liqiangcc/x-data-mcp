# M1 详细设计：不复权 + 因子 / 交易日历 / 指数基准 / 研究快照导出（#383）

- 任务：设计任务（#383 的前置设计），角色 agent。**只出设计**：不改代码、不改 schema、不装 Python 包、不部署。
- 关联 Issue：[#383](https://github.com/liqiangcc/x-data-mcp/issues/383)（本设计对应）、#1（产品范围）、#246（收录扩展）、#308（来源准入）、#381（成交量单位）、#295/#373（性能，不在本设计范围）。
- 输入报告（仓库外，2026-10-26；不在本仓库，正文按章节引用）：调研报告①「研究数据层需求与缺口」（对应 #1/#246/#308 的缺口取证）、调研报告②「ETF 收录前取证」（对应 #246）、调研报告③「全仓库只读分析」、调研报告④ #381 的成交量取证。正文的「调研报告①/②/③/④」即指这四份，结论对应的 Issue 保留在行内。
- 依据：owner 2026-10-26 裁决（方案 (c) + 6 位小数前复权；导出用 bin 命令 + Parquet，文件落 `/root/x-data-exports`，venv 在 `/root` 下；基准只用交易所指数；ETF 放 M3）、[docs/design.md](../design.md)「后续数据扩展要求」、[docs/latest-only-storage.md](../latest-only-storage.md)、[migrations/tasks/021_latest_only.sql](../../migrations/tasks/021_latest_only.sql)、[docs/decisions.md](../decisions.md)（D-08/D-09/D-19/D-20）。
- 基线：main `d738488`（含 #381/PR #382 的手日成交量修复）；生产库 `/opt/x-data-mcp-preview/state/preview.sqlite`（1,185,640,448 B，schema 21），2026-10-26 只读复核（本文 §2.1）。
- 本设计经 Claude 审核后，才按 §14 拆实现任务；审核前不写实现代码。

---

## 0. 结论摘要

| # | 设计点 | 结论 | 证据级 |
|---|---|---|---|
| 1 | 存储 | 每实体**两条日线序列**：`adjustment=0` 不复权（scale 2）、`adjustment=1` 前复权（scale 6）；外加因子事件表、日历表、指数序列与口径元数据、非 bar 数据集的版本登记表。schema 22，只出现在全新 init 链尾（D-08） | 设计 |
| 2 | 精度 | `market_series.price_scale` 按序列记录（2 分 / 3 厘 / 6 为 1e-6 元）；读取方必须用序列的 scale 解释整数价格 | 设计 |
| 3 | 摘要 | 序列摘要升到 `series-fold-v2`（种子含 `adjustment` 与 `price_scale`），`version_id` 仍是 `lower(hex(content_digest))`；非 bar 数据集用同一套"内容摘要派生 `version_id`"规则 | 设计 |
| 4 | 前复权算法 | 复权链用**精确有理数**合成、末次一次舍入到 6dp（`qfq-exact-v1`）。理由见 §4.3：2dp 下有 8.44% 的价格字段恰好落在半分位，28 位近似会把方向翻掉，跨实现不可复现 | 实测（sh.600580） |
| 5 | 与旧 2dp 的差异 | 调研报告① 实测 99.53% 逐位一致、0.47% 全部 ±1 分；本次把 0.47% 归因为"恰好落在 2dp 半分位"，并说明换成精确舍入后 2dp 基线的差异会扩大到约 2%（字段级），因此验收判据是"差异逐条可枚举"，不是"与旧值一致" | 实测 + 推断 |
| 6 | 日历 | 由 `sh.000001` ∪ `sz.399001` 的 `.day` 日期并集推导（`cal-union-v1`），带输入清单与算法版本；只用于"非交易日"判定；1991 年深市周六交易是已知边界 | 设计（数字引 调研报告①） |
| 7 | 指数基准 | 9 条交易所体系宽基序列（含中证），OHLC=0.01 点、volume=手、amount=元；量额口径逐指数登记（成分股合计 / 全市场合计），代码不写死 | 设计（取证引 调研报告① §4.3） |
| 8 | 导出 | 新增 bin `export_research`：Parquet + 清单，落 `/root/x-data-exports/<snapshot_id>/`；清单绑定目录版本、每条序列 `version_id`、来源 generation、文件 sha256；导出只读 | 设计 |
| 9 | 契约 | M1 不新增工具、不新增 dataset、不改 wire 形状；新数据只经导出提供。唯一可见变化是 `market.daily` 的前复权价精度（2dp→6dp）与全部 `version_id` 变化。做不到"完全无变化"的原因与替代方案见 §8 | 设计（Q3 已裁决：不算契约升版、不升 `capability_revision`，D-32） |
| 10 | 日更 | 同一 `(entity, period=day)` item 在一个事务里写两条序列 + 因子事件集；日历与指数作为同一 run 内的服务器内部 item（不新增 wire 面）。存储 +约 1.5GB，写放大与冷启动代价见 §9 | 估算 |

---

## 1. 范围

### 1.1 本次包含

1. schema 22：`market_series` 的 `adjustment` 放宽与 `price_scale`/`instrument_class`、`series_bars` 的价格列语义、`market_events` 的 `adjustment` 分键、`yearly_series_lineage` 的基口径、`evidence_bundles.evidence_kind` 扩展。
2. 不复权日线序列（全市场，与 `.day` 原值逐根相等；volume/amount 按 #381/#239 归一）。
3. 因子事件（gbbq `category=1`，只存已验证类别）与其内容摘要。
4. 6dp 前复权序列（精确有理数，`qfq-exact-v1`）。
5. 交易日历（指数并集推导，`cal-union-v1`）。
6. 指数基准（交易所体系宽基 9 条序列 + 逐指数口径元数据）。
7. 研究快照导出（bin + Parquet + 清单）。
8. 日更维护（plan/run 如何同时维护上述五类）。
9. 迁移与部署、分离点落位、扩展场景走查、实现拆分。

### 1.2 本次不包含

ETF（M3，仅预留 `price_scale=3` 与 `instrument_class`）、北交所/B 股、股本历史、上市/退市日、历史 ST、停牌、涨跌停派生、T 日股票池、历史行业、分钟线、财务/公告、MCP 新工具/新 dataset（放 v8，由后续任务审计）、下游 `x` 仓库改动、实际部署与发布、Python 包安装。

### 1.3 与既有裁决的关系

| 裁决 | 本设计如何吸收 |
|---|---|
| 方案 (c)：不复权 + 因子，同时物化前复权 | §3/§4：两条序列 + 因子事件表；前复权标为派生（带 `algorithm_version` 与血缘），不当事实 |
| 6 位小数前复权 | §4.3：`price_scale=6`；同时把算法换成精确有理数（否则 6dp 的末位仍依赖中间精度） |
| 导出 bin + Parquet，文件 `/root/x-data-exports`，venv 在 `/root` | §7 |
| 基准只用交易所指数 | §6；中证系 4 条算作交易所体系指数、基准取 9 条序列（Q1 已裁决，D-31） |
| ETF 放 M3 | §3.2 只预留 `price_scale=3` 与 `instrument_class=2`，不建 ETF 表、不做 ETF 目录 |

---

## 2. 现状事实（2026-10-26 复核）

### 2.1 数据与库（只读实测）

| 项 | 实测值 | 核对方式 |
|---|---|---|
| 库文件 | 1,185,640,448 B | `ls -l` |
| `market_series` 日线 | 5,224 条，全部 `adjustment=1`，`sum(record_count)=16,749,060`，`first_day` 7,657（1990-12-19），`last_day` 20,749（2026-10-23） | `GROUP BY period, adjustment` |
| `market_series` 年线 | 5,224 条，74,557 根 bar | 同上 |
| `market_events` | 83,545 行 | `count(*)` |
| `catalog_members` | 5,228 | `count(*)` |
| 目录版本 | `catalog-1791873196258239513` | `catalog_head` |
| `series_bars` 体积 | 748,347,392 B / 16,823,617 行 = 44.48 B/bar | 调研报告① `dbstat` 实测，本次未复测（全表扫描代价高） |

### 2.2 代码事实（`file:line`）

| 事实 | 位置 |
|---|---|
| `Adjustment` 只有 `Forward` 一个变体 | `src/market_data.rs:10-13` |
| `Period` 只有 `Day`/`Year` | `src/market_data.rs:5-9` |
| DDL：`CHECK(adjustment=1)` | `migrations/tasks/021_latest_only.sql:11` |
| DDL：价格列叫 `*_cents`（分） | `migrations/tasks/021_latest_only.sql:33-36` |
| DDL：`UNIQUE(entity, period, adjustment)` | `migrations/tasks/021_latest_only.sql:24` |
| DDL：`market_events` 有 `UNIQUE(task_id, item_id)`、无 `adjustment` 列 | `migrations/tasks/021_latest_only.sql:60-100` |
| DDL：`evidence_kind` 只允许 6 个取值 | `migrations/tasks/019_compact_market.sql:8-9` |
| 存储层把 `adjustment=1` 写死在 SQL 里 | `src/sqlite_market/series.rs:251`、`:1139`、`:1444`（INSERT 的 `VALUES(?1,?2,1,…)`）；`src/sqlite_market.rs:190` |
| 启动检查遍历**全部** `market_series` 行（无口径过滤） | `src/sqlite_market/series.rs:1183` |
| `CompactBar` 字段名与读回写死 2dp | `src/compact_market.rs:23-26`、`:39-42`、`:55-58` |
| `cents()` 要求"恰好 2dp"，否则拒绝 | `src/compact_market.rs:812-822` |
| 前复权入口：2dp HALF_UP | `src/qfq.rs:68-74`、`:129-132` |
| 因子规范化：2dp HALF_UP | `src/gbbq.rs:113-131` |
| 仿射链：`Decimal` 除法（28 位有效数字）无中间舍入 | `src/gbbq.rs:133-177` |
| `.day` → 发布 bar 的唯一映射点（`qfq_bars` 按索引 zip 原始记录与 QFQ bar） | `src/official_market.rs:479-510` |
| 成交量单位判定（#381 已合并）：标记常量、`amount_decimal`、`normalized_volume`、`hands_consistent` | `src/official_market.rs:70-71`、`:78`、`:101-113`、`:115` |
| 单位/币种证据写死 `price_unit=yuan`、`volume_unit=share` | `src/official_market.rs:575-594` |
| `"qfq"` 字符串校验散落在 8 处非测试代码 | `src/compact_market.rs:746`、`src/publish.rs:191`、`src/query.rs:124`、`src/yearly.rs:62`、`src/market_update.rs:210`、`src/official_market.rs:1466`、`:2168`、`:2178`、`src/market_storage/compat.rs:63` |
| 年线要求 `adjustment_evidence == "qfq"` 且单位已确认 | `src/yearly.rs:62-64`、`:120` |
| schema 常量 | `scripts/check_current_facts.py:13`（`EXPECTED_COMPACT_SCHEMA = 21`） |
| 迁移链（include_str 顺序） | `src/sqlite_tasks.rs:14-42` |
| 读路径：实体只校验格式与访问权，**不校验目录成员** | `src/data_supply/query.rs:77-95` |
| 快照上限 100 实体 / 每页 1..1000 行 | `src/data_supply/query.rs:85-86`、`:75` |
| 小数输出不固定位数（"不擅自固定两位或六位小数"） | `docs/sqlite-storage-design.md:132` |
| 部署体积门禁 ≤8GiB（目录口径；2026-10-26 前为 ≤4GiB、口径待定，已由 [SLO](../../docs/SLO.md) §4 的 2026-10-26 签字行上调；本设计估算部署后约 5.7GiB，见 §17 第 5 条） | `docs/SLO.md` 第 1 节第 5 行 |

### 2.3 直接影响设计的三个现状缺口

1. **口径不是序列的维度**：`adjustment` 在 DDL 里是单值，代码里 3 处 SQL 写死 `=1`、1 处 INSERT 写死常量 `1`、1 处启动检查不过滤口径，`market_events` 的唯一键是 `(task_id, item_id)` —— 一条 item 只能写一条序列。要新增第二条序列，这三处必须一起改，否则写入要么被 CHECK 拒绝，要么两个序列抢同一个事件槽。
2. **精度不是序列的属性**：`CompactBar` 与 `cents()` 把"分"写进类型与校验，ETF 的 4.178 会被直接拒绝（调研报告② §5.1 已实测该拒绝路径）。加一个 6dp 序列同理。
3. **"qfq" 是字符串常量而不是枚举**：`Adjustment` 有枚举，但校验点用的是 `value != Some("qfq")`。加变体时编译器不会报错，静默拒绝新口径。这是 H2（改结构体/helper 后 grep 全仓调用点）必须逐条处理的清单。

---

## 3. 存储设计（schema 22）

### 3.1 总览

| 表 | 变更 | 层 |
|---|---|---|
| `market_series` | 改：`adjustment IN (0,1)`、新增 `price_scale`、`instrument_class`、`digest_format='series-fold-v2'`、`period=2 ⇒ adjustment=1` | 存储模型 |
| `series_bars` | 改：价格列更名并改为"按 scale 解释的整数"；`volume_quantum` 已裁决预留（Q4 方案 1，D-34） | 存储模型 |
| `market_events` | 改：新增 `adjustment`，唯一键改 `(task_id, item_id, adjustment)`，索引加 `adjustment` | 存储模型 |
| `yearly_series_lineage` | 改：新增 `base_adjustment`（年线只从 `adjustment=1` 派生） | 存储模型 |
| `evidence_bundles` | 改：`evidence_kind` 增加 `'derived'`（日历/因子集/指数口径的取证包） | 证据层 |
| `adjustment_events` | 新：gbbq cat1 解析事实（每实体一组） | 解析事实层 |
| `derived_dataset_versions` | 新：非 bar 数据集的版本登记（日历、因子集） | 派生层 |
| `trading_days` | 新：交易日历（派生） | 派生层 |
| `index_series_meta` | 新：逐指数口径元数据（派生/证据混合，只描述单位与口径） | 派生层 + 证据 |

不变：`source_bindings`（指数与个股共用同一形状，`day_member_sha256` 指该成员的摘要，`security_event_sha256` 对指数取"空事件集"的摘要）、`catalog_*`、任务子系统各表。

### 3.2 `market_series`

```sql
CREATE TABLE market_series(
  series_key INTEGER PRIMARY KEY,
  entity TEXT NOT NULL,
  period INTEGER NOT NULL CHECK(period IN (1,2)),
  -- 0=不复权（价格事实） 1=前复权（派生，见 §4.3）
  adjustment INTEGER NOT NULL CHECK(adjustment IN (0,1)),
  -- 0=股票 1=指数 2=ETF(M3 预留)
  instrument_class INTEGER NOT NULL CHECK(instrument_class IN (0,1,2)),
  -- 整数价格的分母：值 = 整数 / 10^price_scale 元（指数为"点"）
  price_scale INTEGER NOT NULL CHECK(price_scale BETWEEN 0 AND 6),
  version_id TEXT NOT NULL UNIQUE,
  content_digest BLOB NOT NULL CHECK(length(content_digest)=32),
  digest_format TEXT NOT NULL CHECK(digest_format='series-fold-v2'),
  record_count INTEGER NOT NULL CHECK(record_count > 0),
  first_day INTEGER NOT NULL,
  last_day INTEGER NOT NULL,
  coverage_status INTEGER NOT NULL CHECK(coverage_status IN (1,2,3)),
  source_binding_digest BLOB NOT NULL REFERENCES source_bindings(source_binding_digest),
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  last_event_seq INTEGER NOT NULL CHECK(last_event_seq > 0),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(entity, period, adjustment),
  CHECK(first_day <= last_day),
  CHECK(version_id = lower(hex(content_digest))),
  -- 年线只有前复权口径；不复权只做日线
  CHECK(period = 1 OR adjustment = 1)
) STRICT;
```

要点：

1. **`adjustment` 取值 0/1**：`0` 不复权、`1` 前复权。不引入 `2=2dp 前复权`：那会让同一口径存两份（+745MB）并制造"哪份是权威"的问题（§8.3）。
2. **`price_scale` 是序列属性**：M1 的取值只有 2（不复权，A 股分）与 6（前复权）；3 留给 M3 ETF 厘。`CHECK BETWEEN 0 AND 6` 而不是 `IN (2,3,6)`：3dp 之外的整数精度没有业务含义，但把上限写死成 6 能在 DDL 层拦住"用 1e-9 表达"的误用。
3. **`instrument_class`**：M1 只写 0（股票）与 1（指数）；2 只是预留值，M1 不写任何 ETF 行。它与 `price_scale` 是两个正交维度（指数是 scale 2 + class 1；ETF 会是 scale 3 + class 2）。
4. **`UNIQUE(entity, period, adjustment)` 保持**，不加 `instrument_class`：加了只会允许"同一代码既是股票又是指数"，而这不是我们要允许的事。同一 `entity` 的 `instrument_class` 必须一致，由 `task_sqlite check` 的深检保证（§3.2 第 4 条）。
5. `period=2`（年线）只允许 `adjustment=1`：年线从 6dp 前复权日线聚合，不派生不复权年线（M1 没有用例需要它，调研报告① 也没有）。

### 3.3 `series_bars`

```sql
CREATE TABLE series_bars(
  series_key INTEGER NOT NULL REFERENCES market_series(series_key),
  trade_day INTEGER NOT NULL,
  -- 整数价格 = 真实价格 × 10^market_series.price_scale（元，指数为点）
  open_units INTEGER NOT NULL,
  high_units INTEGER NOT NULL,
  low_units INTEGER NOT NULL,
  close_units INTEGER NOT NULL,
  volume INTEGER,
  amount TEXT,
  r7_raw INTEGER,
  -- 已裁决（#381 的 Q4 方案 1，D-34）：手日 bar 的成交量精度（100 股），普通 bar 为 NULL
  -- volume_quantum INTEGER CHECK(volume_quantum IS NULL OR volume_quantum > 0),
  PRIMARY KEY(series_key, trade_day),
  CHECK(high_units >= open_units AND high_units >= close_units AND high_units >= low_units),
  CHECK(low_units <= open_units AND low_units <= close_units),
  CHECK(volume IS NULL OR volume >= 0),
  CHECK(r7_raw IS NULL OR r7_raw BETWEEN 0 AND 4294967295)
) STRICT, WITHOUT ROWID;
```

**为什么更名而不是保留 `*_cents`**：一列叫 `close_cents` 却存 1e-6 元的整数，是给未来每个人埋的坑（M3 的 ETF 还会存 1e-3 元）。更名的实际成本：DDL + `compact_market.rs`（`CompactBar` 字段 + `cents()`）+ `sqlite_market/series.rs` 的读写绑定 + 两个测试文件，全仓 `_cents` 共 89 处（`grep -rn _cents src/`，2026-10-26）；其中 `compact_market.rs` 与 `series.rs` **本来就要为 `price_scale` 改动**（§2.3 第 2 条），剩下的主要是测试字面量。schema 22 走全新 init，没有存量数据迁移成本。价格语义由 `market_series.price_scale` 唯一决定，`series_bars` 不再自称单位。

`volume_quantum`：**已裁决采用方案 1**（#381 的 Q4，D-34）：导出 schema 增加该列；数据库侧按上文取舍在 schema 22 预留一列（不预留则之后要再升一次 schema），具体由实现任务在 PR-1 确认。

### 3.4 `market_events`

```sql
-- 只列变更部分
CREATE TABLE market_events(
  ...
  adjustment INTEGER NOT NULL CHECK(adjustment IN (0,1)),
  ...
  UNIQUE(result_id),
  UNIQUE(task_id, item_id, adjustment),
  FOREIGN KEY(task_id,item_id) REFERENCES task_items(task_id,item_id),
  ...
) STRICT, WITHOUT ROWID;
CREATE INDEX market_events_entity_period
  ON market_events(entity, period, adjustment, seq DESC, new_content_digest, kind);
CREATE INDEX market_events_new_digest ON market_events(new_content_digest);
```

要点：

1. **`(task_id, item_id, adjustment)`**：一个日线 item 在同一事务里写不复权与前复权两条序列，各写一条事件。`result_id` 仍然全局唯一（幂等重放锚点不变），`UNIQUE(task_id,item_id)` 必须放宽，否则第二条序列无法留审计。
2. **事件不带 `instrument_class`**：序列身份已经是 `(entity, period, adjustment)` 且唯一，事件按同一键闭合即可。指数与个股不会共用同一 `(exchange, code)`（M1 的 9 个指数代码与 5,228 个股票代码无交集，用深检保证）。
3. **索引加 `adjustment`**：启动闭合探测是"每序列两次索引 seek"，序列数从 5,224 涨到 10,457（+年线），索引键必须跟着扩展，否则探测会退化成扫全表。
4. 事件 `kind` 不变（`first_publish`/`append`/`replace`/`no_change`/`metadata`/`yearly_rederive`）。不复权与前复权各自独立走这六种 kind；**"原始不变而前复权重写"** 是新增的真实情形（未来生效日的 cat1 事件），由 `adjustment=0` 的 `no_change` + `adjustment=1` 的 `replace` 表达，无需新 kind。

### 3.5 `yearly_series_lineage`

```sql
ALTER/rebuild 增加：
  base_adjustment INTEGER NOT NULL DEFAULT 1 CHECK(base_adjustment IN (0,1)),
  base_instrument_class INTEGER NOT NULL DEFAULT 0 CHECK(base_instrument_class IN (0,1,2)),
```

年线只从 `(adjustment=1, class=0)` 的日线派生；把"基口径"显式写进血缘，避免以后有人用不复权日线重算年线却复用同一条 lineage 行。`base_daily_digest` 与 `base_event_seq` 语义不变（仍是派生时那一行序列的 `content_digest` 与 `last_event_seq`），但**摘要格式变了**，所以 stale 分类（`fresh`/`behind`/`invalid`）在 schema 22 重建后从零开始，不存在跨格式的历史链。

### 3.6 `adjustment_events`（解析事实层）

```sql
CREATE TABLE adjustment_events(
  entity TEXT NOT NULL,
  -- gbbq 解码顺序（同一实体内的稳定序，同日多条事件靠它区分）
  ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
  effective_day INTEGER NOT NULL,
  category INTEGER NOT NULL CHECK(category = 1),
  -- 原始 f32 位模式，原样保留（不可变事实）
  c_bits_1 INTEGER NOT NULL,
  c_bits_2 INTEGER NOT NULL,
  c_bits_3 INTEGER NOT NULL,
  c_bits_4 INTEGER NOT NULL,
  -- 派生：按 gbbq-cat1-2dp-v1 规范化后的精确十进制（可从 c_bits 重算）
  c1 TEXT NOT NULL,
  c2 TEXT NOT NULL,
  c3 TEXT NOT NULL,
  c4 TEXT NOT NULL,
  source_binding_digest BLOB NOT NULL REFERENCES source_bindings(source_binding_digest),
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  -- 知晓时间：gbbq 无公告日，M1 明确写 unknown（分离点 3）
  known_at TEXT,
  PRIMARY KEY(entity, ordinal)
) STRICT, WITHOUT ROWID;
CREATE INDEX adjustment_events_entity_day
  ON adjustment_events(entity, effective_day, ordinal);
```

要点：

1. **事实与派生同表但可校验**：`c_bits_*` 是不可变事实，`c1..c4` 是派生值。二者关系由测试保证（`normalize(c_bits) == c*`），因此"因子可重算"是可执行的断言，不是文档承诺。
2. **`known_at` 恒为 NULL**：TDX 的 gbbq 只给除权日，没有公告日。M1 不猜、不用文件 mtime 冒充（那会随 generation 变化）。任何"时点复权"必须同时声明 `known_at=unknown`，见 BR-M1-9。
3. **只存 cat1**：其他类别（cat 2/3/5/7/8/9/10）语义未经 #54 级证据（调研报告① §4.4、U-1），M1 不入库；`excluded` 计数仍进证据包（沿用 `security_events_digest` 的既有口径）。
4. **整组替换**：一个实体的因子集在 item 事务内 delete+insert（事件数 ≤ 数十），不做增量；摘要变化即版本变化。
5. **与 #381 的关系**：手日成交量只影响 `volume` 的取值与精度，不影响因子与价格；`volume_quantum`（逐 bar 100 股精度）按 Q4 裁决采用方案 1（D-34）：导出 schema 带该列，数据库侧在 schema 22 预留。

### 3.7 `derived_dataset_versions`（非 bar 数据集的版本登记）

```sql
CREATE TABLE derived_dataset_versions(
  dataset_kind TEXT NOT NULL CHECK(dataset_kind IN ('adjustment_events','trading_calendar')),
  dataset_key TEXT NOT NULL,              -- 因子集=entity；日历='exchange_union'
  version_id TEXT NOT NULL UNIQUE,
  content_digest BLOB NOT NULL CHECK(length(content_digest)=32),
  digest_format TEXT NOT NULL,
  record_count INTEGER NOT NULL CHECK(record_count > 0),
  first_day INTEGER NOT NULL,
  last_day INTEGER NOT NULL,
  algorithm_version TEXT NOT NULL,
  source_binding_digest BLOB NOT NULL REFERENCES source_bindings(source_binding_digest),
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  task_id TEXT NOT NULL,
  item_id TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY(dataset_kind, dataset_key)
) STRICT;
```

**为什么需要它**：导出清单必须能给"日历"和"每个实体的因子集"一个稳定身份（§7.3）。它们不是 bar 序列，塞进 `market_series` 会污染序列语义（`record_count`/`period`/`adjustment` 都不适用）；给每类各建一张版本表又会把同一套"摘要派生 `version_id` + 证据 + 任务引用"抄两遍。故用一张**只放身份与审计、不放业务列**的登记表，业务行仍在各自的类型化表里（不是万能 JSON 表）。

### 3.8 `trading_days`（派生）

```sql
CREATE TABLE trading_days(
  trade_day INTEGER PRIMARY KEY,
  -- 位掩码：bit0=sh.000001，bit1=sz.399001（哪个输入的 .day 提供了该日）
  sources INTEGER NOT NULL CHECK(sources > 0)
) STRICT, WITHOUT ROWID;
```

- 行存在 = 该日至少一个输入指数有 bar。**不存 `is_open`**：没有"非交易日"行，避免"日历里全是 true"的冗余与半开区间歧义。
- 整表在一个事务内 delete+insert（约 8,800 行，<100KB），版本由 `derived_dataset_versions` 的 `trading_calendar` 行表达。
- 不存"未来交易日"：日历的 `last_day` 等于推导时输入的最后一个 bar 日期，之后的日期未知。

### 3.9 `index_series_meta`（逐指数口径）

```sql
CREATE TABLE index_series_meta(
  series_key INTEGER PRIMARY KEY REFERENCES market_series(series_key),
  -- 同一指数可能有两个代码（如 000300/399300 同一条序列）
  aliases TEXT NOT NULL DEFAULT '',
  aggregate_scope TEXT NOT NULL CHECK(aggregate_scope IN ('constituents','whole_market','whole_board')),
  member_count INTEGER CHECK(member_count IS NULL OR member_count > 0),
  price_unit TEXT NOT NULL CHECK(price_unit = 'index_point'),
  volume_unit TEXT NOT NULL CHECK(volume_unit = 'hand'),
  amount_unit TEXT NOT NULL CHECK(amount_unit = 'yuan'),
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  evidence_note TEXT NOT NULL,      -- 取证方式与结果（对哪个官方接口、多少点、比值）
  created_at TEXT NOT NULL
) STRICT;
```

- 单位与口径是**数据**，不是代码常量（BR-M1-6）：`volume=手` 与个股的"股"差 100 倍，`amount=元`，OHLC 是 0.01 点；口径分 `constituents`（成分股合计）与 `whole_market`/`whole_board`（全市场/全板块合计）。
- `evidence_note` 存"怎么证出来的"（调研报告① §4.3 的 108 个 OHLC 点与量额比值），`evidence_digest` 指向正式证据包；没有证据的指数不入库（fail-closed）。
- 指数名称不入库：官方没有给我们一份可锚定的指数名称文件，不造字段。

### 3.10 `version_id` 与摘要的定义

| 数据集 | 摘要输入（按序折叠） | `version_id` |
|---|---|---|
| bar 序列（`market_series`） | 种子 = `SHA256("series-fold-v2" ‖ entity ‖ period ‖ adjustment ‖ instrument_class ‖ price_scale)`；逐 bar `SHA256(h_{n-1} ‖ bar_encoding(bar_n))`，bar 按 `trade_day` 严格递增 | `lower(hex(content_digest))` |
| 因子集（`adjustment_events`） | `SHA256("adjustment-events-v1" ‖ entity ‖ n ‖ 逐条 (ordinal, effective_day, category, c_bits_1..4))`；末尾附 excluded 计数 | `lower(hex(content_digest))` |
| 交易日历（`trading_days`） | `SHA256("trading-calendar-v1" ‖ n ‖ 逐日 (trade_day, sources))` | `lower(hex(content_digest))` |
| 研究快照（导出） | 见 §7.3：清单的规范 JSON（不含 `created_at` 与 `files`） | `snapshot-<前 32 hex>` |

规则（与 `docs/latest-only-storage.md` §3 一致，只是扩展了数据集种类）：

1. 摘要**只取决于取值与顺序**，不含 coverage、来源、证据、时间戳。元数据变化用事件日志的 `metadata` 表达，`version_id` 不变。
2. **派生结果不进摘要**：前复权的 6dp 值有自己的摘要（它就是一条 bar 序列），但"它是从前复权算法算出来的"这一事实记在 `algorithm_version` + 血缘，不参与摘要。
3. `bar_encoding` 从 v2 起必须把 `price_scale` 的语义固化（整数 + 序列 scale），因此 **schema 22 重建后所有 `version_id` 都会变**；这是 D-08 全新重建的既定后果，不是本次额外引入的兼容负担。
4. 空序列非法（沿用 `record_count > 0`）。

### 3.11 与 latest-only 模型、事件日志的关系

| 方面 | schema 21（现状） | schema 22（本设计） |
|---|---|---|
| 序列粒度 | `(entity, period, adjustment=1)` | `(entity, period, adjustment ∈ {0,1})` |
| 一个 item 的写集 | 1 条序列 + 1 条事件 | 2 条序列（日线）+ 1 组因子事件 + 2 条事件 + 1 条登记 |
| 事件唯一键 | `(task_id, item_id)` | `(task_id, item_id, adjustment)` |
| 启动检查 | O(#series) 索引 seek | O(#series) 索引 seek，`#series` 从 5,224 变 10,457 |
| 摘要格式 | `series-fold-v1` | `series-fold-v2`（种子含口径与精度） |
| 旧库 | 归档 + 全新 init | 同（不原地升级） |

**事件日志的语义边界不变**：事件证明"这条序列为什么变了"，不存旧价格。不复权序列的 `no_change` 与前复权序列的 `replace` 在同一次提交里出现，正好表达"原始价没变、因子的未来生效日重写了历史前复权"这一真实情形。

---

## 4. 派生设计

### 4.1 不复权序列

- **来源**：`hsjday.zip` 的 `.day` 成员（32 字节记录），**唯一映射点**是 `official_market` 里 `.day` → 发布 `Bar` 的那一处（现状 `src/official_market.rs:479-510` 的 `qfq_bars`，它按索引 zip 原始记录与 QFQ bar）。实现上新增一个 `raw_batch()`，与现有 `qfq_batch()` 共用同一次成员读取与 CRC 校验（`MaterializeSession` 已经缓存了解码结果）。
- **价格**：`price_scale=2`，值 = `.day` 的 `open/high/low/close` uint32 原值（分）。不做任何变换。
- **volume/amount**：按 #381（已随 PR #382 合并，`d738488`）归一后**两条序列同值**。实现上直接复用 `normalized_volume`/`amount_decimal`（`src/official_market.rs:78`、`:101-113`），**不在不复权路径里重写一份判定**：
  - 手日（`r7_raw` 高位 `0xC36400` 且一致性校验通过）→ `volume = raw × 100`；无标记但校验通过（反向不一致）→ `volume = NULL`；标记但校验失败 → `NULL`；其余 → `raw`。
  - `amount` 保持 `f32 → 十进制字符串` 的现状（`amount_decimal`），单位元，f32 精度限制写进证据与导出清单的 `limitations`。
  - `r7_raw` 原样保留，不进查询契约、不进导出主表（可放导出可选列）。
- **覆盖与证据**：`coverage_status` 沿用现有 `.day` 成员覆盖判定；`source_binding_digest` 与前复权序列**相同**（同一天成员、同一事件摘要），因此两条序列共用一份来源绑定与证据包，不新增证据类型。
- **AC-M1-1 的判据**：不复权序列与 `.day` 原值逐根相等。这条 AC 不需要"重算"，只需要"没有偷偷变换"。

### 4.2 前复权序列

- **输入**：不复权 bar 序列（内存里就是刚解析出的 `.day` 记录）+ 该实体的 cat1 事件集（`adjustment_events` 的同一份解码结果）。
- **算法**：`qfq-exact-v1`（§4.3）。输出 `price_scale=6`。
- **不改的边界**（沿用 #54 冻结契约，`src/qfq.rs:1-20`）：`event_date > bar_date` 才生效；事件按日期升序合成、无中间舍入；空事件集 = 恒等链；amount/volume 不复权。
- **事件日参考价**（`displayed_previous_close`）仍按 2dp 输出（它是"显示值"，不是收益序列）：`src/qfq.rs:180-216` 不变。这条要在文档里写明，否则容易被当成"6dp 应该也改参考价"。
- **未来事件**：cat1 里有晚于最新 bar 的生效日（调研报告① 实测最晚 2026-11-02）。`event_date > bar_date` 的规则会把它应用到**所有** bar 上，这是前复权的定义（不是 bug），但必须在导出清单与文档里写明"前复权以快照最后事件为基准，含未来事件"。

### 4.3 与现有 2dp 前复权的差异，以及如何对外说明

**调研报告① 的比对结果**（实测，2026-10-26）：308 只证券、983,973 根 bar、4 个价格字段全部比对，99.53% 完全一致；0.47%（4,589 个字段）**全部是 ±1 分**；分布集中在 `sh.600580`（179 例）、`sh.600072`（34 例）、`sh.600023`（9 例）。调研报告① 把它归因为"rust_decimal 28 位 vs Python decimal 28 位有效数字的舍入方向"。

**本次补充的归因（实测，sh.600580，5,776 根 bar / 23,104 个价格字段 / 25 条 cat1 事件）**：

| 量 | 值 |
|---|---|
| 精确有理数链算出的价格**恰好落在 2dp 半分位**（如 1689/200 = 8.445）的字段 | 1,951（**8.44%**） |
| 其中"28 位近似 + 2dp HALF_UP"与"精确 + 2dp HALF_UP"结论不同的字段 | 476（**2.06%**） |
| 精确值恰好落在 **6dp** 半分位的字段 | **0** |
| 6dp 下"28 位近似"与"精确"不同的字段 | **0** |

**结论**：0.47% 不是"跨语言噪声"，而是**恰好落在半分位上的 bar 由中间精度决定舍入方向**——也就是说，2dp 前复权在原理上就不可跨实现复现，加位数也不能自动修好（6dp 仍有半分位，只是频率低得多，本次样本为 0）。因此 M1 采用**精确有理数链 + 末次一次舍入**：这让 6dp 值只取决于（bar、事件、目标精度、算法版本），与中间精度无关。

**代价（必须如实说）**：换成精确舍入后，"用新算法算出的 2dp"与"库里旧的 2dp"会在**约 2% 的字段**上不同（同一 sh.600580 样本 476/23,104），而 调研报告① 的 0.47% 是两套 28 位近似之间的差异。所以 AC-M1-3 不能写成"与旧序列一致"，只能写成"差异全部落在半分位 bar 上且可逐条枚举"。

**对外说明（三段式，写进发布说明与 `docs/mcp-tools.md` 的当前状态区）**：

1. 变了什么：`market.daily` 的前复权价从 2dp 变为最多 6dp；所有序列的 `version_id` 因摘要格式升级而全部变化（D-08 全新重建）。
2. 为什么：2dp 让 **12.6%** 的历史 bar（前复权价 < 2 元）落在低价段（该比例以 [数据目录](../data-catalog.md) §7 的复核为准；调研报告原值有量纲错误），那里日收益误差中位 0.42–2.9pp；且在半分位 bar 上，2dp 结果依赖实现细节，不可复现。
3. 使用者该怎么做：要按时点复权，用不复权 + 因子事件（导出提供），按 `effective_day ≤ T` 自算；前复权序列是"以快照最后事件为基准"的便利序列，含未来事件，不适合直接做回测输入。

### 4.4 因子集（派生层）与 `algorithm_version`

| 项 | 值 |
|---|---|
| 解析 | `gbbq::parse` + `decode_events`（`src/gbbq.rs`） |
| 规范化 | `normalize_factor`（2dp HALF_UP，`src/gbbq.rs:122-131`）→ `gbbq-cat1-2dp-v1` |
| 变换 | `event_transform`（`src/gbbq.rs:150-160`）→ 仿射 `(a, b)` |
| 合成 | `compose_chain`（`src/gbbq.rs:165-177`）→ 本设计改为精确有理数（§4.3） |

因子集的 `algorithm_version` 记 `gbbq-cat1-2dp-v1`；前复权序列的 `algorithm_version` 记 `qfq-exact-v1`。两者分开：规范化改了（例如以后 cat1 的 2dp 规则修订）不需要重写前复权的算法标识，反之亦然。

---

## 5. 交易日历

### 5.1 输入

| 输入 | 代码 | 起始 | 日期数（调研报告① 实测） |
|---|---|---|---|
| 上证综指 | `sh.000001` | 1990-12-19 | 8,748 |
| 深证成指 | `sz.399001` | 1991-04-03 | 8,703 |
| 并集 | — | 1990-12-19 | **8,793** |

选这两个的依据：

1. 它们是各自市场**历史最长**的交易所发布指数，覆盖期与 A 股行情期基本重合；
2. 两者互补：`sz.399001` 有 45 个日期不在 `sh.000001` 里，`sh.000001` 有 90 个不在 `sz.399001` 里（早期两所节假日不同）；
3. 只用一个会漏：只用 `sh.000001` 会把 **261 根 A 股 bar**（全部是 1991 年深市的周六交易日，如 1991-04-06/04-13/04-20/05-04）判成"非交易日"。

不再加入更多指数：其余候选（`000300` 2005 起、`399006` 2010 起等）起始更晚，对并集没有增量贡献；加入只会让"用了哪几只指数"这个答案变长。

### 5.2 算法与边界

- 算法 `cal-union-v1`：取上述两个 `.day` 成员的全部记录日期，按位掩码合并，升序写入 `trading_days`。
- **判定方向单向**：不在日历 ⇒ 非交易日。在日历 ⇏ 该证券当日可交易（停牌、上市前、退市后都不在日历的语义内）。
- **不能证明完整性**：没有官方日历文件；"指数有 bar"只能证明"该指数当日有行情"。因此：
  - 日历标为派生数据（`derived_dataset_versions`），带输入成员 sha256 与算法版本；
  - 每次重建都做一次**覆盖率自检**：统计 generation 里全部 A 股 `.day` bar 中，落在日历覆盖区间 `[first_day,last_day]` 内、但日历无该行的条数与比例（分母为区间内 bar 数；调研报告① 实测 0.001%，261 根，全部在 1991 年）；超过阈值（0.01%）则该 item 失败并报告，不静默接受。`last_day` 之后的 bar 属 `unknown`，单独记为 `stale_bars`（并给出 `stock_last_day` / `calendar_last_day`），不计入该比例，也不触发阈值（#446）；
  - 导出清单的 `limitations` 固定包含"日历为推导数据，不能用于停牌判定"。
- **1991 年深市**：周六交易是真实历史（不是数据错误），因此不能"修正"成周一至周五；这正是必须用并集而不是"工作日规则"的理由。
- **有效期**：日历只到输入的最后一个 bar 日期；之后的交易日未知（不预测）。已存日历不回退：新日历的 `last_day` 早于已存日历即拒绝（稳定码 `calendar_regressed`），不整表替换（#446）。

---

## 6. 指数基准

### 6.1 清单（M1，9 条序列）

| 序列实体 | 别名 | 口径 | 成分数 | 起始（调研报告①） |
|---|---|---|---|---|
| `sh.000001` | — | `whole_market`（全部沪市，含 B 股） | — | 1990-12-19 |
| `sz.399001` | — | `whole_market`（全部深市） | — | 1991-04-03 |
| `sz.399006` | — | `whole_board`（全部创业板） | — | 2010-06-01 |
| `sh.000016` | — | `constituents`（上证50） | 50 | 2004-01-02 |
| `sh.000300` | `sz.399300` | `constituents`（沪深300） | 300 | 2005-01-04 |
| `sh.000905` | `sz.399905` | `constituents`（中证500） | 500 | 2007-01-15 |
| `sh.000852` | — | `constituents`（中证1000） | 1,000 | 2005-01-04 |
| `sh.000906` | — | `constituents`（中证800） | 800 | 2007-01-15 |
| `sh.000688` | — | `constituents`（科创50） | 50 | 2019-12-31 |

- `000300`/`399300`、`000905`/`399905` 是**同一条序列的两个代码**（调研报告① 实测 bar 数相同）。存一条，另一个代码进 `aliases`，避免同一内容存两份、两个 `version_id`。
- 明确**不收**：`sh.880xxx`（1,138 个）与 `sh.887xxx`（190 个）是通达信自编板块指数，不是交易所/中证发布（调研报告① §4.3）；`sz.399005`（中小板指）与 `sz.399303`（国证2000）是可选的后续追加项（加一行登记即可，见 §12 场景 1）。
- Q1（**已裁决**，D-31）：中证指数有限公司发布的 4 条（`sh.000300` 沪深300、`sh.000905` 中证500、`sh.000852` 中证1000、`sh.000906` 中证800）**算作交易所体系指数**，基准保留 9 条序列；原裁剪方案（退化为 5 条：`sh.000001`、`sz.399001`、`sz.399006`、`sh.000016`、`sh.000688`）不采用。

### 6.2 单位与口径（写进 `index_series_meta`）

| 字段 | 值 | 证据（调研报告① §4.3 实测） |
|---|---|---|
| OHLC | 0.01 点（`price_scale=2`，`price_unit='index_point'`） | 96 个中证 OHLC + 12 个上证 OHLC 与官方接口逐一精确匹配 |
| volume | 手（100 股） | 中证接口 `tradingVol`(股) ÷ `.day` vol = 100.0000（24/24）；上证/深证接口本身以手发布，比值 1.0000000 |
| amount | 元（f32） | 官方 `tradingValue`(亿元)×1e8 ÷ `.day` amount ≈ 0.999999–1.000006 |
| 口径 | 逐指数 | `000300` 等 6 个中证 = 成分股合计（`consNumber` 与成分数一致）；`000001` 全沪市；`399001` 全深市；`399006` 全部创业板 |

三条不能混用的结论（写进证据文本）：指数 volume 与个股 volume **差 100 倍**；指数 amount 是元；量额口径**逐指数不同**。

### 6.3 存储与可见性

- 指数是普通 bar 序列：`market_series(instrument_class=1, adjustment=0, price_scale=2)` + `series_bars` + `market_events`。走同一条提交路径（摘要、事件、启动检查都复用），不新建一套 bar 表。
- **指数不进 QFQ**：没有除权概念，`adjustment=0` 是唯一口径；不派生年线（`period=2` 不存在）。
- `r7_raw` 原样保留、永不解释（调研报告① 实测 `sh.000001` 的 r7 在 2026-10-23 为 28,641,085，不是除权基准价）。
- **MCP 不可见**：MCP 读路径按 `instrument_class=0 AND adjustment=1` 解析序列（§9.2），所以 `query_data` 取不到指数；导出读模型显式读全部类别。这是 AC-M1-10 的可测边界。
- 证据：`evidence_bundles.evidence_kind='derived'` 的一条包，内容是"单位/口径的取证方式与结果"（对哪个接口、多少点、比值区间），`source_binding_digest` 指向该指数 `.day` 成员的来源绑定。
- **坏 bar 剔除（#383 owner 2026-10-27 裁决，方案 a）**：官方 `.day` 中指数早期存在 OHLC 自相矛盾的 bar（实测 `sh.000001` 1991–1995 年 16 根、`sz.399001` 9 根；例 `sh.000001 19910509 close 10929 > high 11012`）。判定只看该 bar 自身四价：`low <= min(open, close)`、`max(open, close) <= high`、`low <= high`，任一不满足即整根剔除——不修补、不夹逼、不估算，其余 bar 照常入库。被剔除 bar 的日期与原始 OHLC 写进该序列的 `version` 证据包（`excluded_bars`，OHLC 为十进制值），因此确定、可复现；`market.index` 元数据暴露 `excluded_bar_count`，导出清单同样携带该计数。**只适用于指数**：个股序列遇 OHLC 违例仍然 fail-closed。

---

## 7. 研究快照导出

### 7.1 bin 命令与参数

新增二进制 `export_research`（`src/bin/export_research.rs`）：

```
export_research --db <sqlite-path> [--out <dir>] [--dataset all|daily|index|calendar|factors]
                [--adjustment both|raw|qfq] [--start YYYY-MM-DD] [--end YYYY-MM-DD]
                [--entities sh.600000,sz.000001 | --catalog-version <id>]
                [--compression snappy|none] [--row-group-rows N] [--dry-run] [--quiet]
```

| 参数 | 语义 | 默认 |
|---|---|---|
| `--db` | 库路径，**强制 `mode=ro` + `query_only=1`** | 无（必填） |
| `--out` | 产物根目录 | `/root/x-data-exports`（owner 裁决） |
| `--dataset` | 导哪几类；`all` = daily(raw+qfq) + index + calendar + factors | `all` |
| `--adjustment` | 日线口径；`both` 出两个文件 | `both` |
| `--start`/`--end` | 闭区间过滤 `trade_day`（对 factors 用 `effective_day`） | 全历史 |
| `--entities` / `--catalog-version` | 二选一；不指定则取 `catalog_head` 的当前版本全体成员 | `catalog_head` |
| `--compression` | Parquet 压缩 | `snappy` |
| `--row-group-rows` | 行组大小（内存与列统计的平衡） | 65536 |
| `--dry-run` | 只算 `snapshot_id` 与清单，不写文件 | 关 |

失败语义：任一 AC 不满足（版本漂移、血缘无法证明、目录版本不存在、范围为空）→ 非零退出码 + 一行结构化原因，**不写半成品**。跨来源代**不是**失败条件（见 §7.3 的 M1 修复 B）。

`--entities`/`--catalog-version` 只约束个股日线与因子集；指数与日历由登记表（`index_registry` + `trading_days`）决定，不受 `--entities` 影响——否则"导出 100 只股票"会连带丢掉基准。

### 7.2 文件布局

```
<out>/<snapshot_id>/
  manifest.json          # 最后写；没有它 = 这个目录不是有效快照
  daily_raw.parquet
  daily_qfq.parquet
  factors.parquet
  calendar.parquet
  index_daily.parquet
```

- 先写 `<out>/.tmp-<pid>/`，全部成功后再 `rename` 成 `<out>/<snapshot_id>/`（同一文件系统，原子）。
- 已存在同名目录 → 拒绝覆盖（同一 `snapshot_id` 意味着同内容；若内容不同则说明摘要算法有 bug，必须暴露而不是覆盖）。
- 目录里不放 README：说明都在 `manifest.json`。

### 7.3 清单与绑定

```json
{
  "schema": "x-data-mcp.research-snapshot.v1",
  "snapshot_id": "snapshot-<32 hex>",
  "request": {"dataset":"all","adjustment":"both","start":"1990-12-19","end":"2026-10-23","entities":"catalog"},
  "catalog": {"version_id":"catalog-1791873196258239513","member_count":5228},
  "generations": [
    {"generation_id":"<hex>","manifest_sha256":"<hex>"},
    {"generation_id":"<hex>","manifest_sha256":"<hex>"}
  ],
  "algorithms": {"series_digest":"series-fold-v2","qfq":"qfq-exact-v1",
                 "factor_normalize":"gbbq-cat1-2dp-v1","calendar":"cal-union-v1"},
  "series": [
    {"entity":"sh.600000","period":"day","adjustment":"raw","instrument_class":"stock",
     "price_scale":2,"version_id":"<hex>","record_count":8000,"first_day":"1999-11-10",
     "last_day":"2026-10-23","source_binding_digest":"<hex>",
     "source_generation":{"generation_id":"<hex>","manifest_sha256":"<hex>"},
     "coverage":"complete","excluded_bar_count":0}
  ],
  "derived": [
    {"dataset":"trading_calendar","version_id":"<hex>","record_count":8793,
     "first_day":"1990-12-19","last_day":"2026-10-23","algorithm_version":"cal-union-v1",
     "source_generation":{"generation_id":"<hex>","manifest_sha256":"<hex>"}},
    {"dataset":"adjustment_events","entity":"sh.600000","version_id":"<hex>","record_count":18,
     "source_generation":{"generation_id":"<hex>","manifest_sha256":"<hex>"}}
  ],
  "files": [
    {"path":"daily_raw.parquet","rows":16749060,"bytes":123456789,"sha256":"<hex>",
     "compression":"snappy","columns":["entity","trade_day","open",...]}
  ],
  "limitations": [
    "qfq 以快照最后事件为基准，含未来生效事件；时点复权请用 raw + factors 自算",
    "adjustment_events.known_at 未知（源无公告日）",
    "trading_calendar 为推导数据，不能用于停牌判定",
    "amount 源自 f32，非精确十进制"
  ]
}
```

- **`snapshot_id` 的定义**：`snapshot-` + `sha256(canonical_json({schema, request, catalog, generations, algorithms, series, derived}))` 的前 32 个 hex 字符。规范 JSON 用已有的 `serde_json_canonicalizer`（`Cargo.toml` 里已在）。**排除 `files` 与任何运行信息**，所以同一输入两次导出得到同一个 id（AC-M1-7），而文件字节数/压缩参数不影响身份。
- **四层绑定**（BR-M1-7）：目录版本（`catalog_head`）、每条序列的 `version_id` 与摘要格式、每条序列/派生数据集的来源 generation（从 `source_bindings` 反查，逐条写入 `source_generation`）、每个文件的 sha256。
- **跨代导出（M1 修复 B）**：一条日更可能跨过一次 generation 切换，留下“部分实体在旧代、部分在新代”的库。此时**允许**一次全量导出：`generations` 列出全部来源代，`series`/`derived` 的每一项各自带 `source_generation` 与 `version_id`，所以混合状态是显式、可追溯的，而不是被拒绝。只有某一行的 `source_binding_digest` 在 `source_bindings` 里查不到（血缘无法证明）才失败（`EXPORT_LINEAGE`）。设计原意是“不能产出无法追溯来源的半新半旧快照”，逐行记录来源代满足这一点，同时不再让生产每天跨代时全量导出不可用。
- **可复现**：`manifest.json` 里不写 `created_at`/`started_at`/`finished_at`/WAL 增量等运行信息；它们只出现在 CLI 的一行摘要（`ExportOutcome`）里。因此同一库、同一输出选项的两次导出，`snapshot_id`、`manifest.json` 和全部 Parquet 文件的 sha256 都逐字节相同（AC-M1-7）。
- 清单里 `coverage` 只放 `complete/incomplete/unknown` 三值，理由串留在库里（`market_series.coverage_status` 的 reason 在证据包中），避免清单变成第二个事实源。
- 一致性：导出在**单个只读事务**里完成（SQLite 给一致视图），因此 `series` 与 bar 内容天然对齐；`--dry-run` 也走同一事务。长事务期间 WAL 会增长（日更仍在写），CLI 摘要报告起止时间与 WAL 增量（不写进快照，不阻塞）。

### 7.4 Parquet 列结构

统一约定：价格列 `DECIMAL(18,6)`（2dp/3dp 值在 6dp 下精确可表示，不需要按序列换类型）；`trade_day` 用 `DATE`（`date32`）；`price_scale` 逐行带，文件因此可自描述，M3 的 ETF（3dp）不需要改 schema。

**daily_raw.parquet / daily_qfq.parquet**

| 列 | 类型 | 说明 |
|---|---|---|
| `entity` | `STRING` | `sh.600000` |
| `trade_day` | `DATE` | 交易日 |
| `open`/`high`/`low`/`close` | `DECIMAL(18,6)` | 元；raw 文件实际只有 2 位有效小数 |
| `price_scale` | `INT8` | 2（raw）/ 6（qfq） |
| `volume` | `INT64` | 股（已按 #381 归一）；NULL = 单位无法确认 |
| `amount` | `DECIMAL(20,2)` | 元（源 f32） |
| `r7_raw` | `UINT32` | 原始第 8 字段，仅审计 |
| `volume_quantum` | `INT32` | 每手股数（个股恒 100）；NULL = 单位无法确认。D-34 owner 裁决（方案 1）：导出 schema **自带**该列，消费方不必猜"股/手"，M3 的 ETF（不同每手）也不需要改 schema |

**factors.parquet**

| 列 | 类型 | 说明 |
|---|---|---|
| `entity` | `STRING` | |
| `ordinal` | `INT32` | gbbq 解码序 |
| `effective_day` | `DATE` | 除权日（生效时间） |
| `known_at` | `DATE`（全 NULL） | 知晓时间未知，显式留列 |
| `category` | `INT8` | 恒 1 |
| `c1`/`c2`/`c3`/`c4` | `DECIMAL(18,2)` | 规范化后的冻结契约输入 |
| `factor_a`/`factor_b` | `DECIMAL(28,18)` | 单事件仿射（派生；消费者可只用 c1..c4 自己算） |

**calendar.parquet**

| 列 | 类型 | 说明 |
|---|---|---|
| `trade_day` | `DATE` | 行存在 = 交易日 |
| `sources` | `INT32` | 位掩码（1=sh.000001，2=sz.399001） |

**index_daily.parquet**

| 列 | 类型 | 说明 |
|---|---|---|
| `entity` | `STRING` | 指数代码（`sh.000300`） |
| `aliases` | `STRING` | 逗号分隔（`sz.399300`） |
| `trade_day` | `DATE` | |
| `open`/`high`/`low`/`close` | `DECIMAL(18,6)` | **点**（`price_scale=2`） |
| `price_scale` | `INT8` | 2 |
| `volume` | `INT64` | **手**（与个股差 100 倍） |
| `amount` | `DECIMAL(20,2)` | 元 |
| `aggregate_scope` | `STRING` | `constituents`/`whole_market`/`whole_board` |
| `member_count` | `INT32` | 成分数（非成分口径为 NULL） |

不导出 `market_events`（审计链在库里；导出是研究输入，不是审计包）。

### 7.5 Python 侧（已安装并冒烟：R-9 批准后实测）

经 R-9 批准后执行 `pip install`（对外部网络的写操作）。设计里的 venv 名
`x-data-research` 落地时缩短为 **`/root/.venvs/x-research`**（磁盘，非 `/tmp`）：

```bash
python3 -m venv /root/.venvs/x-research
/root/.venvs/x-research/bin/pip install 'pyarrow==26.0.0' 'duckdb==2.0.0'
```

实测版本与 wheel 体积（`pip install` 下载的 wheel，非解包后）：

| 包 | 版本 | wheel | 说明 |
|---|---|---|---|
| `pyarrow` | 26.0.0 | 54.0 MB | 纯 wheel，无编译 |
| `duckdb` | 2.0.0 | 24.4 MB | 纯 wheel，无编译 |

冒烟脚本 **`scripts/export_smoke.py`**（本 PR 新增，CI 里跑）：

- `--self-check`：用 pyarrow 按 §7.4 契约自造 5 个文件再双读，**本机已通过**——证明这份契约本身能被第三方 reader 表达；
- `--snapshot-dir DIR`：读 `<DIR>/manifest.json`，逐文件校验 arrow 逻辑类型、Parquet 物理类型与行数，并用 duckdb 做 `count(*)`/`min(trade_day)`/`max(trade_day)` 流式聚合。

本机**未**跑 `--snapshot-dir`：真实导出要 Rust 二进制，而本机禁止编译；该路径由 CI 的 `export` job 执行（`.github/workflows/ci.yml`：先用 `cargo test --features export smoke_export_for_external_readers` 落一份真实快照，再让本脚本读它），已在 PR #408 通过，结果见 §7.8。物理类型检查只在真实导出上严格（pyarrow 自写文件用最小宽度 `FLBA`，不是 §7.4 的 9/16 字节），该检查已用反证确认非空转：故意写 16 字节 `amount` 时脚本报 `fixed length 16 != 9`。

读大文件的约束不变：按列投影 + `ParquetFile.iter_batches()` 或 duckdb 流式聚合，全历史 1,675 万行不一次读入。

### 7.6 Rust 侧依赖选型（CI 编译与体积）

| 选项 | 依赖树 | 体积/编译 | 结论 |
|---|---|---|---|
| `parquet`（default-features=false, features=["snap"]），用低层 `SerializedFileWriter` | `parquet` + `snap` + `twox-hash`/`half`/`seq-macro` 等少量（`thrift` 编解码在 `parquet` 内部，不是独立依赖） | **实测**（CI `export` job，无缓存的一次 release 构建）：`target/release/export_research` = **5,030,424 B**（4.8MiB），构建 **110s** | **推荐** |
| `parquet` + `arrow` | 再加 `arrow-array/buffer/schema/select/cast/...` | 估计 +5–8MB、编译 +2–4min | 不采用：M1 只需要"按列写定长类型"，不需要 Arrow 计算生态 |
| 自己写 Parquet 编码 | 0 依赖 | 体积最小 | 不采用：thrift 编码 + 页格式 + 统计信息，自研成本远大于依赖成本 |

落地方式（控制默认构建成本）：

```toml
[features]
default = []
export = ["dep:parquet"]

[dependencies]
parquet = { version = "59.3.2", optional = true, default-features = false, features = ["snap"] }

[[bin]]
name = "export_research"
path = "src/bin/export_research.rs"
required-features = ["export"]
```

- 默认 `cargo build`/`cargo nextest` **不编译 parquet**，CI 的既有 Rust 关键路径不受影响；新增 CI job **`export`**（`.github/workflows/ci.yml`）专门跑 `cargo nextest run --features export`、`cargo test --features export smoke_export_for_external_readers`（落一份真实快照给 pyarrow/duckdb 读）与 `cargo build --features export --release`，并把二进制字节数与构建秒数写进 job summary。实测（PR #408 首次运行，无缓存）：**5,030,424 B / 110s**；`export_research` 是唯一带该 feature 的产物，所以这个字节数是"整只二进制"而不是 parquet 的边际体积——边际值需要一次 stub writer 的对照构建，没有为它单独跑一轮 CI。
- 实际依赖树（`Cargo.lock` 由 `cargo metadata` 生成）：+106/−3 行，新增包为 `parquet`、`snap`、`twox-hash`、`half`、`seq-macro`、`tiny-keccak`、`const-random`(+`-macro`)、`crunchy`、`libm`、`num-bigint 0.5.1`（仓库已用 0.4，两版并存，semver 不兼容是正常的）；**没有** `arrow-*`。默认构建不拉这些。
- **发布管线要改**：`scripts/build_release.sh` 需要带 `--features export`，否则发布包里没有导出命令。这是 owner 批准项（§10.3）。
- 压缩选 `snap`（纯 Rust，无 C 依赖）：`zstd` 体积更小但引入 C 代码与更大的二进制；M1 的导出体积不是门禁项（§7.7），先用 snappy。

### 7.7 体积（M1 修复 B 已实测每行字节数）

原估算是 **340MB**，依据是“CSV+gzip 同口径 347MB”和“6dp 不改变 i64 宽度，压缩率略差”。
M1 修复 B 用夹具实测后，这个估算低于物理下限，已修正：

| 范围 | 行数（调研报告① 实测基准） | 实测 B/行 | 外推 |
|---|---|---|---|
| 全历史不复权 | 16,749,060 | 32.3 | ~541MB |
| 全历史 6dp 前复权 | 16,749,060 | 55.9 | ~937MB |
| 因子事件 | 69,283 | — | <2MB |
| 日历 | 8,793 | — | <0.5MB |
| 指数 | ~40,000 | — | ~1MB |
| 合计 | — | — | **约 1.5GB**（私有栈按 24 个实体外推为 1.9GB，同一量级） |

实测方法：`scripts/export_smoke.py --measure`（pyarrow 26.0.0，100,000 行确定性夹具，snappy +
字典编码 + format 1.0 + 65,536 行/组，与 §7.6/§7.8 的实现设置一致）。Rust 侧的同一夹具由
`src/research/tests.rs` 的 `daily_parquet_bytes_per_row_match_the_measured_fixture` 锁住上界。

**为什么和 340MB 对不上**：340MB / 16,749,060 行 / 2 个文件 ≈ 10 B/行/文件，低于列宽下限。
前复权文件每行至少有 4 个 `INT64` 价格（32B）+ `INT64` volume（8B）+ 9B `amount` +
4B `DATE` + `price_scale`/`r7_raw`，即约 55B，且 6dp 的低位接近随机，snappy 压不动；
实测 55.9 B/行与这个下限一致。不复权的 2dp 值有结构，能压到 32.3 B/行，但仍远高于 10 B/行。
旧的 347MB CSV+gzip 数字不是“两个日线文件”的同口径实测，不能作为 Parquet 估算的依据。
压缩方式仍按 §7.6 选 snappy（纯 Rust、无 C 依赖）；换 zstd 会改设计取舍，不在本修复里做。

放 `/root/x-data-exports`（不在 `/opt` 部署树里），因此**不挤占 SLO 第 5 行的部署体积门禁**；但会占根文件系统（当前可用约 28GB，调研报告① §1.1 实测），每次全历史导出约 1.5GB，保留策略需要 owner 定（本次不定）。

### 7.8 PR-8 实现落点与偏差

代码位置（全部在现有 crate 内，不新建 crate）：

| 文件 | 职责 | 编译门槛 |
|---|---|---|
| `src/research/mod.rs` | 请求/选择、绑定类型（catalog/generation/index/序列/派生/文件计划）、`open_read_only`、`plan()`（= §11 的 `research::build_snapshot`）、`verify_unchanged()`、列与文件常量、`LIMITATIONS` | 恒编译 |
| `src/research/read.rs` | 只读连接（`SQLITE_OPEN_READ_ONLY` + `query_only=1` + schema 22 校验）、catalog/序列/因子集/日历读模型、逐行来源代解析 `resolve_lineage`、流式行迭代 `for_each_*`、十进制定标换算 | 恒编译 |
| `src/research/manifest.rs` | `algorithms()`、`identity()`（canonical JSON → `snapshot_id`）、`files_value()`、`document()` | 恒编译 |
| `src/research/parquet.rs` | 五份文件的 Parquet 编码（低层 `SerializedFileWriter`） | `export` |
| `src/research/export.rs` | `ExportOptions`、三步式 `SnapshotWriter`（规划 → 落临时目录 → 复验 + 写清单 + rename）、`export()`、`dry_run()` | `export` |
| `src/bin/export_research.rs` | 参数解析与一行 JSON 摘要 | `export` + `required-features` |
| `scripts/export_smoke.py` | 第三方 reader 冒烟（§7.5）与 `--measure` 体积夹具（§7.7） | — |

与设计文本的偏差（都是落地时发现的必要调整，逐条记录）：

1. **命名**：§11 分离点 5 写的是 `research::build_snapshot` / `SnapshotRequest` / `Snapshot`；实现用 `plan()` / `ExportRequest` / `SnapshotPlan`。语义一致（纯读、无副作用、入口只做参数解析），改名是为了让"这一步只做规划、真正写文件是 `export()`"在类型名上就看得出来。读模型对外仍是一处：入口与未来的 MCP 研究入口都调 `plan()`。
2. **`DECIMAL` 的物理类型**：§7.4 只规定逻辑类型。`INT64` 的 `DECIMAL` 精度上限是 18，所以 `DECIMAL(20,2)` 的 `amount` 写成 `FIXED_LEN_BYTE_ARRAY(9)`、`DECIMAL(28,18)` 的因子仿射写成 `FIXED_LEN_BYTE_ARRAY(16)`（Parquet 的十进制定长编码，大端补码）；`DECIMAL(18,6)` 的价格仍是 `INT64`。两种写法都已用 pyarrow 26.0.0 与 duckdb 2.0.0 实测读回。
3. **`volume_quantum`**：§7.4 原表没有这一列，按 D-34 方案 1 补上（`INT32`，NULL = 未知）。M1 生产写路径恒写 NULL，所以测试里用一条 SQL 单独置 100 来证明该列已接线——这也说明它是"留给 M2/M3 的位置"，不是当下的数据。
4. **可复现性（M1 修复 B 补强）**：`WriterProperties` 不设 `set_writer_version`（用默认的 1.0，兼容性最好），`set_created_by` 写固定串而不是 crate 版本，文件里不写时间戳；`manifest.json` 也不写 `created_at`/`started_at`/`finished_at`/WAL 增量（这些只在 CLI 摘要里），`identity` 排除 `files`/压缩参数/行组大小。因此同输入两次导出 `snapshot_id`、`manifest.json` 与每个 Parquet 文件的 sha256 都逐字节相同（AC-M1-7 的测试比较全部文件字节，不只是 id）。
5. **一致性（AC-M1-8）**：`SnapshotWriter` 分三步，`finalize()` 在**新连接**上重新 `plan()` 并与写文件时的计划逐字段比对（`verify_unchanged`），通过后才写 `manifest.json` 再 `rename`。因此"日更在导出期间提交"只会得到 `EXPORT_DRIFT` 且目录为空，不会留下半新半旧的快照；`--dry-run` 也在单一只读事务内规划。
6. **只读（AC-M1-9）**：连接用 `SQLITE_OPEN_READ_ONLY` + `PRAGMA query_only=1`；测试直接对读连接执行 `UPDATE` 断言失败，并断言导出前后主库文件字节完全相同。
7. **跨代导出（M1 修复 B）**：§7.3 原写“来源 generation 必须唯一，出现两个 → 混合导出，失败”。实现改为逐行解析 `source_bindings`：`series`/`derived` 每项写 `source_generation`，清单顶部写去重排序后的 `generations`；只有某一行的 binding 查不到时才 `EXPORT_LINEAGE`。这样跨代日更后仍能做全量导出，且每个数据集都能追溯到自己的来源代与 manifest sha256，不违背“不能有无法追溯的半新半旧快照”的设计意图。

验证状态（本机禁止 Rust 编译，按 §15）：本机只跑了 `cargo fmt --check`（通过）与 `scripts/export_smoke.py --self-check`（通过）；Rust 侧全部由 PR #408 的 CI 证明，三个 job（`rust`/`export`/`package`）在 run `40336874624` 这一轮全绿：

- `rust`：`clippy --all-targets --all-features -D warnings`、`nextest` 全量（930 个）、doc tests；
- `export`：`--features export` 的 938 个测试（含 19 个 `research` 测试）、一份真实快照被 pyarrow 26.0.0 / duckdb 2.0.0 读回（5 个文件的 §7.4 逻辑类型逐列相等，duckdb 聚合行数与 `min/max(trade_day)` 相符，`daily_raw` 的 3 行非 NULL `volume_quantum`），以及 release 构建；
- `package`：发布包结构未变（导出命令尚未进发布包，见 §10.3 第 3 条）。

---

## 8. 契约影响

### 8.1 结论

| 面 | M1 是否变化 | 说明 |
|---|---|---|
| 工具数量 | 否 | data 23 / acquire 10 不变 |
| dataset 枚举、wire 字段、错误码 | 否 | 不新增 dataset、不加字段 |
| `market.daily` 前复权价的**数值精度** | **是**（2dp → 最多 6dp） | 见 §8.2 |
| 所有序列的 `version_id` | **是** | 摘要格式 v2 + 全新重建（D-08） |
| 不复权 / 因子 / 日历 / 指数 | 否（MCP 不可见） | 只经导出；AC-M1-10 用测试固定这条边界 |
| `capability_revision` | 否（拟） | 见 §8.2 的理由；反方意见与替代方案见 §8.3 |

### 8.2 为什么"精度变化"不算契约升版

1. 公共契约没有承诺 2dp：`docs/contracts/generic-data-v7.md` 全文没有精度条款；`docs/sqlite-storage-design.md:132` 明确"不擅自固定两位或六位小数"；价格在 wire 上是十进制字符串。2dp 只出现在 `docs/source-closure.md` 的 #54 门禁描述里，那是**派生算法**的验收记录，不是 wire 契约。
2. wire 形状、错误码、幂等/取消/版本语义全部不变；使用方按"十进制字符串"解析的代码不需要改。
3. `version_id` 变化不是契约变化：D-08 的全新重建本来就会让全部 `version_id` 变；`stale_version` 语义照旧（旧 id 曾存在但已被取代 → 需要重新列版本）。
4. 因此 M1 只做**发布说明 + 文档当前状态区**的更新，不升 `capability_revision`；v8（后续任务审计）再一次处理"新增 dataset/工具"的升版。

### 8.3 做不到"完全无变化"的部分与替代方案

- **做不到的**：前复权序列从 2dp 变 6dp 是 owner 裁决的直接后果，无法"不暴露"——除非 M1 同时保留一条 2dp 前复权序列（+745MB，两条前复权序列互为冗余，且"哪条权威"无法回答），或者 MCP 读路径把 6dp 现算成 2dp（双重舍入：`round2(round6(x)) ≠ round2(x)`，在半分位 bar 上仍会与旧值不同，等于既付了派生成本又没有保证）。
- **若审核认为精度属于契约**：替代方案是 M1 直接把 capability 升到 `generic-data-v8`，后续任务在 v8 上继续加 dataset/工具（**一次升版，不是两次**）。这条不需要改本设计的数据结构，只改 §8.1 的一行与发布说明。Q3 已裁决：按"不升版"处理（不升 `capability_revision`，发布说明里交代，D-32）。
- **下游未核实**：`x` 仓库的 `KlineReader` 是否假设 2dp，本仓库无法核实（P-02 未决，另一个仓库）。部署前必须由 owner 确认，否则 6dp 可能在下游被截断或格式化错。这是**部署阻塞项**，不是设计缺陷。

### 8.4 需要同步改的文档（实现 PR 里做，本设计 PR 不改）

| 文档 | 改什么 |
|---|---|
| `docs/source-closure.md` | #54 门禁第 1 条"2dp HALF_UP 业务十进制输入"补一句：输出精度改为 6dp（`qfq-exact-v1`），2dp 参考价仍按显示值保留 |
| `docs/mcp-tools.md` | 当前状态区加"日线前复权价精度最多 6 位小数" |
| `docs/latest-only-storage.md` | 新增 §13：schema 22 的两条序列、精度、摘要 v2、事件分键 |
| `docs/day-file.md` | 补"不复权序列 = `.day` 原值 + #381 归一；前复权是派生" |
| `docs/design.md` | §13 列出的五处 |
| `docs/STATUS.md`、`docs/SLO.md` | 部署后的事实与体积口径（Q2） |
| `scripts/check_current_facts.py` | `EXPECTED_COMPACT_SCHEMA` 21→22，以及新增常量（若加） |

---

## 9. 日更：plan/run 如何同时维护五类数据

### 9.1 每个实体 item 的写集

现有 item 粒度是 `(entity, period)`。M1 保持这个粒度，一个日线 item 在一个写事务里写：

1. `adjustment=0` 不复权序列（bar UPSERT / 前缀追加 / 全量替换，按 §9.1 的判定沿用现状逻辑）；
2. `adjustment_events` 因子集（整组 delete+insert）；
3. `adjustment=1` 前复权序列（用同一批不复权 bar + 同一批事件现算 6dp）；
4. 两条 `market_events` 行（`adjustment=0` 与 `1`）；
5. 一条 `derived_dataset_versions` 行（`dataset_kind='adjustment_events'`）。

为什么必须同一事务：**未来生效事件**会只改前复权（原始价不动）。若两条序列分开提交，会出现"前复权重写了、原始没动"或反之的中间态，而这两者的组合正是使用者判断"发生了什么"的依据。同一事务让"原始 + 因子 + 前复权"永远自洽。

判定逻辑（沿用 `docs/latest-only-storage.md` §6）：

| 情形 | 不复权 | 前复权 |
|---|---|---|
| 无新 bar、无新事件 | `no_change` | `no_change` |
| 有新交易日 | `append` | `append` |
| 新事件（生效日 ≤ 最后 bar） | `no_change` | `replace`（全历史重写） |
| 新事件（生效日 > 最后 bar，未来事件） | `no_change` | `replace`（全历史重写） |
| 已存日期的价格被改写且无修订证据 | `RevisionUnverified`（fail-closed，现状不变） | 同 |
| 覆盖非 Complete / 单位漂移 / 事件摘要漂移 | 拒绝（现状不变） | 拒绝 |

### 9.2 日历与指数：同一 run 内的服务器内部 item

- 在同一个 `run_market_update` 里，逐实体 item 之后追加 **10 个内部 item**：`derived:calendar`（1 个）+ `derived:index:<entity>`（9 个）。
- 它们用现有 `task_items` 行（`item_id` 带 `derived:` 前缀、`target_json` 写 dataset），进度查询里 `entity`/`period` 为 null —— `ProgressItem` 的字段本来就是可选的（`src/data_supply/wire.rs:399-412`），**wire 形状不变**。
- 为什么不做成新 dataset / 新工具：那要在 M1 就升 capability（§8.3 说的"两次升版"）。为什么不做成导出命令顺带刷新：导出必须只读（BR-M1-11），让"导出"写库会破坏运维心智与 `mode=ro` 约定。
- 失败语义：内部 item 失败 → 该 item `success=false` + `reason_code`，run 整体 `partially_failed`；不静默跳过，也不让整个 run 因为日历失败而回滚已提交的行情。
- 顺序：日历必须在指数之后（日历输入就是指数 bar）；两者都在逐实体 item 之后（指数与日历只依赖 generation，不依赖个股 item 的结果）。

### 9.3 耗时与存储（估算，按"性能先放宽"）

| 项 | 现状 | M1 估算 | 依据 |
|---|---|---|---|
| 同代 no-change write-loop | 70.9s（schema 21，v6，5,224 items，SLO 第 1 行） | 85–100s | 每 item 多一次序列元数据核对 + 一次事件集核对（估算 +20–40%） |
| 跨代 write-loop | 97.4s（同上，第 2 行） | 120–145s | 同上 + 前复权全量重算的 CPU |
| 首次全市场采集 | 200.3s（第 4 行） | 260–320s | 精确有理数复权：16.7M bar × 4 字段 bigint 乘除，估 20–40s；另加不复权序列写入 |
| 年线重聚合 | 233.3s（第 3 行） | 不变（±5%） | 只换精度，不换算法 |
| 存储 | 库 1.19GB | **约 2.7GB** | +745MB 不复权 +745MB 前复权（i64 宽度不变）+7MB 因子 +0.2MB 日历 +1.8MB 指数 |
| 事件日志 | 10,448 行/日 | 15,672 行/日（+5,224） | 每实体多一条不复权事件；约 +1.8MB/日、+650MB/年（沿用 350B/行估算） |
| 启动检查 | 冷 11.4s / 热 0.36s（合成夹具，10,400 序列） | **冷约 20–23s**（序列数翻倍） | 探测是 O(#series) 索引 seek，常数不变 |

**风险（必须在上线前实测）**：冷启动预算。D-17/XP-043 已经记录"冷启动 11–13s 超过隧道硬编码 10s"，schema 22 把序列数从 5,224 涨到 10,457，冷启动可能到 20s+。缓解：启动探测仍是 O(series) 而非 O(events)；必要时把不复权序列的闭合探测降级到 `task_sqlite check` 深检（记录为偏差）。这条列为部署前实测项，不在设计里假装通过。

---

## 10. 迁移与部署

### 10.1 方式

- 走 D-08 既定做法：**归档旧库 + 全新 init 重建**，不做原地升级、不做数据迁移。
- `022_research_data.sql` 追加到 init 链尾（`src/sqlite_tasks.rs:14-42` 的 `include_str!` 列表 + 顺序断言）。因为 `market_series`/`series_bars`/`market_events`/`evidence_bundles` 都有 CHECK/唯一键变化，022 里对这几张表用"建新表 + 改名"的 12 步重建写法（SQLite 不支持改 CHECK/UNIQUE），并且只在空表上执行（全新 init 与 18→compact 升级路径的空表分支）。
- `scripts/check_current_facts.py:13` 的 `EXPECTED_COMPACT_SCHEMA` 21→22，并同步 README/STATUS 的 schema 表述（`check_current_facts` 会盯着这三处，改漏了 CI 会红）。
- 旧二进制遇 schema 22 必须明确拒绝（现状"unsupported schema"路径，已有测试）。

### 10.2 回滚

| 场景 | 做法 |
|---|---|
| 上线后发现问题 | 回到上一版 release + 归档的 schema 21 库（D-08 标准做法）；导出产物是独立文件，不受影响 |
| 数据重建中途失败 | 全新 init 本身是原子的（库文件不存在则视为初始化失败），不存在"半个 schema 22 库" |
| 只想撤掉导出功能 | `export` 是可选 feature；发布包不带 `--features export` 即可，不影响库与日更 |

### 10.3 需要 owner 批准的步骤

1. 合并 / 打 tag / 发布 / 部署（D-10 的同类边界：实现授权不等于部署授权）。
2. 前复权精度变化对外说明（发布说明 + `docs/mcp-tools.md`），以及 Q3 的升版选择。
3. `scripts/build_release.sh` 增加 `--features export`（发布管线变更）。
4. SLO 第 5 行的体积口径/阈值（Q2）：**已裁决**，2026-10-26 定为 ≤8GiB（目录口径），见 [SLO](../../docs/SLO.md) §4 与 [DECISIONS](../decisions.md) D-21；本条不再需要单独批准。
5. `/root/.venvs` 的 `pip install`（对外部网络写操作）。
6. 下游 `x` 仓库 `KlineReader` 对 6dp 的确认（P-02）。

---

## 11. 分离点落位

| # | 分离点 | 本设计的设计点 | 边界靠什么守住 |
|---|---|---|---|
| 1 | 来源接入层 ↔ 领域层 | `.day` 与 gbbq 仍由 `official_market` 适配（同一 `MaterializeSession`）；指数的"成员清单"是 `src/index_registry.rs` 的一处登记；未来新来源（交易所公告等）各自一个 Adapter + #308 证据包 | 领域层只收"不复权 bar / cat1 事件 / 日历日期 / 指数口径"这些中立事实；领域代码里**不出现** `hsjday`、`gbbq`、`infoharbor` 之类的来源名；`index_series_meta` 存的是取证结论而不是来源文件名 |
| 2 | 原始证据 → 解析事实 → 派生 | 原始：generation 里的 zip 字节（不可变、已绑 sha256）；解析事实：不复权 bar、`adjustment_events`（含 f32 位模式）、日历输入日期；派生：前复权 6dp、因子仿射、日历并集、指数口径 | 每条派生都有 `algorithm_version`，并且**可从下层重算**：AC-M1-2 用同一实现的参考路径与 Python `Fraction` 参考实现双验；`adjustment_events` 的 `c_bits`→`c1..c4` 有单测；日历只依赖两个 `.day` 成员的日期 |
| 3 | 生效时间 ↔ 知晓时间 | `adjustment_events.effective_day`（有）与 `known_at`（NULL，源没有）；ST 的同类问题留给 M2/#308 | 任何按 T 的查询必须同时声明两个时间；M1 的结论是"因子只有生效日"，写进导出 `limitations`，**不假装无前视** |
| 4 | 存储模型 ↔ 研究读模型 | 存储：`market_series`/`series_bars`/`adjustment_events`/`trading_days`/`index_series_meta`。读模型：`src/research/`（`SnapshotRequest` → `Snapshot` → 清单 + 流式行迭代） | 研究接口不出现表名/SQL；导出与（未来的）MCP 研究入口共用同一个读模型；换存储实现不改导出 |
| 5 | 核心用例 ↔ 入口适配 | 核心：`research::build_snapshot`（纯读、无副作用）。入口：`export_research` bin（M1）；MCP v8 的研究入口（后续） | 入口只做参数解析与权限/路径；导出不复制查询逻辑；AC-M1-10 固定"MCP 看不到新数据集" |
| 6 | 交易规则 ↔ 数据 | M1 不引入任何涨跌停/最小变动常量；只把"规则要按生效日期管理"的位置留出来（M2 的规则表），指数口径用数据表而不是代码常量 | 代码里没有幅度常量可 grep 到；指数的 `volume=手`/`口径` 来自 `index_series_meta` 行，不是 `if code == ...` |
| 7 | 编排 ↔ 原子能力 | M1 的原子能力是"一次导出、一个固定范围"；日更仍是一次 `run_market_update`（含内部派生 item），不新增"一键回测" | 不新增任何编排型工具；导出拒绝隐式多步（不做"导出 + 刷新 + 修复"） |

防过度设计的自我约束（按复杂度预算）：

- 不新建 crate；新模块都在现有 crate 内（`src/research/`、`src/sqlite_market/derived.rs`）。
- 不为每张表建 trait：导出读模型直接吃 `rusqlite::Connection`（只读），只在真需要替换实现时再抽。
- 不造规则 DSL：M1 没有规则表；指数口径是 9 行数据。
- 不预拆空 crate、不预建空 trait、不预造通用 `Dataset` 抽象。
- 顺手消除循环依赖：`Adjustment` 扩展会碰到 `market_data`/`publish`/`storage` 三者的循环（调研报告③ §2.3）。实现时按"改到哪顺手拆哪"处理（把 `market_data` 里对 `publish::EffectiveVersion` 的依赖改为领域自有类型），**不单独立项重构**。

---

## 12. 扩展场景走查

| # | 场景 | 会改的层/分离点 | 新增 | 修改 | 能否只改适配层 + 一处登记 |
|---|---|---|---|---|---|
| 1 | 新增证券类型（ETF/指数/北交所/B 股） | 分离点 1、2、6 | 该类型的登记行（`instrument_class` 值 + `price_scale`）；ETF 还需要身份名单输入（M3） | `catalog` scope、`Adjustment`/单位证据、导出列（已预留 `price_scale` 逐行） | **指数：能**（`index_registry.rs` 加一行 + `index_series_meta` 一行）。**ETF：不能只改适配层**——需要新身份输入、`price_scale=3` 的读取路径验证、量额单位（份/手）与 gbbq 适用性；这就是 M3 单独立项的理由。**B 股：不能**（`price_currency ≠ CNY`，`series_bars` 需要币种字段，属结构改动） |
| 2 | 新增一个官方来源 | 分离点 1 | 新 Adapter + #308 证据包 + `source_bindings` 的新 `role` | 装配处一处登记；capability 文本（若对外可见） | **能**。领域层只认"已验证的来源角色"，不认来源名 |
| 3 | 新增字段或数据集（如财务） | 分离点 2、4 | 独立领域模型 + 类型化表 + migration + 导出文件（或 v8 的 dataset 分支） | `derived_dataset_versions` 的 `dataset_kind` 取值（若也是派生数据集）；读模型加一个投影 | **不能**：`docs/design.md` 已裁定新类别用独立模型；强行套 Bar 会违反"新类别不强行使用 Bar" |
| 4 | 新增频率（分钟线） | 分离点 2、4 | 新 `period` 值或独立表；时间戳列（`trade_day` 是日序号整数，分钟需要秒级） | `Period` 枚举、聚合/血缘、导出列 | **不能**：`series_bars` 的主键与 `trade_day` 语义都是"日"，分钟线必须新增表或改主键。这是"存储跟着数据走"的正当例外 |
| 5 | 交易规则变更（涨跌停幅度） | 分离点 6 | 规则表一行（生效区间 + 依据） | 无代码改动 | **能**（M2 落地后）。M1 只保证代码里没有幅度常量 |
| 6 | 新增使用方或入口（v8 之后再加导出格式） | 分离点 4、5 | 新入口适配（MCP 工具 / 另一个 bin）+ 新格式 writer（纯函数：`Snapshot` → 字节流） | 读模型不变；清单 `files[].path` 多一个条目 | **能**。格式 writer 不碰查询、版本、清单逻辑 |

达不到"只改适配层"的三个场景及原因：场景 3（新数据类别）、场景 4（新频率）是 `docs/design.md` 已裁定的独立模型；场景 1 的 ETF/B 股有真实的**结构**差异（精度、币种、身份来源），不是语义声明能覆盖的。不为达标强行抽象。

---

## 13. 需要修改 `docs/design.md` 的地方（单列，不在本 PR 改）

1. 「后续数据扩展要求」目前只有"同类新增来源/新增周期/新增类别"三类，**没有"派生数据"这一类**。补一段：派生数据（复权因子、前复权价、涨跌停价、T 日股票池、交易日历）必须可从下层重算、带 `algorithm_version`、**不进 `version_id`**、血缘单独记录。
2. 扩展契约没有覆盖"**按生效日期管理的规则数据**"（交易规则）。补一条：规则数据是独立数据集，带依据引用与生效区间，代码不写死幅度。
3. 没有覆盖"**时点证券主表**"（既不是行情也不是目录快照）。补一条：证券主表按生效区间记录，目录快照只是主表在某一时点的投影。
4. 「产品闭环与排除范围」里"不将保存历史版本等同于无前视偏差回测数据"之后补一句：**前复权序列本身含未来事件**，时点复权只能用不复权 + 因子。
5. （本设计新增建议）补一句"**价格精度是序列属性**，读取方必须用序列声明的精度解释整数价格"，否则 M3 的 ETF 会再踩一次"默认 2dp"。

---

## 14. 实现拆分（9 个可独立合并的 PR，PR-9 可选）

通用约束：本机不编译，Rust 正确性只由 PR 上的 CI 证明；每个 PR 都要跑 §15 的本机脚本门禁。所有 PR 共享一个前提：**schema 22 落地前，任何"两条序列"的写入都会被 CHECK 拒绝**，因此 PR-1 必须先行。

| PR | 范围 | AC | 测试计划（CI） | 依赖 |
|---|---|---|---|---|
| **PR-1** schema 22 + 存储口径化 | 022 迁移（§3.1–3.5、§3.10）；`market_series.adjustment/price_scale/instrument_class`；`series_bars` 价格列更名；`market_events.adjustment` + 唯一键/索引；`yearly_series_lineage` 基口径；`evidence_kind` 扩展；`CompactBar` 字段与 `cents()` 改为按 scale；存储层 3 处 `adjustment=1` SQL、1 处 INSERT 常量与 1 处无过滤全表遍历的参数化；启动检查按三键；`EXPECTED_COMPACT_SCHEMA` 22 | AC-M1-12；迁移在全新 init 与空表升级两条路径可建；旧二进制拒绝 schema 22 | 迁移/升级测试（现有 `schema_upgrade` 类）；序列读写往返（2/3/6 三种 scale）；`cents()` 对 3dp 的接受与对超 scale 的拒绝；启动检查对两条序列的闭合；事件唯一键冲突测试 | 无（第一个） |
| **PR-2** 不复权序列物化 | `official_market` 新增 `raw_batch()`；同一 item 写两条序列 + 两条事件；`Adjustment::Raw` 与 `"raw"` 字符串校验参数化（§2.2 的 9 处） | AC-M1-1；AC-M1-11（部分） | `.day` → 不复权 bar 逐根相等（含 #381 四个判定分支）；两序列同事务提交/回滚；未来事件只改前复权；`RevisionUnverified` 保持 | PR-1 |
| **PR-3** 精确有理数复权（6dp） | `qfq-exact-v1`：精确链 + 末次一次舍入；`forward_adjust_reference` 保留为 2dp 历史 oracle；新增 `forward_adjust_exact(bars, events, scale)` 与其参考实现；前复权序列 `price_scale=6`；`algorithm_version` 落库 | AC-M1-2；AC-M1-3 | 与参考实现逐位相等（夹具 + 现有冻结域 94 事件回归）；Python `Fraction` 参考夹具逐位相等；2dp 基线的差异逐条落在半分位（用 §16.1 的样本）；bigint 路径的边界（空事件、未来事件、同日多事件） | PR-1、PR-2 |
| **PR-4** 因子事件表 | `adjustment_events` + `derived_dataset_versions`；写入路径（整组替换）；摘要 `adjustment-events-v1`；`normalize(c_bits) == c*` 断言 | BR-M1-2；AC-M1-2 的一半 | 摘要稳定性（同输入同版本、换顺序换版本）；`known_at` 恒 NULL；非 cat1 不入库、excluded 计数进证据；导出读模型能读到 | PR-1、PR-2 |
| **PR-5** 指数序列 | `src/index_registry.rs`（9 条登记）+ `index_series_meta`；指数物化（`instrument_class=1`、scale 2、`adjustment=0`）；指数证据包（`evidence_kind='derived'`）；MCP 读路径过滤 `instrument_class=0` | AC-M1-6；AC-M1-10（指数部分） | 登记与库内行一致；单位/口径证据必填（缺证据拒绝）；`query_data(sh.000001)` 返回 absent；`r7_raw` 原样；`aliases` 往返 | PR-1 |
| **PR-6** 交易日历 | `trading_days` + `cal-union-v1`；覆盖率自检（bar 落在日历外的比例与清单）；`derived_dataset_versions` 行 | AC-M1-4；AC-M1-5 | 用 16 个 `.day` 夹具（含 1991 周六）验证并集；单指数缺 bar 不误判；阈值超限失败；日历 `last_day` = 输入最后 bar | PR-5（输入来自指数成员） |
| **PR-7** 日更集成 | 逐实体 item 写集扩展（§9.1）；`derived:calendar` + `derived:index:*` 内部 item（§9.2）；失败语义；耗时/冷启动实测记录 | AC-M1-11 | 全量 run 的 item 计数与结果；内部 item 失败 → `partially_failed`；进度查询形状不变（golden test）；冷启动探测计数 | PR-2、PR-4、PR-5、PR-6 |
| **PR-8** 导出 | `src/research/`（读模型 + 清单 + 流式行迭代）、`src/research/parquet.rs`（feature `export`）、`src/bin/export_research.rs`、`[[bin]] required-features`、CI 新 job（`--features export` 的测试 + release 构建 + 体积/编译时间记录）、venv 文档 | AC-M1-7；AC-M1-8；AC-M1-9；AC-M1-10 | 清单确定性（同输入同 `snapshot_id`）；文件 sha256 自洽；版本漂移拒绝；跨来源代允许且逐序列记录来源代，血缘无法证明时拒绝（M1 修复 B）；只读（`mode=ro`，写库则测试失败）；小夹具 Parquet 用 pyarrow/duckdb 读回（CI 里用 duckdb CLI 或 Python，需在 PR 里确认 runner 有） | PR-3、PR-4、PR-5、PR-6 |
| **PR-9**（可选，可与 PR-8 合并）文档与运维收口 | §8.4 的文档同步、`docs/tasks/` 实现记录、发布说明草稿、部署前实测清单（冷启动、write-loop、体积） | — | `check_current_facts` / `check_deploy_facts` / hygiene | 其余全部 |

拆分原则：PR-1 只做"结构 + 参数化"，不改任何数值行为，也不新增序列——PR-1 之后库里仍只有 `adjustment=1` 一条序列（价格仍是 2dp），因此它可以单独合并、单独回滚；PR-2 才写第二条序列；PR-3 才把前复权从 2dp 换成 6dp。也就是说，"变贵"（PR-2 的存储）与"变准"（PR-3 的算法）是两个可分别回滚的步骤。

---

## 15. 本机验证与 CI 验证方式

- 本机**禁止** `cargo build/test/check/clippy/nextest/run`；只允许 `cargo fmt`。
- 本机能跑的：`git diff --check`、`cargo fmt --check`、`python3 scripts/check_current_facts.py`、`python3 scripts/check_daily_sync_eval.py`、`python3 scripts/check_deploy_facts.py`、`python3 scripts/check_public_hygiene.py`，以及 `.github/workflows/ci.yml` 里的各 `python3 -B -m unittest discover -s scripts -p 'test_*.py'`（`test_runtime_peer.py` 需要 sudo 与编译产物，跳过并注明）。
- Rust 正确性只能由 PR 上的 GitHub Actions 证明；没有 CI 结果时一律写"未运行"。
- 每个实现 PR 的 CI 验证方式（写进 PR 正文）：
  1. `cargo fmt --check`、`cargo clippy --all-targets --all-features --locked -- -D warnings`；
  2. `cargo nextest run --locked --no-fail-fast`（现有全量）+ `cargo test --doc`；
  3. `cargo build --locked`；
  4. PR-8 额外：`cargo test --features export`、`cargo build --release --features export`，记录二进制大小与编译时间（体积预算的实测值）；
  5. 需要真实 generation 的 AC（AC-M1-1/2/4/7）不在 CI 里伪造：用固定的小夹具（若干 `.day` 成员 + 对应 gbbq 片段）覆盖逻辑，真实全量验收放到部署前的私有栈步骤，结果贴 PR/Issue。

---

## 16. 实测、未验证与待确认

### 16.1 本次新增实测（可复现）

脚本：`/root/.herdr-projects/reports/x-data-mcp/21/work/tie_check.py`、`tie_check2.py`（全部在 `systemd-run --scope -p MemoryMax=150M -p MemorySwapMax=0` 下运行，RSS < 100MB）。

- 样本：`sh.600580`，5,776 根 bar（2002-06-06 … 2026-10-23），25 条 cat1 事件，23,104 个价格字段。
- 结果：精确 2dp 半分位 1,951 个字段（8.44%）；"28 位近似 vs 精确"在 2dp 下不同 476 个（2.06%）；6dp 半分位 0 个；6dp 下两者 0 个不同。
- 输入：active generation `c184e9c4…` 的 `hsjday.zip` 成员 `sh\lday\sh600580.day` + 调研报告① 已解码的 `gbbq_cat1.tsv`。
- 这条实测只覆盖一只证券，**不能**外推成"全市场 2.06%"；它的作用是证明"差异来自半分位"这个机制存在且可枚举。

### 16.2 未验证（如实列出）

| # | 项 | 为什么未验证 | 取证方法 |
|---|---|---|---|
| U-1 | 全市场 6dp 复权的耗时与峰值内存 | 本机不能编译，无法测 Rust 实现 | PR-3 在 CI 与私有栈上测（16.7M bar 全量） |
| U-2 | Parquet 导出的真实体积与压缩率 | 未实现 | PR-8 用全历史导出实测 |
| U-3 | `parquet` crate 的编译时间与二进制增量 | 本机不能编译 | PR-8 的 CI job 记录 |
| U-4 | 冷启动在 schema 22 下的实际耗时 | 需要编译产物 + 真实库 | 部署前在私有栈用合成夹具复测（沿用 latest-only 文档 §8 的方法） |
| U-5 | 下游 `x` 仓库对 6dp 的兼容性 | 另一个仓库，P-02 未决 | owner 确认 |
| U-6 | 指数在"成分股型指数"上历史早期的 volume 是否也是手 | 官方接口只有近年（调研报告① U-7） | 抽样历史日期，用交易所年鉴/成分股加总反推 |
| U-7 | 中证系是否算"交易所指数" | 已裁决（D-31）：算作交易所体系指数，基准 9 条序列 | Q1（已关闭） |
| U-8 | `pyarrow`/`duckdb` 的可用版本与 wheel 体积 | 未安装（本任务明确不装） | 批准后 `pip install` 并记录版本 |
| U-9 | #381 的逐 bar 成交量精度（`volume_quantum`）是否落地 | 已裁决（D-34）：采用方案 1，导出 schema 增加 `volume_quantum` 列 | Q4（已关闭） |

### 16.3 待确认（与 Issue 的 Q1–Q5 对应）

见 #383 的「待确认问题」。本设计对每一项都给了默认方案，审核时只需确认或否决。

---

## 17. Also noticed（范围外，只记录）

1. `src/sqlite_market/series.rs:1183` 的启动检查遍历全部 `market_series` 行且不带口径过滤；schema 22 之后必须按三键，否则两条序列会互相误判。
2. `"qfq"` 字符串校验 9 处（§2.2）没有编译期保护；建议在 PR-2 顺手改成 `Adjustment` 的 `matches!`/方法，避免第三次踩。
3. `src/data_supply/query.rs:77-95` 不校验目录成员：只要库里有序列，`query_data` 就能读到任意合法实体。M1 靠 `instrument_class` 过滤把指数挡住，但"目录外实体可读"本身是个独立问题（与 #246 的收录范围相关）。
4. `series_bars` 缺 `trade_day` 单列索引（#373/#379 已测：2026 单年冷读 5.2–6.5s）。M1 的导出按 `series_key` 顺序读，不需要该索引；跨截面按日期的分析仍会慢，属于 性能耗时分解任务 的范围。
5. `docs/SLO.md` 第 5 行的"部署体积"口径已定（2026-10-26：目录口径 ≤8GiB，见 SLO §4）；M1 的 +1.5GB 会让目录口径从 4.2GiB 到约 5.7GiB，余量约 2.3GiB。本设计不自行改门禁，只提供估算。
6. `docs/latest-only-storage.md` 写于 schema 21 实现前，其中"占位"项（§12）仍有三处未定稿（年线 stale 出参、事件列最终 DDL、capability 文本）；schema 22 会让"事件列最终 DDL"这一项再动一次，建议在 PR-1 里一并定稿。
7. 调研报告① 的 99.53% 比对与本次的 2.06% 是两个不同的量（前者是两套近似实现的 bar 级差异，后者是精确 vs 近似的字段级差异），对外引用时不要混用。
