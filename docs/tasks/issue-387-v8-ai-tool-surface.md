# v8 详细设计：面向 AI 的 MCP 工具面（#387）

> **状态**：设计稿，待审核。只改文档的设计 PR；实现、部署、合并都需另行批准。
> **Issue**：[#387](https://github.com/liqiangcc/x-data-mcp/issues/387)（本文与 Issue 互相引用）。
> **输入**：XDM-18 审计报告（工作看板 `x-data-mcp/18`，2026-10-26 实测）；M1 设计
> [issue-383](issue-383-m1-research-data.md) §8（新数据只经导出，MCP 暴露放 v8）；
> owner 2026-10-26 的 7 条分离点与扩展走查要求；owner 在 reading-mcp 的同类改造
> （`tools/list` 38,773 B → 3,460 B，评测 token 降到 v1 的 4.0–9.3%）。
> **本文不复述**：部署版本、generation、数据量（见 [STATUS](../STATUS.md)）；wire 形状的完整
> schema（见运行时 `get_data_capabilities` 与新契约 [generic-data-v8](../contracts/generic-data-v8.md)）。
> 证据分级：**实测**（本机只读命令输出，可复现）/ **推断**（由实测外推，写明依据）/ **未验证**（缺证据，
> 写明取证方法）。

---

## 0. 结论摘要

| # | 结论 | 证据级 |
|---|---|---|
| 1 | 工具面按**使用者**拆成三个入口（研究 / 编排 / 运维），acquire 保持独立：研究入口 **5 个只读工具**、编排入口 **15 个**（== 日更 Skill 的 allowlist，一字不差）、运维入口 **9 个**（取证/重试/取消/遗留）。**隧道只暴露研究入口** | 实测（工具归属按今天 23 个工具的实际字节与使用证据分配） |
| 2 | 研究入口 `tools/list` 从今天的 24,280 B（4 工具，含 outputSchema）降到**预算 ≤11,000 B**；一次会话的发现成本从 **48.1K token 降到 ≤3.5K** | 实测 + 推断（描述瘦身与 outputSchema 移出后的字节预算，§6.3） |
| 3 | 响应改成**列式 + 版本清单一次**：单股一年日线 130,126 B → 原型 **16,052 B（−87.7%）**，模型可见 token 32.5K → **4.0K** | 实测（用 XDM-18 抓到的 242 条真实记录重建列式原型） |
| 4 | 新增**截面查询**（`selection.kind=universe` + `filter.kind=cross_section` + `fields`）：某日全市场涨幅前 10 从 **54 次调用 / ≈828K token** 降到 **1 次 / ≈35K token**；再降必须做服务端排序，那是 XDM-14 之后的扩展点，**本设计不承诺** | 实测（身份与列宽按真实数据外推，§8.2） |
| 5 | M1 的新数据集（不复权、6dp 前复权、因子事件、交易日历、指数基准）在 v8 变成 **dataset 分支**而不是新工具；研究入口工具数因此保持 5 | 实测（今天 `query_data` 的 schema 结构）+ M1 设计 §3 |
| 6 | XDM-18 §4 的 14 个入参拒绝**逐条**给出友好写法，全部落在**解码层归一化**（不改业务语义，参与幂等摘要），并各配一条测试 | 实测（本次已重跑 14 个拒绝，§2.1） |
| 7 | 契约升 `generic-data-v8`：`contract_version` 新增 `v2`（读取面 + 任务面），`v1` 并行保留；不改 schema；切换清单按 v6/v7 的锚点写法另立文件 | 拟议（§9） |
| 8 | v8 **不**做 `export_data`（D-29 的「放进 v8 一起设计」按「设计 + 前置条件」交付）；导出仍是 M1 的 bin 命令 | 拟议（§4.7） |
| 9 | 预算由 CI 断言：`tools/list` 字节（Rust 集成测试）+ 夹具响应的 token（Python）+ 入口封闭性（Rust）。不靠人看 | 拟议（§8.4、§15） |

**三个最容易被做错的点**（本设计专门守住）：

1. **入口不是权限**。三个入口是同一读取核心上的适配（分离点 5），边界靠**入口过滤 + 一条交集为空的测试**守住，
   不靠"客户端应该只调用只读工具"这种约定。研究入口里出现 `update_data` 就是设计错误。
2. **列式只改形状**。`version_id` 仍是内容摘要、`latest_per_entity` 仍是服务端快照内解析、精确小数仍是
   十进制字符串、空值仍不伪造。省下来的字节全部来自**重复**（逐行 `version_ref`、逐实体 manifest、
   两份 text），不来自丢语义。
3. **时点语义不提前实现**。`as_of` / `known_at` 只写名字与语义（分离点 3），v8 收到就明确拒绝；
   M2 的证券主表/ST/停牌/涨跌停到位前，任何"按时点"的承诺都是前视偏差。

---

## 1. 范围

### 1.1 本次包含（设计）

1. **入口拆分**：研究 / 编排 / 运维三个入口的 socket、单元、工具分配、隧道暴露面（§3）。
2. **研究工具**：5 个只读工具的设计，覆盖 XDM-18 的 8 个评测任务与 M1 的 5 类新数据（§4）。
3. **响应格式 v2**：列式、版本清单一次、身份字符串、小数、空值、text 分级（§5）。
4. **`tools/list` 瘦身**：描述长度规范、outputSchema 移出、各入口字节预算（§6）。
5. **入参易用性**：14 个实测拒绝逐条处理 + 归一化层位置（§7）。
6. **评测**：8 任务基线 + v8 预算 + 框架 + CI 跑法（§8）。
7. **契约与迁移**：`generic-data-v8`、`v2` 分支、并行期、切换清单、影响面（§9）。
8. **与 M1 的衔接**：读取核心复用与 M1 各 PR 的影响点（§10）。
9. **分离点落位**（§11）、**6 个扩展场景走查**（§12）、**`docs/design.md` 待改处**（§13）、
   **实现拆分**（§14）、**验证方式**（§15）。

### 1.2 本次不包含

- 不写实现代码、不改 wire 形状、不改 schema、不改部署。
- 不做服务端筛选/排序/聚合（谓词、`order_by`、`limit`、top-N）——方向等 XDM-14 实测（D-19）。
- 不实现 `as_of` / `known_at`（M2）；不实现证券主表、ST、停牌、涨跌停。
- 不新增 MCP 文件传输（`export_data` 见 §4.7）。
- 不改 acquire server（仍 10 工具）；不改写受理语义（D-18）。
- 不删除任何现有工具（`compute_metrics` 只移到运维入口）。
- 不改下游 `x` 仓库（P-02 未决，另一仓库，须另行批准）。

### 1.3 与既有裁决的关系

| 裁决 | 与本设计的关系 |
|---|---|
| D-18（显式 items 保持 fail-closed） | 写受理语义不动；v8 只改读取面与任务面的**响应形状**，`update_data` 的入参只加别名归一化 |
| D-19（先测量再定方向） | 服务端筛选/聚合**不在 v8 承诺**，只留扩展位（§4.8） |
| D-21 / D-32（6dp 前复权不升版） | v8 的升版理由是**入口与 wire 形状**，不是精度；6dp 在 v8 里是一个 `adjustment` 选项 |
| D-29（导出首期 bin 命令；`export_data` 放进 v8 设计） | v8 设计并明确**不做**，前置条件见 §4.7 |
| D-30（严谨回测级、M1→M2→M3） | v8 只暴露 M1 已落地的数据；M2/M3 的字段用 `as_of`/`known_at` 预留位接（§4.6） |
| D-33（schema 22 归档重建） | v8 **不改 schema**，与 M1 的 schema 22 是两条独立线；部署方式按各自清单 |
| 2026-09-12（MCP 只提供原子能力，编排由 AI 显式选择） | 编排入口只是"给 Skill 用的原子能力集合"，不是工作流工具；不返回 `next_actions`（§11 分离点 7） |

---

## 2. 现状事实（2026-10-26 复核）

### 2.1 只读实测（本次，preview.35 / `generic-data-v7` / schema 21）

方法与 XDM-18 一致：只连本机 socket 发 `initialize` + `tools/list` + 只读工具调用；不调用任何写工具；
每个 python 命令都在 `systemd-run --scope -p MemoryMax=150M -p MemorySwapMax=0` 下运行；MemAvailable
开工 549 MB、结束 528 MB（停止线 400/300 MB 未触发）。

| 项 | XDM-18（2026-10-26T03:54Z） | 本次复核 | 判定 |
|---|---|---|---|
| data `tools/list` 线上字节 | 115,496 | **115,496** | 一致 |
| data 工具数 | 23 | **23** | 一致 |
| 其中 description / inputSchema / outputSchema | 10,023 / 32,457 / 71,218 | **10,079 / 32,457 / 71,218**（description 差 56 B 是引号计入方式） | 一致 |
| acquire `tools/list` | 6,040（10 工具） | **6,040**（10 工具，无 outputSchema） | 一致 |
| 研究四工具 desc+input（去 outputSchema） | 9,459 | **9,225** | 同量级（口径差：本次不含 name/键） |
| 14 个入参拒绝 | 逐条有精确消息 | **逐条复现**（§2.1.1） | 一致 |
| `get_data_capabilities` 按 dataset+operation 过滤 | 22,140 线上 / 9,677 struct | **22,140 线上**（`{dataset:"market.daily",operation:"query_data"}`） | 口径不同（线上 = 2 × payload + 信封），两者都说明详版 ≈10–11 KB |

> 口径：本文一律用**线上响应行字节**（socket 上 `len(line)`）；token 用 ASCII/4 估算
> （研究响应全 ASCII），非 ASCII 按 1.5 字符/token（reading-mcp 的做法）。

#### 2.1.1 14 个拒绝的复核结果（本次重跑）

| # | 用例 | 结果（错误码 / 消息片段 / 线上字节） |
|---|---|---|
| 1 | `entity: "sh.600000"` | `invalid_arguments` / `invalid type: string "sh.600000", expected struct Security` / 403 |
| 2 | `version: {"kind":"latest"}` | `invalid_arguments` / `unknown variant \`latest\`, expected one of …` / 487 |
| 3 | 日期 `20261020` | `invalid_arguments` / `input contains invalid characters` / 345 |
| 4 | 缺 `start_date` | `invalid_arguments` / `missing field \`start_date\`` / 331 |
| 5 | 缺 `contract_version` | `invalid_arguments` / `missing field \`contract_version\`` / 343 |
| 6 | `market.daily` + `selection.kind=catalog` | `invalid_arguments` / `INVALID_MARKET_COMMAND: invalid selection for dataset` / 385 |
| 7 | 多余字段 `limit` | `invalid_arguments` / `unknown field \`limit\`, expected one of …` / 529 |
| 8 | `get_data_capabilities` 带 `contract_version` | `invalid_arguments` / `unknown field \`contract_version\`, expected \`dataset\` or \`operation\`` / 413 |
| 9 | `get_data_capabilities(operation:"query")` | `unsupported_operation` / `operation not registered` / 335 |
| 10 | `list_tasks(operation:"update_data")` | `invalid_arguments` / `unknown variant \`update_data\`, expected one of \`start_collection\`, \`verify_stopped_writes\`, \`verify_publication_result\`` / 517 |
| 11 | 不存在的 `task_id` | `task_not_found` / `TASK_NOT_FOUND` / 301 |
| 12 | `get_task` 缺 `contract_version` | `invalid_arguments` / `missing field \`contract_version\`` / 265 |
| 13 | `code: "sh.600000"`（把展示名塞进 code） | `invalid_arguments` / `INVALID_MARKET_COMMAND: six digit security code required` / 391 |
| 14 | 缺 `filter` | `invalid_arguments` / `missing field \`filter\`` / 323 |

`verify_data_gap` 的 `symbol`、`cancel_task` 的 `expected_revision`、`item_id` 的二次编码三项本次**未重跑**
（分别是写工具与出参形状，避免任何写路径接触）；它们沿用 XDM-18 的实测记录，并在 §7.2 标为
"沿用 2026-10-26 03:54Z 的实测"。

### 2.2 代码事实（`file:line`）

| 事实 | 位置 |
|---|---|
| `tools/list`、capabilities、dispatch 由同一 `entries()` 生成（单一描述源） | `src/data_supply/registry.rs:180`、`src/data_supply/mcp.rs:588-600` |
| `tools/list` 现在**内联 outputSchema** | `src/data_supply/mcp.rs:597` |
| 已有的入口式过滤点：`self.access != Access::local() && e.requires_local()` | `src/data_supply/mcp.rs:595`（capabilities 侧 `:296`） |
| `requires_local()` 把写/核实/任务控制列为"本机上下文" | `src/data_supply/registry.rs:47-66` |
| `Access { principal, datasets, securities }` | `src/data_supply/mod.rs:7-40` |
| text 与 structuredContent 同值（一份 JSON 序列化两次） | `src/mcp.rs:690-700` |
| `capability_revision` 常量 | `src/data_supply/mcp.rs:354`、`src/data_supply/registry.rs:146` |
| `TaskOperation` 只有 3 个变体（缺 `update_data`） | `src/data_supply/wire.rs:337-341` |
| `query_data` 的选择/过滤枚举（`Selection`/`Filter`/`VersionChoice`） | `src/data_supply/wire.rs:73-104` |
| 单次查询上限 100,000 行 / 16 MiB、页大小 1..1000、快照缓存 32/64 MiB | `src/data_supply/query.rs:10-13,71-75` |
| 服务就绪唯一定义（`wait_service_ready.py`） | D-17；`scripts/wait_service_ready.py` |
| 隧道只转发 data socket | `docs/STATUS.md`「服务」表 |

### 2.3 三个结构性缺口（本设计要解决的）

1. **工具面按能力组织，不按使用者组织**。研究会话要为 23 个工具付 28.9K token 定义，其中
   20.5% 是 0 生产使用的核实工具（XDM-18 §2.3），而真正需要的能力（某日全市场截面）**没有入口**。
2. **重复占大头**。单股一年日线的 130,126 B 里 49.3% 是逐行重复的 `version_ref`（`version_id` 与
   `integrity_sha256` 还是两个相同的 64 hex）；10 实体截面的 78% 是逐实体重复的 manifest；
   每个响应再乘 2（text + structuredContent）。
3. **入参形状与项目其它地方不一致**。全项目文档、日志、任务 scope 都写 `sh.600000`，只有 MCP 入参
   要 `{exchange, code}`；任务/日志/文件名都是 `YYYYMMDD`，只有 wire 要 `YYYY-MM-DD`。

---

## 3. 入口设计

### 3.1 三个入口 + acquire

| 入口 | socket | systemd 单元 | 工具数 | 使用者 | 隧道暴露 |
|---|---|---|---|---|---|
| **研究** research | `run/research.sock`（新增） | `x-data-mcp-research`（新增） | **5** | ChatGPT/Claude（经隧道）、本机 AI 研究会话 | **是（唯一）** |
| **编排** ops | `run/mcp.sock`（沿用） | `x-data-mcp-data`（沿用） | **15** | 日更 Skill（Devin）、部署/冒烟脚本、`x` 仓库 `KlineReader` | 否 |
| **运维/取证** maint | `run/maint.sock`（新增） | `x-data-mcp-maint`（新增） | **9** | operator（人）、取证脚本 | 否 |
| **来源接入** acquire | `run/acquire.sock`（不变） | `x-data-mcp-acquire`（不变） | **10** | 日更 Skill | 否 |

为什么 `mcp.sock` 留给编排入口而不是研究入口：日更 Skill 的客户端配置、部署脚本、`x` 仓库都连
`mcp.sock`；把它换成研究入口会**静默**让 Skill 失去 plan/run。隧道改成连 `research.sock` 是**加法**
（新增 socket + 改隧道 drop-in），回滚只需把隧道指回 `mcp.sock`。

### 3.2 工具分配（24 个工具逐条归属）

**研究入口（5）**：`get_data_capabilities`、`query_data`、`get_data_availability`、`list_data_versions`、
`list_catalog_facets`（新增，§4.5）。

**编排入口（15，== 日更 Skill 的 allowlist）**：`get_data_capabilities`、`plan_market_update`、
`run_market_update`、`start_collection`、`update_data`、`get_task`、`get_task_progress`、`list_tasks`、
`get_request`、`get_data_availability`、`query_data`、`list_data_versions`、`get_yearly_base_years`、
`verify_publication_result`、`verify_stopped_writes`。

**运维/取证入口（9）**：`get_data_capabilities`、`backfill_data`、`republish_data`、`retry_task`、
`cancel_task`、`inspect_verification_task`、`verify_data_gap`、`run_worker_once`、`compute_metrics`。

校验：5 + 15 + 9 − 4（研究∩编排：capabilities/query/availability/versions）− 1（研究∩运维：capabilities）
− 1（编排∩运维：capabilities）= 23 个现有工具 + 1 个新增 = **24**。

分配依据（不是按名字猜）：

| 工具 | 今天的证据（XDM-18） | 归属理由 |
|---|---|---|
| `query_data` / `list_data_versions` / `get_data_availability` / `get_data_capabilities` | 研究读路径；Skill 第 0 步与 readback 也用 | 三个入口都需要，是**同一份**工具（同 schema、同响应），不是三份实现 |
| `plan_market_update` / `run_market_update` | 生产 6 次 plan/run | 只给编排：它们受理任务 |
| `start_collection` / `update_data` | 生产 54 / 519 个任务 | 只给编排：Skill 的首采与补漏 |
| `get_task` / `get_task_progress` / `list_tasks` / `get_request` | Skill §5 的轮询与恢复 | 只给编排：研究会话不需要任务面 |
| `get_yearly_base_years` | Skill §3.6 年线显式重推 | 只给编排 |
| `verify_publication_result` / `verify_stopped_writes` | Skill §5 允许；生产 0 行 | 给编排（Skill allowlist 里有） |
| `inspect_verification_task` / `verify_data_gap` / `retry_task` / `cancel_task` / `run_worker_once` | 生产 0；Skill 明确不用 | 只给运维（本机人操作） |
| `backfill_data` / `republish_data` | 生产 0；Skill allowlist 没有 | 只给运维 |
| `compute_metrics` | 生产 0；仓库自己的回测直读 SQLite | 只给运维；v9 再评估删除（Q4） |
| `list_catalog_facets` | 无（新增） | 研究：解决"不知道行业码要多花 484K"（XDM-18 T4） |

### 3.3 边界靠什么守住

1. **依赖方向**：研究入口的工具集合是一个**静态 allowlist 常量**（`Entry.entry: EntryKind`），
   不是运行时判断"这个工具读不读库"。入口过滤沿用已有的 `public_tools()` 过滤点
   （`src/data_supply/mcp.rs:595`），不引入新机制（复杂度预算）。
2. **契约**：`requires_local()` 继续作为纵深防御（研究入口全部是只读工具，本来就 `requires_local()==false`）。
3. **测试**（三条，缺一不可）：
   - `research_tools ∩ (write ∪ verify ∪ task ∪ acquire) = ∅`（按 §3.2 的三张表断言，双向：研究入口的
     每个工具必须在白名单里，白名单里每个工具必须真的出现在研究入口）；
   - 每个入口的 `tools/list` 与 `get_data_capabilities` 的工具集合一致（单一 registry 不变量）；
   - 研究入口 `tools/call` 一个不在白名单的工具名 → `tool_not_found`（不是 `forbidden`：入口不可见，
     不泄露存在性）。
4. **启动检查**：`--entry` 与 registry 里的归属表不一致 → 拒绝启动（fail-closed，不静默降级为全量）。

### 3.4 部署形态

- **一个二进制、一个 `--entry` 参数**：`data_service --entry research|ops|maint`（默认 `ops`，
  保证旧单元文件不改也能启动）。**不**新建 crate、不做动态插件（分离点 1、复杂度预算）。
- 三个 systemd 单元共用同一个二进制与同一个库文件（单写者边界不变：只有 `ops` 入口能受理写请求，
  worker 只跑在 `ops` 入口）。
- 就绪探针沿用 D-17 的唯一定义：每个单元 `ExecStartPre=wait_service_ready.py <sock> --timeout 120
  --expect-tool <该入口必有的一个工具>`（research → `query_data`，ops → `plan_market_update`，
  maint → `cancel_task`）。
- **隧道**：`x-data-mcp-tunnel` 的 `ExecStart` 从 `run/mcp.sock` 改为 `run/research.sock`；
  drop-in 的 `ExecStartPre` 一并改。**这一步在生产 `/etc` 下，不在仓库内，需 owner 批准并单独执行**
  （D-17 的既有欠账：生产 drop-in 仍调用 `/opt` 上不受版本控制的探针）。
- `docs/STATUS.md`「服务」表从 2 行变 4 行；`check_deploy_facts.py` 的单元清单同步（实现 PR 做）。

### 3.5 过渡与回滚

| 阶段 | 动作 | 回滚 |
|---|---|---|
| T1 | 实现合并：三个入口都在，`mcp.sock` 变成编排入口的 15 个工具（**可见的缩小**，见下） | 换回旧二进制即恢复双 server 形态（23 + 10） |
| T2 | 部署日：三单元启动，隧道**先并行观察**（同时连 `mcp.sock` 与 `research.sock`，只读比对 `tools/list` 与研究任务），观察 ≥1 次真实日更 | 隧道指回 `mcp.sock`（一条 drop-in 改动） |
| T3 | 隧道只留 `research.sock`，`docs/STATUS.md` 与当前状态区翻转 | 同上 |

注意：v8 的 `mcp.sock` 从"23 个工具"变成"15 个工具"，这是**可见的缩小**。Skill 的 allowlist 正好是
这 15 个，所以 Skill 不受影响；但任何**仓库外**的脚本如果调用了 `inspect_verification_task` 等
8 个工具，会得到 `tool_not_found`——部署前必须用 `scripts/deploy_preview.sh` 与
`scripts/smoke_release.py` 的实际调用集合核对（§9.5 的「部署脚本」行）。

---

## 4. 研究工具设计（5 个）

### 4.1 工具清单

| 工具 | 意图 | 主要入参 | 出参（v2） | 预算（模型可见） |
|---|---|---|---|---|
| `get_data_capabilities` | 版本握手 + 按需取 schema | 可选 `dataset`、`operation` | 无过滤：**紧凑目录**（≤1.5 KB）；过滤：单项详版（含 `input_schema`/`output_schema`） | 目录 ≤400 token |
| `query_data` | 面板 / 截面 / 目录 / 新数据集 | `dataset`、`selection`（`single`/`batch`/`universe`/`market`）、`filter`（`daily`/`yearly`/`catalog`/`cross_section`）、`adjustment`、`fields`、`page_size`、`cursor`、`version` | 列式（§5） | 面板 ≤5K / 截面 ≤35K |
| `get_data_availability` | 覆盖与新鲜度 | `dataset`、`selection`（含 `universe`）、`range` | 逐实体：一份共享 `related_tasks` + 每实体一行；`universe`：一行汇总 | 单实体 ≤1K / 全市场 ≤150 token |
| `list_data_versions` | 版本清单（可复现性） | 同上 | 列式版本表（每实体一行） | ≤1K |
| `list_catalog_facets` | 目录维度（行业码/名称/成员数） | `dataset`（本期只有 `securities.catalog`）、`catalog_version` | `{facets:{industries:[{code,name,members}]}}` | ≤600 token |

研究入口**没有** `export_data`、没有任务面、没有任何写工具。研究入口的工具集合在
`tools/list` 里只有这 5 项。

### 4.2 数据集矩阵（M1 的新数据怎么进来）

| dataset | v8 可见入口 | `selection` | `filter` | 列（`fields` 可选子集） | 来源 |
|---|---|---|---|---|---|
| `securities.catalog` | research / ops | `catalog`（冻结 scope） | `catalog`（`code`/`name_query`/`industry_code`） | `code, name, industry_code, industry_name` | 既有 |
| `market.daily` `adjustment=raw` | research / ops | `single`/`batch`/`universe` | `daily`/`cross_section` | `date, open, high, low, close, volume, amount, volume_quantum?` | M1 PR-2 |
| `market.daily` `adjustment=qfq` | research / ops | 同上 | 同上 | 同上（价格 6dp） | M1 PR-3 |
| `market.yearly` | research / ops | `single`/`batch` | `yearly` | `year, open, high, low, close, volume, amount` | 既有 |
| `market.adjustment_events` | research | `single`/`batch`/`universe` | `daily`（生效日区间） | `effective_day, category, c1, c2, c3, c4` | M1 PR-4 |
| `market.calendar` | research | `market` | `daily`（日期区间） | `trade_day` | M1 PR-6 |
| `market.index` | research | `single`/`batch`/`universe` | `daily`/`cross_section` | `date, open, high, low, close, volume, amount` + manifest 里的逐指数口径 | M1 PR-5 |

三条约束：

1. **新 dataset 不是新工具**。依据 `docs/generic-data-tools.md`「新增 dataset 通常只扩展已有工具的
   schema 分支与处理器」，这也是研究入口能保持 5 个工具的原因。
2. **`market.daily` 的 `adjustment` 是读取选项，不是新 dataset**（D-21：不复权是解析事实，前复权是
   派生缓存）。缺省 `raw`（不复权是事实；前复权含未来事件，不能当缺省，见 M1 设计 §13 第 4 条）。
   响应 manifest 必带 `adjustment{algorithm_version, basis}` 与 `price_scale`。
3. **`market.index` 与 `market.daily` 分开**。M1 的 `instrument_class` 过滤让指数在 M1 阶段对 MCP
   不可见（AC-M1-10）；v8 用**独立 dataset** 暴露，避免"`market.daily` 里混进指数"改变既有查询语义，
   也避免 M2/M3 的 ETF（`price_scale=3`）与 B 股（币种）再动一次 `market.daily`。

### 4.3 截面查询（研究入口的核心新增能力）

```json
{"dataset":"market.daily","contract_version":"v2","adjustment":"raw",
 "selection":{"kind":"universe","catalog_version":"<64-hex 或 \"latest\">"},
 "filter":{"kind":"cross_section","start_date":"2026-10-23","end_date":"2026-10-24"},
 "fields":["close"],"page_size":1000}
```

- **`selection.kind=universe`**：冻结的目录版本 + 全市场成员。`catalog_version` 必填（可复现）；
  写 `"latest"` 时服务端解析成当前目录版本并**回显**在响应 `snapshot.catalog_version`。
  这与"`latest_per_entity` 是服务端快照内解析"是同一套语义，不新增歧义。
- **`filter.kind=cross_section`**：日期区间**上限 5 个交易日**（截面不能被当成全量面板用；超限拒绝并
  提示改用 `filter.kind=daily` + `selection.kind=batch`）。
- **`fields`**：投影。缺省按 dataset 的"核心字段集"（§4.2 的列，去掉可选列）；显式 `fields` 只能是
  该 dataset 允许的子集；未知字段拒绝并**列出可选字段**（不是裸 `unknown field`）。
- **响应布局**：`layout:"cross_section"`，`trade_days` 一次、`entities` 一次、
  `columns[field][day_index][entity_index]`（§5.1）。
- **为什么不做 top-N**：XDM-18 的 T2 期望"服务端 top-N ≈1K token"，但那属于服务端排序/聚合，
  方向等 XDM-14 实测（D-19、§4.8）。v8 的诚实预算：1 次调用、**≈35K token**（§8.2 的推导），
  相对今天的 828K 是 −96%。

### 4.4 面板与版本固定

- 面板：`selection.kind=single|batch` + `filter.kind=daily|yearly`；响应 `layout:"panel"`，
  `entities` 一次，`dates[entity_index]` 每实体一份（停牌/上市前本来就没有 bar，不能靠位置推断），
  `columns[field][entity_index][row_index]`。
- 版本固定：`version.kind=latest_per_entity`（缺省）/ `explicit_version_map` / `catalog_version`；
  响应里 `versions:[{entity, version_id}]` **只出现一次**；`integrity_sha256` 不再出现
  （契约写明 `integrity_sha256 == version_id`，XDM-18 §3.4 第 2 条）。
- `stale_version` 语义不变：旧 `version_id` 曾存在但已被取代 → 需要重新列版本。

### 4.5 目录维度（`list_catalog_facets`）

- 输入：`{dataset:"securities.catalog", catalog_version?: "<hex>"}`（缺省 = 当前目录版本，回显）。
- 输出：`{dataset, catalog_version, facets:{industries:[{code:"X2603", name:"半导体", members:236}]}}`，
  约 90 行 × 25 B ≈ 2.3 KB（**推断**，按今天 catalog 的行业分布外推）。
- 为什么单独一个工具而不是塞进 capabilities：capabilities 是**契约发现**（schema、readiness），
  它的内容不应随数据版本变化；行业维度是**数据事实**，会随目录版本变。混在一起会让
  `check_current_facts` 的"capabilities ↔ tools/list 同源"不变量变得含糊。
  替代方案（Q2）与代价写在 Issue。

### 4.6 时点参数（`as_of` / `known_at`）预留

- **语义**（写进 v8 契约，M2 实现）：`as_of` = 查询时点（T 日收盘后视角）；`known_at` = 只使用该时刻
  **已经知道**的信息（公告日 ≤ `known_at`）。两者分离是分离点 3：ST 的生效日与公告日不同。
- **v8 的行为**：不接受这两个参数。为了让 AI 拿到可操作的错误而不是裸 `unknown field`，解码层维护一个
  **保留名列表**：请求带 `as_of`/`known_at` → `invalid_arguments` + `reserved_for_m2` +
  `"temporal point-in-time queries arrive with the securities master table (M2); see docs/data-catalog.md §3.2 rows 6/9/11/12"`。
- **不提前实现的依据**：AGENTS.md「未确认的数据范围、存储策略、历史时点语义和性能阈值不能写成既定要求」。
  没有证券主表/ST/停牌/涨跌停的数据，任何 `as_of` 都只能返回前视偏差的结果。
- **M2 的接入方式**（写清接口，不实现）：`as_of` 进 `filter`（不是 `selection`），因为它约束的是
  **记录集**而不是成员集；`known_at` 进顶层（它约束**整条查询**的信息集）。新增 dataset
  （`market.security_master`、`market.risk_warnings`、`market.suspensions`、`market.price_limits`）
  按 §4.2 的方式挂到 `query_data`，**不加工具**。

### 4.7 导出（`export_data` 在 v8 不做）

D-29 说「MCP 的 `export_data` 放进 v8 一起设计」。本设计的设计结论是**v8 不做**，并把前置条件写死：

| # | 前置条件 | 现状 |
|---|---|---|
| 1 | 访问授权（谁可以导、导出谁的授权范围） | 未设计：`Access` 只有 datasets/securities 两个维度 |
| 2 | 取文件方式（MCP 不传文件字节，只能是路径 + 清单） | 需要客户端能读本机文件；隧道侧（ChatGPT/Claude）**读不到**本机路径 |
| 3 | 过期清理与配额 | 未定（D-29 的 `/root/x-data-exports` 也没有配额） |
| 4 | 格式与 Decimal 编码冻结 | M1 的 Parquet schema 尚未实测（M1 §16.2 U-2/U-3） |
| 5 | 副作用边界（导出会不会触发刷新/修复） | BR-M1-11 已定"导出只读"，但 MCP 侧还要回答"导出失败是否重试" |

因此 T8 的 MCP 侧成本在 v8 是 **0 次调用**：导出走 bin 命令，清单（`snapshot_id` + 每条
`version_id` + generation + 文件 sha256）由 bin 产出；AI 若需要把清单带进对话，用
`query_data`/`list_data_versions` 复核版本，**不新增工具**。

### 4.8 服务端筛选的扩展点（不承诺）

- **不新增任何字段**：不加 `predicates`、`order_by`、`limit`、`top_n`。依据
  `docs/generic-data-tools.md`「未明确的动态默认值不能写入 schema 假装已经实现」。
- **扩展位**：`filter` 是判别联合，将来加一个 `kind:"predicate"`（或给 `cross_section` 加
  `order_by`/`limit`）是**契约增量**（`contract_version` v3），不是 v8 的隐式行为。
- **触发条件**：XDM-14 的耗时分解落地后，按实测瓶颈决定"窄投影 / 放宽批量 / 服务端筛选 / 降级"
  四选一（D-19）。本设计只保证：v8 的列式 + `fields` 投影把"传输侧"的浪费先消掉，让 XDM-14 测到的
  是**真正的服务端成本**而不是 JSON 冗余。

---

## 5. 响应格式（`contract_version: "v2"`）

### 5.1 列式信封

**面板**（`layout:"panel"`）：

```json
{"dataset":"market.daily","contract_version":"v2","layout":"panel","adjustment":"qfq",
 "snapshot":{"catalog_version":"<hex>","generation_ref":"<hex>"},
 "entities":["sh.600000"],
 "dates":[["2025-10-24","2025-10-27","…"]],
 "versions":[{"entity":"sh.600000","version_id":"<64-hex>"}],
 "metadata":{"units":{"price":"CNY","volume":"share","amount":"CNY"},
             "adjustment":{"algorithm_version":"qfq-exact-v1","price_scale":6,"basis":"raw+cat1"},
             "coverage_status":"complete","source":"official_market"},
 "columns":{"open":[["12.57","…"]],"high":[["12.68","…"]],"low":[["12.41","…"]],
            "close":[["12.56","…"]],"volume":[["183342863","…"]],"amount":[["2383331328","…"]]},
 "row_count":242,"next_cursor":null,"page_complete":true}
```

**截面**（`layout:"cross_section"`）：`trade_days` 替代 `dates`，`columns[field]` 的**第一维是天**：

```json
{"layout":"cross_section","trade_days":["2026-10-23","2026-10-24"],
 "entities":["sh.600000","sh.600004","…"],
 "columns":{"close":[["12.41","12.56"],["9.80","9.91"]]}}
```

**目录 / 版本 / 日历**（`layout:"table"`）：没有 `dates`，`columns[field][row_index]` 一维。

规则：

1. `entities`（或 `trade_days`）**只出现一次**；不在每行重复。
2. `versions` 每实体一行，**只出现一次**；`integrity_sha256` 不再出现（等式写进契约）。
3. `metadata` 是**共享常量**：`units`/`adjustment`/`coverage_status`/`source` 只写一次；逐实体不同的
   字段（`published_at`）留在 `versions` 的行里。
4. 逐行的 `kind`（常量 `"market"`）、`first_trading_date`/`last_trading_date`/`year_unfinished`
   （日线恒 null）不再输出；需要时按 dataset 的固定语义解释，不逐行占字节。
5. `row_count`/`next_cursor`/`page_complete` 语义与今天一致（分页边界不变）。

### 5.2 身份、小数、空值

- **身份**：输出统一 `"sh.600000"`（`exchange` + `.` + 6 位 `code`）；输入**两种都收**
  （字符串或 `{exchange, code}`），解码层规范化成 `(exchange, code)`，规范化结果进幂等摘要。
  依据：全项目其它地方（`scope_key=["market","sh.600017","day"]`、`x` 仓库 `marketToExchange`、
  文档、日志）都写 `sh.600000`，只有 wire 入参要结构体（XDM-18 §4 第 1 条）。
- **精确小数**：价格/成交量/成交额仍是**十进制字符串**，不改成 JSON number（浮点会改语义）。
  6dp 前复权的 `"12.345678"` 也是字符串；`price_scale` 在 `metadata.adjustment` 里声明，
  读取方必须用 scale 解释（M1 BR-M1-4）。
- **空值**：**不输出**。
  - 表/目录布局：某行的字段为 null → 该键省略（行式记录）；
  - 列式布局：某字段**全为 null** → 整列省略；**部分 null** → 该位置写 JSON `null`
    （列是位置对齐的，省略会错位——这是"空值不输出"的唯一例外，写进契约）；
  - 常量 null（如 catalog 的 `adjustment`）→ 不出现在 `metadata`。
- **不输出的字段**：`integrity_sha256`（= `version_id`）、逐行 `entity`、逐行 `version_ref`、
  逐行 `kind`、日线恒 null 的三个字段。这些都是 XDM-18 §3.2 实测的纯重复。

### 5.3 分页与 cursor

- 分页边界、`page_size` 1..1000、单次快照 100,000 行 / 16 MiB **不变**（`src/data_supply/query.rs:10-13`）。
- 列式响应下 `next_cursor` 的含义不变：绑定 dataset/契约/身份/过滤/页大小/版本快照。
- **新增**：cursor 绑定摘要里的契约版本参与比较；v1 的 cursor 在 v2 请求上 → `start_new_query`
  （错误消息点名哪一项变了：`contract_version`）。跨用拒绝的精确错误码本次**未验证**（只读探测未
  构造该场景），列为 §16 的未验证项。

### 5.4 text 分级（按入口与客户端）

依据 XDM-18 §3.5 的三条客户端证据：

| 客户端 | 证据 | 结论 |
|---|---|---|
| OpenAI / ChatGPT | 官方 MCP 指南要求"把同一份 JSON 也放进 `content` 数组" | 研究入口**必须**保留 text |
| Anthropic / Claude connector | `mcp_tool_result` 只有 `content:[{type:"text"}]`，**没有** `structuredContent` | 研究入口 text 是模型唯一通道 |
| Devin | 只讲 transport/配置，未规定 structuredContent；本仓库 Skill 已按"大输出被截断到 overflow 文件"处理 | 编排入口**不能**只给摘要；**未验证** |
| `x` 仓库 `KlineReader` | 只读 `result.structuredContent` | 本地消费方不需要 text |

设计：

| 入口 | `text` 默认 | 说明 |
|---|---|---|
| research | `full` | `text` == `structuredContent` 的同一份 compact JSON。响应本身已经是列式（T1 16 KB、T2 ≈140 KB），重复的代价从 130 KB 级降到 16–140 KB 级，且这是 ChatGPT/Claude 的**唯一**通道 |
| ops | `summary` | payload ≤2 KB → `text` == 完整 JSON（小响应没必要省）；>2 KB → 有界摘要：`{"ok":true,"tool":…,"counts":{…},"next_cursor":…,"row_count":…,"refs":{…},"detail":"see structuredContent"}`（≤300 B），且**必含 Skill 已依赖的字段**（`status`/计数/`next_cursor`） |
| maint | `summary` | 同 ops；运维是人在本机看 `structuredContent` |

- 开关：`X_DATA_MCP_TEXT_MODE=full|summary|off`（缺省按入口；`off` 仅供本地脚本压测，**不得**在隧道侧使用）。
- 偏离规范的说明写进 v8 契约：MCP 规范是 SHOULD 不是 MUST；`summary` 模式下 `structuredContent`
  仍是完整数据，任何读 `structuredContent` 的客户端不受影响。
- **部署前必须实测 Devin**（§16 U-2）：如果 Devin 只读 `text`，`summary` 会让 Skill 看不到
  `failed_by_reason` 的明细——那时把 ops 缺省改成 `full`，只保留开关。

### 5.5 字节预算（实测原型）

| 场景 | 今天（模型可见） | v8 原型（实测/推断） | 变化 |
|---|---|---|---|
| T1 单股一年日线 242 根 | 130,126 B（32.5K token） | **16,052 B（4.0K token）**，实测原型 | −87.7% |
| T1 单股 5 根（同日） | 5,088 B | ≈700 B（推断：列式每行 66 B + 信封 ≈400 B） | −86% |
| 10 实体单日（今天的 batch 路径） | 25,912 B，其中 manifest 20,192 B | ≈3.0 KB（manifest 一次 ≈1.5 KB + 10×66 B） | −88% |
| T2 全市场 2 日 `close` | ≈828K token（54 次调用） | **≈138.6 KB（34.7K token）**，1 次调用 | −96% |
| T4 行业 236 只 | 8,365 B/20 条 → 3 页 ≈6.3K token | ≈4.5 KB（236 × 19 B）+ facets 2.3 KB | ≈−60% |
| T3 新鲜度 | 423 B（plan）+383 B（sync_status） | ≈400 B（`selection.kind=universe` 一行） | 持平 |

> 口径提醒：线上字节 ≈ 2 × 模型可见字节（text + structuredContent）。研究入口保留 text 后，
> **线上**字节是模型可见的 2 倍，但模型只看到一份。XDM-18 的 32.5K 就是按 `text` 130,126 B 算的，
> 与本表同口径。

---

## 6. `tools/list` 瘦身

### 6.1 描述长度规范

每个工具的 `description`：

1. **≤240 B、≤3 句**（实测：今天 23 个工具里 17 个超 240 B，最长 1,092 B）；
2. **不含示例**（示例进 capabilities 的详版与契约文档）；
3. **不含跨工具编排**（不写"先调 A 再调 B"、不提 Skill 名）——既有规则，`docs/generic-data-tools.md`
   「跨工具编排不进入 Tool/Capability 输出」；
4. **必须写缺省值的一句话**（如 `version` 缺省 `latest_per_entity`、`adjustment` 缺省 `raw`），
   因为缺省值是最容易被猜错的地方；
5. 出错时的可选值**由错误消息给**（§7.1），不在描述里罗列枚举。

### 6.2 outputSchema 移出 `tools/list`

- `tools/list` 只发 `name` / `description` / `inputSchema`（MCP 规范允许不提供 `outputSchema`）。
- 需要 output schema 的客户端：`get_data_capabilities({dataset, operation})` → 单项详版含
  `input_schema`/`output_schema`/`example`/`schema_digest`。
- **同源不变量**：`tools/list`、capabilities、dispatch 仍由 `entries()` 一处生成
  （`src/data_supply/registry.rs:180`、`src/data_supply/mcp.rs:588-600`），不新增第二份 schema。
- 今天 `tests/mcp/data_stdio.rs:99-103` 断言"capabilities 的 `output_schema` == tools/list 的
  `outputSchema`"；v8 改成"capabilities 的 `output_schema` == `entries()[i].output`"（§9.5）。
- 无过滤 capabilities 改成**紧凑目录**（≤1.5 KB）：每项只含 `tool_name`/`dataset`/`contract_version`/
  `implemented`/`operator_only`/`entry`，不含 `description`/`declared_scope`/`active_generation_ref`
  （今天 35 个 item 里这些重复常量占 20,088 B）。详版按 `dataset`+`operation` 取。

### 6.3 各入口字节预算

推导（今天的实测值 → 预算）：

| 入口 | 工具数 | 今天 desc+input | 今天含 outputSchema | 描述瘦身（≤240 B） | 新增入参 | v8 预算（含 name/键） |
|---|---|---|---|---|---|---|
| research | 5 | 9,225 | 24,280 | −506 → 8,719 | +1,000（`universe`/`cross_section`/`fields`/`market`）+480（facets） | **≤11,000** |
| ops | 15 | 28,674 | 84,028 | −3,729 → 24,945 | +1,100 | **≤27,500** |
| maint | 9 | 14,615 | 33,297 | −1,667 → 12,948 | 0 | **≤14,500** |
| acquire | 10 | 5,385 | 6,040 | 0（不改） | 0 | **6,040（不变）** |

- 预算按**线上 compact JSON**（`serde_json` 无空格）计，含 `name`/键名（约 +60 B/工具）。
- 预算由 Rust 集成测试断言（`tests/mcp/v8_tool_surface.rs`）：对三个入口分别序列化 `tools/list`，
  `assert!(bytes <= budget)`，失败时打印实际字节与超出的工具名。
- 预算不是"越少越好"的 KPI：`plan_market_update` 的 423 B 响应与 576 B 描述是今天的**正面样板**，
  瘦身的目标是去掉示例与跨工具说明，不是删掉有用的一句话。

### 6.4 为什么不做"运行时动态拼装 schema"

静态 registry 仍是唯一来源，只是**发布哪些工具、发布多细**由入口与"是否详版"决定。不做按需拼装
（不可测、缓存难、`check_current_facts` 解析不了）。

---

## 7. 易用性（14 个拒绝逐条处理）

### 7.1 归一化层放哪

**位置**：`src/data_supply/wire.rs` 的解码（`Deserialize` 之后、`QueryPlan`/命令构造之前）加一层
`normalize`，只做**形状归一**，不碰业务判断：

| 归一化 | 规则 | 是否进幂等摘要 |
|---|---|---|
| 实体 | `"sh.600000"` / `{exchange,code}` → `(exchange, code)`；6 位数字校验不变 | 是（规范化后的值） |
| 日期 | `YYYYMMDD` / `YYYY-MM-DD` → `YYYY-MM-DD` | 是 |
| `contract_version` | 缺省 `"v1"`（有契约的工具）；`get_data_capabilities` 明确**不接受** | 是 |
| `version` | 缺省 `{"kind":"latest_per_entity"}`；`{"kind":"latest"}` 是别名 | 是 |
| `filter` | catalog 可省略（缺省 `{"kind":"catalog"}`）；market 数据集缺省 → 拒绝并提示"必须给日期区间" | 是 |
| `operation`（`list_tasks`） | 枚举补齐 `update_data`/`plan_market_update`/`run_market_update` | 是（任务过滤） |
| `symbol`（`verify_data_gap`） | 接受 `"sh.600006"`，内部转 `Security` | 是 |
| `expected_revision`（`cancel_task`） | 缺省读当前 revision（描述已说"只有终态任务强制"） | 否（是 CAS 令牌，不是业务参数） |
| `item_id` | 输出改结构化 `{dataset, entity, period}`；`retry_task` 仍收原字符串 ID | 否（出参形状） |

**硬约束**：归一化只能"接受更多写法、产出同一语义"；任何改变身份、版本、时点、幂等的改动都不在
本层（AGENTS.md）。测试方式：对同一业务请求分别用"友好写法"和"契约写法"调用，断言两者产生的
**规范化请求**（幂等摘要）**逐字节相等**。

### 7.2 逐条处理（XDM-18 §4 的 15 行）

| # | 今天的拒绝 | v8 的友好写法 | 语义是否变 | 证据级 |
|---|---|---|---|---|
| 1 | `entity` 字符串 → `expected struct Security` | 接受 `"sh.600000"`；输出统一字符串 | 否（同一 `(exchange, code)`） | 实测（本次重跑） |
| 2 | `version.kind=latest` → `unknown variant` | `version` 可省略（缺省 `latest_per_entity`）；`latest` 作别名 | 否（显式 pin 仍需完整枚举） | 实测 |
| 3 | 缺 `contract_version` → `missing field` | 有契约的工具缺省 `v1`；`get_data_capabilities` 明确不接受并给替代（`dataset`/`operation`） | 否 | 实测 |
| 4 | 日期 `20261023` → `invalid characters` | 接受 `YYYYMMDD` 与 `YYYY-MM-DD`，规范化；错误点名字段与期望格式 | 否 | 实测 |
| 5 | 缺 `filter` → `missing field` | catalog 可省略；market 拒绝并写"必须给日期区间（`filter.kind=daily`）" | 否 | 实测 |
| 6 | `market.daily` + `selection.kind=catalog` → `INVALID_MARKET_COMMAND` | 错误列出该 dataset 允许的 `selection` 种类；schema 用 dataset 判别 | 否 | 实测 |
| 7 | `list_tasks.operation="update_data"` → `unknown variant` | **修枚举**（`TaskOperation` 补 `update_data` 等，`src/data_supply/wire.rs:337-341`） | 否（只是能过滤） | 实测 |
| 8 | 身份两套写法（`verify_data_gap` 用 `symbol`） | 统一 `"sh.600006"` 或同一对象类型 | 否 | 沿用 2026-10-26T03:54Z |
| 9 | `item_id` 是二次编码 JSON 字符串 | 出参改 `{dataset, entity, period}`；`retry_task` 仍收原 ID | 否（`retry_task` 的入参兼容） | 沿用 |
| 10 | `cancel_task` 必填 `expected_revision` | 改为可选：缺省读当前 revision；显式传时保留 CAS | 否（CAS 语义保留） | 沿用 |
| 11 | `get_task` 要 `contract_version`、`get_task_progress` 不要 | 统一：都缺省 `v1` | 否 | 实测 |
| 12 | `update_data.items` 无 dataset 判别 | 加 `if/then` 或 `items.kind` 常量判别；错误点名"daily 要 start/end，yearly 要 years" | 否 | 沿用（schema 阅读） |
| 13 | 写 selection 用 `explicit`/`default`，读用 `single`/`batch` | 统一命名，映射写进契约 | 否 | 沿用 |
| 14 | 任务里的 `contract_version` 是 `explicit-range-task-v1` | 出参区分 `task_contract_version` 与 `wire_contract_version` | 否 | 沿用 |
| 15 | `cursor` 跨用 | 语义不变；错误点名哪一项变了（entity/filter/page_size/version/contract） | 否 | **未验证**（§16 U-3） |

**不做的事**：不改 `page_size` 1..1000、不改 `wait_seconds` 上限 50、"超了按 50 处理不报错"的
友好行为、不改 `plan_market_update` 的 423 B 响应形状（XDM-18 明列为"不是问题"）。

### 7.3 与写路径的关系

`update_data`/`start_collection`/`backfill_data`/`republish_data` 的**入参形状只加别名**（实体、日期、
缺省 `contract_version`），**受理语义一条不改**：调用级 fail-closed（D-18）、≤100 item、幂等命名空间、
`STALE_BASE` 的 item 级 `needs_verification` 全部照旧。测试：同一 `request_id` + 友好写法与契约写法
→ 第二次调用必须 `reused:true`（证明摘要一致）。

### 7.4 测试

- 每个拒绝一条集成测试（`tests/mcp/data_stdio.rs` 的 `invalid_arguments` 家族），断言：友好写法成功、
  契约写法成功、两者幂等摘要相等、业务语义字段一致。
- 保留 2 条"仍然拒绝"的测试：真正缺必填、真正跨 dataset 的 selection —— 归一化不是"什么都收"。

---

## 8. 评测

### 8.1 基线（沿用 XDM-18 §5，不重新发明任务）

8 个任务 + 发现成本 T0，全部来自 XDM-18 的实测路径：

| # | 任务 | 今天的路径 | 今天（调用次数 / 模型可见 token） | v8 路径 |
|---|---|---|---|---|
| T0 | 新会话的发现成本 | `tools/list` + `get_data_capabilities({})` | 1+1 / **48.1K** | 研究入口 `tools/list` + 紧凑目录 |
| T1 | 某只股票近一年日线（242 根） | `query_data` | 1 / **32.5K** | `query_data`（列式、`fields` 缺省） |
| T2 | 某一天全市场涨幅前 10 | 目录翻页 + 53 次 batch | 54 / **≈828K** | 1 次截面（2 个交易日 `close` + 实体表） |
| T3 | 数据是否已更新到最新交易日 | `plan_market_update` + `sync_status` | 1–2 / ≈200 | `get_data_availability(universe)` |
| T4 | 查某个行业的成员（236 只） | catalog + `industry_code` 3 页 | 3 / 6.3K（不知道行业码 +484K） | `list_catalog_facets` + 1 次 `query_data` |
| T5 | 一次日更的 plan/run | plan×2 + run×2 + 轮询 ~10 + acquire 8 | ~22 / ≈7K | 编排入口，不变（进度只回计数） |
| T6 | 某个任务为什么失败 | `list_tasks` → `get_task` → `get_task_progress` | 3 / 5.0K | 编排入口：`get_task` 带 `failed_by_reason` + 前 N 条 |
| T7 | 核实一次发布结果 | 受理 → `get_task` → `get_task_progress` | 3 / ≈750（推断） | 编排入口，不变 |
| T8 | 导出 50 股 × 3 年 | 无 MCP 路径（直读 SQLite） | 0 / — | bin 命令；**MCP 侧 0 次调用** |

### 8.2 v8 预算与推导

| # | v8 预算（模型可见 token） | 调用次数 | 推导 |
|---|---|---|---|
| T0 | **≤3.5K** | 2 | 研究入口 `tools/list` ≤11,000 B（2.75K）+ 紧凑目录 ≤1.5 KB（0.38K） |
| T1 | **≤5K** | 1 | 列式原型 16,052 B（本次实测，242 条真实记录重建）；`versions`/`metadata` 各一次 |
| T2 | **≤40K** | 1 | 实体表 5,224 × 13 B ≈ 68 KB + 2 日 `close` × 38 KB ≈ 144 KB ≈ 36K；留 10% 余量 |
| T3 | **≤200** | 1 | 一行汇总（`last_trade_day` + 覆盖计数 + `generation_ref`） |
| T4 | **≤2K** | ≤2 | facets ≈2.3 KB（推断，按行业分布外推）+ 236 × 19 B ≈ 4.5 KB → ≈1.7K token |
| T5 | **≤7K** | ≤22 | 与 v7 持平；`get_task_progress` 的 `unfinished` 分页只看计数/`next_cursor` |
| T6 | **≤2K** | ≤2 | `get_task` 直接带 `failed_by_reason` 计数 + 前 20 条错误（≈1.2 KB） |
| T7 | **≤750** | ≤3 | 与 v7 持平 |
| T8 | **MCP 侧 0** | 0 | 导出走 bin 命令（§4.7） |

判定口径（照 reading-mcp）：**总 token（含工具定义）不高于 v7、调用次数不多于 v7**；
研究任务（T0–T2、T4）另加绝对预算（上表），因为今天的基线差一个数量级，"不高于 v7"太松。

### 8.3 评测框架

沿用 `.devin/evals/` 的做法，分两条线：

1. **运维任务（T3/T5/T6/T7）→ Devin eval**（真实 Skill、真实客户端）：
   - 新增 `.devin/evals/v8-ops-eval.txt` + `.devin/evals/v8-ops-config.json`：`deny` 仍是
     `read/edit/grep/glob/exec`；`allow` == 编排入口 15 个工具 + acquire 的 8 个 Skill 工具；
   - `scripts/run_daily_sync_eval.sh` 泛化为 `scripts/run_mcp_eval.sh <run-id> <profile>`
     （`daily-sync` / `v8-ops`），输出 `request_id`/`task_id`/`generation_id`、停止原因、
     工具定义 token × 轮数；
   - 一致性门禁：`scripts/check_daily_sync_eval.py` 扩成同时校验两个 config 的 allowlist == 对应
     入口的工具集合（`v8-ops` 用编排入口的 15 个）。
2. **研究任务（T0–T2、T4、T8）→ 夹具 + 计量脚本**（不需要 Devin）：
   - `scripts/v8_token_budget.py`：读 `.devin/evals/v8-baseline.json`（每个任务的基线字节/调用数/
     预算），对**夹具响应**（`fixtures/v8/*.json`，来自实现 PR 在私有栈上抓的真实响应，脱敏后入库）
     计算 `token = ASCII 字符/4 + 非 ASCII 字符/1.5`，断言 ≤预算；同时断言 `tools/list` 的
     **工具定义 token × 调用次数**在预算内；
   - 夹具里必须记录 `version_id`/`snapshot`/`generation_ref`，否则结论不可复现（XDM-16 的要求）；
   - 真实全市场截面（T2）的字节由实现 PR 在私有栈上实测并贴 Issue；CI 只用夹具，不连生产。

### 8.4 在 CI 里怎么跑

| 检查 | 位置 | 跑什么 | 是否需要数据 |
|---|---|---|---|
| 入口封闭性 + `tools/list` 字节预算 | `tests/mcp/v8_tool_surface.rs`（Rust） | 三个入口分别序列化 `tools/list`，断言工具数与字节预算；研究入口 ∩ 写/核实 = ∅；无 `outputSchema`；capabilities 与 registry 同源 | 否（只读 registry） |
| token 预算 | `scripts/test_v8_token_budget.py`（Python） | `v8_token_budget.py` 对夹具 + `v8-baseline.json` 断言 | 否（夹具） |
| eval 配置一致性 | `scripts/check_daily_sync_eval.py`（扩） | 两个 config 的 allowlist == 对应入口工具集合；prompt sha256 | 否 |
| 真实研究任务 | 手动（隧道 + ChatGPT/Claude） | 记录 token/调用次数，贴 Issue | 是（生产只读） |
| 真实日更 | 手动（Devin eval） | 见 8.3 | 是（生产写路径，按 Skill 既有流程） |

`ci.yml` 只需加一条 Python 单测（`test_v8_token_budget.py`）；Rust 侧的
`tests/mcp/v8_tool_surface.rs` 随现有测试集自动执行，不需要新 job。

### 8.5 记录与可复现

每个评测结果必须记录：入口、契约版本、`catalog_version`、每条序列 `version_id`、`generation_ref`、
调用序列（工具名 + 参数摘要）、字节与 token、测量时刻。缺任一项的结果只能算"观测"不能算"验收"
（与 XDM-16 的可复现要求一致）。

---

## 9. 契约与迁移

### 9.1 升版内容（`generic-data-v8`）

| 面 | v7（今天） | v8 | 兼容性 |
|---|---|---|---|
| `capability_revision` | `generic-data-v7` | **`generic-data-v8`** | 客户端按 revision 握手，不匹配即停（既有 fail-closed 行为） |
| data 工具数 | 23 | **24**（+`list_catalog_facets`） | 旧工具名全部保留，无重命名、无删除 |
| `tools/list` | 含 `outputSchema` | **不含** `outputSchema`；描述 ≤240 B | 客户端失去内联校验；schema 从 capabilities 取 |
| 响应形状 | 行式对象数组 + 逐行 `version_ref` | **列式 + 版本清单一次**（`contract_version:"v2"`） | v1 分支保留（§9.2） |
| 入口 | 1 个 data socket（23 工具） | **3 个入口**（5 / 15 / 9） | `mcp.sock` 变成 15 个工具（对 Skill 无影响，对仓库外脚本有影响，§9.5） |
| 入参 | 结构体实体、严格日期、必填契约版本 | **别名归一化**（实体/日期/缺省值） | 旧写法继续可用 |
| 时点参数 | 无 | **保留名 + 明确拒绝**（`as_of`/`known_at`） | 新增错误，不是新语义 |
| schema | 21 | 21（**不变**） | 部署不需要归档重建（与 M1 的 22 分开） |
| `contract_version` | `v1` | `v1` + **`v2`** | 每工具独立分支；缺省 `v1` |

**v2 覆盖哪些工具**（其余工具只做入参归一化与 text 分级，响应形状保持 v1）：

| 面 | v2 | 保持 v1 |
|---|---|---|
| 读取 | `query_data`、`get_data_availability`、`list_data_versions`、`list_catalog_facets`（新工具直接 v2） | — |
| 发现 | `get_data_capabilities`（紧凑目录 + 详版） | — |
| 任务 | `get_task`、`get_task_progress`、`list_tasks` | `get_request` |
| 写受理 | — | `start_collection`、`update_data`、`backfill_data`、`republish_data`、`retry_task`、`cancel_task` |
| 核实 | — | `verify_*`、`inspect_verification_task`、`run_worker_once`、`compute_metrics` |

理由：写受理的**响应**是 `WriteAcceptance`（小、已经稳定），改它没有收益、只有风险（D-18 的语义面）。

### 9.2 v7 并行期与下线

- **并行期从合并起算，覆盖两个验收点**（与 XDM-18 §6.4 一致）：
  1. 日更 Skill 在 `generic-data-v8` 下跑通一次**真实日更**（T5）；
  2. 下游 `x` 仓库 `KlineReader` 在 v8 上适配通过（P-02 的另一半，另一仓库，须另行批准）。
- **并行期不短于部署后 10 个交易日**（与 #304 的 Skill-only 连续验收同一口径）：两个验收点都完成、
  且满 10 个交易日后，才能提“下线 v1 分支”的兼容评估。任一验收点未完成 → 并行期无限延长，
  **不以“时间到了”为由下线**。
- **v1 分支与 v7 契约文档**在并行期内保留：`docs/contracts/generic-data-v7.md` 标为历史，
  新增 `generic-data-v8.md`；两个 Skill 的 revision 断言在**部署日**翻转（不在实现 PR 里提前翻）。
- **下线旧分支**（v1 响应 + v7 契约文档）走独立兼容评估（`docs/generic-data-tools.md`「下线旧分支
  需独立兼容评估」），**不夹在 v8 同一次切换里**。

### 9.3 切换清单（新文件 `docs/contracts/v8-switch-checklist.md`）

按 v6/v7 清单的写法（锚点不用行号），部署日翻转的常量：

| 文件 : 锚点 | 旧 | 新 | 何时翻 |
|---|---|---|---|
| `scripts/check_current_facts.py` `EXPECTED_DATA_TOOLS` | 23 | **24** | 与 capability bump 同提交（代码常量） |
| 同上 `EXPECTED_DEPLOYED_DATA_TOOLS` | 23 | **24** | 部署日 |
| 同上 `EXPECTED_DATA_CAPABILITY_REVISION` | `generic-data-v7` | `generic-data-v8` | 与 capability bump 同提交 |
| 同上 `EXPECTED_SKILL_CAPABILITY_REVISION` | `generic-data-v7` | `generic-data-v8` | 部署日（与 Skill 同提交） |
| 同上 `REQUIRED_FACTS` 的 `23 data tools` 正则 | 23 | **24** | 部署日（与文档当前区同提交） |
| 同上 `EXPECTED_COMPACT_SCHEMA` | 21 | 21（**不变**，M1 的 22 是另一条线） | — |
| `scripts/smoke_release.py:49,174` | `len(tools) == 23` | `== 24`，并改为**按入口**断言（研究 5 / 编排 15） | 与 capability bump 同提交 |
| `scripts/deploy_preview.sh:299` 的 `EXPECTED_TOOLS` | 19 个工具名（v3 时代历史脚本） | 先确认是否仍在用；在用则改为入口名集合 | 部署前确认（§17 第 3 条） |
| `tests/service/local_runtime.rs:214` | 23 | 24 | 同提交 |
| `tests/mcp/data_stdio.rs:70` | 23/22 | 24/23 | 同提交 |
| `tests/service/acquire_service.rs:941,1009` | 10 | 10（不变） | — |
| `.devin/skills/x-data-daily-sync/SKILL.md` revision 断言 | `generic-data-v7` | `generic-data-v8` | 部署日 |
| 同上 allowlist（15 个） | — | **不变**（编排入口就是这 15 个） | — |
| `.devin/skills/x-data-status/SKILL.md` | v7 | v8 | 部署日 |
| `.devin/evals/daily-sync-config.json` + 新 `v8-ops-config.json` | v7 allowlist | v8 allowlist（同 15 + acquire 8） | 部署日 |
| 5 个「当前状态」区（README / daily-sync / mcp-tools / generic-data-tools / source-closure）+ `docs/STATUS.md` | v7 / 23 | v8 / 24 / 三入口 | 部署日 |

另需在 `docs/STATUS.md`「服务」表加两行（research / maint 单元与 socket），并在
`check_deploy_facts.py` 的单元清单里同步（实现 PR 做，不在本设计 PR）。

### 9.4 回滚

| 场景 | 做法 | 代价 |
|---|---|---|
| 实现合并后发现问题（未部署） | 回退 PR；`mcp.sock` 仍是旧二进制 | 无 |
| 部署后响应形状有问题 | symlink 切回上一版 release（v8 不改 schema，**不需要库还原**） | 已用 v2 受理的写请求在回滚后读任务时会报未知契约（v7→v6 已有同类代价，`docs/STATUS.md` 记录过） |
| 入口拆分有问题 | 隧道 drop-in 指回 `mcp.sock`（此时它是编排入口，ChatGPT 会看到 15 个工具——**能工作但比今天更宽**，属可接受的临时状态）；或停 `research`/`maint` 单元 | 一次 drop-in 改动 + 一次隧道重启 |
| 研究入口暴露了不该给的工具 | 停 `x-data-mcp-research` 单元（隧道随之退出，`Restart=always` 会重试 → 需先停隧道） | 研究会话不可用，直到修好 |

### 9.5 影响面

| 面 | 影响 | 需要谁 |
|---|---|---|
| **日更 Skill** | allowlist **不变**（15 个工具）；revision 断言 v7→v8；`get_task_progress` 的 v2 出参（`failed_by_reason` 直出）可以简化 §3.2/§3.5 的"只看计数"说明；`SKILL.md` 的"registry 很大 / 不得读 overflow"整段可以删（入口变小，编排入口的 `tools/list` 也从 84,028 B 降到 ≤27,500 B） | 本仓库 |
| **状态 Skill** | revision 断言 | 本仓库 |
| **Devin eval** | 新 profile + config；prompt sha256 常量随之更新（`scripts/check_daily_sync_eval.py:13`） | 本仓库 |
| **`x` 仓库 `KlineReader`** | 只列影响点：① 读 `payload.records[]` → 改读 `columns`（v2）；② 读 `record.version_ref.version_id` → 改读 `versions[entity].version_id`；③ 读 `manifestItem.metadata.adjustment` → 改读顶层 `metadata.adjustment`；④ 6dp 精度（D-32 / P-02 的部署阻塞项）；⑤ 连 `mcp.sock`（编排入口）仍可用，或改连 `research.sock`（Q6）。**修改需另行批准** | owner + 另一个仓库 |
| **Rust 测试** | 工具数硬编码 3 处（§9.3）；`outputSchema` 的 jsonschema 校验 14 处（`tests/mcp/data_stdio.rs:102,164,188,201,227,285,332,665,968,1064,1191`、`tests/mcp/metrics_stdio.rs:77,82`、`tests/mcp/yearly_presence.rs:373`、`tests/verification/stopped_verification.rs:1123`）改为从 capabilities 或 registry 取；capabilities↔tools/list 相等断言（`data_stdio.rs:99-103`）改为对比 `entries()` | 本仓库 |
| **文档事实门** | `check_current_facts.py` 的 `data_tool_names(registry)` 解析 `src/data_supply/registry.rs`；入口分片后仍是"一个 registry"，但计数要按入口（`data_tool_names` 返回全部 24，另加三个入口的计数断言） | 本仓库 |
| **部署脚本** | `deploy_preview.sh` 的 `EXPECTED_TOOLS`、`smoke_release.py` 的 23、`switch_binary.sh` 的 S7 提示行（v7 清单已记录该文案陈旧） | 本仓库 |
| **公开仓库卫生** | 新增文档不得出现邮箱/主机名/IP/内部路径（`check_public_hygiene.py`） | 本 PR 已遵守 |

---

## 10. 与 M1 的衔接

### 10.1 复用 M1 的读取核心（分离点 5）

M1 的 PR-8 交付 `src/research/`：`SnapshotRequest` → `Snapshot`（清单 + 流式行迭代），
导出 bin 与（v8 的）MCP 研究入口**共用同一个读模型**。v8 只做三件事：

1. **投影**：把 `Snapshot` 的行迭代按 `fields` 投影成列式（面板 / 截面 / 表三种布局）；
2. **选择**：把 `selection.kind=universe` 解析成"目录版本冻结的成员集合"（M1 的清单里已有目录版本）；
3. **分页**：复用 M1 的清单与游标语义，不新造快照机制。

**不重复实现读路径**的判据（可测）：v8 的实现 PR 里 `src/data_supply/` 不得出现 SQL；所有读都经
`src/research/` 的用例入口（用 `grep -r "SELECT" src/data_supply/` 为空作为一条 CI 检查/评审项）。

### 10.2 M1 实现 PR 里影响 v8 的改动点

| M1 PR | 改动 | 对 v8 的影响 | v8 侧要做的事 |
|---|---|---|---|
| PR-1 schema 22 | `market_series.adjustment/price_scale/instrument_class`；`series_bars` 价格列按 scale 解释 | v8 的 `adjustment` 选项与 `price_scale` 元数据依赖这些列 | v8 只读，不改；`price_scale` 直接进 `metadata.adjustment` |
| PR-2 不复权序列 | 同一 item 写两条序列；`Adjustment::Raw` 与 `"raw"` 参数化 | `adjustment:"raw"` 的读取路径由 M1 提供 | 研究入口把 `adjustment` 从"只有 qfq"扩成 `raw`/`qfq`，缺省 `raw` |
| PR-3 精确 6dp | `qfq-exact-v1`、`algorithm_version` 落库 | 前复权响应的精度与 `algorithm_version` | 列式输出保留字符串，不改精度；`algorithm_version` 进 `metadata` |
| PR-4 因子事件 | `adjustment_events` + `derived_dataset_versions` | 新 dataset `market.adjustment_events` | 加 dataset 分支 + 列定义（`effective_day/category/c1..c4`）；`known_at` 不输出（恒 NULL，M1 已声明） |
| PR-5 指数 | `index_registry` + `index_series_meta`；**MCP 读路径过滤 `instrument_class=0`**（AC-M1-10） | 这条过滤会让指数在 v8 里"看不见" | **v8 必须替换这条过滤**：改为"`dataset=market.index` 才读 `instrument_class=1`"，并加测试（`query_data(market.daily, sh.000001)` → `absent`；`query_data(market.index, sh.000001)` → 有数据） |
| PR-6 交易日历 | `trading_days` + `cal-union-v1` | 新 dataset `market.calendar`（market 级，无实体） | 加 `selection.kind=market` 与 `layout:"table"` |
| PR-7 日更集成 | `derived:calendar`/`derived:index:*` 内部 item；进度里 `entity`/`period` 为 null | v8 的任务面 v2 要把"内部 item"如实呈现（`item_id` 结构化后是 `{dataset:"derived", ...}`） | 任务面 v2 的 `item_id` 形状要能表达非证券 item（不能假设一定有 entity） |
| PR-8 导出 | `src/research/`、清单、`snapshot_id` | v8 的研究入口与截面都跑在它上面 | 复用；v8 不新增导出工具（§4.7） |

**接口约定**（避免两条线互相等待）：M1 的 `SnapshotRequest` 至少要能表达
`(dataset, 成员集合或目录版本, 日期区间, adjustment, 字段投影, 页大小/游标)`；v8 的 PR-3/PR-4 依赖它。
如果 M1 的 PR-8 晚于 v8 的 PR-3，v8 的 PR-3 只能先落在"现有 `query.rs` 的行式读 + 列式投影"上，
并在 PR-8 落地后改接——**这是可接受的临时状态，但要写进 PR 正文**（避免悄悄变成第二份读路径）。

---

## 11. 分离点落位

| # | 分离点 | 本设计的设计点 | 边界靠什么守住 | 过度设计防线 |
|---|---|---|---|---|
| 1 | 来源接入层 ↔ 领域层 | acquire 保持独立（10 工具、不改）；研究入口只依赖读取核心；运维入口才碰 generation 与取证 | **依赖方向**：研究入口的 allowlist 里没有任何写/核实/采集工具；**测试**：研究入口 ∩ 写/核实 = ∅（双向）；**契约**：`requires_local()` 继续作纵深防御 | 不新建 crate；不加动态插件；入口归属是 `Entry` 上的一个枚举字段 |
| 2 | 原始证据 → 解析事实 → 派生 | 列式只改读模型；`adjustment` 与 `algorithm_version` 证据留在 `metadata`；不复权是缺省（事实优先），前复权是选项（派生） | **契约**：`metadata.adjustment{algorithm_version, price_scale, basis}` 必出现且只出现一次；**测试**：同一 `version_id` 的 qfq 可由 raw + 因子重算（沿用 M1 AC-M1-2）；响应里不出现"派生表" | 不新增派生结果表；不把 qfq 写死进响应 |
| 3 | 生效时间 ↔ 知晓时间 | `as_of`/`known_at` 只预留名字与语义；v8 明确拒绝；`market.adjustment_events` 只输出 `effective_day`（`known_at` 恒 NULL 就不输出） | **契约**：保留名错误码 `reserved_for_m2`；**测试**：请求这两个参数必被拒且消息指向 M2；**没有数据就不给字段** | 不预先造 `as_of` 空参数、不返回"可能是 null 的 known_at" |
| 4 | 存储模型 ↔ 研究读模型 | 截面/面板/表三种布局都是读模型契约；`src/data_supply/` 不写 SQL | **依赖方向**：MCP 层只调 `src/research/` 用例；**测试**：换存储实现不改响应 schema（列式布局的 golden test 与库无关） | 不为每张表建 trait；`series_bars` 的列名不泄漏到响应 |
| 5 | 核心用例 ↔ 入口适配 | 研究 / 编排 / 运维 = 同一读取核心的三个适配；导出是第四个适配（bin） | **契约**：`tools/list`、capabilities、dispatch 同源（`entries()`）；**测试**：入口过滤后 capabilities 与 `tools/list` 仍一致；三个入口的 `query_data` 行为逐字节相同 | 不做"万能工具"；不复制三份 schema；入口只是 allowlist |
| 6 | 交易规则 ↔ 数据 | v8 **不**暴露涨跌停价；M2 的规则数据（生效区间 + 依据）到位后，作为 `market.price_limits` dataset 挂到 `query_data`，不加工具 | **契约**：规则数据带 `effective_from`/`basis_ref`；**测试**：创业板 2020-08-24 前后两套幅度（M2 落地时）；v8 只断言"代码里没有幅度常量" | 不造规则 DSL；不预先建规则表 |
| 7 | 编排 ↔ 原子能力 | 编排入口只提供原子能力（plan/run/task/status），不提供"一次跑完选股+回测"；研究入口不提供工作流 | **契约**：工具只返回本动作的 `state/outcome/certainty/refs/evidence`，不返回 `next_actions`/其他工具名 | 不把 Skill 流程塞进工具；编排入口 == Skill allowlist 正是"原子能力"的证明 |

**现有循环依赖**（`market_data`、`publish`、`storage`，XDM-1 §2.3）：v8 会碰读取路径（列式投影、
`adjustment` 选项），**碰到时顺手消除，不单独立项重构**。

---

## 12. 扩展场景走查（6 个）

先读 `docs/design.md`「后续数据扩展要求」：本设计与之**一致**（新增来源只改 Adapter；新增周期/计算
改 schema 与契约版本；新增类别用独立模型 + 通用工具 schema 分支）。需要补的 3 处单列在 §13。

| # | 场景 | 会改哪些层 / 分离点 | 新增 | 修改 | 能否"只改适配层 + 一处登记" |
|---|---|---|---|---|---|
| 1 | 新增证券类型（ETF / 指数 / 北交所 / B 股） | 分离点 1、2、6；契约层（`Dataset`/`Exchange`/记录列） | 目录 scope 的一行登记（`bj`/基金/指数成员）；该类型的 `price_scale` 与币种列 | `wire.rs` 的 `Exchange`（今天 `sh`/`sz` 被内联在 9 个工具的 schema 里）、`catalog` 解析、`market.index` 之外可能新增 `market.fund` dataset、`x` 仓库映射 | **指数：能**（M1 的 `index_registry` 一行 + v8 的 dataset 分支）。**ETF：不能**——需要身份名单输入（M3）、`price_scale=3` 的读取验证、量额单位（份/手）、cat11；**B 股：不能**（币种字段是结构改动）。这三条是 `docs/data-catalog.md` §3.2 第 13/16 行已裁定的 M3/不在范围 |
| 2 | 新增一个官方来源 | 分离点 1 | 新 Adapter + #308 证据包 + `source_bindings` 的新 `role` | 装配处一处登记；capabilities 的 `supported_sources` | **能**（设计目标）。研究入口不受影响，只多一个 `pending_role`；v8 的入口拆分让"新来源"不会顺带涨研究入口的工具数 |
| 3 | 新增字段或数据集（例如以后放开财务数据） | 分离点 2、4；契约层 | 独立领域模型 + 类型化表 + migration + **一个 dataset 分支 + 一列定义** | `wire.rs` 的 `Dataset`、`query.rs` 的投影、v8 契约文档的 dataset 矩阵 | **能**（v8 之后）：因为 schema 细节已移出 `tools/list`，新增 dataset **不再涨 `tools/list` 体积**（今天每个 dataset 会让 query/availability 各涨 2–4 KB，XDM-18 §8 场景 3）。财务本身仍要独立模型（`docs/design.md` 已裁定，不强行套 Bar） |
| 4 | 新增频率（分钟线） | 分离点 2、4 | 新 `period` 或独立 dataset + 时间戳列（`trade_day` 是日序号整数） | `Period` 枚举、聚合/血缘、列定义、分页预算 | **不能**：`series_bars` 的主键与 `trade_day` 语义都是"日"；240 根/日 × 5,224 只 = 125 万根/日，必须列式 + 导出，不能走对话。这是"存储跟着数据走"的正当例外 |
| 5 | 交易规则变更（涨跌停幅度调整） | 分离点 6 | 规则表一行（生效区间 + 依据引用） | **无代码改动**；v8 之后加 `market.price_limits` dataset 时也只有 dataset 分支 | **能**（M2 落地后）。v8 保证：代码里没有幅度常量，规则来自数据 |
| 6 | 新增使用方或入口（v8 之后再加一种导出格式 / 再加一个入口） | 分离点 4、5 | 新入口 = `EntryKind` 一个变体 + 一个 socket + 一条 allowlist；新导出格式 = 一个 writer（`Snapshot` → 字节流） | 三个入口的字节预算表加一行；`tools/list` 按入口断言的测试加一条 | **能**：入口是一个枚举 + 一张静态表；导出格式是纯函数。代价是每加一个入口多一个 systemd 单元与一条就绪探针 |

**达不到"只改适配层"的三处**：场景 1 的 ETF/B 股（结构差异：精度、币种、身份来源）、场景 4（分钟线
的主键语义）、以及场景 3 的财务（`docs/design.md` 已裁定的独立领域模型）。不为达标强行抽象
（复杂度预算）。

---

## 13. 需要修改 `docs/design.md` 的地方（单列，本 PR 不改）

1. 「运行、访问与配置设计」：补一段"**工具面按使用者分入口**"——研究/编排/运维三个入口、
   隧道只暴露研究入口、入口是同一读取核心上的适配（分离点 5）、`mcp.sock` 的语义变化。
2. 「后续数据扩展要求」→「三类扩展及影响范围」：补第四类"**工具面/入口扩展**"（新增入口只改
   `EntryKind` + 装配；新增 dataset 不涨 `tools/list`），并说明"记录 schema 移出 `tools/list`、
   按需从 capabilities 取"是新增类别的成本控制手段。
3. 「后续数据扩展要求」→「扩展契约」：补两条——① **`outputSchema` 不进 `tools/list`**，
   schema 细节按需取，`tools/list` 只保证"不查帮助也能传对参数"；② **时点参数
   （`as_of`/`known_at`）属于契约保留名**，没有对应数据时不接受、不静默忽略。
4. 「AI 编排与原子能力」：补一句"入口 ≠ 编排：编排入口只是把同一批原子能力放在一个 socket 上，
   不返回工作流、不返回 `next_actions`"。
5. M1 设计 §13 已列的 5 条（派生数据、规则数据、时点主表、前复权含未来事件、价格精度是序列属性）
   与本设计不冲突，**建议一并改**（两条线合并成一次 `docs/design.md` 更新，避免同一节改两次）。

---

## 14. 实现拆分（6 个可独立合并的 PR）

通用约束：本机不编译，Rust 正确性只由 PR 上的 CI 证明；每个 PR 都跑 §15 的本机脚本门禁；
每个 PR 的正文写清 AC 编号、测试计划、依赖与 CI 验证方式。

| PR | 范围 | AC | 测试计划（CI） | 依赖 |
|---|---|---|---|---|
| **PR-1** 入参归一化层 | `src/data_supply/wire.rs` 的 `normalize`（实体字符串/日期/缺省 `contract_version`/缺省 `version`/catalog 的 `filter` 可省/`TaskOperation` 补 `update_data` 等/`symbol` 别名/`expected_revision` 可选）；错误消息点名可选值；保留名 `as_of`/`known_at` 的 `reserved_for_m2` | AC-V8-10、AC-V8-11 | 每个拒绝一条集成测试（友好写法成功、契约写法成功、幂等摘要逐字节相等）；2 条"仍然拒绝"；`list_tasks(operation=update_data)` 能过滤到生产那 519 个任务类型 | 无（第一个） |
| **PR-2** 入口拆分 + `tools/list` 瘦身 | `Entry.entry: EntryKind` + 三张 allowlist；`data_service --entry`；`research.sock`/`maint.sock`；`public_tools()` 去 `outputSchema`；capabilities 紧凑目录 + 详版；23 个描述改到 ≤240 B；`tests/mcp/v8_tool_surface.rs`（字节预算 + 封闭性 + 同源） | AC-V8-1、AC-V8-2、AC-V8-3、AC-V8-7（tools/list 部分） | 三个入口的 `tools/list` 字节断言；研究入口 ∩ 写/核实 = ∅（双向）；capabilities ↔ `entries()` 逐字节相等；`tests/mcp/data_stdio.rs` 的 14 处 outputSchema 校验改为从 capabilities/registry 取；工具数 23→24 | PR-1（错误消息形状稳定后再冻字节） |
| **PR-3** 列式响应 v2（读取面） | `query_data`/`get_data_availability`/`list_data_versions` 的 v2：三种布局、`versions` 一次、`metadata` 共享、空值规则、`fields` 投影；text 分级 + `X_DATA_MCP_TEXT_MODE`；`v1` 分支保留 | AC-V8-4、AC-V8-12 | 列式 golden test（固定夹具逐字节）；`fields` 投影与全字段的数值逐位相等；`v1`/`v2` 同请求数值一致；text 三档开关；分页边界不变 | PR-2 |
| **PR-4** 截面 + M1 数据集 | `selection.kind=universe`/`market`、`filter.kind=cross_section`（5 日上限）、`list_catalog_facets`；`market.index`/`market.adjustment_events`/`market.calendar` dataset 分支；替换 M1 PR-5 的 `instrument_class=0` 过滤 | AC-V8-5、AC-V8-6、AC-V8-8 | 截面布局与 `trade_days` 长度；窗口超限拒绝；`market.daily(sh.000001)` → `absent` 而 `market.index(sh.000001)` 有数据；日历无实体；facets 与 catalog 一致 | PR-3、M1 的 PR-4/5/6 |
| **PR-5** 任务面 v2 | `get_task`/`get_task_progress`/`list_tasks` 的 v2：`failed_by_reason` 直出 + 前 20 条错误、结构化 `item_id`（含非证券 item）、`reason_code`/`status` 过滤 | AC-V8-9（T6 部分） | 失败任务的诊断信息一次取全（夹具）；`item_id` 往返（`retry_task` 仍收旧字符串 ID）；内部 item（`derived:*`）不假设有 entity | PR-3 |
| **PR-6** 契约与切换清单 | `docs/contracts/generic-data-v8.md`、`docs/contracts/v8-switch-checklist.md`、`docs/design.md` §13 的五处、Skill 与 eval 的**部署日**改动（单独提交，不提前翻）、发布说明草稿、`STATUS`/当前状态区的部署日清单 | AC-V8-13、AC-V8-14 | `check_current_facts` / `check_deploy_facts` / `check_daily_sync_eval` / `check_public_hygiene` / 全部 `scripts` 单测；`docs/contracts/*` 的 schema 快照（若新增） | PR-1…PR-5 |

拆分原则：PR-1 只改"接受什么写法"，不改任何响应字节，可以单独合并、单独回滚；PR-2 只改"发布哪些
工具、发布多细"，响应仍是 v1 行式，因此**入口拆分可以先上线、列式后上线**；PR-3 才改响应形状（v2）；
PR-4 才引入新能力（截面与新 dataset）；PR-5 与 PR-3 独立；PR-6 是部署前的收口。

---

## 15. 本机与 CI 验证方式

- 本机**禁止** `cargo build/test/check/clippy/nextest/run`；只允许 `cargo fmt`（D-11）。
- 本机能跑：`git diff --check`、`cargo fmt --check`、`python3 scripts/check_current_facts.py`、
  `python3 scripts/check_daily_sync_eval.py`、`python3 scripts/check_deploy_facts.py`、
  `python3 scripts/check_public_hygiene.py`，以及 `.github/workflows/ci.yml` 里的各条
  `python3 -B -m unittest discover -s scripts -p '<test_*.py>'`（`test_runtime_peer.py` 需要 sudo 与
  编译产物，跳过并注明）。
- Rust 正确性只由 PR 上的 GitHub Actions 证明；没有 CI 结果时一律写"未运行"。
- 每个实现 PR 的 CI 验证方式（写进 PR 正文）：
  1. `cargo fmt --check`、`cargo clippy --all-targets --all-features --locked -- -D warnings`；
  2. 现有全量测试 + doc tests（`tests/mcp/v8_tool_surface.rs` 在其中）；
  3. `cargo build --locked`；
  4. 需要真实数据的 AC（T2 的真实字节、T4 的真实行业分布）**不在 CI 里伪造**：用固定夹具覆盖逻辑，
     真实数字在私有栈上实测后贴 Issue。
- 本设计的文档 PR 自身要跑：`git diff --check`、`cargo fmt --check`（无 Rust 改动）、四个 check 脚本、
  全部 `scripts` 单测、`prepush_check.py`。

---

## 16. 实测、未验证与待确认

### 16.1 本次新增实测（可复现）

- `tools/list` 字节复核：data 23 工具 / **115,496 B**（desc 10,079 + input 32,457 + output 71,218）；
  acquire 10 工具 / **6,040 B**；按入口汇总：研究 4 工具 desc+input 9,225（含 output 24,280）、
  编排 15 工具 28,674（含 output 84,028）、运维 9 工具 14,615（含 output 33,297）。
- 14 个入参拒绝逐条复现（§2.1.1）。
- 列式原型：用 XDM-18 抓到的 242 条真实记录（`sh.600000` 2025-10-24 起）重建列式信封，
  **16,052 B**（行式 127,669 B），每行 66.3 B。
- 截面外推：实体表 5,224 × 13 B ≈ 68 KB；`close` 每行 7.3 B × 5,224 × 2 日 ≈ 76 KB；
  合计 ≈144 KB ≈ 36K token。
- 脚本与产物：本任务工作目录（仓库外）的 `capture_tools.py`、`probe_inputs.py`、
  `analyze_sizes.py`、`entry_budget.py`、`proto_columnar.py`，全部在
  `systemd-run --scope -p MemoryMax=150M -p MemorySwapMax=0` 下运行。

### 16.2 未验证（如实列出）

| # | 项 | 为什么未验证 | 取证方法 |
|---|---|---|---|
| U-1 | 各客户端是否把 `structuredContent` 注入模型上下文 | 需要真实客户端会话 | 用 ChatGPT 隧道、Claude connector、Devin 各跑一次同一 `query_data`，对比模型实际看到的字段/ token |
| U-2 | Devin 的截断阈值与 `text` 依赖 | 官方文档未规定；只有 Skill 的经验性描述 | 部署前用 Devin 跑一次 v8 编排入口，观察大响应是否被截断、`summary` 是否够用 |
| U-3 | cursor 跨用拒绝的精确错误码 | 只读探测未构造该场景 | 实现 PR 里用两条查询构造（换 entity/换 `contract_version`） |
| U-4 | 全市场截面的真实字节 | 未实现；本次按 242 条外推 | PR-4 在私有栈上实测一次，贴 Issue |
| U-5 | `list_catalog_facets` 的真实体积 | 未实现；按行业分布外推 | PR-4 实测 |
| U-6 | `x` 仓库 `KlineReader` 的适配成本 | 另一个仓库，P-02 未决 | owner 确认；读该仓库的 reader 与测试 |
| U-7 | 三个 systemd 单元的内存与冷启动影响 | 需要编译产物 | 部署前在私有栈上测（单库三进程只读，内存增量按页缓存估） |
| U-8 | 真实 tokenizer 计数 | 用 ASCII/4 估算 | 用一次 API 的 usage 字段对同一响应计量 |
| U-9 | 隧道侧实际看到的研究入口工具数 | 隧道配置不在仓库 | 隧道改指 `research.sock` 后用 `tools/list` 复核 |

### 16.3 待确认（与 Issue 的 Q1–Q6 对应）

见 #387 的「待确认问题」。本设计对每一项都给了默认方案（A1–A5），审核时只需确认或否决。

---

## 17. Also noticed（范围外，只记录）

1. `TaskOperation` 只有 3 个变体（`src/data_supply/wire.rs:337-341`），生产 579 个任务里 519 个是
   `update_data` —— XDM-18 判为"缺陷不是风格"，v8 的 PR-1 修它。
2. `src/data_supply/mcp.rs:595` 已有"按访问上下文过滤工具"的机制，入口拆分直接复用这个过滤点，
   不需要新机制（本设计的复杂度预算依赖这一条）。
3. `scripts/deploy_preview.sh:299` 硬编码 19 个工具名，是 v3 时代的历史脚本；v8 前应确认它是否仍在
   被使用，否则会误报 smoke 失败。
4. `get_data_capabilities` 无过滤时 `input_schema`/`output_schema` 全是 null（XDM-18 §3.3），与
   `docs/generic-data-tools.md` 的"完整 input_schema、output_schema"目标不符；v8 的"紧凑目录 +
   按需详版"顺带修掉这个不一致，但**语义变了**（无过滤不再返回详版），要写进契约。
5. `docs/mcp-tools.md` 的「读路径有界化说明」段仍是"开发分支"口吻，v8 改响应形状时这段会再次过期，
   建议在 PR-6 一并改成现状描述。
6. 4 个 verify/inspect 工具占 `tools/list` 20.5% 却 0 生产使用（XDM-18 §2.3）——v8 只把它们移出入口，
   **是否下线要独立的兼容评估**（`docs/generic-data-tools.md`「下线旧分支需独立兼容评估」）。
7. `get_data_availability` 的 `related_tasks` 逐实体重复（5 实体 12,180 B，XDM-18 §3.3）；v8 的
   "共享常量只出现一次"规则顺带修掉，但那是**响应形状**变化，属 v2，需要在契约里写清。
