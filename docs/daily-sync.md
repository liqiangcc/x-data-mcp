# MCP 每日同步与 Skill 编排

## 当前状态

**PARTIALLY IMPLEMENTED / CAPACITY-BLOCKED FOR FULL-MARKET PRODUCTION**

当前运行时包含两个 MCP server：

- `x-data-acquire`：远端文件探测、下载和本地安装。
- `x-data-mcp`：任务、版本、发布、查询、核实和计算。

截至 2026-09-21，immutable generation、每代 #131 验证后条件激活、任务受理时
冻结 `source_generation_id`、generation-bound daily/yearly update、truthful capability
和仓库 Skill 已实现。ECS 已通过 100-item MCP 功能 smoke，但这**不等于**全市场或
生产闭环完成：

- `start_collection` 与 default daily selection 的单任务预算仍为 100 个证券；
- #259 的首次全市场 DB ≤2.5GiB、每日增长 ≤8/16MiB 尚未通过；
- `production_ready` 仍为 false；当前不得在 ECS 上启动 5,221-symbol collection。

当前权威状态见 [#259 容量审计](https://github.com/liqiangcc/x-data-mcp/issues/259#issuecomment-5756919342)
和 [#262 Epic 更正](https://github.com/liqiangcc/x-data-mcp/issues/262#issuecomment-5756920960)。
运行时 `tools/list`、capability 与结构化 readiness 仍是机器事实入口；本文不替代
runtime readiness，也不把 100-item smoke 扩大为全市场验收。

Issue/Epic 中的“v15”是交付规划标签；当前 SQLite `SCHEMA_VERSION=18`，二者不是
同一版本序列。

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
| `check_official_updates` | 只读观测远端文件事实；结果区分 changed、unchanged、unknown |
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

Skill 已交付于 `.devin/skills/x-data-daily-sync/SKILL.md`。它的当前能力边界是
generation 与有界数据任务编排；受 100-item 预算与 #259 容量 blocker 约束，尚不能
据此声明全市场 unattended daily sync 已验收。Skill 必须：

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
| immutable generation + verify/activate | 有 | ECS generation `868f4fa1…` | 100-item 功能前置 PASS |
| generation-bound daily update | 有 | ECS 100/100 | 有界功能 PASS，非全市场 |
| generation-bound yearly update | 有 | ECS 3/3 | 有界功能 PASS |
| 5,221-symbol collection/update | 100-item budget 阻塞 | 无 | FAIL / 未验收 |
| 首采 DB ≤2.5GiB、日增 ≤8/16MiB | 当前 `bar_json`/audit payload 不满足 | 100-item dbstat 已显示超标风险 | FAIL |
| 新 Devin 会话仅 Skill+MCP | Skill 文件存在 | 无独立会话证据 | 未验收 |
| 双 MCP 可复现部署 | release 包含双 binary | 仓库 `deploy_preview.sh` 仍是旧单服务拓扑 | 未收口 |
| overall production readiness | capability 恒实报告 false | 结构化 blocker 可观测 | 非生产 |

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

## 当前禁止声明

在全市场、容量、Skill 独立会话和生产治理验收完成前，禁止声明：

- “全市场每日更新可完全由 AI 通过 MCP 安全完成”；
- “acquire 下载成功即可 update”；
- “tools readiness=ready 即 generation 可用”；
- “100-item smoke 已满足 5,221-symbol 容量/性能 AC”；
- “Skill 已通过独立新会话的全市场验收”。
