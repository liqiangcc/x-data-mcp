# generic-data-v6 契约（latest-only 存储）

本文是面向调用方的 `generic-data-v6` 目标契约：每实体每周期只存一条当前序列
（latest-only，compact schema 21）。依据为内部实现规格
`SPEC-LATEST-ONLY` §6（契约变更表）与 `SPEC-RULINGS-LATEST-ONLY`
（冲突以 RULINGS 为准）；两文件不在仓库内，相关裁决已随文转述。

**状态**：目标契约，未部署。当前部署仍为 `generic-data-v5` / compact
schema 20（见 [STATUS](../STATUS.md)）。代码面 S1（存储+读路径）、
S2/S2b（日线写路径）、S3（年线 stale 出参）与 S4（capability bump、
wire/description 收尾）已在 `feat/latest-only-storage` 落地：capability
revision 已为 `generic-data-v6`（`src/data_supply/mcp.rs:343`）。
凡下文标注「实现已核」的论断附 文件:行号；未标者为契约目标。
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
| query_data | `version.kind` 形状不变 | market 行带 `version_ref`（version_id/integrity_sha256 同 hex）；年线 stale 行级状态见 §6（已定稿） | `explicit_version_map` 过期 id → `stale_version`（曾存在）或 `version_not_found`（未存在）（实现 `src/sqlite_supply.rs:1227-1245` `verify_versions_meta_conn`）；批量内 invalid 年线实体按行级 `status:"absent"`+`reason` 返回不连坐（RULINGS A1，§6.1） |
| get_data_availability | 无 | `version_ref`=当前行；年线实体增报 stale 分类（`stale:"behind"`+`as_of_day`；invalid → `availability:"absent"`+`reason`，§6.2） | 行级 `integrity_failure` 不连坐整批（RULINGS A1，已行级化） |
| get_yearly_base_years | 无 | 入参是调用方 pin 住的日线 `base_daily_version_id`，出参是该日线基下的年份集合——**不返回年线 lineage**（OWNER-RULING-PAR-1A R2'，语义不变） | pin 的日线 id **已被取代**（曾存在、非当前 digest）→ 行级 `reason_code:"base_superseded"`（`YearPresenceReason::BaseSuperseded`，wire.rs:300；判定 `src/sqlite_supply.rs:115-129`）；从未存在 → `base_unavailable`；伪造/损坏 → `integrity_failure` |
| update_data / backfill_data | `base_version_id` 语义=**当前** head 的 version_id（内容 hex）；`expected_head`/`expected_yearly_head` CAS 同义 | receipt `version_id`=新内容 id；`no_change` 不变 | 见 §3 写路径分档（R1 定稿：stale base → item 级 `needs_verification`） |
| republish_data | 保留（RULINGS Q1：#247 显式 republish 合同，非版本概念） | 语义为「显式区间的**源数据刷新**」，落到统一的 append/replace/no_change/metadata 提交函数；description 已是新措辞（`REPUBLISH_DESCRIPTION` `registry.rs:130`：被取代 base → item 级 `needs_verification`（STALE_BASE: 前缀），绝不静默重 pin） | 同 update_data |
| verify_publication_result | 无 | 新语义见 §5（SPEC §6.6） | 目标任务/事件不存在 → `task_not_found`/`version_not_found`（沿用现有码 `mcp.rs:828-845`） |
| verify_stopped_writes / verify_data_gap / inspect_verification_task | 无 | 无 | 无 |
| start_collection / retry_task / cancel_task / list_tasks / get_task / get_task_progress / get_request / run_worker_once / compute_metrics | 无 | 无（进度与结果中的 `version_id` 一律为内容 hex） | 无 |

wire 类型 `VersionChoice`/`Query`/`VersionRef`/`ManifestEntry`/`Row`
形状全部保留（`src/data_supply/wire.rs` 相关定义不变）；`cursor`/
快照分页语义不变（SPEC §6.5：快照 token 天然隔离翻页间替换）。

## 5. `verify_publication_result` 新语义（SPEC §6.6，DESIGN-BRIEF §2.5）

- 对**仍是当前序列**的更新事件：验证 ①事件行存在
  ②`new_content_digest == 当前 series.content_digest`
  ③evidence 捆绑可重放（实现 `src/sqlite_market/verification.rs:304-313`：
  `new_digest == series.content_digest` → 读 evidence 捆绑重放）。
- 对**已被后续更新覆盖**的历史任务：只验证「该 `result_id` 的事件
  存在 + 事件 digest 链闭合到当前 `series.content_digest`」；
  不能再验证那一刻的数据内容（单版本下旧 bar 集合不存在）。
  （实现 `verification.rs:314-328`：事件在实体链中定位，
  `tail` 逐环 `old==prev.new` 且末环 `new==当前 digest`；
  不闭合 → integrity 错误 fail-closed。）
- `market_events.result_digest` 继承旧 `market_update_results`/
  `publication_results` 的 result_digest 锚点角色（RULINGS Q12；
  canonical 形状由 `series_commit::event_result_digest` 重算比对，
  `verification.rs:271-289`）。
- 伪造/不存在的 `result_id` → `task_not_found`/`version_not_found`
  （沿用现有码 `mcp.rs:513-514, 828-845`，OWNER-RULING-PAR-1A R3）。
- 判定层三值结论（`publication_verification/decision.rs:9-14`
  `Conclusion`，wire `snake_case`）：全部 item 有可验证事件证据 →
  `all_recorded`；有缺证且目标仍 `NeedsVerification` →
  `insufficient_evidence`；否则 `not_applicable`。receipt 形状
  `decision.rs:16-25`（verification_task_id/target_task_id/双方
  revision/conclusion/reason/missing_receipt_item_ids）。

## 6. 年线 stale 出参形状（已定稿，实现已核）

RULINGS A1/Q10：stale 出参形状含 `behind|invalid` 与 `as_of_day`。
判定逻辑（`src/sqlite_market/series.rs:660-700` `YearlyState` /
`yearly_state`）：

- **fresh**：记录的 base 仍等于当前日线状态；
- **behind**：base 之后只有 `append`/`no_change`/`metadata` 事件——
  年线数据对其 `as_of_day` 仍正确，`query_data` 照常返回并带
  stale 标记；
- **invalid**：base 之后出现 `replace`（除权/修订）或语义变更
  提交——该实体年线不得返回（fail-closed）；批量查询按行级
  状态返回不连坐（RULINGS A1）；
- **integrity**：`lineage.base_daily_digest` 不在该实体日线事件链 →
  integrity 错误（伪造/损坏；`yearly_state` 对缺 lineage、缺日线
  序列、digest 脱链、`base_event_seq` 越过链尾一律 `Error::Integrity`）。

### 6.1 `query_data`

`ManifestEntry` 三个可选字段（`src/data_supply/wire.rs:140-150`，
均为 `Option` + `skip_serializing_if`，None 时字段整体缺席；
`entity/version_ref/status/metadata` 照旧输出，`status` 枚举仍为
`present|absent`）：

| 情形 | 行形状 | 实现 |
|---|---|---|
| fresh / behind | `status:"present"`；behind 加 `"stale":"behind"` + `"as_of_day":"<YYYY-MM-DD>"` | `src/sqlite_supply.rs:399-403, 444-445` |
| invalid，**单实体**查询 | 整次调用报错 **`stale_version`**（`Error::Stale`，错误文本含 reason code） | `src/sqlite_supply.rs:406-414`；映射 `mcp.rs:514` |
| invalid，**批量**查询 | 该实体行 `status:"absent"`、`version_ref:null` + `"reason":<AbsentReason>`（其余实体正常返回） | `src/sqlite_supply.rs:415-430` |

样例（断言见 `src/data_supply/mcp/tests.rs:578-579`（behind/as_of_day）、
`:660`（单实体 `stale_version`）、`:686-687`（批量 absent+reason））：

```json
{"entity":{"exchange":"sh","code":"600006"},"version_ref":{"dataset":"market.yearly","contract_version":"v1","entity":{"exchange":"sh","code":"600006"},"version_id":"<hex64>","integrity_sha256":"<hex64>"},"status":"present","metadata":{...},"stale":"behind","as_of_day":"2026-06-08"}
{"entity":{"exchange":"sh","code":"600006"},"version_ref":null,"status":"absent","metadata":null,"reason":"daily_replaced_since_derivation"}
```

### 6.2 `get_data_availability`

`AvailabilityItem` 同样三个可选字段（`src/data_supply/wire.rs:451-460`）：

| 情形 | 行形状 | 实现 |
|---|---|---|
| fresh | `availability:"available"` 等正常值，无 stale | — |
| behind | 正常 availability 值 + `"stale":"behind"` + `"as_of_day":"<YYYY-MM-DD>"` | `src/sqlite_supply.rs:1743-1752` |
| invalid | **`availability:"absent"`** + `"reason":<AbsentReason>`；`version_ref:null` | `src/sqlite_supply.rs:1753-1779` |

断言：`items[0]["stale"]=="behind"`（`src/data_supply/mcp/tests.rs:587`）；
invalid 行 `{"availability":"absent","reason":"daily_replaced_since_derivation"}`
（`mcp/tests.rs:706-710`）。invalid 以 `absent`+`reason` 表达，与真正
`absent`（version_ref 存在但无记录）/`unavailable`（integrity 损坏）
可区分。

### 6.3 `get_yearly_base_years`

`YearPresenceResult.reason_code`（`src/data_supply/wire.rs:296-318`）
枚举全集与判定（`src/sqlite_supply.rs:110-129`）：

| reason_code | 触发 |
|---|---|
| `null` | `status:"resolved"`，`present_years` 为实际集合（可为 `[]`） |
| `base_unavailable` | pin 的日线 id 从未提交 |
| `base_superseded` | pin 曾提交、已被更新 head 取代（`digest_seen` 命中但非当前序列；OWNER-RULING-PAR-1A R2'） |
| `integrity_failure` | 伪造/损坏：实体或周期不符、内容校验失败、lineage 不匹配 |

样例（`tests/mcp/yearly_presence.rs:128-148`）：

```json
{"entity":{"exchange":"sh","code":"600008"},"base_daily_version_id":"<hex64>","status":"unavailable","version_ref":null,"present_years":null,"reason_code":"base_superseded"}
```

### 6.4 行级 `reason` 枚举全集

`AbsentReason`（`src/data_supply/wire.rs:151-157`，snake_case）只有两值，
由 `YearlyInvalidReason::code()` 一一映射
（`src/sqlite_market/series.rs:677-690`）：

| wire 值 | 触发 |
|---|---|
| `daily_replaced_since_derivation` | base 之后出现日线 `replace` 事件 |
| `value_semantics_changed` | base 之后的提交改变了年线聚合所依赖的版本证据 |

### 6.5 metadata 事件的分类边界（OWNER-ACCEPT-S1 C3）

`metadata`/`no_change`/`append` 事件默认仍判 **behind**；但当提交的
版本证据三元组 **{source, unit_currency, adjustment_evidence}** 发生变化
时记 `semantics_changed=1`（DDL `021_latest_only.sql:72-75`，C3），
该年线判 **invalid(`value_semantics_changed`)** 而非 behind
（`YearlyInvalidReason::SemanticsChanged`，`series.rs:667-679`）——
价格口径/单位/复权证据改变后，旧年线内容不再可证。

### 6.6 与 wire 码的映射速查

| 位置 | 输出 |
|---|---|
| `query_data` 单实体 invalid | wire 错误码 `stale_version` |
| `query_data` 批量 invalid 行 | `status:"absent"` + `reason`（§6.4） |
| `query_data` behind 行 | `stale:"behind"` + `as_of_day` |
| `get_data_availability` invalid 行 | `availability:"absent"` + `reason` |
| `get_data_availability` behind 行 | `stale:"behind"` + `as_of_day` |
| `get_yearly_base_years` superseded 行 | `reason_code:"base_superseded"` |
| lineage 伪造/损坏（三处共用） | 整次 integrity 错误 fail-closed |

## 7. 部署与切换

部署形态不变：归档旧库 + 全新 init + 全市场重建（无数据迁移；
SPEC §7）。新二进制只认 schema 21（RULINGS Q8）；回滚=旧二进制+
归档库（旧二进制对 user_version=21 报 unsupported schema，
fail-closed 符合既定模式）。部署日需要改的文档、脚本断言与 Skill
句子清单：[v6-switch-checklist](v6-switch-checklist.md)。
