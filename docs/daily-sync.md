# MCP 每日同步与 Skill 编排

## 当前状态

**OPERATIONAL OWNER-ONLY PREVIEW / CLASS-SPECIFIC SLOS**

当前运行时包含两个 MCP server（v0.2.0-preview.29，代码 `4493d45`，本机
`/opt/x-data-mcp-preview/` systemd 部署）：

- `x-data-acquire`：远端文件探测、下载和本地安装，10 acquisition tools。
- `x-data-mcp`：任务、版本、发布、查询、核实和计算，20 data tools。

当前开发分支（XP-014，未部署）为 data server 注册 21 data tools（新增
`get_yearly_base_years`）并宣告 `generic-data-v5`；preview.29 部署实例仍报告
20 data tools / `generic-data-v3`，strict Skill 会话遇 revision 不匹配即停止。

显式 init 的新库使用 fresh compact schema 20；`SCHEMA_VERSION=18` 仅为 legacy
upgrade source schema（既有库升级来源），不是当前部署 schema。

immutable generation、每代 #131 验证后条件激活、任务受理时冻结
`source_generation_id`、generation-bound daily/yearly update、truthful capability
和仓库 Skill 已实现，并已有全市场真实 generation 证据（preview.26–28 连续验收）：

- `securities.catalog` 全目录 5,227 实体；daily 首采约 340s；yearly 全量约 900s；
  DB 约 831MB；同代 no-change `update_data` 全量 5,222 items 约 616s（约 10min）。
- 单任务 batch 上限为 100 个实体，这是受理分批边界；全市场 collection/update
  已通过分批 MCP 编排完成，不再因该上限而不可运行。
- class-specific SLO（2026-09-28 owner 批准，#259 re-baseline 后关闭）：同代
  no-change full-market `update_data` hard ≤15min、checkpoint 后 net growth
  baseline ≈3.6KiB/attempted item、hard ≤4KiB/item（5,222 items 对应 hard
  ≈20.4MiB）、必须 `data_versions +0`；preview.27/schema20 实测 5,222 items /
  616s / +18.5MB / 0 new versions——PASS。首采 DB ≤2.5GiB、部署 ≤4GiB、
  首采 ≤20min 亦 PASS。
- acquisition、initial collection、yearly、cross-generation envelope 与真实
  append/rewrite 分开计量：cross-generation 约 37min 仅为 preview.29 measured
  baseline、尚无批准 hard SLO；真实 append/rewrite 待独立证据。#320 的本地
  retained-generation release harness（c360967a→2e918330、5,222 items）测得
  96.4s、5,165 typed metadata appends、45 envelope attempts（21 published、24
  fail-closed）、integrity check PASS；测量环境 ecs-node x86_64、2 vCPU、
  1.9GiB RAM+4GiB swap、rustc 1.97.1、release profile，代码 6f9f0a4，单进程
  cargo test -j1 顺序 ≤100-item 任务，冷/热缓存状态未记录；这是离线证据，
  不等于 preview.29/生产 SLO，也不能替代 deployed runtime 验收。

preview.29 Phase-G 部署为 fresh-init schema 20：旧 801MB DB 已归档至
`/opt/x-data-mcp-preview/state/preview.sqlite.v20-archived-20260928T053759Z`。
2026-09-29 explicit fresh rebuild 后，2026-09-30 operator-path 已激活 generation
`2e9183306d4292c96602185152770a5ef54fbc28d1c9c909d3297c9d9bfe70a8`，
并发布 catalog `catalog-1790660536962300201`（5,227 records）、5,223 个
`market.daily` published versions 和 5,223 个 `market.yearly` derived
versions；最新集中行情日为 2026-09-29（5,211 entities）；4 个官方
`hsjday` 成员缺失保持 absent（`sz.001246`、`sz.301569`、`sz.301660`、
`sz.301718`）。daily/yearly 样本 QFQ manifest/readback 已验证，
当前 DB 约 881MB；x 消费侧 bounded `KlineReader` live readback 已通过
（`liqiangcc/x` PR #150，证据见
`evidence/issue-298-x-consumer/preview29-x-consumer-20260929.json`），但 Security
Master、历史 universe、交易日历、pool/情绪、raw/point-in-time 价格与完整
simulator 会话仍未纳入该 slice；package smoke（synthetic fixture、live source
NOT_RUN）不能替代。

`production_ready=false` 表达 declared owner/local scope；external distribution
is not authorized。运行时 `tools/list`、capability 与结构化
readiness 仍是机器事实入口；本文不替代 runtime readiness。

## 关注点分离

| 层 | 职责 | 不负责 |
|---|---|---|
| Domain | generation/version/task 状态、证据充分性、合法状态转换 | MCP 工具名和调用顺序 |
| Use case | 完成 acquire、verify、activate、update、query 等单一业务动作 | 调用者工作流策略 |
| MCP Adapter | 封闭 schema、权限、错误和结果映射 | 跨工具编排 |
| Capability | 支持矩阵、前置条件、副作用、outcome、runtime/governance readiness | 返回下一工具或 Skill 内容 |
| Devin Skill | 根据用户目标与结构化状态跨两个 server 编排 | 自行签发兼容性或修订 PASS |
| 文档 | 人类安装、边界和排障 | 作为运行时授权/readiness 来源 |

Tool 返回当前动作的事实：`state`、`state_revision`、`outcome`、`outcome_certainty`、稳定 `refs`、证据引用和 reason codes。Tool/Capability 不返回 `next_actions`、其他工具名、Skill URL 或 Skill 正文。

## Generation 模型

目标目录布局：

```text
official-generations/
  <generation_id>/
    base.zip
    gbbq.zip
    hsjday.zip
    snapshot-manifest.json
    consistency-decision.json
active-generation.json
```

Generation 生命周期：

```text
remote_observed
  → acquisition_pending
  → downloaded
  → byte_verified
  → consistency_pending
  → consistency_verified | not_verified | conflict
  → active
  → superseded
```

约束：

- `downloaded` 和 `byte_verified` 不代表可用于行情发布。
- 只有 `consistency_verified` generation 可被条件激活。
- generation 目录完成后不可修改；active pointer 只引用 generation identity。
- 已运行 source session 始终读取其绑定的不可变 generation；active pointer 变化不替换任务输入。
- 新数据任务冻结 exact `source_generation_id`。
- acquisition 只能签发下载/字节事实，不能因为 HTTP、ETag、Last-Modified、文件名或单文件 SHA 正确而自行签发 #131 兼容结论。

Immutable 不表示所有约 550MiB generation 永久保留在 ECS 热盘。#259 容量目标与 #260 仍需共同裁定 superseded
文件的热保留、冷归档或整代 retire 规则；在裁定前不得自动删除。任何被 task/session 引用的 generation
必须保留，状态需区分 hot、cold、evidence-only 和 unavailable，不能在删除原字节后继续声称可完整 source replay。

## 目标工具职责

### `x-data-acquire`

| Tool | 单一职责 |
|---|---|
| `get_acquisition_capabilities` | 返回 revision、schema、readiness、blocker 和 active generation 事实 |
| `check_official_updates` | 只读观测远端文件事实；`changed` 是相对上次 persisted `seen` 的 drift，`pending_roles`/`active_change` 是相对 active generation manifest 的待采集差异，unknown 仍是停止信号 |
| `acquire_generation` | 以持久 request identity 异步下载并 byte-verify 一个 candidate generation；不激活 |
| `get_acquisition_task` | 读取下载、校验、持久化阶段和稳定 refs |
| `get_acquisition_request` | 在响应丢失或超时后按 request identity 恢复事实 |
| `verify_generation` | 使用 #131 契约和显式 CompatibilityEvidence 产生 consistency decision；不激活 |
| `activate_generation` | 对 `consistency_verified` generation 执行 expected-previous 条件激活；不更新行情 |
| `retire_generation` | owner/operator 显式确认后按引用与留存规则退役 generation |
| `sync_status` | 只读返回 active/pending generation、manifest 和 consistency 状态 |
| `run_worker_once` | operator/debug-only；正常长期服务不由 Skill 调用 |

### `x-data-mcp`

| Tool | 单一职责 |
|---|---|
| `start_collection` | 无已发布 head 时首次建立 catalog/daily/yearly 数据 |
| `update_data` | 已有 head 时，对冻结 verified generation 执行 routine refresh 和 revision decision |
| `backfill_data` | 消费 proven historical gaps 或明确支持的 yearly range |
| `republish_data` | 仅 market.daily 固定基线 pure enrichment；不授权 known-value revision |
| `verify_data_gap` | 生产历史缺口证据；不承担正常新增交易日更新 |
| `get_task_progress` | 分页读取固定任务进度和结果 |
| `verify_publication_result` | 仅在 publication outcome unknown 时检查固定证据 |
| `get_data_availability` | 读取版本、coverage、gap 和 unresolved range 事实 |
| `query_data` / `list_data_versions` | 读取固定已发布版本，不隐式采集或修复 |
| `compute_metrics` | 对显式固定版本执行已注册确定性计算，不隐式采集 |
| `run_worker_once` | operator/debug-only；正常长期服务由 background worker 执行任务 |

每个 Tool description 使用固定结构：`Use when`、`Requires`、`Effect`、`Success proves`、`Does not`。受理成功不等于执行完成；task succeeded 也不自动等于 coverage complete。

## Skill 编排

项目 Skill 的目标位置：

```text
.devin/skills/x-data-daily-sync/SKILL.md
```

Skill 已交付于 `.devin/skills/x-data-daily-sync/SKILL.md`。它的当前能力边界覆盖
generation 生命周期与全市场分批数据任务编排（单任务 batch 上限 100 个实体，由
Skill 分批 orchestrate）；但“独立新会话仅调用项目 Skill、不读源码、不使用
shell”的最终验收仍待完成，尚不能据此声明全市场 unattended daily sync 已验收。
Skill 必须：

- 使用 `triggers: [user]`，不在无用户请求时自主下载、激活或发布。
- 仅允许所需的 `x-data-acquire` / `x-data-mcp` MCP tools；禁止 shell、SSH、curl、SQLite 和文件旁路。
- 启动时核对 capability/schema/generation/task contract revisions；不匹配即停止。
- 根据结构化 `state/outcome/certainty/refs/reason_codes` 编排，不解析自由 message，不依赖 Tool 返回下一步。
- 使用有界轮询；超出预算返回持久 request/task/generation refs，不无限等待。
- 对 `unknown`、`not_verified`、`conflict`、未授权 revision 和 outcome unknown 保持 fail closed。
- 正常 background runtime 不调用 `run_worker_once`。

Skill 通过仓库分发，正文只保存在 `.devin/skills/`。如需跨项目使用，再从同一源打包 pinned Devin Plugin；业务 MCP 不返回 Skill URL/content。

## 目标工作流

下面的顺序属于 Skill，不属于 Tool/Capability 返回。

### 无远端变化

```text
读取 capabilities/status
→ check remote facts
→ unchanged
→ 输出 active generation 和检查时间
```

不下载，不创建 generation/data task。

### 有新 generation、已有 daily heads

```text
读取 capabilities/status
→ check remote facts
→ acquire candidate generation
→ 按 request/task refs 等待 byte verification
→ verify generation consistency
→ VERIFIED_COMPATIBLE 时条件激活
→ update_data（冻结 generation + existing heads）
→ 分页读取 task progress
→ availability
→ 小范围 query 验证
```

任何 `unknown`、`not_verified` 或 `conflict` 分支在激活前停止。正常新增交易日由 `update_data` 内部验证 raw-day prefix/chunk 和 per-security GBBQ event continuity；AI 不逐证券调用 `verify_data_gap`。

### 全新数据库

```text
verified active generation
→ start_collection(securities.catalog)
→ progress
→ start_collection(market.daily)
→ progress
→ availability/query
→ 可选派生 market.yearly
```

### 结果不明

- Acquisition timeout：按原 acquisition request/task ref 查询，不更换 ID 盲目重发。
- Data acceptance timeout：按原 request ID 查询绑定。
- Publication outcome unknown：调用 publication verification；不把超时当失败。
- Started task 停止状态不明：使用 stopped-write evidence 流程；不强制释放占用。

## Capability 要求

Capability 必须表达：

- exact `(tool, dataset, contract_version)` 支持矩阵；
- input/output schema 和 schema digest；
- possible outcomes；
- request/idempotency 语义；
- side-effect classification；
- runtime readiness（active verified generation、DB/source 状态）；
- governance readiness（声明使用范围、证据 gate）；
- capability/generation/task contract revisions；
- active generation ref 作为观测事实。

`production_ready=false` 必须附结构化原因。`republish_data` 不得广告 yearly；`verify_data_gap` 不得保留已退休 blocker；`start_collection` 不得继续声称已解决来源为 NOT_VERIFIED。

## 已冻结的治理决定

#262 comment-5751060523 已批准 D1–D6 默认值：validator 全部 PASS 后才自动签发
CompatibilityEvidence；generation retention/retire、durable acquisition store、按任务
冻结 source session、条件 activate/显式 retire 和 revision 兼容矩阵均以该裁定为准。
后续实现不得重新把这些项目标成“开发前未决”，也不得把 HTTP/ETag/文件名或单文件
SHA 当作 #131 PASS。

## 当前验收矩阵

只有“实现 + 自动测试 + 对应规模真实证据”三列同时存在才可标 PASS：

| 能力 | 实现/测试 | 真实证据 | 当前裁定 |
|---|---|---|---|
| immutable generation + verify/activate | 有 | 本机真实 generation 全生命周期 | PASS |
| generation-bound daily update | 有 | 全市场真实 generation | PASS |
| generation-bound yearly update | 有 | 全市场真实 generation | PASS |
| 全市场 collection/update | 有 | 分批 MCP 编排：catalog 5,227、daily 约 340s、no-change update 5,222 items 约 616s | PASS（batched） |
| 首采 DB ≤2.5GiB、部署 ≤4GiB、首采 ≤20min | 有 | DB 约 831MB、preview.28 部署包、首采约 340s | PASS |
| 同代 no-change `update_data`（≤15min、≤4KiB/item、`data_versions +0`） | 有 | preview.27/schema20：5,222 items / 616s / +18.5MB / 0 new versions | PASS |
| 跨代 envelope / 真实 append·rewrite | 有实测 | cross-gen 约 37min（preview.29 measured baseline，无批准 hard SLO）；#320 后 proven append 可走 typed metadata path，rewrite/unsupported metadata 仍 envelope/fail-closed；typed 全市场生产 timing 待测 | measured / 不外推 |
| 新 Devin 会话仅 Skill+MCP | Skill 文件存在 | 无独立会话证据 | 未验收 |
| 双 MCP 可复现部署 | release 双 binary + `deploy_phase_g.sh` | preview.29 双 systemd 服务在运行 | PASS |
| overall production readiness | capability 恒实报告 false | owner-only declared scope 可观测 | owner-only operational；external distribution not authorized |

## 验收

#260 完成至少需要：

1. A source session 在 B 下载/激活后仍读取 A immutable bytes，不出现 A manifest + B bytes。
2. Mixed、缺失、损坏或无显式兼容证据的 generation 不激活。
3. Acquisition 每个 crash point 可按 request identity 恢复确定事实。
4. 从 verified generation 到全市场 daily update 的 changed/unchanged/修订分支均可仅通过 MCP 完成。
5. Tools/list、capability、handler 和 examples 来自同一 descriptor；广告分支有正向契约测试。
6. 全新 Devin 会话仅调用项目 Skill 完成 unchanged/changed 流程，不读源码、不用 shell。
7. Skill 和服务端 revisions 不匹配时停止，而非猜测兼容。
8. ECS 2 vCPU/约 2 GiB 环境的本地日更新目标不超过 3 分钟、硬上限 5 分钟；网络下载单独计量。

截至 preview.26–28 实测：第 4 项已由分批 MCP 编排完成（全市场证据见上文当前状态与
验收矩阵）；第 8 项的旧统一日更目标已被 2026-09-28 owner re-baseline supersede——
同代 no-change `update_data` 按新合同（≤15min / ≤4KiB/item / `data_versions +0`）
实测 PASS，cross-generation 约 37min 仅为 measured baseline、不构成通过；第 6 项
独立新会话仅 Skill 验收仍未执行。

## 当前禁止声明

在 Skill 独立会话验收、外部分发授权和其余类别 SLO 收口前，禁止声明：

- “cross-generation、真实 append/rewrite 已有批准的 hard SLO 或已 PASS”
  （cross-gen 约 37min 仅为 measured baseline）；
- “旧统一 5min/16MiB 门禁仍是现行合同”（已由 owner re-baseline supersede）；
- “acquire 下载成功即可 update”；
- “tools readiness=ready 即 generation 可用”；
- “Skill 已通过独立新会话的全市场验收”；
- “已获外部分发或对外生产部署授权”（declared scope 仍为 owner 本地使用）。
