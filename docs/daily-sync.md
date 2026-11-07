# MCP 每日同步与 Skill 编排

## 当前状态

> 部署与运行事实以 [STATUS](STATUS.md) 为准，性能与容量目标以 [SLO](SLO.md) 为准。
**OPERATIONAL OWNER-ONLY PREVIEW / CLASS-SPECIFIC SLOS**

当前运行时包含两个 MCP server（systemd 部署在生产主机；生产主机、launcher 所在机器、
部署版本、generation 等机器事实
见 [STATUS](STATUS.md)）：

- `x-data-acquire`：远端文件探测、下载和本地安装，10 acquisition tools。
- `x-data-mcp`：任务、版本、发布、查询、核实和计算。一个 `data_service` 进程服务三个
  socket：`mcp.sock` 的 `ops` 入口（编排入口，16 data tools，含只读
  `get_yearly_base_years`、`plan_market_update`/`run_market_update` 与
  `refresh_exchange_sources`，x 与日更 Skill 的连接不变）、`research.sock` 的 `research` 入口
  （只读研究面，工具数 5）、`maint.sock` 的 `maint` 入口（本机运维，工具数 9，不经隧道）；
  capability revision `generic-data-v10`（market 单版本序列：version_id=内容 hex，latest-only），
  文本模式 `full`；strict Skill 会话遇 revision 不匹配即停止。日更 Skill 仍连 `mcp.sock`；
  隧道自 2026-11-02 起在 aws-node fallback 上连 `research.sock`（只读研究面）；
  2026-11-05 起日更和主数据生产机是 vm2-126（无 tunnel，D-74），aws-node 冻结为 fallback。

M2 已上线：6 个解析事实数据集在 `query_data` v2 上可读，其中
`market.security_master`、`market.risk_warnings`、`market.suspensions`、
`market.price_limit_rules` 由**同一次日更 run 的 4 个内部派生 item** 维护
（见下文「内部派生 item」）；交易所上市/退市名单、历史 ST（#308）、停复牌公告与
简称历史（D-61）读同一个 `X_DATA_MCP_SOURCE_STORE`，未配置时按部署形态区分：默认（stdio、测试、
非 ops 入口、legacy 后端）记 `pending_source`、`risk_warnings` 写 0 行且读取仍为
`absent`；生产形态（`data_service` 的 ops 入口 + compact 后端）是 item 失败（稳定码
`source_store_unconfigured`），不静默写 0 行（见下文「内部派生 item」）。生产形态启动时
还会在 journal 留一条同码告警并继续服务（不拒绝启动）；已配置时只记录来源库路径
（不含密钥），dev 豁免开启时打印 `non-production override`。
涨跌停规则原文未接入时仍按 `pending_source` 记录、`trading_rules` 写 0 行。因此日更成功后，M2 数据集的新鲜度与日线同步，
不需要单独跑任务（详见 [STATUS](STATUS.md)「数据」段的 M2 数据集行）。

已部署实例为 fresh compact schema 27（2026-11-05 preview.45 on vm2-126，D-74；aws-node
fallback 为 schema 26；M1 的 schema 22 +
M2 PR-1 的 023 + M2 PR-2 的 024 + M3 PR-1 的 025 + R2 的 026 + M3-2 PR-B 的 027），三个入口合计
去重后注册 25 项 data 工具；
显式 init 的新库按代码的 schema 建立；`SCHEMA_VERSION=18` 仅为 legacy
upgrade source schema（既有库升级来源），不是当前部署 schema。

immutable generation、每代 #131 验证后条件激活、任务受理时冻结
`source_generation_id`、generation-bound daily/yearly update、truthful capability
和仓库 Skill 已实现，并已有全市场真实 generation 证据（历史：preview.26–28 连续验收）：

- 历史实测与门禁沿革（早期 preview 时期，当前目标见 [SLO](SLO.md)）：`securities.catalog` 全目录 5,227 实体；daily 首采约 340s；yearly 全量约 900s；
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

历史记录（preview.29 时期，非当前部署；当前事实见 [STATUS](STATUS.md)）：preview.29 Phase-G 部署为 fresh-init schema 20：旧 801MB DB 已归档至
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
| 执行 Skill 的 AI 客户端 | 根据用户目标与结构化状态跨两个 server 编排 | 自行签发兼容性或修订 PASS |
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

Immutable 不表示所有约 550MiB generation 永久保留在 ECS 热盘。热盘上的 superseded generation 按自动保留规则处理：激活成功后保留最近 2 个（含刚被取代的前一代，其作为回滚目标始终保留），更旧者自动退役为 `evidence_only`；
冷归档仍需 #259 容量目标与 #260 另行裁定，被自动退役的字节不再可用于完整 source replay。任何被 task/session 引用的 generation
必须保留，状态需区分 hot、cold、evidence-only 和 unavailable，不能在删除原字节后继续声称可完整 source replay。

退役前有 M3 门禁（`#446` §8.3，PR-8）：该代不可再生的行业原始成员必须已按 sha 在来源库读回，且**激活过的代**其 `generation_facts` 覆盖全部必需 kind。任一项不满足就拒绝删除，返回 `source_store_evidence_missing` 或 `derived_facts_missing`（附缺失清单；索引缺失先跑 `source_store replay --scope generation_facts`），自动保留遇到拒绝只跳过该代、不使激活失败，下轮再试。证据缺失如果是「字节在库里但 `origin` 不是 `generation:<sha>`」（来源库出现之前采集的遗留代），owner 用 `source_store register-generation-member --generation <sha> --generation-dir <代目录> --store <来源库>` 补登记后再重试（只追加、幂等，见 [来源库](source-store.md) §4.3）；其余情况人工退役因门禁被拒时，只有 owner 能用 `source_store exempt` 写豁免；Skill/agent 不得自行豁免或补登记。

**从未激活的代不要求派生事实（#583）。** 任务受理时从 active pointer 冻结 `source_generation_id`，所以从未换过 active pointer 的代不可能写过任何派生事实；要求它先有 `generation_facts` 行，只能靠手工豁免绕开，2026-11-06 事故里的 3 个未激活代就是这种情况。现在门禁对这类代跳过派生事实步骤，**来源证据检查不变**（缺证据仍 `source_store_evidence_missing`，字节不动），激活过的代行为一字不变。判定「从未激活」要正向证据：acquire 库的 `generations.state` 只可能是激活前的 `byte_verified`/`consistency_verified`/`not_verified`/`conflict`/`corrupt`，`active`/`superseded`/`evidence_only` 或未知状态一律按激活过处理；此外 active pointer 指名、`activate_generation` 请求记录里该代有 `state=active` 的结果、或有受理未落结果的激活请求，也都算激活过（指针交换与行更新之间的崩溃窗口靠这份持久请求日志兜住）。判定所需的事实读不出来时退役被拒（`retire_gate_unavailable`），不删字节。跳过时回复 `detail` 带 `derived facts not required: generation was never activated`。

`acquire_generation` 被 `generation_capacity` 拒绝时，同样需要 owner/operator 先腾出热代；退役现在可能被上述门禁拦下，所以「腾位」不再是必然成功的动作：先按提示重放索引，来源库证据缺失先试补登记（遗留代）或由 owner 显式写证据豁免（`source_store exempt ... --owner-approve-evidence`）。不要为了腾位删除 generation 目录或绕过 acquire。

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
| `activate_generation` | 对 `consistency_verified` generation 执行 expected-previous 条件激活；不更新行情。激活成功后自动保留：按安装时间保留最近 2 个 `superseded` generation（含刚被取代的前一代），更旧者自动退役为 `evidence_only`（字节删除）；有 live pin 或在途获取任务的代跳过且不使激活失败；回复 `retired_generations` 列出本次退役的 id，replay 返回同一列表 |
| `retire_generation` | owner/operator 显式确认后按引用与留存规则退役 generation；与上面的自动保留共用同一 pin/在途任务检查与 M3 退役门禁，active 代永不退役；来源证据对所有代都要检查，派生事实只对**激活过的代**要求（从未激活的代不要求派生事实，见上文；#583）；门禁拒绝时返回 `source_store_evidence_missing`/`derived_facts_missing`（或未配置/不可读时为 `retire_gate_unconfigured`/`retire_gate_unavailable`），原字节保留 |
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

Skill 通过仓库分发，正文只保存在 `.devin/skills/`。如需跨项目使用，再从同一源打包为 pinned plugin；业务 MCP 不返回 Skill URL/content。

**启动方式的坑（2026-11-06 事故）**：日更 agent 的工具白名单没有文件读取，Skill 正文只能靠启动提示词带进去。用 pi 1.3.0 经 herdr 发 `/skill:x-data-daily-sync` 时，斜杠命令没有被展开，会话里没有 Skill 正文，agent 只能凭 MCP 的结构化返回判断停止条件，漏掉 §3.0、§3.3、§3.3a、§3.6、§4 仍报 `completed`（`scripts/check_daily_run.py` 把这类会话判为未通过）。因此启动脚本（部署机 `/root/x-daily-sync/start-440.sh`）必须把 `.devin/skills/x-data-daily-sync/SKILL.md` 去掉 frontmatter 后包进 `<skill name="x-data-daily-sync">` 放进提示词（约 55 KB），不能只写 `/skill:` 一行。每次日更跑完用 `check_daily_run.py` 核对会话里 Skill 正文是否生效。该目录是部署机状态，迁移时随 `docs/new-host-runbook.md` 的 rsync 一起带走。

## 目标工作流

下面的顺序属于 Skill，不属于 Tool/Capability 返回。

### 无远端变化（`day` 不在 `pending_roles`）

```text
读取 capabilities/status
→ check remote facts
→ day 未 pending
→ 补发布判定（只读，Skill §1.1）
→ 已有成功收尾的日线 run：no_update，输出 active generation 和检查时间
→ 没有：恰好一次 plan_market_update；eligible=0 则 no_update，eligible>0 则补发布
```

不下载，不验证、不激活 generation，不创建 generation/data task（补发布除外，见下）。

active generation 可能已含新行情而 head 没发布：operator 用 `update_data` 代采后，或 Skill
激活 generation 后在 `run_market_update` 前中断。`plan_market_update` 的 `eligible` 看不出
这一点，因为它只看 head 的 `last_day`，目标日无源 bar 的实体（停牌、无成交）每天都在
`eligible` 里。所以判据是 active generation 激活后是否已有一次成功收尾的 A 股
`run_market_update`，取法全部来自现有只读工具：

| 事实 | 来源 |
|---|---|
| 当前 generation 的激活时间 | `sync_status.active.activated_at` |
| 之后是否有成功收尾的日线 run | `list_tasks(dataset=market.daily, operation=update_data, status=[succeeded, partially_failed], created_from=<activated_at>)`，取 `contract_version=market-plan-run-task-v1` 的项（显式 `update_data` 补漏任务不是） |
| 是 A 股 run 而不是 ETF 批次 | `get_task_progress.total > task.plan_counts.eligible`（A 股 run 追加 17 个内部派生 item，ETF-only 批次不追加） |

没有这样的 run 才调用一次 `plan_market_update`（`market.daily`，v1，不传 `end_date` 和 `scope`；
只读，不建任务）。`eligible>0` 就从 Skill §3.0 起按必做清单补发布（到 §3.1 重新 plan 后再 run），不经过 acquire/verify/activate。
判定只覆盖日线 run 本身；run 之后的补漏、ETF、年线步骤若被中断，这里看不到，由 operator 补做。

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

v7 起全市场日更改由 `plan_market_update`/`run_market_update` 完成（步骤与失败
路由见 [generic-data-v8](contracts/generic-data-v8.md) §7）。同一次日线 run 里，
worker 还会追加 **17** 个服务器内部派生 item（9 个指数基准 + 交易日历 + 5 个
全市场数据集：`derived:security_master`、`derived:security_names`、
`derived:risk_warnings`、`derived:suspensions`、`derived:trading_rules` + M3 的
`derived:industry_snapshot`、`derived:delisted_series_check`）；它们的 item 级结果只用于报告
（`entity`/`period` 为 null），不进入实体补漏清单。四个同源 item
（`derived:security_master`、`derived:security_names`、`derived:risk_warnings`、
`derived:suspensions`）共用 `X_DATA_MCP_SOURCE_STORE`；`derived:trading_rules` 仍按
`pending_source`。缺来源库时按部署形态区分：
- 默认策略（stdio、测试、`research`/`maint` 入口、legacy 后端）下**成功且写 0 行**
  （evidence 里带 `pending_sources`），只有 `error` 才是失败；
- 生产策略（`data_service` 的 ops 入口 + compact 后端，代码显式下传 Required）下
  变量缺失或路径不可读 → 每个 item **失败**，`error` 以稳定码
  `source_store_unconfigured` 开头，run 收尾 `partially_failed`；日线实体 item 已先行
  提交，上一版主表仍是 latest。空 store（首次部署）合法，只校验变量已配置且路径可读。
  四个失败是同一个配置问题，按错误码聚合，不要当成四个独立故障。
  配置正确的 store 里若没有**完整且未过期**的冻结集，item 以
  `source_snapshot_not_found` / `snapshot_incomplete` / `source_refresh_overdue` 失败：
  先跑 `refresh_exchange_sources`（每次日更 `tail`、每周 `full`，见下文「来源刷新」），
  不要用补漏或重试实体 item 处理。
- 启动可见性（3a）：ops + compact 启动时检查同一变量；缺失且未开 dev 豁免 →
  journal 一条以 `source_store_unconfigured` 开头的告警，**仍继续 bind socket 并服务**
  （research/maint 读面不受影响）。已配置时记录来源库根路径（不含密钥）；变量存在但
  路径不可读也只告警。Skill 看不到 journal，所以部署与巡检清单要把「启动日志无该码」
  当作配置完成条件。
- 怎么看：运行终态后按 `get_task_progress`/`get_task` 的失败项读 `error`；
  `source_store_unconfigured` 的四个同码失败按**一个部署配置问题**报告 owner/operator（不要只报一个、不要 `retry_task`、
  不要用 `update_data` 补漏），配置变量后重新 plan/run。日线实体 item 已先行提交，
  上一版主表仍是 latest，所以本次没有新 master 版本是预期结果。
- 显式开发豁免 `X_DATA_MCP_ALLOW_UNCONFIGURED_SOURCE_STORE=1` 时回到
  `pending_source`，并在启动时（ops+compact）或首次 Required 使用时打印
  non-production override；它只限本地验收，部署单元不设置，Skill 不设置或绕过
  `X_DATA_MCP_SOURCE_STORE`。

tool 面
（`get_task_progress`/`get_task`）只暴露失败项，成功项的取值按 Skill §4 用
`list_data_versions` 回读（M3 item 写的是索引表而不是版本行，取值看 evidence）。
M3 的 `derived:delisted_series_check` 是**只读**检查（不写业务表）：它在实体 item
之前登记「主表已退市（`delisting_date ≤ today`、非代码复用歧义）且有**不复权**日线序列」
的实体，把 `record_count`/`first_day`/`last_day`/`content_digest` 冻结进自己的 item，
实体 item 跑完后逐个比对。唯一允许的变化是补采尾巴（`last_day` 前进到退市日以内、
首日不变、条数增加，且登记以来的事件全是 append）；其余（序列消失、变短、同区间换内容、
越过退市日——含「登记值本身末根就晚于退市日、本次没变」——或改写事件）→ item 失败并在
error/evidence 里列出实体，run 收尾
`partially_failed`，**不重建、不删除**；被源改写的历史走 `republish_data`。
退市日未知或代码被两个发行主体复用的实体不登记，只写进 evidence 的 `skipped`。
首次登记发生在写主表那一代之后的一代，所以升级后的第一代日更 `checked` 可能为 0。
同一代日更的 eligible 集是「当前目录 ∪ 上一版目录成员 ∩ 有首次采集标记」：目录成员
消失（核实放行后）不会让退市日当天的最后几根 bar 掉出 plan。M3 的
`derived:industry_snapshot` 只维护行业快照与区间：来源不带
`tdxhy.cfg` 时成功且 `no_change`；文件存在但解析失败或缺字典 → item 失败，
字节已先落来源库、上一版区间不丢。同一内容被再次观测（前一条登记内容相同且带发布方时间）只是
延长未闭合区间、`no_change`；内容变回旧内容（A → B → A）是新的一次变更，会开新区间。
没有发布方 `last_modified` 的登记不算观测（只证明我们持有这份字节），该内容已在表中
时直接跳过；若这条观测会改变**已公布**区间的起点，日更 fail-closed 指向
`source_store replay --scope industry`，不要手工改表。

### ETF 批次（M3-2 PR-C2；v10 起日更 Skill 默认包含）

ETF 日更是 `plan_market_update`/`run_market_update` 的一个可选范围，不是新工具，也没有新的
编排脚本：仓库里没有日更 plan/run 的脚本，编排在 Skill/操作员侧，所以这里只定义调用形态。
**A 股批次不变**：不传 `scope` 时 plan、`plan_id`、item 与结果与 v9 逐字节相同。

v10 起日更 Skill（`.devin/skills/x-data-daily-sync/SKILL.md` §3.3a）在 A 股日线与补漏
**全部终态之后、年线之前**追加一个独立 ETF 批次（`scope:"sh_sz_etf_current"`，分任务形态，
不用 `sh_sz_a_and_etf_current`）；操作员手工编排时同样在 A 股批次之后追加：

```text
plan_market_update {dataset: market.daily, contract_version: v1, end_date: D, scope: sh_sz_etf_current}
→ run_market_update {…, request_id, target_day, plan_id, scope: sh_sz_etf_current}   # scope 与 plan 相同
→ run_worker_once / get_task_progress（同 A 股批次）
```

- 成员：`sh_sz_etf_current` ∩ 已有 ETF 不复权序列。日更 plan 不会把没有序列的 ETF 列为 eligible，
  所以 Skill 在 ETF 批次终态后补发一次 `start_collection`
  `{dataset:"market.daily", contract_version:"v2", request_id, selection:{kind:"scope", scope:"sh_sz_etf_current"}}`
  收新上市成员（只收尚无序列且 `.day` 有 bar 的成员，幂等；无新成员时返回 `nothing to collect`，属正常结果）。
  一次性全量首采（`sh_sz_etf_listed_ever`，含退市 ETF）仍由操作员显式发起，不属于日更。
  退市 ETF 不在 current，不进日更。
- 同一时刻只能有一个未终态的 `market.daily` 更新任务（`plan_in_progress`），所以 A 股批次终态后
  再发 ETF 批次。想同任务就用 `scope: sh_sz_a_and_etf_current`（含 17 个派生 item）；ETF-only
  任务**不**追加派生 item。
- ETF item 的结果：`no_change` / `metadata`（只有事件摘要变化）/ 追加尾部 bar；非前缀改写以
  `etf_history_rewrite` 失败并给出首个差异日，操作员按「ETF 不复权历史不应变」调查来源，
  不用 `update_data` 补漏或 `retry_task` 重试。ETF item 失败不影响同任务的 A 股 item。
- 年线：`market.yearly` 的 plan 不含 ETF，也没有 `scope` 参数（D-70 Q6）。
- 验收读回：`query_data` 带 `adjustment: raw`（ETF 省略口径会报 `adjustment_required_for_etf`），
  3dp 与 `last_trade_date` 应前进到目标日。
- 耗时：SLO 第 1 行的 A 股口径不含 ETF；ETF 单任务、同任务、分任务三种形态在上线任务里按
  [generic-data-v10](contracts/generic-data-v10.md) §1.7 的参数形态各测一次，未测不写 PASS。

### 来源刷新（v9：`refresh_exchange_sources`，四个同源 item 的输入）

v9 起四个同源 item（`derived:security_master`、`derived:security_names`、
`derived:risk_warnings`、`derived:suspensions`）**不再自己发请求**：它们只读
`X_DATA_MCP_SOURCE_STORE` 中由 `refresh_exchange_sources` 冻结的快照，并要求该来源最近
一次**完整**刷新距今不超过 `H=14` 天（超过即 `source_refresh_overdue` fail-closed，旧版本
仍可读）。刷新任务只写来源库/索引与运行记录，不发布数据集；日更的请求预算与「先刷新、
再派生」的顺序由 Skill 负责：

```text
（每次日更，capability revision >= generic-data-v9）
→ refresh_exchange_sources {"mode":"tail"}          # 开窗 + 上一自然季度；整表类重读整表（深市上市/简称先探页 1）
→ 拿到 {run_id,status:"running"}（带计划，计数为 0），用 {"status_of":run_id} 轮询到终态
→ 读终态 run 记录：requests/reused/changed/failed_windows/unobserved_windows/verified_through/失败列表
→ plan_market_update → run_market_update（四个同源 item 读本次冻结集）
→（每周一次，放在当天 run 终态之后）refresh_exchange_sources {"mode":"full"}
→ 用 {"status_of":<run_id>} 轮询到 succeeded/failed
```

- `tail` **异步**（#582）：调用只校验并写 run 记录，立即返回 `{run_id,status:"running"}`，
  抓取在本进程后台线程（客户端断开不影响）；用 `{"status_of":run_id}` 轮询到
  `succeeded`/`failed`。**原因**：tail 的 28 个请求约 40 s，探针触发整表时最多 549 个
  （1 req/s 下约 13 分钟），而客户端预算是 60 s、一个连接只有一个线程；同步跑会把该连接上
  后面的调用全部堵住，超时后又没有 `run_id` 可取回结果。`running` 期间的计数是 0，不是进度；
  `status_of` 不返回 per-source 行与 `revision`（只有 run 行与失败列表是持久记录）。
- **每周一次 `{"mode":"full"}`**：窗口类来源补齐全部季度、整表类来源重读整表，并推进
  `verified_through`。**调用位置**：当天日更 `run_market_update` 终态之后（或单独在低峰跑），
  不要插在 `tail` 与 plan/run 之间；`full` 只写来源库/索引与 run 记录，不发布数据集，
  下一次日更才读新快照。`full` **异步**：调用立刻返回 `{"status":"running","run_id":…}`，抓取
  在本进程后台线程（客户端断开不影响）；用 `refresh_exchange_sources({"status_of":run_id})`
  轮询到 `succeeded`/`failed` 再继续。`running` 期间的计数是 0，不是进度。`tail` 只覆盖开窗
  与上一自然季度，不能单独让闸门长期通过。`full` 放低峰（周末候选）。
- 返回 `refresh_already_running`：已有运行中的刷新（`tail` 与 `full` 互斥），读
  `details.run_id` 并轮询**那个** run 到终态，不要再起新 run。
- 预算：`tail` <300s（仅未触发整表时）、每张被触发的整表各自 <600s（D-67，run 总量不设
  门禁，按表读 `source_refresh_run_sources.elapsed_ms`）、`full` 按来源组计量（D-73：
  原有 7 来源合计 <60min、ETF 4 来源合计 <40min、run 总量不设门禁，按来源读
  `source_refresh_run_sources.elapsed_ms` 之和）；1 req/s 不变。
- **全新数据库（fresh init）**：**首次日更前先跑一次 `full`**（顺序：`full` → 首次 `tail` →
  `plan`/`run`）。理由不是避免 `source_snapshot_not_found`（fresh init 的 `replay --scope all`
  已把交易所索引写回，item 走 fallback 选择），而是**页集身份**与诚实的 `verified_through`：
  没有刷新 run 记录时，选择器按页号跨抓取日拼接，只有 `full` 的 run 成员表能固定页集身份，
  并把 `verified_through` 推进到本次实际抓取日。
  另外 fresh init 后 `sh_sz_a_listed_ever` 首采依赖**已发布的 `security_master`**：
  `market.security_master` 由日更 run 的 `derived:security_master` item 维护，fresh 库里
  还没有已发布版本时该 scope 会报 `security_master has no published version`；
  要先跑一次 `market.daily` 的 plan-run（让派生 item 发布主表）再发 listed_ever 首采。
- 这四个 item **零请求、零探针**（含深市上市/简称的页 1 探针）：整表身份来自刷新 run 的
  成员页，日更不得与刷新任务叠加出 2 req/s。刷新任务的单飞闸门只看刷新，看不到日更
  worker，所以这条由回归用例守（计数假传输断言日更跑完 0 次尝试）。
- `source_refresh_overdue` → 先 `full`（返回 `refresh_already_running` 就轮询那个 run），
  再重跑 `plan_market_update`/`run_market_update`。
  不要用 `update_data` 补漏，也不要把它当成实体 item 重试；上一版主表/数据集仍是 latest。
- `source_snapshot_not_found` / `snapshot_incomplete` → 同样先 `full`；`snapshot_incomplete`
  还可能来自页集合不完整（缺页、页数或行数与发布方计数不符），`full` 重跑一次仍失败则报
  owner/operator。
- v8 运行时的 `ops` 入口没有该工具（item 内联获取），本步跳过；v9 运行时该工具缺失是
  `SKILL_CONTRACT_MISMATCH`。
- G2 的读数与 run 表查询步骤见 [来源刷新 runbook](source-refresh-runbook.md)。

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

v9 下首次日更 `run_market_update` 前还要先跑一次
`refresh_exchange_sources {"mode":"full"}`（见上文「来源刷新」）。preview.42 的 fresh init 顺序即
`full` → catalog → `market.daily` → 首次 `tail` → `plan`/`run` → listed_ever → 年线（XDM-165）。

### 退市股一次性首采（操作员发起，非日更步骤）

退市股没有 daily head，日更 `plan`/`run` 不会把它们列为 eligible。M3 PR-4 提供显式
一次性入口，只在操作员明确要求时执行（典型时机：部署/升级后一次），不进日更 plan，
日更 AI 不得自行发起：

```text
start_collection{dataset:"market.daily", contract_version:"v2",
  selection:{kind:"scope", scope:"sh_sz_a_listed_ever"}}
```

服务端解析 `security_master`（沪深、`instrument_class=0`、无 series）∩ 当前 generation
的 `.day` 成员，预算 1..500；无 bar 成员跳过并记入证据，已有 head 的实体跳过
（重复提交只采缺失项）。item 级发布/验证语义与 v1 首采相同。`securities.catalog`
读写都拒绝该 scope。日更 Skill 的完整步骤见
[.devin/skills/x-data-daily-sync/SKILL.md](../.devin/skills/x-data-daily-sync/SKILL.md) §3.9。

### 结果不明

- Acquisition timeout：按原 acquisition request/task ref 查询，不更换 ID 盲目重发。
- Data acceptance timeout：按原 request ID 查询绑定。
- Publication outcome unknown：调用 publication verification；不把超时当失败。
- Started task 停止状态不明：使用 stopped-write evidence 流程；不强制释放占用。

Acquisition crash 恢复现状语义（PAR-2 实测，测试见 `acquire_crash_points`）：

- evidence 已写而 decision 未写的 verify 中断可幂等重入：字节一致的残留 evidence
  视为已写并继续写 decision；字节不一致则 fail-closed（`different bytes`），不覆盖、
  不删除，需人工核查该 generation 目录。
- request 行已提交而下载 task 行未写时，该 request_id 永久返回
  `request_binding_lost`——这是确定终态，不是半成品；调用方必须换新 request_id
  重新发起 acquire。
- install rename 已发生而 task 记 failed（UnverifiedStage）时，落盘的 generation
  目录在下次服务 open 时被收养为 `byte_verified`，可继续 verify/activate——
  “任务 failed 但产物成功”是机械可判定的现状语义，不是失败信号。

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

> **历史（preview.29 时期，2026-09 口径）**：下表保留当时的验收记录，不再代表当前部署或当前门禁。
> 当前性能与容量目标见 [SLO](SLO.md)，当前部署与运行事实见 [STATUS](STATUS.md)。

| 能力 | 实现/测试 | 真实证据 | 当时裁定（历史） |
|---|---|---|---|
| immutable generation + verify/activate | 有 | 本机真实 generation 全生命周期 | PASS |
| generation-bound daily update | 有 | 全市场真实 generation | PASS |
| generation-bound yearly update | 有 | 全市场真实 generation | PASS |
| 全市场 collection/update | 有 | 分批 MCP 编排：catalog 5,227、daily 约 340s、no-change update 5,222 items 约 616s | PASS（batched） |
| 首采 DB ≤2.5GiB、部署 ≤4GiB、首采 ≤20min | 有 | 历史（preview.28 部署包口径）：DB 约 831MB、首采约 340s | 历史 PASS；当前口径待定，见 [SLO](SLO.md)，当前事实见 [STATUS](STATUS.md) |
| 同代 no-change `update_data`（≤15min、≤4KiB/item、`data_versions +0`） | 有 | preview.27/schema20：5,222 items / 616s / +18.5MB / 0 new versions | PASS |
| 跨代 envelope / 真实 append·rewrite | 有实测 | cross-gen 约 37min（preview.29 measured baseline，无批准 hard SLO）；#320 后 proven append 可走 typed metadata path，rewrite/unsupported metadata 仍 envelope/fail-closed；typed 全市场生产 timing 待测 | measured / 不外推 |
| 全新 AI 客户端会话仅 Skill+MCP | Skill 文件存在 | 无独立会话证据 | 未验收 |
| 双 MCP 可复现部署 | release 双 binary + `deploy_phase_g.sh` | 历史（preview.29 时期）：双 systemd 服务在运行 | 历史 PASS；当前部署见 [STATUS](STATUS.md) |
| overall production readiness | capability 恒实报告 false | owner-only declared scope 可观测 | owner-only operational；external distribution not authorized |

## 验收

#260 完成至少需要：

1. A source session 在 B 下载/激活后仍读取 A immutable bytes，不出现 A manifest + B bytes。
2. Mixed、缺失、损坏或无显式兼容证据的 generation 不激活。
3. Acquisition 每个 crash point 可按 request identity 恢复确定事实。
4. 从 verified generation 到全市场 daily update 的 changed/unchanged/修订分支均可仅通过 MCP 完成。
5. Tools/list、capability、handler 和 examples 来自同一 descriptor；广告分支有正向契约测试。
6. 全新 AI 客户端会话仅调用项目 Skill 完成 unchanged/changed 流程，不读源码、不用 shell。
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
