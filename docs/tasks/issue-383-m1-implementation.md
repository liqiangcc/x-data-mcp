# M1 研究数据底座：实现记录（PR-1…PR-8）

关联 Issue：[#383](https://github.com/liqiangcc/x-data-mcp/issues/383)。
唯一依据是 [M1 设计](issue-383-m1-research-data.md)（§14 的 PR 拆分）；本文记录**代码交付了什么、
怎么验证的、与设计哪里不一致、还有什么没验证**。

**状态（2026-10-26）**：代码已合并到 main、CI 绿、**未部署**。生产实例仍是 schema 21
（[STATUS](../STATUS.md)）。真实 generation 上的验收（AC-M1-1/2/4/7）与冷启动/write-loop/体积
实测在 [M1 部署清单](issue-383-m1-deploy-checklist.md) 里，本文不把未运行写成 PASS。

## 1. 交付总览

| PR | 合并提交 | 范围（设计 §14 那一行） | AC | CI（run 全部 success） |
|---|---|---|---|---|
| PR-1 | `6d01163`（#389） | schema 22 结构 + 存储口径化 | AC-M1-12 | [40272199857](https://github.com/liqiangcc/x-data-mcp/actions/runs/40272199857) |
| PR-2 | `8cf6e74`（#390） | 不复权序列物化、一 item 双序列 | AC-M1-1（部分）、AC-M1-11（部分） | [40283518591](https://github.com/liqiangcc/x-data-mcp/actions/runs/40283518591) |
| PR-3 | `7c7040b`（#394） | 精确有理数前复权 `qfq-exact-v1`、6dp | AC-M1-2、AC-M1-3 | [40289495006](https://github.com/liqiangcc/x-data-mcp/actions/runs/40289495006) |
| PR-4 | `c276ddd`（#397） | 因子事件表 + 派生数据集版本登记 | BR-M1-2、AC-M1-2（一半） | [40295406026](https://github.com/liqiangcc/x-data-mcp/actions/runs/40295406026) |
| PR-5 | `ec2f932`（#400） | 指数基准 9 条 + 逐指数口径 | AC-M1-6、AC-M1-10（指数部分） | [40308782659](https://github.com/liqiangcc/x-data-mcp/actions/runs/40308782659) |
| PR-6 | `8f4d0ea`（#402） | 交易日历 `cal-union-v1` + 覆盖率自检 | AC-M1-4、AC-M1-5 | [40315674968](https://github.com/liqiangcc/x-data-mcp/actions/runs/40315674968) |
| PR-7 | `1b50379`（#406） | 日更集成 10 个内部派生 item | AC-M1-11 | [40322944055](https://github.com/liqiangcc/x-data-mcp/actions/runs/40322944055) |
| PR-8 | `d502f8d`（#408） | `src/research/` 读模型 + Parquet 导出 bin | AC-M1-7/8/9/10 | [40339245530](https://github.com/liqiangcc/x-data-mcp/actions/runs/40339245530)（`rust`/`export`/`package`） |

AC 的定义在 Issue #383 的验收表（AC-M1-1…12），本文按 PR 行引用。所有 PR 都跑过本机脚本门禁
（`check_current_facts` / `check_daily_sync_eval` / `check_deploy_facts` / `check_public_hygiene`
与 `scripts/test_*.py`），`test_runtime_peer.py` 需要 sudo 与编译产物，本机跳过；Rust 正确性
全部由 CI 证明（本机禁止编译，D-11）。

## 2. 各 PR 的落点与偏差

### PR-1：schema 22 结构（`6d01163`）

- 022 迁移只在全新 init / 空表升级路径执行，全部用"建新表 + 改名"改 CHECK/UNIQUE
  （`migrations/tasks/022_research_data.sql:1-14`）。`market_series` 加
  `adjustment`/`instrument_class`/`price_scale` 与 `UNIQUE(entity, period, adjustment)`
  （`022:31-61`）；`series_bars` 价格列 `*_cents`→`*_units` 并预留 `volume_quantum`
  （`022:74-84`）；`market_events` 加 `adjustment`、唯一键改三列（`022:130-158`）；
  `yearly_series_lineage` 加基口径（`022:101-105`）；`evidence_kind` 加 `derived`（`022:16-22`）。
- 摘要种子升到 `series-fold-v2`（`src/sqlite_market/series.rs:45`），`COMPACT_SCHEMA_VERSION=22`
  （`src/sqlite_tasks.rs:40`）；价格整数按序列 `price_scale` 解释（`src/compact_market.rs:17`），
  前复权常量 `PRICE_SCALE_QFQ=6`（`src/compact_market.rs:24`）。
- 存储层 `adjustment=1` 的 SQL 常量参数化；启动闭包探测按三键过滤
  （`src/sqlite_market/series.rs:1295`）。
- 偏差（PR 正文"设计里需要说明的地方"）：`bar_encoding` 字节格式不变，`price_scale` 由摘要种子
  绑定；`Adjustment::Raw` 与 `"raw"` 字符串校验留到 PR-2；`volume_quantum` 只建列、M1 恒写 NULL；
  `evidence_kind='derived'` 提前扩（022 只重建 `evidence_bundles` 一次）。
- 旧校验对账：`cents()` 的溢出/越界/2dp 恰好值三条校验全部保留，只把"分"换成"序列单位"
  （PR 正文的对账表）；`seed_digest` 域分隔 `x-data-series-v1`→`v2`。

### PR-2：不复权序列（`8cf6e74`）

- `official_market` 新增 `raw_batch()`，同一日线 item 在同一事务里写两条序列与两条事件；
  不复权随已验证的前复权重写一起重写。
- 偏差：不复权序列遇到源 `.day` 成员重写时"跟随前复权重写"（设计只说前复权保持
  `RevisionUnverified`）；两条序列日期范围必须一致，缺失即 fail-closed；raw 事件的重放锚点用
  `<forward_result_id>:raw`；MCP 任务面固定投影前复权。校验处数实际 11 处（设计 §2.2 写 8 处、
  §17 写 9 处）。
- 没有删除测试；32 个 `commit_range_ops*` 调用点与 6 个 `Commit` 字面量是机械补参数/补字段
  （PR 正文）。

### PR-3：精确有理数复权（`7c7040b`）

- 复权链改成精确有理数合成、末次一次舍入：`ExactAffine`/`exact_event_transform`/
  `exact_compose_chain`（`src/gbbq.rs:227,341,364`）；`forward_adjust_exact` 与逐 bar 参考实现
  （`src/qfq.rs:219,269`），`ALGORITHM_VERSION = "qfq-exact-v1"`（`src/qfq.rs:204`）；2dp 基线
  `forward_adjust` 保留为历史 oracle，不再写库（`src/qfq.rs:103,167`）。
- 验收：Python `Fraction` 夹具逐位相等（`tests/algorithms/qfq_exact.rs`；
  `evidence/issue-383-qfq-exact/`）；AC-M1-3 断言差异只落在 2dp 半分位字段
  （语料 2,992 个字段 = 3.63%，6dp 半分位 0 个）。
- 偏差：**`algorithm_version` 没有落成结构化列**——schema 22 没有
  `market_series.algorithm_version`，实现把它写进版本证据的 `adjustment_evidence.reason`
  （`src/official_market.rs:928-929`），随 `evidence_digest` 绑定；做成列需要 schema 23 或改 022。
  年线同样取 6dp（`SeriesShape::FORWARD_STOCK_UNITS`）；`to_bar` 不补尾随零（"最多 6 位"）。
- 旧校验对账：`FORWARD_STOCK_CENTS`→`FORWARD_STOCK_UNITS`（调用点 8 处全改）；`commit_plan` 的
  默认精度改为按口径取；7 处测试夹具从 2dp 编码改到序列精度；冻结客户端显示断言改为
  "6dp 值一次 2dp 舍入后等于显示值"（PR 正文的对账表，逐条给出理由）。

### PR-4：因子事件与派生登记（`c276ddd`）

- `adjustment_events`（`migrations/tasks/022_research_data.sql:204-224`）存 f32 位模式与派生
  2dp 值，`known_at` 恒 NULL（`022:222`）；`derived_dataset_versions`（`022:235-266`）登记
  因子集与日历；摘要 `adjustment-events-v1`；`normalize(c_bits) == c*` 有断言。
- 偏差：`digest_format` 与 `algorithm_version` 取值按设计语义分开（前者摘要算法名、后者派生标签）；
  给新表补 `CHECK(version_id = lower(hex(content_digest)))`；`effective_day` 沿用 epoch-day；
  `known_at` 用 `Option<NaiveDate>` 而不是新枚举；`dataset_kind` 保持封闭 CHECK（新增数据集按
  设计 §12 场景 3 走 migration）；空因子集删除行与登记行；no_change 也整组重写。
- 对账：`RawBatch::new`/`no_change` 多一个因子集参数，生产 9 处 + 测试 4 处调用点全量更新，
  **没有校验被删**（PR 正文）。

### PR-5：指数基准（`ec2f932`）

- 9 条交易所体系宽基登记在 `src/index_registry.rs:131`（含中证 4 条，D-31），量额口径与成分数
  逐指数登记，代码里没有 `if entity == ...`（`src/index_registry.rs:7,131`）；
  `index_series_meta`（`022:268-292`）存单位与口径，`evidence_note` 非空 CHECK；
  指数物化为 `instrument_class=1`、`price_scale=2`、`adjustment=0`；MCP 读路径按
  `instrument_class=0 AND adjustment=1` 解析（AC-M1-10）。
- 偏差：`index_series_meta` 没有 `source_binding_digest` 列，绑定摘要写进 `derived` 证据包；
  `evidence_note` 加非空 CHECK；指数 `price_currency` 记 `unknown`（指数是点位，不冒充 CNY）；
  `aliases` 用逗号分隔 + 往返测试；指数 volume 不套用 #381 的手日归一（指数本来就是手）。

### PR-6：交易日历（`8f4d0ea`）

- `trading_days`（`022:294-297`）一行 = 至少一个输入指数当日有 bar；`cal-union-v1`
  （`src/trading_calendar.rs:34`），摘要 `trading-calendar-v1`（`src/trading_calendar.rs:37`）；
  三值 `DayStatus`（在日历里 / 非交易日 / 超过 last_day 未知），没有 `is_trading_day` 入口；
  覆盖率阈值 1/10000 用整数比较，`total=0` 按失败处理。
- 偏差：覆盖率按**存储的不复权序列**统计（AC 字面），不是重读 generation 里的 `.day`；
  "last_day 之后未知"与"不在日历 ⇒ 非交易日"的冲突用三值 API 解决；输入成员 sha256 折叠成
  `calendar-inputs-v1` 写进 `source_bindings.day_member_sha256`；清单有上限（512 天 / 2048 条），
  截断只出现在失败运行。
- 本 PR 没有删除或重写旧代码、旧测试（PR 正文对账表逐文件列出"只加"）。

### PR-7：日更集成（`1b50379`）

- 一次日线 plan-run 在实体 item 之后追加 10 个服务器内部 item：`derived:index:<entity>` × 9 +
  `derived:calendar`（`src/task_control/derived.rs:29-33`；`src/data_supply/derived.rs:47`；
  `src/data_supply/worker.rs:240,287`）。它们复用 `task_items` 与进度面，`entity`/`period` 为
  null，wire 形状不变（设计 §9.2）。
- 失败语义：内部 item 失败只记自身结果，run 收尾 `partially_failed`；不确定结果转
  `needs_verification`，不回滚已提交行情。
- 偏差：内部 item 行必须在实体提交之前追加（否则撞终态）；`check_binding` 只对
  `derived:index:<entity>` 放行 dataset target；小任务用 `error` 文本而不是 `reason_code`；
  进度有界读阈值只看 plan eligible；按证券的任务投影（`frozen_daily_ranges`、
  `publication_versions`、`range_results`、`successful`↔`evidenced` 对账）跳过内部 item。
- CI：909/909 passed、5 skipped（PR-6 基线 892）；新增的日历阶段成本用例在合成夹具
  （120 序列 × 40 bar）上断言 coverage 扫描 < 30s。**全市场耗时仍需私有栈实测**（设计 §9.3）。

### PR-8：研究快照导出（`d502f8d`）

- `src/research/`：`plan()`（`src/research/mod.rs:510`）、只读连接
  （`src/research/read.rs:58-80`：`SQLITE_OPEN_READ_ONLY` + `query_only=1` + schema 22 校验）、
  清单（`src/research/manifest.rs`）、五份 Parquet（`src/research/parquet.rs`）、三步式
  `export()`/`dry_run()`（`src/research/export.rs:271,286`）、`LIMITATIONS`
  （`src/research/mod.rs:587`）；bin 只在 feature `export` 下编译（`Cargo.toml:6-17`）；
  CI 新 job `export`（`.github/workflows/ci.yml:102`）跑 `--features export` 的测试、release
  构建与 pyarrow/duckdb 读回。
- AC：AC-M1-7 断言到 `files` 逐项相等；AC-M1-8 的 `verify_unchanged` 在新连接上重新 `plan()`
  比对，漂移 → `EXPORT_DRIFT` 且不留残留；AC-M1-9 对读连接写必失败、导出前后主库字节相同；
  AC-M1-10 固定 MCP 看不到指数/raw。
- 偏差：命名 `plan()`/`ExportRequest`/`SnapshotPlan`（设计写 `build_snapshot`/`SnapshotRequest`/
  `Snapshot`）；`DECIMAL(20,2)`/`DECIMAL(28,18)` 用 `FIXED_LEN_BYTE_ARRAY`（INT64 精度上限 18）；
  `volume_quantum` 按 D-34 补列；`WriterProperties` 不写版本/时间戳，`identity` 排除
  `created_at`/`files`/压缩参数。
- 实测：`export_research` 小夹具快照 5,030,424 B、release 构建 110s；pyarrow 26.0.0 + duckdb
  2.0.0 读回 5 个文件（逻辑类型逐列等于设计 §7.4、行数与 `min/max(trade_day)` 相符）。
  **真实全量导出未跑**，parquet 边际体积未单独测。
- 设计文档 §7.8 引用的是更早一轮绿 run `40336874624`（head `43950e5`）；最终一轮是
  `40339245530`（head `8c8484a`），两者都 success。

## 3. 分离点落位（实现对照设计 §11）

| # | 分离点 | 实现落点 | 边界怎么守 |
|---|---|---|---|
| 1 | 来源接入 ↔ 领域 | `.day` 与 gbbq 只在 `official_market` 适配；指数清单是 `src/index_registry.rs:131` 的一处登记 | 领域类型只收不复权 bar / cat1 事件 / 日历日期 / 指数口径；领域代码里没有 `hsjday`/`gbbq`/`infoharbor` 字样（PR-4/5/6 正文逐条列出） |
| 2 | 原始证据 → 解析事实 → 派生 | 原始：generation zip（不可变）；解析事实：不复权 bar、`adjustment_events`（f32 位模式）、日历输入日期；派生：6dp 前复权、因子仿射、日历并集、指数口径 | 每条派生带算法版本，且可从下层重算：AC-M1-2 双验（同一实现参考路径 + Python `Fraction`）；`c_bits`→`c1..c4` 有断言 |
| 3 | 生效时间 ↔ 知晓时间 | `adjustment_events.effective_day` 有值，`known_at` 恒 NULL（`022:222`） | 导出 `LIMITATIONS` 明写"因子只有生效日"（`src/research/mod.rs:587`），不假装无前视 |
| 4 | 存储模型 ↔ 研究读模型 | 存储：`market_series`/`series_bars`/`adjustment_events`/`trading_days`/`index_series_meta`；读模型：`src/research/` | 研究接口不出现表名/SQL；导出只调 `plan()`；换存储不改导出 |
| 5 | 核心用例 ↔ 入口适配 | 核心：`research::plan()`（纯读）；入口：`export_research` bin | 入口只做参数解析与一行 JSON 摘要（`src/bin/export_research.rs`）；AC-M1-10 固定 MCP 边界 |
| 6 | 交易规则 ↔ 数据 | M1 没有涨跌停/最小变动常量；指数口径是 `index_series_meta` 的行 | 代码里没有幅度常量可 grep；指数口径不是 `if entity == ...` |
| 7 | 编排 ↔ 原子能力 | 原子能力 = 一次导出、一次 plan-run | 没有新增编排型工具；导出拒绝隐式多步 |

## 4. 未验证清单（设计 §16.2 的现状）

| # | 项 | 现状 |
|---|---|---|
| U-1 | 全市场 6dp 复权的耗时与峰值内存 | **未验证**。CI 只证明正确性与合成夹具；全量在私有栈测（部署清单 §3） |
| U-2 | Parquet 导出的真实体积与压缩率 | **部分**：小夹具 5,030,424 B；全历史未跑（部署清单 §4） |
| U-3 | `parquet` crate 的编译时间与二进制增量 | **已验证（CI）**：`export` job 记录 release 构建 110s；体积记录见该 job 的 job summary |
| U-4 | 冷启动在 schema 22 下的实际耗时 | **未验证**：设计估算冷 20–23s（序列数 5,224→10,457），部署前实测（部署清单 §2） |
| U-5 | 下游 `x` 仓库对 6dp 的兼容性 | **代码侧已核对**（另一仓库只读）：`KlineReader` 无 2dp 假设、无版本 pin；两个上游契约点部署前实测（部署清单 §5） |
| U-6 | 指数早期 volume 是否也是手 | **未验证**（M1 收录口径已按官方接口取证，历史早期未抽样） |
| U-7 | 中证系是否算交易所体系指数 | **已裁决**（D-31）：算，基准 9 条 |
| U-8 | `pyarrow`/`duckdb` 版本与 wheel 体积 | **已验证（CI）**：export job 在 runner 的 venv 里装并读回（`.github/workflows/ci.yml:140-144`） |
| U-9 | `volume_quantum` 是否落地 | **已落地**：导出列存在（`src/research/mod.rs:405`），DB 列预留（`022:84`），M1 恒 NULL |

## 5. 文档同步（本 PR 之前缺的）

设计 §8.4 列的文档在 PR-1…PR-8 里只同步了一部分（`source-closure`、`mcp-tools`、
`latest-only-storage` 的 §10、`day-file`、`STATUS` 的 schema 行）。本 PR（PR-9）补齐：

- `docs/design.md`：设计 §13 的五处（派生数据、规则数据、时点主表、前复权含未来事件、价格精度
  是序列属性）。
- `docs/latest-only-storage.md` §13：schema 22 的两条序列、精度、摘要 v2、事件分键。
- `docs/day-file.md`：不复权 = `.day` 原值 + #381 归一；前复权是派生。
- `docs/data-catalog.md`：M1 三项的状态改为「已提供（代码已合并，未部署）」、导出一节重写、
  使用授权限制、2dp/6dp 的部署/代码分开写。
- `docs/STATUS.md` 与 README/daily-sync/generic-data-tools/mcp-tools/source-closure 的
  「M1 PR-1…PR-6」更正为 PR-1…PR-8。
- `docs/decisions.md`：`P-02` 标为已撤销（x 侧不存在版本固定逻辑）。

## 6. Also noticed（范围外，只记录）

1. **`market_data` ↔ `publish` 循环依赖仍在**（XDM-1 §2.3）：`src/market_data.rs:484,499` 用
   `crate::publish::EffectiveVersion`，`src/publish.rs:9,41,231` 又用 `market_data` 的类型。
   M1 各 PR 都没动 `MarketDataPipeline` 的签名，本 PR 只改文档，未拆。
2. **`algorithm_version` 没有结构化列**（PR-3 偏差）：现在只在版本证据的 reason 字符串里，
   要查询得解析 JSON；建议在 schema 23 或下一次重建时补列。
3. **M2 设计对 `derived_dataset_versions.dataset_kind` 的开放集合要求**
   （`docs/tasks/issue-391-m2-security-master.md:975`）与 M1 的封闭 CHECK 是两条线的接口点，
   由 M2 决策，本 PR 不预造。
4. **v8 设计 §13 第 5 条**（`docs/tasks/issue-387-v8-ai-tool-surface.md:834-835`）建议把 M1 的
   `docs/design.md` 五处与 v8 自己的五处合并成一次更新；本 PR 已把 M1 的五处改完，v8 PR-6 只需
   做它自己的 1–4 条，不要重复加同一段。
5. **数据目录的 AC 编号与 Issue 的 AC 编号不是同一套**（`docs/data-catalog.md` §3.1 的
   AC-M1-1…5 是数据目录视角的验收建议，Issue #383 的 AC-M1-1…12 是实现验收）。本 PR 已在该节
   注明两套编号不同，但没有重编号。
6. **`STATUS.md` 的「PR-1…PR-6」范围已过时**（PR-8 的 closing note 也提过）：本 PR 已更正。
7. **SLO 第 4 行的「首采后 DB ≤2.5GiB」与 M1 的体积估算冲突**：设计 §9.3 估库约 2.7GB
   （≈2.51GiB），比门槛略高；SLO 第 5 行（部署体积）在 2026-10-26 已上调到 8GiB，第 4 行没动。
   部署前实测后由 owner 决定改门槛还是砍体积，本 PR 只记录，不改 SLO。
8. **设计 §9.3 的「序列数 5,224 → 10,457」只数了日线**：年线序列也参与启动闭包探测，
   schema 22 的 `market_series` 实际是 15,681 行（5,224 不复权 + 5,224 前复权 + 5,224 年线
   + 9 指数），冷启动估算应按 15,681 复核。生产现库是 10,448 行（5,224 + 5,224，只读
   `select period, count(*)` 核对，2026-10-26）。
