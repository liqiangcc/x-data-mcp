# Issue #219——yearly 派生 + compute_metrics 正向路径验收计划(freeze v1)

状态:**已冻结**(冻结即机械期望值起算;一次中断 → 回 PLAN 记录 → 重跑)

## 0. 范围(bounded)

| 项 | 值 |
|---|---|
| 数据集 | `market.yearly`(从已发布 `market.daily` 本地派生) |
| 工作集 | **6 只证券**,冻结于 §1(3 首采 pin + sz.000001 全字段判别股 + sz.000035 cat15 + sh.688318 官方文档样本) |
| 操作 | `update_data`,`dataset=market.yearly`,显式 items,`publication.mode=current_head`,`expected_yearly_head=null`(首次发布) |
| 数据源 | `X_DATA_MCP_OFFICIAL_MANIFEST` + `X_DATA_MCP_OFFICIAL_DIR=/home/box/acceptance-run-issue3/snapshot`(同 update5219 机制;`OfficialYearlyVerifier` 要求 env 已安装,聚合本身为纯本地重算) |
| 基座 | `acceptance-run-issue4-update/db.sqlite` 拷贝 → `/home/box/acceptance-run-yearly/db.sqlite`(**不改已封存工件**) |
| 二进制 | `target/release/mcp_data`(release;动机=验收窗口,不构成性能声明) |
| 模式 | MCP 常驻进程 + `run_worker_once` 排空循环 |
| 切片 | 1 个 `update_data` 请求,6 items |
| 裁决类 | **验收裁决:代码 + 数据 + 证据固化为一个 PR** |

## 1. 冻结工作集与机械期望

| symbol | base_daily_version_id | first_date | 年数 |
|---|---|---|---|
| sz.000002 | `collection-version-e75dbaa86f6913776c0ad7847c9d6e3fd222f67e5f8865dd7a6082859123e5b0` | 1991-01-29 | 36(1991–2026) |
| sh.600006 | `collection-version-20312846826be3e15b3fbb413b9037d85a457bb9384f56c906fd7d9174f7dfbd` | 1999-07-27 | 28(1999–2026) |
| sh.600004 | `collection-version-69e9b01833330f412909066752f35c4eb93be816be4f7dbaa8d6bacff8adc171` | 2003-04-28 | 24(2003–2026) |
| sz.000001 | `collection-version-eac2c3f1543d0213af7ef4388773269f148b09610abaa4d17b015eb4418ed5a1` | 1991-04-03 | 36(1991–2026) |
| sz.000035 | `collection-version-31370cfe4b048be5d054af8e6ae1592c1c0db954c97bc42dd2ef07d8c4624d65` | 1994-04-08 | 32(1994–2026 **不含 2012**) |
| sh.688318 | `collection-version-19111da225f71cfb898afdfa724bd47cd269df94c0a07d35dcfdc49527f1e54c` | 2020-04-27 | 7(2020–2026) |

**冻结事实**:sz.000035 已发布 daily 在 2012 全年无 bar(2011-11-08 → 2013-09-17,~22 个月停牌缺口)——如实数据缺失,不是已证明 gap。其 item 只请求 32 个实际存在年;请求 2012 会按契约 `MissingYear` 显式失败,故不请求。该缺失转而成为 compute `missing_year` 边界腿的天然输入。

机械期望:`yearly_bars` 新增 == **163**;yearly 版本新增 == **6**;2026 为上海历当前年(执行日 2026-09-18,晚于末 bar 2026-09-14)→ `year_unfinished=true`。

**基线计数**(run 前机械重查):`yearly_bars=0`,`yearly_lineage=0`,`data_versions=5219`,`daily_bars=16629206`,`effective_heads=5219`,`tasks=109`,`market_update_results=5219`。

## 2. 断言表(每条独立可复核)

| # | 断言 | 判定方式 |
|---|---|---|
| Y1 | 输入完整性 | gen-2 快照 manifest sha == `b8a0d7b9…444d3`;env 指纹入档 |
| Y2 | 受理 | 6 items 全部 accepted;frozen items 绑 base version + years + current_head/null |
| Y3 | 派生执行 | 6 items 全部 success;yearly 版本新增 == 6;`yearly_bars` 增量 == 163 |
| Y4 | 逐 bar 正确性 | 每条 yearly bar == 对 `daily_bars` 的**独立 SQL 重聚合**(首 bar open/末 bar close/max high/min low/首末交易日;volume/amount 为 null 因基座全 null),全量比对不采样 |
| Y5 | lineage | 每记录绑 `base_daily_version_id` == item base、`algorithm_version=local-natural-year-v1`、首末交易日 == 重算值;`year_unfinished` == (year==2026) |
| Y6 | 消费面 | `query_data` yearly filter 返回逐年记录(date=年最后交易日,含 lineage/version_ref);`list_data_versions`/`get_data_availability` yearly 正常返回且 honest |
| Y7 | compute 正向 | sz.000002 `consecutive_direction` range 2019–2025 direction=down min_periods=3 → `Computed`;冻结期望:`observed_direction=down`、`run_length=6`、`run_length_kind=lower_bound`、`matches=true`、anchor=2019(28.26)、first_change=2020(25.79)、end=2025(4.65)、`observations_used=7`;同请求两次逐字节一致(除 envelope id) |
| Y8 | compute 边界 | (a) sz.000002 range 2005–2008 → `NotComputable`(`non_positive_price`,年收 ≤0);(b) sz.000035 range 2010–2014 → `NotComputable`(`missing_year`,2011→2013 断档);(c) sz.000002 range 2019–2026 `completed_only=true` → `excluded_tail=[2026]` 后同 Y7 结果;(d) 同区间 `completed_only=false` → `provisional=true` 且 2026 参与计算 |
| Y9 | provenance | yearly 版本 compat/evidence 绑 base version sha + derive as_of + `NOT_PRODUCTION_READY` verbatim |
| Y10 | env-absent | 同拷贝库、无官方 env:yearly `update_data` 受理后 worker verbatim 拒执 `official-file source not installed` |
| Y11 | 只读不变量 | daily 侧计数不动(`daily_bars`/`effective_heads`/`market_update_results`);`data_versions` 增量 == 6;tasks +1;三 blocker + `NOT_PRODUCTION_READY` verbatim |
| Y12 | 断点续跑 | (若中断)同请求重跑幂等:不重复发布 |

**机械核对**:Y4/Y5/Y7 由脚本 SQL + API 独立重算,不读实现逻辑的输出。

## 3. 执行序列

| 步 | 动作 | 判定 |
|---|---|---|
| S0 | 环境指纹 + DB 拷贝 + 基线机械计数 | Y1 |
| S1 | `official_source inspect`(env on) | Y1 |
| S2 | 单请求 `update_data` yearly 6 items → 受理证据 | Y2 |
| S3 | `run_worker_once` 排空 | Y3 |
| S4 | SQL 断言:版本数/bar 数/逐 bar 重聚合/lineage | Y3–Y5 |
| S5 | 消费面 query/versions/availability | Y6 |
| S6 | compute_metrics 正向 + 边界三组 | Y7, Y8 |
| S7 | provenance verbatim | Y9 |
| S8 | env-absent 腿 | Y10 |
| S9 | 前后计数对账 + `/gates` | Y11 |
| S10 | results.json | Y12(视情况) |

## 4. 风险与边界

- **派生语义如实**:基座 daily 的 `adjustment_evidence=Confirmed:"qfq"` 基于 `forward_adjust over category==1 GBBQ affine chain`,而 **#54 GBBQ/QFQ 语义本身未验**——本验收证明"年聚合管线对已发布数据按契约正确",**不构成"QFQ 业务正确"声明**;sz.000002 早期负年收是前复权链的如实结果,不修补。
- **`current_head` 首发**:expected_yearly_head=null;若同一 base+years 重发产生新版本属正常 head 演进(Y12 若触发按幂等语义记)。
- **负价与 compute**:`non_positive_price` 是契约内合法结果,不是数据错误——Y8a 验收的正是"消费端如实拒算而非伪造序列"。
- **缺年**:请求年不在 base 覆盖内 → `MissingYear` 显式失败,不补造。
- **非目标**:yearly 跨代 refresh、QFQ 语义、性能阈值、多实体批量 compute。
- 三 blocker 与 `NOT_PRODUCTION_READY` 原样;本切片不退休任何 blocker。
