# 开放 Issue 整理（2026-11-05）

关联：[#296](https://github.com/liqiangcc/x-data-mcp/issues/296)（治理收口）。基线：main `926647b`（CI success）。

## 0. 范围与读法

- 读了 16 个开放 Issue 的正文和全部评论、相关 PR、本仓库文档与代码。**没有改任何 Issue，没有改代码，没有访问线上。**
- 线上读数我没读。依赖线上的结论引用 [STATUS](../STATUS.md)、[SLO](../SLO.md) 里 Claude 2026-11-05 只读核实的数字（owner 未复核），并标「需线上核实」。
- `gh issue view` 因 Projects (classic) 弃用报错，正文和评论改用 REST API 读取。
- 「现状」写已完成的（PR、提交、文档位置）和还剩什么。「建议」只用三个词：**可关闭**、**改写范围**、**保留**，并写依据。
- 日更和主数据已在 vm2-126（D-74）。Issue 里写 aws-node、ecs-node 的地方按过时处理，见 §5。

## 1. 总表

| # | 标题 | 现状 | 建议 |
|---|---|---|---|
| 1 | 数据平台：核心目标与首期范围 | 子任务 #2–#7 已关闭，首期范围已在生产形态交付。剩 #308、#446 的扩展项，另有两个 AC 悬空：稳定验证入口（`README.md:173` 写 `./scripts/verify` 尚未实现）、访问边界（`STATUS.md:262` 写待 owner 明确）。正文仍写「NOT_RUN」「性能待确认」 | **保留**到 #446、#308 收口，正文顶部改写为当前状态（O8） |
| 246 | catalog 收录扩展：北交所/基金/指数/B 股 | 指数 9 条（D-27、D-31）和 ETF（#509）已收，债券已定排除。北交所、B 股、LOF 未收，数据目录列为「不在范围」。正文「现收 5,226」过时 | **改写范围**为「北交所/B 股/LOF 是否收录」（O7） |
| 260 | MCP 每日同步语义闭环 | 10-13 核对 35 条 AC 中 30 条满足、1 条被取代，剩 AC-R03（冷归档范围）和 Skill-only `day=changed`（#440）。复选框全未勾，正文仍写 ecs-node | **保留**；O3 裁决 R03、#440 通过后**可关闭** |
| 262 | Epic：v15 紧凑存储 + generation 闭环 | 9 项 DoD 只剩 DoD-7（#440）。正文含 ecs-node、已被取代的 SLO（5min/16MiB/4GiB）和一个内网地址 | **保留**；与 #260 同时、#440 通过后**可关闭**。先清正文（O12） |
| 268 | 只读运维观测 Console | 只有 `web-console/` fixtures 原型（2026-09-22）和与正文逐字相同的 `docs/tasks/issue-web-console-draft.md`。前置 #259 已关闭，无后端、无排期 | **保留**（延后）；与 #295 解耦（O5） |
| 295 | `query_data` 全市场筛选慢 | 无进展。测量（XDM-14）排在 2026-11-09 之后（D-75）。正文基线 schema 24 / v8 过时；v8 已有 `cross_section`，#525 将加 `get_market_snapshot` | **保留**，正文基线改写；O5 |
| 296 | 治理：统一当前事实 | 残余 5 项：1、2 等 O3 与 #440，3、4 是小改动，5 已满足。正文基线（preview.37、15 工具）过时 | **保留**；残余完成后**可关闭** |
| 304 | 连续 10 个交易日 Skill-only 验收 | 有效 PASS 3/10，全是 `day=unchanged`。11-05 是第一次真实有更新日，STATUS 记录执行模型提前结束、经提示补步；按 #440「无人工补步」不能算 PASS | **保留**；剩余天数规则随 O1 改；计数关系见 §3 |
| 308 | 历史风险警示状态来源准入 | Final Gate 已由 D-58 按市场分档；写入器 #467、`known_at` #498、沪市标题分类 #503 已合并并部署。剩：replay 的 105 行 `source_document_sha256` drift（无 Issue 跟踪）、AC 未逐条对账、证据目录缺失 | **保留**；drift 修复并线上核实后**可关闭** |
| 387 | v8：面向 AI 的 MCP 工具面 | 本 Issue 范围「设计审核 + 拆出实现」已完成：6 个实现 PR 合并，preview.36 起部署，Q1–Q6 全部裁决（D-35…D-40、D-53、D-56） | **可关闭**（O4）。未验证项转 §2 #387 |
| 433 | M2 后续 | 6 项中 1、2、4 已完成，3 的数据已入库。剩 5（可交易性/涨跌停/市值读面）和 6（AC 文字修订） | **改写范围**为这两项（O6） |
| 440 | Skill-only `day=changed` 闭环 | 0/1，无进展。边界 3 与现行 Skill 规则冲突（O2）；边界 2 已有实测和测试，不必人为制造 | **保留**，改写基线和边界；O2 |
| 441 | 冷缓存读取 5.2–6.5 s | 无进展。根因（`series_bars` 无 `trade_day` 索引）在 schema 27 仍成立。测量主机不是 vm2 | **保留**；在 vm2 复测，<5 s 则**可关闭**（O11） |
| 446 | M3 退市股、历史行业、交易所名单 | 实现 PR-1…12 与 13a 已合并，preview.39 起部署，Q1–Q8 已裁决（D-62）。AC-M3-1/2/3/4/7/10 已执行；5、6、9 部分；8 无去向 | **保留**；补 §2 #446 的证据后**可关闭**（需线上核实） |
| 509 | M3-2 ETF 日线收录 | 设计 #510，实现 #511–#518、#521，vm2 preview.45 已部署并完成首次 ETF 日更；AC-M3-2-1…10 均有对应实现或文档 | **可关闭**（需线上核实 2 项，见 §2） |
| 525 | 读取工具按语义拆分 | 设计已合并（#526，D-75），评测脚本已合并（#530）。PR-1…3 未开始，评测在 11-09 之后 | **保留**（在办） |

建议关闭的汇总见本文末。

## 2. 逐项说明

### #1 数据平台

- **已完成**：#2–#7 全部 CLOSED。首期范围（目录、日线、年线）10-13 评论已记为在生产形态交付。AC-05 的性能门禁已由 [SLO](../SLO.md) 签字（D-01：write-loop <600s）。
- **剩余**：
  - AC-10 要「稳定验证入口」。`README.md:173` 写明 `./scripts/verify` 尚未实现，现在的入口是 CI 的命令清单。
  - AC-09 要「仅授权访问」。`STATUS.md:262` 写明当前边界是否可接受「尚待 owner 明确决定」。
  - 「后续证券状态扩展」（ST、退市、退市股行情）由 #308、#446 承接。
- **正文过时**：「业务实现、测试、性能和部署验证：NOT_RUN」；待确认表里的「性能指标待确认」「历史时点可见性待确认」（M2 已做 `as_of`/`known_at`）；东财行业（09-11 评论已改 TDX 研究行业二级，正文未改）；「当前服务器」。
- **建议**：保留。正文顶部加「当前状态」段，指向 STATUS、SLO、data-catalog。收口时点见 O8。

### #246 catalog 收录扩展

- **已完成**：指数基准 9 条（`market.index`，D-27、D-31，v8 PR-4 #410）。ETF 由 #509 收录，**不进** `securities.catalog`，走 `CatalogScope`（D-70 Q5）。债券 09-20 评论已定排除。
- **未收**：北交所 610、B 股 176、LOF/封闭式/REITs。[data-catalog](../data-catalog.md) §3.2 #16 写「不在范围」，#509 的非范围同样列出。
- **正文过时**：「12,423 成员现收 5,226」。现状见 STATUS「数据」（A 股、退市 338、指数 9、ETF 1,760 条序列）。三条 AC 未勾：第 3 条「catalog 发布验证扩到新成员数」对 ETF 不适用（走 scope），需改文字。
- **建议**：改写范围。标题和 AC 缩到北交所、B 股、LOF。O7 请 owner 定是否收。

### #260 MCP 每日同步语义闭环

- **已完成**（10-13 评论）：35 条 AC 中 30 条满足、1 条被取代。崩溃点矩阵 #336（`tests/service/acquire_crash_points.rs`）。自动保留 `RETAINED_SUPERSEDED=2` 随 preview.35 部署（`src/acquire/service.rs:44`）。热 generation 上限有测试：`ten_daily_updates_never_hit_generation_capacity`、`hot_capacity_refuses_new_candidates_without_deletion`（`tests/service/acquire_service.rs:772,802`）。
- **剩余**：AC-R03（退役后区分 hot/cold/evidence-only/unavailable）等 owner 裁决；`day=changed` 证据等 #440。
- **AC-R03 的现状**：代码只有 `active`/`superseded`（有字节）和 `evidence_only`（无字节）两层，`sync_status.generations[].state` 如实返回，没有冷归档层（`src/acquire/service.rs:67-74`）。如果裁决「不建冷归档」，R03 按现有行为即满足（O3）。
- **正文过时**：「在 ecs-node 上通过 Devin/MCP」；35 个复选框全未勾；AC-C09 写「实时 22-tool」，现为 25 个 data 工具 + 10 个 acquisition 工具（`check_current_facts.py` 输出）；评论里 ECS 出现约 24 次。
- **建议**：保留。勾选已满足项；O3 裁决后，#440 通过即可关闭。

### #262 Epic

- **已完成**（10-13 评论）：DoD-3 由 #336 覆盖，DoD-8 部署体积 2.3G，生产已全新重建。唯一未满足的是 DoD-7（Skill-only `day=changed`），由 #440 跟踪。
- **正文过时**：
  - 主机与环境：ecs-node、「ECS 2 vCPU/约 2GiB」、本机 8 核、官方输入路径。
  - 目标值：「部署 ≤4GiB」已是 ≤8GiB（SLO 第 5 行）；「日更 ≤5min、增长 ≤16MiB」09-28 被取代，现为 SLO 第 1–3 行（write-loop <600s、增长 ≤4KiB/item）。
  - 实现路线：「v15 紧凑存储」已被 D-09 的 latest-only（schema 21 起）取代。
- **公开仓库卫生**：正文第 40 行含一个 tailnet 地址。本文不复制。`scripts/check_public_hygiene.py` 的规则会命中它（我用该脚本的 `scan_text` 扫了 16 个 Issue 的正文和评论，只有这一处）。建议 owner 删除，并清掉编辑历史（O12）。
- **建议**：保留，与 #260 一起在 #440 通过后关闭。

### #268 只读运维观测 Console

- **现状**：`web-console/`（809 行 JS，仅 fixtures，2026-09-22）；设计稿 `docs/tasks/issue-web-console-draft.md` 与 Issue 正文逐字相同（5,271 字符）。前置条件「#259 硬门通过」已满足（#259 09-28 关闭）。无后端端点，没有排期。
- **与 #295 的矛盾**：#295 和 D-19 写「服务端筛选随 #268 一起评估」，而 #268 契约第 7 条明确「不提供 ad-hoc 全市场行情扫描」。两处不能同时成立（O5）。
- **正文过时**：「ECS 2 vCPU/~2GiB」。
- **建议**：保留（延后）。不建议现在做：当前使用方是 AI 客户端，[调用日志](../call-log.md)（`XDM_CALL`）已有运维可见性，且 vm2 到期去向未定（约 2027-01-04）。

### #295 `query_data` 全市场筛选慢

- **现状**：决策单，等测量。D-19「先测量」未做；XDM-14 排在 2026-11-09 之后（D-75、[#525 设计](issue-525-read-tools.md) §10）。
- **正文过时**：基线 schema 24 / `generic-data-v8`；现为 schema 27 / v10。383 s 来自 preview.28/schema 20，不得引用。v8 已交付 `filter.kind=cross_section`（单日全市场、`fields` 投影），#525 将加 `get_market_snapshot`，「research 只有 5 个工具」很快不成立。
- **建议**：保留，正文基线改写。「随 #268 一起评估」一句改掉（O5）。

### #296 治理（本单）

| 残余 | 现状 | 缺口 |
|---|---|---|
| 1 AC-R03 | 未裁决 | O3 |
| 2 DoD-7 | 等 #440 | 见 #440 |
| 3 热 generation | Skill §2 第 7 条（`.devin/skills/x-data-daily-sync/SKILL.md:136`）、[daily-sync](../daily-sync.md)「`generation_capacity`」段、STATUS 已写：Skill 无权退役，只报告并交 owner/operator；§6 把受理拒绝列为停止条件，不重试 | STATUS 没写「被拒后不重试」的验收口径；vm2 有一个旧代 `5ac3ef2b…` 因 `derived_facts_missing` 没被自动退役（STATUS「数据」），稳态上限（STATUS 写「最多稳定在 5/6」）是推断，未实测。需线上核实 |
| 4 工作流状态措辞 | STATUS 已改（`STATUS.md:436-437` 写 plan/run 已实现并上线）。同类漂移在别处：`docs/generic-data-tools.md:49-50` 仍写 `run_market_update`「未部署」、`refresh_exchange_sources`「v9 代码，未部署」 | `check_current_facts.py` 的 `CURRENT_REGIONS` 只校验版本、schema、工具数、revision，没有工作流状态断言。要不要加，本单还没决定 |
| 5 性能尾部独立 | 满足：#295、#268 仍独立开放 | — |

正文基线（preview.37、schema 24、`generic-data-v8`、ops 15 工具）过时。现行值：preview.45、schema 27、`generic-data-v10`、ops 16 / research 5 / maint 9 / acquire 10。建议保留；残余 1–4 完成后可关闭，关闭条件里的「五个事实面一致」靠 §6 的清单逐项核。

### #304 Campaign

- 计数 3/10：Run 3（09-28）、Run 5（09-29）、10-08，全是 `day=unchanged`。10-13 的 a/b/c 三次不计。提示词 `daily-ai-sync-v2` 的 SHA-256 与 `.devin/evals/daily-ai-sync-v2.txt` 一致（我重算了）。
- Skill 自 v2 提示词冻结后已变（v10 钉值 #524、必做步骤清单 #528），提示词未变，符合「正文不得原地修改」。
- 11-05 是第一次真实有更新日。STATUS 记录执行模型（SWE 2 Flash）提前结束、漏步骤，经提示后走完。STATUS 没写这是不是全新会话、是否只用 Skill。按 #440「无人工补步」不能算 PASS；记 FAIL 还是 INCONCLUSIVE 要读 transcript，我没读。
- 正文过时：正文没有写版本号，基本不过时。需要改的是「剩余 7 天须含至少 1 个 `day=changed` 日」，随 O1 重算。
- 建议：保留。计数关系见 §3。

### #308 历史风险警示状态

- **已完成**：
  - Stage A–C 评论（09-28 至 09-29）。
  - D-58（10-28）：深市 `SSGSGMXX`/`tab2` 为 `APPROVED_SOURCE`；沪市公告索引事件级为 `APPROVED_SOURCE`，生效日 `OWNER_RISK_ACCEPTED`；沪市 2000 年前和北交所 `NOT_AVAILABLE`；无公告一律 `unknown`。
  - 写入器 #467；`known_at` 与 coverage 两句口径 #498（D-66 Q4）；沪市标题分类 D-68/#503。
  - 线上：preview.42 起 1,289 行，D-68 后沪市 884 行区间（aws-node 读数，STATUS）。沪市实际可观测起点是 2004Q1，2000–2003 的 16 个季度是 `not_observed`（data-catalog §3.2 #9、§3.3）。
- **剩余**：
  1. `replay --verify-only` 的 `risk_warnings` drift：vm2 上 105 行沪市记录的 `source_document_sha256` 不同，原因是同一公告出现在多个查询页时 replay 与日更的页顺序不同；STATUS「已知问题」写「修复待开发」，没有 Issue。这正是 AC-E01「从官方原始字节重放」的一致性，建议并入本单。
  2. 12 条 AC 全未勾，与 D-58 的对应关系没写（AC-T02「同日公告、生效次日、盘中未知、更正公告」我在 docs 里没找到对应规则文字，需作者确认）。
  3. 正文 dataset 名 `securities.status_history` 与实际 `market.risk_warnings` 不同。
  4. 评论引用的 `evidence/issue-308-tdxgp-gp29/` 在 origin/main 里没有，评论里的 12 个证据提交号在本克隆里也查不到。我没有逐个查其他分支，是否另存别处需作者确认。
- **建议**：保留。drift 修复并线上核实重放 `match` 后可关闭。

### #387 v8 工具面

- **本单范围**：正文写明「只到设计经审核 + 拆分出实现任务为止」。已完成：设计 #388；实现 #393、#403、#405、#410、#409、#414；多 socket #434、#437、#438；Q1–Q6 裁决 D-35…D-40、D-53、D-56；preview.36（10-27）最小切换，preview.38 单进程三 socket，Ops-D（隧道改指 `research.sock`）11-02 执行。
- **依赖的数字已被后续裁决取代**：AC-V8-1「研究 5 工具 ≤11,000 B」现为 ≤12,200 B（实测 12,021 B），D-75 实施后将是 15 工具、18,600 B；AC-V8-2/13 的 15/24 工具现为 16 / 25。#387 的数字 AC 不应再作为验收口径。
- **未验证 / 未做**（转入 `v8-switch-checklist` §1、§9.6 和 #525 §9 的评测，不阻塞关闭）：
  - AC-V8-9 的 Devin eval 没跑（Devin 不可用），Ops-C 改用 pi 实测，结论 `summary` 不作缺省（D-36 退回，#447）。
  - T0–T8 只有夹具实测（`scripts/v8_token_budget.py`），无真实全市场截面的字节与耗时。
  - Ops-E（maint 启用）未开始；v7/v1 分支退役评估（switch-checklist §1.1）未做；MCP `export_data` 只设计（D-39）。
- **建议**：可关闭（O4）。依据：本单范围已交付，数字 AC 已被 D-75 和契约预算取代，未做项已在 switch-checklist 有归属。

### #433 M2 后续

| 项 | 现状 |
|---|---|
| 1 交易所名单适配器 | 完成：#458、#459，preview.39 起部署。337 个退市行全有日期；`code_migrations` 109 行（108 candidate + 1 confirmed）；`sh.600018` 已分 2 行（STATUS） |
| 2 历史 ST | 完成：见 #308 |
| 3 交易规则 | 数据完成：27 行涨跌停规则 + 2 行最小变动，`verified`（#450、#454、#456，D-64）。派生函数 `research::price_limit` 在，但只被库内测试用 |
| 4 简称历史 | 深市完成（7,344 行，D-61）；沪市、北交所 `NOT_AVAILABLE` |
| 5 读面 | **未做**。`research::tradability`、`market_cap`、`price_limit` 无任何 MCP 入口或导出调用方（`src/research/mod.rs:760` 注释说明 `price_limits.parquet` 不写）；#525 的 `get_security_state` 只返回事实，不含派生判断，`get_price_limit_rules` 只返回规则行 |
| 6 AC 文字更新 | **未做**。#391 正文（已关闭）仍写「cat5/7/8 不出现在任何状态里」（D-50 已准入）和「`as_of`/`known_at` 仍被 v8 拒绝」（v8 已接受） |

- 第 5 项是 D-30 ③「可交易性可判定」和 [data-catalog](../data-catalog.md) §4「做不了：严格可交易性回测」的缺口，不能降级。
- 正文过时：「preview.37」；「279 个实体退市日未知」「全部 rule_pending」。
- 建议：改写范围，只留 5、6（O6）。

### #440 Skill-only `day=changed` 闭环

- **现状**：0/1，Issue 创建后无评论。
- **边界 1**（GBBQ-only 或 base-only changed）：#303 修复后没有同类事实的实测（Run 3、Run 5、10-08 三文件均 unchanged）。需遇到该事实组合。
- **边界 2**（热 generation 6/6）：已有实测（10-13 Run b，`generation_capacity` 后整体停止、未写任何东西，见 #304 评论）和测试。**不要为验收人为制造 6/6**：需要多拉候选代，对生产有副作用。STATUS 推断稳态最多 5/6，不会自然到 6/6（推断，未实测）。
- **边界 3**（已有 active generation 但 heads 未发布）：与现行 Skill 冲突。Skill §1（`SKILL.md:109-112`）规定 `day` 不在 `pending_roles` 就立即结束、不得再做 catalog/market 只读探测，这是 Run 4 FAIL（#311）后定的规则。而 10-08 的缺口恰是 operator 代采后 active generation 已含 10-07 行情、heads 仍停在 09-29，`no_update` 不覆盖（10-08 评论写「评估中」，follow-up Issue「待定」，至今没开）。Skill 激活后中断同样会落进这个缺口。按现行 Skill，边界 3 不可能通过（O2）。
- **正文过时**：基线 `generic-data-v8`/schema 24/「ops 15 工具」；现为 v10/27/16。
- **建议**：保留，改写基线，边界 2 改为「引用 10-13 Run b 与测试」，边界 3 等 O2。

### #441 冷缓存读取

- **现状**：#373 已于 10-28 关闭（PR #379：热缓存 2.0–3.8 s，冷缓存 `--workers 4` 5.2–6.5 s）。根因 `series_bars` PK 为 `(series_key, trade_day)` 的 WITHOUT ROWID 表、无 `trade_day` 索引，我核对了 `migrations/tasks/021_latest_only.sql`、`022_research_data.sql` 及后续迁移，没有新增该索引，根因在 schema 27 仍成立。
- **测量环境过时**：数字来自 2 vCPU / 1.9GiB 的主机（aws-node 级）。vm2 是 2 vCPU / 约 7.6GiB，同脚本随机 4K 读 2,834 IOPS 对 1,619（D-74）。**推断**：随机读为瓶颈时 vm2 冷读会更快，未测。
- **与其他单的关系**：它读的是离线分析脚本（`scripts/analysis/boll.py`，D-19 允许的旁路），不是 MCP 在线读，与 #295 的 `screen_market` 方向独立。
- **建议**：保留。把 vm2 冷/热读并入 XDM-14 的同一轮测量；<5 s 则按「接受现状」关闭，≥5 s 再在两条路里选（O11）。需线上核实（vm2 只读复制库上测，不动生产数据）。

### #446 M3

- **已完成**：设计 #449；实现 #453–#468（来源库、共用交易所入口、行业、退市股首采、不消失检查、退役门禁、读模型、导出 v3、历史 ST、停牌、简称）；刷新 R1–R6（#491…#499）；部署 preview.39（10-31），preview.41–43 修复。Q1–Q8 已裁决（D-62）。
- **AC 现状**（设计 §14.12 第 7 条的执行状态表、STATUS）：
  - AC-M3-1、2、3、7、10：已执行。
  - AC-M3-4：§14.12 的执行状态表没有这一行，§14.3 仍写「未执行」。相关读数：preview.42 部署时 `replay --scope all` 的 industry 为 7 快照 / 5,235 assignments、`database=match`（STATUS）。是否等于 AC-M3-4 要求的 `source_store import` 场景，我没有核到，需作者确认。
  - AC-M3-5：四种场景里，缺派生事实在 vm2 上自然出现（`5ac3ef2b…` 未被自动退役，`derived_facts_missing`）；`retire_gate_unconfigured` 与 `source_store_evidence_missing` 已在 preview.39 复现（§14.12）；owner 豁免用于 11-01、11-02 退役的几个代；「blob 不可读」我没找到记录。
  - AC-M3-6：首轮 `checked=0`；11-01 第 1 代日更 `checked 337 / unchanged 337`（STATUS）。设计要求的「两代对照读数」没回填。
  - AC-M3-9：除 `risk_warnings` 的 105 行 drift（见 #308）外 `match`。
  - **AC-M3-8**（跨机逐位复现）：设计 §14.1 标「建议另拆 Issue」，至今没有 Issue。它是 D-30 ④「结果可复现」的一部分，不能悄悄放弃（O10）。
- **正文过时**：「设计任务 XDM-91 进行中、设计待审核」；Q1–Q8「待 owner」（已裁决）；schema 25（现 27）。
- **建议**：保留。补 AC-M3-4/5③/6/9 的读数、定 AC-M3-8 去向后可关闭。需线上核实。

### #509 M3-2 ETF

- **已完成**：设计 #510；PR-A #511、A2 #513、B #514、C #515、C2 #516、D #517、首采分批 #518、SZSE 公告日期修复 #521、日更补采 Skill #528；D-70…D-73；发布 preview.44（11-04）和 preview.45；vm2 部署（11-05）。
- **AC 对照**（STATUS、SLO、data-catalog、各 PR 正文）：
  - 1 身份：`security_master` 1,846 行，128 个退市日（沪 82 / 深 46）。
  - 2 精度、A 股不变：CI 夹具已证（#514、#517）。AC 要求的线上「A 股 `version_id` 与改动前逐一相同」没有记录；vm2 是 fresh init 重建，对账基准要用 aws-node 冻结库，需线上核实。
  - 3 量纲、4 成员集、5 采集：ETF 序列 1,760 条，86 只无日线，首次 ETF 批次 1,631/1,631 成功，`sz.158081` 首采。
  - 6 复权：只交付不复权 + cat1/cat11 事实（Q3）。
  - 7 契约：`generic-data-v10`；`tools/list` 增量已记录（ops 实测 30,918 B，预算调到 31,000 B，#516）。
  - 8 来源与重放：CI 夹具已证（#511 正文：`replay --scope exchange` 从来源库重建索引行）；线上 vm2 的 `full` 在 SLO 备注里。
  - 9 性能：`full` 的 ETF 4 来源合计 30.5 min，<40 min（D-73，单次）；A 股 + ETF 6,872 项 write-loop 280.0 s；ETF only 37.0 s；设计 §11.2 的「推断会超 600s」（≈752 s）未发生（不同主机，不可直接比）。
  - 10 偏差：data-catalog §3.3 第 9、10 条已写。
- **不阻塞关闭，但交付价值受限**：vm2 无 tunnel，客户端（x、ChatGPT）读不到 ETF 和 11-05 之后的数据（D-74、STATUS）。
- **正文过时**：「当前事实」表用 11-02 的 active generation `5ac3ef2b…`；「设计 + 分批实现，部署另走 D-08」中的「未开始」。
- **建议**：可关闭。需线上核实两项：vm2 抽样读回 3dp（`query_data` 带 `adjustment:"raw"`）；A 股序列相对 aws-node 冻结库的逐条对账（AC-M3-2-2）。

### #525 读取工具拆分

- 设计已批准（D-75，PR #526），评测脚本 #530 已合并，PR-1…3 未开始，评测排在 2026-11-09 日更连续 3 次「正常」之后（「正常」未定义，见 O9）。
- 正文里的设计文档路径是占位符，实际为 [`docs/tasks/issue-525-read-tools.md`](issue-525-read-tools.md)。
- 与 #387 的数字 AC、#433 的读面缺口有交叉：`research` 工具列表预算 12,200 B → 18,600 B 要在 PR-1 里改 CI（`tests/mcp/v8_tool_surface.rs:127`）。
- 建议：保留。

## 3. #304、#440 与 D-63、D-69 的计数关系

| 计数 | 数什么 | 当前 | 一个样本的条件 | 清零 / 重置 | 出处 |
|---|---|---|---|---|---|
| #304 | 全新 AI 会话仅用 Skill 完成日更的有效 PASS | 3/10，全是 `day=unchanged` | evaluator 按 MCP 结构化事实判 PASS；无 shell、无人工补步 | FAIL 不计，不清零 | #304 正文与评论 |
| #440 | `day=changed` 完整闭环 PASS，另需边界 1/2/3 的证据 | 0/1 | 同上，且当日有更新 | — | #440 |
| D-63 | 生产**跨代**日更（有新 generation 激活）的 G1 `run_market_update` write-loop <600 s | **1/3**（vm2，11-05，175.3 s） | G1 = 任务 `created_at`→`last_progress_at`，不含来源刷新、补漏 | 任一次 ≥600 s 清零并附原因；D-74 把计数从 0 重新算，aws-node 11-02 那 1 次不带过来 | D-63、D-74、SLO 第 2 行 |
| D-69 | 生产日更的全市场年线 write-loop <600 s，限方案 A 上线（preview.44）后的样本 | **1/3**（vm2，11-05，127.9 s） | 单任务全市场年线 | 同上 | D-69、SLO 第 3 行 |

关系：

1. **四个计数互相独立，不能相加或互相替代。** D-63、D-69 量的是秒（性能豁免的解除条件），#304、#440 量的是 Skill 行为是否合规。
2. 同一次生产跨代日更可以同时给 D-63 一个样本、D-69 一个样本。只有当它由全新会话仅用 Skill、无人工补步跑完，才可能是 #440 的一次 PASS，并计入 #304。
3. **11-05 这一次**：D-63、D-69 各记 1（SLO、STATUS 已记，判定由 owner 按原始数据计算）；按 #440 规则不算 PASS（经提示补步）。
4. 没有新 generation 的日子（休市、日线文件未更新）既不增加 D-63，也不清零。D-69 只写「生产日更」，没要求跨代。
5. **最早 3/3 是 2026-11-09（周一）**：11-05、11-06、11-09 都有新 generation 且 <600 s 的前提下。需线上核实。D-75 写的「2026-11-09 日更连续 3 次正常」恰好同一天，但「正常」没有定义（O9）。
6. D-63 满足后 D-52（SLO 第 2 行 G1 临时豁免）按其解除条件解除；D-69 满足后第 3 行豁免解除。是否解除由 owner 根据原始数据计算（SLO 已写）。
7. [decisions](../decisions.md) 里 D-52、D-63、D-69 的状态格（第 21、25、28 行）仍写「计数 0/3」，与 SLO 的 1/3 和 D-74 的重置不一致。
8. 计数绑定主机：vm2 是试用机，约 2027-01-04 到期。换机时按 D-74 先例重新计数，这一点建议在 D-63 状态格里写明。

## 4. 需要 owner 决定的事项

| 编号 | 问题 | 推荐 | 涉及 |
|---|---|---|---|
| O1 | Skill-only 验收（P-05，一直待决）的关闭口径：#260/#262/#296 的 DoD-7 要「至少一次有更新日完整闭环」，#304 要「10 个交易日」，两者要不要绑在一起 | **解耦**：#440 首次 PASS 即关 #260/#262/#296；#304 继续累计作为稳定性度量 | #260 #262 #296 #304 #440 |
| O2 | #440 边界 3 与 Skill §1 的「no_update 立即结束」冲突 | **(a)** 修订规则：`day` 不在 `pending_roles` 时，允许且只允许一次只读 `plan_market_update`（v7 契约写明只读、不建任务）；`eligible>0` 则从 §3.1 继续，否则 `no_update`。同步改 #304 规则 A 与 Run 4 的失败判据。(b) 保持现状，改写边界 3，把「operator 代采后必须自己跑 plan/run」写进运行手册。理由：(a) 同时覆盖 operator 代采和 Skill 激活后中断，每个无更新日只多一次只读调用。这是我的推断，未试 | #440 #304 |
| O3 | AC-R03 冷归档 | **不建冷归档层**；AC 改为「状态只有有字节（active/superseded）与 `evidence_only`，`sync_status` 如实返回，不伪造可重放」，结论写进 `decisions.md` 后勾 R03 | #260 #296 |
| O4 | #387 收口方式 | **关闭**，数字 AC 由契约预算和 D-75 取代，未做项（Ops-E、v7/v1 退役评估、`export_data`）留在 `v8-switch-checklist` | #387 |
| O5 | #295 与 #268 解耦（D-19 和 #295 写「服务端筛选随 #268 评估」，#268 契约第 7 条相反） | **解耦**：服务端筛选归 `research` 入口的 `screen_market`（#525 后续，等 XDM-14），Console 不做行情扫描；改 D-19 和 #295 的措辞 | #295 #268 #525 |
| O6 | 可交易性、涨跌停、市值的读面（#433 第 5 项）形态 | **MCP 只读工具**，放 `research` 入口、排在 #525 PR-2 之后，薄封装 `research::tradability` 等；不选导出 `price_limits.parquet`（16.7M 行，设计 U-8 体积未测）。理由：fail-closed 的 `unknown` 要在服务端判定（推断） | #433 #1 |
| O7 | 北交所、B 股、LOF 是否收录 | **本阶段不收**；写入 data-catalog §3.3，#246 改成这三类的延后记录 | #246 #1 |
| O8 | #1 收口：AC-10 稳定验证入口、AC-09 访问边界、关闭时点 | 承认 CI 命令清单就是稳定入口，改 `README.md:173` 的措辞；owner 在 STATUS 明确访问边界；#446、#308 关闭后关 #1 | #1 |
| O9 | D-75「日更连续 3 次正常」的定义 | 定义为 **D-63 计数到 3/3**（同样要求生产、跨代、G1 <600 s），否则评测窗口没有统一判据 | #525 #295 |
| O10 | AC-M3-8（跨机逐位复现快照）去向 | **另开 Issue**，不阻塞 #446；D-30 ④ 是首期定义的一部分，不能无声放弃 | #446 #1 |
| O11 | #441 的 <5 s 目标是否仍有效 | 在 vm2 复测一次；<5 s 就按「接受现状」关闭，否则再选索引或本地缓存 | #441 |
| O12 | 授权改写 Issue 正文，原则：环境、版本只写「见 STATUS」，不写主机名和版本号；删 #262 的内网地址及编辑历史；过时段落折叠保留，不删证据 | **批准**。vm2 约 2027-01-04 到期，不这样改下次搬机又要重改 | #1 #260 #262 #268 #295 #296 #304 #440 #446 #509 |

## 5. Issue 里过时的环境与版本描述

| 过时写法 | 出现在 | 现行（以 STATUS、SLO 为准） |
|---|---|---|
| ecs-node、aws-node 作生产；ECS 2 vCPU/约 2GiB | #260、#262 正文，#260/#262 评论，#268 | 日更和主数据：vm2-126（D-74）；aws-node 为 fallback + tunnel |
| preview.37、schema 24、`generic-data-v8`、ops 15 工具 | #296、#440；#295 的 schema 24/v8；#433 的 preview.37 | preview.45、schema 27、`generic-data-v10`；ops 16 / research 5 / maint 9 / acquire 10 |
| 部署 ≤4GiB | #262 | ≤8GiB（SLO 第 5 行） |
| 日更 ≤5min、增长 ≤16MiB | #262 | write-loop <600s（SLO 第 1–3 行）；增长 ≤4KiB/item |
| 业务实现 NOT_RUN、性能指标待确认 | #1 | 已交付；SLO 已签字 |
| 东财二级行业 | #1 正文 | TDX 研究行业二级 |
| 设计待审核（XDM-91 进行中） | #446 | 设计 #449 已合并，实现已部署 |
| 实时 22-tool | #260 AC-C09 | 25 个 data 工具 + 10 个 acquisition 工具 |
| 内网地址 | #262 正文 | 删除 |

标签：#387、#433、#509、#525 没有阶段标签；#440、#441、#446 标「待开发」，但 #446 已实现并部署。标签调整等 owner 对上表裁决后一起做。

## 6. 与本单（#296）同类的文档漂移

本任务只出文档，下面这些**没改**，供后续单子认领：

1. `docs/generic-data-tools.md:49-50`：`run_market_update`「未部署」、`refresh_exchange_sources`「v9 代码，未部署」。
2. `docs/decisions.md` 第 21、25、28 行：D-52、D-63、D-69 状态格仍写 0/3。
3. `docs/data-catalog.md` 第 416–420、423 行：缺口表「代码已合并，未部署」。M2/M3 已随 preview.39–42 部署，ETF 已在 vm2 preview.45。第 384–388 行有一句免责声明，表格本身没改。
4. `docs/tasks/issue-446-m3-delisted-industry.md` §14.3：AC 表全部「未执行」，§14.11 已执行多项。
5. `README.md:173`：`./scripts/verify` 尚未实现（见 O8）。

## 7. 未验证与限制

- 线上读数（vm2、aws-node）一律引用 STATUS、SLO，未复核。
- #304、#440 各次运行的 transcript、XDM-xx 报告（仓库外）未读。
- 没有跑 CI、代码或脚本以外的验证：只重算了提示词 SHA-256，并对 Issue 文本跑了 `check_public_hygiene.py` 的规则。
- 「推断」处（vm2 冷读更快、O2 方案 (a) 可行、O6 的理由）未试验。
- GitHub Projects (classic) 上的卡片状态没有读（`gh issue view` 报错）。

## 8. 建议关闭的 Issue

- **现在可关闭**：#387（需 O4）。
- **需线上核实后可关闭**：#509（2 项）、#446（补读数 + O10）、#308（drift 修复后）、#441（vm2 复测 <5 s）。
- **前置完成后可关闭**：#260、#262、#296（均在 #440 通过且 O3 裁决后）；#1（#446、#308 关闭后，O8）。
- **继续开放**：#268、#295、#304（累计中）、#440、#525；#246、#433 改写范围后保留。
