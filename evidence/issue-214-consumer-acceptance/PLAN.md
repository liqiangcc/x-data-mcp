# Issue #214——消费侧读路径验收计划（freeze v1）

状态：**已冻结**（冻结即机械期望值起算；一次中断 → 回 PLAN 记录 → 重跑）

## 0. 范围（bounded）

| 项 | 值 |
|---|---|
| 数据集 | 已发布 `market.daily`（5219 版本 / 16,629,206 bars）+ `securities.catalog`（1 版本 / 5224 成员）；`market.yearly` 无已发布版本 |
| 验收对象 | **消费侧读路径**：`query_data` / `list_data_versions` / `get_data_availability` / `compute_metrics` / `list_tasks` / `get_task` / `get_task_progress` / `get_request` / `get_data_capabilities` |
| 服务 | `target/release/data_service` @ main `f77d7d5`（含 #213 `X_DATA_MCP_UNSAFE_NONLOCAL_FS` 开发豁免，如实记录） |
| DB | `/home/box/acceptance-run-issue4-update/db.sqlite`（update5219 验收库，9.0G，**只读消费，不拷贝**） |
| 传输 | unix socket `/home/box/preview-deploy/run-realdata/mcp.sock` + `target/release/mcp_connect`；服务与客户端均带 `X_DATA_MCP_UNSAFE_NONLOCAL_FS=1` |
| 模式 | 常驻服务进程 + 单 stdio 客户端顺序调用；无并发、无写操作 |
| 裁决类 | **验收裁决：断言代码 + 结果 + 证据固化为一个 PR** |

## 1. 基线事实（run 前机械重查，与冻结值核对）

| 表 | 冻结值 |
|---|---|
| `data_versions` | 5219 |
| `catalog_versions` | 1 |
| `daily_bars` | 16,629,206 |
| `effective_heads` | 5219 |
| `market_update_results` | 5219 |
| `tasks` | 109（succeeded 103 / partially_failed 4 / failed 2） |
| `yearly_bars` / `proven_gaps` | 0 / 0 |

冻结内容值（首采 D8 验收已固定，本计划 verbatim 复用）：

| symbol | version_id | bar_count | first bar | last bar |
|---|---|---|---|---|
| sz.000002 | `collection-version-e75dbaa86f6913776c0ad7847c9d6e3fd222f67e5f8865dd7a6082859123e5b0` | 8479 | 1991-01-29 (-8.83/-8.83/-8.83/-8.83) | 2026-09-14 (3.05/3.07/3.03/3.05) |
| sh.600006 | `collection-version-20312846826be3e15b3fbb413b9037d85a457bb9384f56c906fd7d9174f7dfbd` | 6467 | 1999-07-27 (1.16/1.17/0.92/0.94) | 2026-09-14 (5.19/5.39/5.19/5.36) |
| sh.600004 | `collection-version-69e9b01833330f412909066752f35c4eb93be816be4f7dbaa8d6bacff8adc171` | 5630 | 2003-04-28 (0.96/1.56/0.94/1.55) | 2026-09-14 (7.71/7.79/7.7/7.76) |

锚点 bar：sz.000002 @ 2000-01-10 → (-8.14/-8.06/-8.15/-8.06)。

治理 verbatim（三 blocker 原样，逐字断言）：
- `GBBQ_QFQ_NOT_VERIFIED (#54: float32 business decimal, event and QFQ semantics)`
- `FILE_ACCESS_USE_NOT_VERIFIED (long-running server acquisition/use boundaries)`
- `FILE_SET_CONSISTENCY_NOT_VERIFIED (independent .day/gbbq hashes, update times and compatibility evidence)`
- `status=NOT_PRODUCTION_READY`

## 2. 断言表（每条独立可复核）

| # | 断言 | 判定方式 |
|---|---|---|
| C1 | 服务绑定 | initialize 返回 `0.2.0-preview.12`；tools/list == 19 个工具冻结清单；`/gates` `production_ready=false` + 三 blocker verbatim |
| C2 | 能力发现 | `get_data_capabilities` 有界返回；`market.daily`/`securities.catalog`/`market.yearly` 在册；写操作 readiness=blocked 且 blocking_reasons 逐字含三 blocker |
| C3 | 全量分页完整性 | 三证券各全区间分页（page_size=1000）：记录数 == 冻结 bar_count；date 严格升序无重复；首/末 bar 四值命中冻结值；sz.000002 锚点 bar 命中；末页 `page_complete=true`、`next_cursor=null`；每条 record `version_ref.version_id` == 冻结 version_id |
| C3b | 目录分页 | `securities.catalog` 全量分页返回 5224 行，entity 唯一，含 name/industry 字段 |
| C4 | 版本绑定 | explicit version 绑定冻结 version_id 返回同数据；伪造 `version_id`/`integrity_sha256` → 显式错误、无 latest 回退；无数据实体（sh.601091 目录在册无 `.day`；sh.999999 不在册）→ 空 records + manifest absent，不报错不伪造 |
| C5 | cursor 纪律 | 伪造 cursor → 显式错误；跨工具/跨条件复用 cursor → 显式拒绝 |
| C6 | 版本列表 | `list_data_versions` sz.000002 → 恰 1 项，version_id == 冻结，`integrity_sha256` 为 64 位小写 hex，`published_at` 存在，`page_complete=true` |
| C7 | 可用性 | `get_data_availability` sz.000002 → `availability` 值非 complete-claimed；`coverage_evidence` 逐字含 gen-2 manifest sha `b8a0d7b9aec1a270549d1e53ba26c0ab04842a23c05d1d1f7e75dd44a9f444d3` 与 `NOT_PRODUCTION_READY`；`confirmed_gaps` 为空 |
| C8 | 计算拒绝路径 | `compute_metrics`（yearly 无已发布版本）：固定 version_ref + 伪造 digest → 显式拒绝（非 Computed）；同一请求两次结果一致；未知 metric / 缺字段 → schema 拒绝 |
| C9 | 任务内省 | `list_tasks` 全量分页合计 == 109 且状态分布 == {103,4,2}；`get_task`(`task-fe036513920bb442a6c4c0b43b3337a9`)→ succeeded；`get_task_progress` 同 task 返回分页快照；`get_request`(`update-052`)→ bound task + terminal 结果 |
| C10 | 只读不变量 | 全部消费调用前后：§1 全部表计数不变；`tasks`/`data_versions`/`daily_bars`/`market_update_results`/`proven_gaps` 无新增行 |
| C11 | 边界拒绝 | 未知字段 / `page_size` 越界(0 与 >1000) / 错误 `contract_version` / 未知 dataset → 全部显式错误 |
| C12 | 诚实字段 | query 响应含 `snapshot_id`/`query_time`/`limitations`；version_manifest 含 metadata（coverage_status/coverage_reason/units）；不声称未验证的完整性 |

**机械核对**：所有计数/verbatim/逐 bar 断言由脚本直接 SQL + API 实测，不接受采样推断；失败即 fail，不豁免。

## 3. 执行序列

| 步 | 动作 | 断言 |
|---|---|---|
| S0 | 环境指纹：git HEAD + `sha256sum` 二进制 + DB size + socket 路径 → `evidence.json` | 记录 |
| S1 | DB 只读机械基线（§1 全部计数）→ `baseline.json` | C10 前置 |
| S2 | initialize + tools/list + capabilities + `/gates` | C1, C2 |
| S3 | 三证券全量分页 + 目录全量分页 | C3, C3b |
| S4 | 版本绑定正负路径 + 空实体 | C4 |
| S5 | cursor 纪律 | C5 |
| S6 | list_data_versions + get_data_availability | C6, C7 |
| S7 | compute_metrics 拒绝路径 | C8 |
| S8 | 任务内省四工具 | C9 |
| S9 | 边界拒绝序列 | C11 |
| S10 | 诚实字段抽查（含于 S3/S6 响应内） | C12 |
| S11 | 基线复算 vs §1 → results.json | C10 |

## 4. 风险与边界

- **只读验收**：全部调用为读路径；写路径（start_collection/update_data/backfill_data）已有独立验收，不在本切片重测——但 C11 抽查一次 `start_collection` 的 schema 拒绝以覆盖"消费调用不产生写副作用"边界。
- **不构成生产就绪声明**：三 blocker 与 `NOT_PRODUCTION_READY` 原样保留；本验收只证明"已发布数据集上消费面按契约正确"。
- **fs opt-out 是开发豁免**：服务在 overlayfs 上运行依赖 #213 豁免；本验收结果不证明生产部署形态（需真实本地 fs），如实记录于证据。
- **yearly 空集**：compute_metrics 只验收固定绑定拒绝路径；yearly 发布后的正向计算验收是独立切片。
- **无性能断言**：记录事实耗时，不设阈值。
- **DB 不拷贝**：直接读验收库；任何意外写都会被 C10 捕获并判 fail——这是对"消费面无副作用"的直接验证。
