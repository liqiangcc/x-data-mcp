# generic-data-v6 契约（latest-only 存储）

本文是面向调用方的 `generic-data-v6` 目标契约：每实体每周期只存一条当前序列
（latest-only，compact schema 21）。依据为内部实现规格
`SPEC-LATEST-ONLY` §6（契约变更表）与 `SPEC-RULINGS-LATEST-ONLY`
（冲突以 RULINGS 为准）；两文件不在仓库内，相关裁决已随文转述。

**状态**：目标契约，未部署。当前部署仍为 `generic-data-v5` / compact
schema 20（见 [STATUS](../STATUS.md)）。代码面 S1（存储+读路径）与
S2/S2b（日线写路径）已在 `feat/latest-only-storage` 落地；capability
revision 仍为 `generic-data-v5`（`src/data_supply/mcp.rs:343`），S4
统一 bump。凡下文标注「实现已核」的论断附 文件:行号；未标者为契约目标。
部署日的文档/脚本切换清单见 [v6-switch-checklist](v6-switch-checklist.md)。

## 1. 适用面与不变量

- `capability_revision`：`generic-data-v5` → `generic-data-v6`。
- 单版本语义只作用于 `market.daily` 与 `market.yearly`：每个
  (entity, period, adjustment) 至多一行当前序列（`market_series`，
  `migrations/tasks/021_latest_only.sql:8-27`）。
- `securities.catalog` **不变**：仍是多版本目录
  （`catalog_versions` 表保留，`list_data_versions` 对 catalog
  仍枚举全部版本，`src/sqlite_supply.rs:241-243`）。
- acquire socket 10 个 acquisition tools 全部不变。
- 存储：compact schema 21（`src/sqlite_tasks.rs:38`
  `COMPACT_SCHEMA_VERSION = 21`）；旧二进制打开新库 fail-closed
  （`src/sqlite_tasks.rs:233` 附近 schema 门禁）。
- 审计：每个已提交 item 结果对应一行 `market_events`（kind ∈
  `first_publish|append|replace|no_change|metadata|yearly_rederive`，
  DDL `021_latest_only.sql:68-69`；`metadata` kind 为 RULINGS A5
  新增）。事件行与序列状态同事务提交。

## 2. version_id = hex(content_digest)

- `version_id = lower(hex(content_digest))`，64 字符小写 hex
  （RULINGS Q5：裸 hex，无前缀）。DDL 层绑定
  `CHECK(version_id = lower(hex(content_digest)))`
  （`021_latest_only.sql:26`）。
- `content_digest` 为**有序 bar 序列折叠**（`series-fold-v1`，
  SPEC §2.2）：只锚定 bar 值（交易日 + OHLC/volume/amount/r7_raw）。
  **不含 coverage、来源（source binding）、evidence 元数据**
  （RULINGS Q6 已接受，并作为契约写明）：coverage/units/来源元数据
  变更而 bar 不变时，`version_id` 不变。下游把 `version_id` 当
  「值锚」用，不把它的不变当作元数据不变的证据；元数据以
  `get_data_availability` / manifest 的 `coverage_status`、units、
  evidence 字段为准。
- `VersionRef.integrity_sha256` = 同一 hex 值（SPEC §2.4；实现
  `src/sqlite_supply.rs` `series_version_ref`/`market_ref`）。
- 幂等含义：同内容重提交 → 同 `version_id`；`update_data`
  `no_change` 后 `version_id` 不变；append/replace 后必变。
- 与 v5 区别：v5 的 `version_id` 为时序派生 `version-{nanos}-{seq}`
  且一个实体有多个历史版本；v6 下 `version_id` 纯内容派生且
  历史版本 id 在 `market_series` 中不再可查（事件日志仍可证其曾存在，
  见 §3 错误语义）。

## 3. 错误码：`stale_version` vs `version_not_found`

新增 wire 码 `stale_version`（`Error::Stale` → `mapped()`
`src/data_supply/mcp.rs:514`）。区分规则（实现
`src/sqlite_market/series.rs:283-292` `require_series`）：

| version_id 情形 | wire 码 | 实现 |
|---|---|---|
| 曾提交、后被后续事件取代（digest 在该实体事件链 `new_content_digest` 中，但非当前 series） | `stale_version` | `digest_seen` 命中 → `Error::Stale` |
| 从未提交过（digest_seen 未命中） | `version_not_found` | `Error::NotFound` → `mcp.rs:513` |
| 当前 series 行 | 正常解析 | — |

调用方语义（沿用 SURVEY §6 既定方向）：`stale_version` ⇒ pin 缓存
失效，重新 `list_data_versions` 取当前 head 后重试一次；不得把
stale pin 静默落到新内容上。

**`stale_version` 的适用范围（OWNER-RULING-PAR-1A R1，定稿）：**
`stale_version` **只用于 `query_data` 的显式版本读取**
（`explicit_version_map` 及显式 pin 的读取校验）。写路径
（`update_data`/`backfill_data`/`republish_data` item 的
`base_version_id`）受理期对「曾存在已被取代」的 base **保持**
`needs_verification`（`STALE_BASE:` 前缀，item 级失败，不使整次
调用报错）——批量调用的其余 item 不受影响。

写路径现状即契约（S2b，行号已核）：

| 阶段 | 情形 | wire/结果 |
|---|---|---|
| 受理（`fixed_facts` → `require_series`，`src/sqlite_market/compact_repo.rs:347-359`） | base 曾存在已被取代 | item 级 `needs_verification`（`STALE_BASE:` 前缀，`src/market_update/tasks.rs:277` → `task_error` `mcp.rs:831`） |
| 受理 | base 从未存在 | `invalid_arguments`（"explicit published daily base unavailable"，`tasks.rs:274-276`） |
| 受理 | base 实体/周期不符 | `invalid_arguments`（"fixed daily base identity mismatch"，`tasks.rs:124-131`） |
| 提交 CAS（`commit_series_tx` head 比对，`series_commit.rs:299-303`） | 受理后 head 被并发推进 | `Error::Stale` fail-closed，该 item 失败，不静默写新 head |

## 4. 逐工具变化表（data 面 21 个；`src/data_supply/registry.rs:173-307`）

| 工具 | 入参变化 | 出参变化 | 错误变化 |
|---|---|---|---|
| get_data_capabilities | 无 | `capability_revision=v6`；description 注明单版本语义（S4） | 无 |
| list_data_versions | 无 | market dataset 每实体至多 1 item；`version_id`=当前内容 id；`published_at`=最近事件提交时间（实现 `src/sqlite_supply.rs:235-265`）。catalog 不变 | 无 |
| query_data | `version.kind` 形状不变 | market 行带 `version_ref`（version_id/integrity_sha256 同 hex）；年线 stale 行级状态见 §6 TODO(S3) | `explicit_version_map` 过期 id → `stale_version`（曾存在）或 `version_not_found`（未存在）（实现 `src/sqlite_supply.rs:1227-1245` `verify_versions_meta_conn`）；批量内 invalid 年线实体按行级状态返回不连坐（RULINGS A1，行级形状 TODO(S3)） |
| get_data_availability | 无 | `version_ref`=当前行；年线实体增报 stale 分类（fresh/behind/invalid + `as_of_day`，形状 TODO(S3)） | 行级 `integrity_failure` 不连坐整批（RULINGS A1；S1 现状为整次 `Integrity` 错误，S3 修行级化，OWNER-ACCEPT-S1 C2） |
| get_yearly_base_years | 无 | 字段名 `base_daily_version_id` 保留，值=派生时日线 version_id；stale 标记形状 TODO(S3) | 当前实现：stale/伪造 base → 行级 `integrity_failure`，缺失 base → `base_unavailable`（`src/sqlite_supply.rs:104-135`） |
| update_data / backfill_data | `base_version_id` 语义=**当前** head 的 version_id（内容 hex）；`expected_head`/`expected_yearly_head` CAS 同义 | receipt `version_id`=新内容 id；`no_change` 不变 | 见 §3 写路径分档（R1 定稿：stale base → item 级 `needs_verification`） |
| republish_data | 保留（RULINGS Q1：#247 显式 republish 合同，非版本概念） | 语义改为「显式区间的**源数据刷新**」，落到统一的 append/replace/no_change/metadata 提交函数；S4 更新 description（现行 `REPUBLISH_DESCRIPTION` `registry.rs:130` 仍是 v19 措辞） | 同 update_data |
| verify_publication_result | 无 | 新语义见 §5（SPEC §6.6） | 目标任务/事件不存在 → `task_not_found`/`version_not_found`（沿用现有码 `mcp.rs:828-845`） |
| verify_stopped_writes / verify_data_gap / inspect_verification_task | 无 | 无 | 无 |
| start_collection / retry_task / cancel_task / list_tasks / get_task / get_task_progress / get_request / run_worker_once / compute_metrics | 无 | 无（进度与结果中的 `version_id` 一律为内容 hex） | 无 |

wire 类型 `VersionChoice`/`Query`/`VersionRef`/`ManifestEntry`/`Row`
形状全部保留（`src/data_supply/wire.rs` 相关定义不变）；`cursor`/
快照分页语义不变（SPEC §6.5：快照 token 天然隔离翻页间替换）。

## 5. `verify_publication_result` 新语义（SPEC §6.6，DESIGN-BRIEF §2.5）

- 对**仍是当前序列**的更新事件：验证 ①事件行存在
  ②`new_content_digest == 当前 series.content_digest`
  ③evidence 捆绑可重放。
- 对**已被后续更新覆盖**的历史任务：只验证「该 `result_id` 的事件
  存在 + 事件 digest 链闭合到当前 `series.content_digest`」；
  不能再验证那一刻的数据内容（单版本下旧 bar 集合不存在）。
  registry description 须在 S4 改写明（registry.rs:188-196 附近条目）。
- `market_events.result_digest` 继承旧 `market_update_results`/
  `publication_results` 的 result_digest 锚点角色（RULINGS Q12：
  canonical 形状在 S2 实现时给出并附测试）。
- 伪造/不存在的 `result_id` → `task_not_found`/`version_not_found`
  （沿用现有码，OWNER-RULING-PAR-1A R3）。
- 实现现状：schema-21 facts read 以事件行为结果锚点
  （`src/sqlite_market/verification.rs:171-243` `read_facts_series`），
  判定层按「事件存在 + receipt 绑定（task_id/revision/digest 形状）」
  检查（`src/publication_verification/decision.rs:80-140`）。
  R3 已裁决现判定层不够：S4 前半按 SPEC §6.6 落「当前事件全验
  vs 被覆盖只验存在+链闭合」两个分支并各补一个测试（
  OWNER-RULING-PAR-1A R3）。

## 6. TODO(S3)：年线 stale 出参形状（未定稿）

RULINGS A1/Q10：stale 出参形状在 S3 定稿，必须含 `behind|invalid`
与 `as_of_day`。判定逻辑已定（实现 `series.rs:653-700`
`YearlyState`）：

- **fresh**：记录 base 仍等于当前日线状态；
- **behind**：base 之后只有 `append`/`no_change`/`metadata` 事件——
  年线数据对其 `as_of_day` 仍正确，`query_data` 照常返回并带
  stale 标记；
- **invalid**：base 之后出现 `replace`（除权/修订）——该实体年线
  不得返回（fail-closed）；批量查询按行级状态返回不连坐
  （RULINGS A1）；
- **integrity**：`lineage.base_daily_digest` 不在该实体日线事件链 →
  integrity 错误（伪造/损坏）。

**S3 必须回答的问题**（定稿后由 Owner 通知补入本文档）：

1. `query_data`：behind/invalid 在 `version_manifest`/`ManifestEntry`
   里的字段名与位置（`status` 枚举新增值还是 `metadata` 子字段？）；
   单实体 invalid 查询的错误码（`stale_version` 还是 integrity 系）。
2. `get_data_availability`：item 级 stale 字段名（`stale:"behind"|
   "invalid"`? `as_of_day` 字段位置）、`reason` 枚举取值；
   invalid 与真正 `absent`/`unavailable` 的可区分表达。
3. `get_yearly_base_years`：stale 标记挂在哪个字段、reason 枚举
   是否新增。R2 已定方向：对 behind/invalid 的年线返回已记录
   lineage 并带分类，只有伪造 lineage 才是行级
   `integrity_failure`（现状一律 `integrity_failure`，S3 修）。
4. 行级 `reason` 枚举全集与 wire 码映射表。
5. metadata 事件后年线分类边界（OWNER-ACCEPT-S1 C3）：哪些
   metadata 字段（units/currency/adjustment 证据）影响年线聚合、
   命中时应判 invalid 而非 behind。

当前实现差距（S1/S2b，S3 修复）：query manifest 对 Invalid 年线
报 `Absent` 且无 reason（`src/sqlite_supply.rs:377-391`）；
availability 对 Invalid 年线整次返回 `Integrity` 错误
（`src/sqlite_supply.rs:1684-1691`）；均不满足 RULINGS A1 行级化
要求（OWNER-ACCEPT-S1 C2）。

## 7. 部署与切换

部署形态不变：归档旧库 + 全新 init + 全市场重建（无数据迁移；
SPEC §7）。新二进制只认 schema 21（RULINGS Q8）；回滚=旧二进制+
归档库（旧二进制对 user_version=21 报 unsupported schema，
fail-closed 符合既定模式）。部署日需要改的文档、脚本断言与 Skill
句子清单：[v6-switch-checklist](v6-switch-checklist.md)。
