# generic-data-v8 部署日切换清单

**状态**：已执行（2026-10-27，最小切换；2026-10-28 preview.38 起单进程三 socket，见 §9.7）。
当前部署为 `generic-data-v8` / 15 data tools（`mcp.sock` 的 `ops` 入口）、`research.sock` 的 `research`
入口与 `maint.sock` 的 `maint` 入口（仅本机运维）/ compact schema 24（见 [STATUS](../STATUS.md)）；
隧道改指 `research.sock`（Ops-D）仍未做；Ops-C 已由 AI 客户端（pi）实测（2026-10-28），结论见 §2.3。
契约本身见 [generic-data-v8](generic-data-v8.md)。

已合并并随 preview.38 部署：`data_service --entry research|ops|maint`（缺省 `ops`）、
三张静态 allowlist、`tools/list` 不再内联 `outputSchema`、capabilities 紧凑目录 + 详版、
`query_data` 的 v2 列式布局、任务面 v2、入参归一化。仓库内**不**创建 systemd 单元、
**不**改隧道、**不**改生产主机——本文所有生产步骤都需 owner 逐条批准。

位置一律用**锚点**（原文句子、区域标题、表行名）标注，不用行号：这些文档的部署句在
并行 PR 中会被对齐到当日事实，行号会漂移。

## 0. 顺序约束与批准边界

1. **实现阶段（已完成，合并日）**：入口拆分、v2 wire、任务面 v2、`list_catalog_facets`
   已合并；`EXPECTED_DATA_TOOLS` 已是 24，`EXPECTED_DEPLOYED_DATA_TOOLS` 仍是 23。
   **`capability_revision` 的 bump 与部署事实翻转是同一组事实**，提前 bump 会让
   `check_current_facts.py` 要求把「已部署 v8」写进当前区（与事实相反）。所以 bump
   放在 §5 的部署日提交里；本次最小切换只做这一批改动。
2. **本清单 §3–§5（部署日）**：socket/单元/隧道 + 常量 + 文档当前区 + Skill/evals，
   同一批提交、同一次部署窗口内完成。§5 的提交**包含代码常量**，因此 CI 必须跑 Rust
   测试——它是部署日提交，不是文档 PR。
3. **Skill 与 eval 的改动不进提前的 PR**：它们被生产日更读取，提前改会让当前 Skill 对
   仍是 v7 的生产实例报 `SKILL_CONTRACT_MISMATCH` 并停止日更。只在部署日改。
4. **schema 依赖**：v8 的发布二进制是 `COMPACT_SCHEMA_VERSION = 24`（M1 的 022 +
   M2 PR-1 的 023 + M2 PR-2 的 024），**不能服务 schema 21 库**（`src/sqlite_tasks.rs`）。所以 v8 部署必须与 M1 的归档 + 全新 init
   同一窗口（[M1 部署清单](../tasks/issue-383-m1-deploy-checklist.md)），不能单独热切二进制。
5. **批准清单**：§2 的六项阻塞、§3 的每个单元、§4 的每条隧道命令、§5 的 Skill 改动，
   逐项需 owner 明确批准。本清单只是执行步骤，不是授权。

## 1. 并行期、下线条件与窗口长度

v7 面 = `mcp.sock` 上的编排入口 + v1 wire + [generic-data-v7](generic-data-v7.md) 契约。
并行期从**部署日**起算，终点不按日历自动到达。

| 项 | 值 | 依据 |
|---|---|---|
| 最短长度 | 部署后 **10 个交易日** | 覆盖一次完整月切（月初重算）与至少一次除权事件 |
| 上限 | 无。未满足下线条件就一直并行 | 不以「时间到了」为由下线 |
| 起算 | `x-data-mcp-data` 以 `--entry ops` 重启成功并跑完第一次真实日更 | §3 第 4 步 |

下线条件（**全部满足**才进入 §1.1 的退役动作）：

1. 日更 Skill 在 `--entry ops` 上连续 10 个交易日成功，无 `SKILL_CONTRACT_MISMATCH`
   中断、无因工具不可见导致的编排失败。
2. 研究入口经隧道完成 v8 设计的 8 个评测任务（XDM-18 §3.6），T0/T1/T2/T4/T6 预算达标。
3. 下游 `x` 仓库只读核对通过，且**不需要**改连接入口（D-40；要改需另行批准）。
4. 并行期内没有 P1/P2 事故归因于 v8 的响应形状（v1 分支、`tools/list` 去 `outputSchema`、
   text 分级）。
5. owner 签字确认以上四条有证据（Issue 记录，不是口头）。

### 1.1 退役动作（满足 §1 之后，单独一次提交）

1. [generic-data-v7](generic-data-v7.md) 状态行从「已部署」改为「历史快照」，指向 v8；
   [v7-switch-checklist](v7-switch-checklist.md) 已经是历史快照，不改。
2. 移除 `v1` 读取面与任务面分支：**需要独立的兼容性评估**，不夹在 v8 切换里。评估输入是
   §1 条件 1–3 的实测记录；在此之前 v1 分支必须保留（AC-V8-13 的「v1 仍可读」）。
3. `scripts/check_current_facts.py` 的 `EXPECTED_SKILL_CAPABILITY_REVISION` 与
   `EXPECTED_DEPLOYED_DATA_TOOLS` 在 v8 部署后已等于 `EXPECTED_*`，退役时若删除旧文档需同步
   减少 `CURRENT_DOCS` 计数。
4. `mcp.sock` 继续是编排入口：研究入口是**加法**，v7 面退役不等于删掉 `mcp.sock`。

## 2. 部署前必须完成的六项（阻塞；§2.1／§2.2／§2.6 已于 2026-10-28 裁决，见 D-56）

### 2.1 单写者租约：三个进程不能同时持有同一个库

`DatabaseLease::acquire` 对库文件本身取独占 flock（`try_lock`，`src/local_runtime/paths.rs:75-105`），
`data_service` 启动即取（`src/bin/data_service.rs:92`）。三个单元指向同一个库文件时，
第二个进程拒绝启动。**必须先裁决**：

- (a) 一个进程多 socket（改动最小，但要改 `data_service` 的参数与生命周期）；
- (b) research/maint 用只读后端、不取写租约（要确认只读连接看不到半提交状态）；
- (c) 三进程按需启停（隧道要研究入口常驻，不可接受）。

§3 的单元表按「一个入口一个进程」写（即 (b)）；若裁决为 (a)，§3 合并成一个单元、三个 socket
由同一进程创建，其余步骤不变。

> **已裁决（2026-10-28，D-56）：选 (a) 单进程多 socket。** `data_service` 接受重复的
> `<socket> --entry <kind>` 参数（`--entry` 绑定它前面的 socket），启动检查只做一次，
> 之后绑定 N 个 endpoint；共享同一个 lease／backend／generation／`WorkerGate`，每个
> listener 用自己的入口 allowlist，worker 只在 ops 入口跑一次；任一 socket 绑定失败则整体
> 不启动（fail-closed）。§3 因此是**一个 data 单元、三个 socket**。仍待执行的是 §2.6 的
> 内存实测与 `MemoryMax`。

### 2.2 maint 的写工具与「只有 ops 受理写」冲突

maint 清单含 `backfill_data`/`republish_data`/`retry_task`/`cancel_task`/`verify_data_gap`/
`run_worker_once`（`src/data_supply/registry.rs:70-80`），而 worker 只在 ops 入口跑
（`src/bin/data_service.rs:90`），写租约也只有 ops 持有。**必须先裁决**：

- (a) maint 只做取证（去掉写工具，`retry_task`/`cancel_task` 回 ops）；
- (b) 写请求从 maint 转投 ops（新增一层转发，v8 不做）；
- (c) 单进程多 socket（与 2.1 同一个裁决）。

在裁决前，`x-data-mcp-maint` 不能上线：它启动后会因为没有租约而失败，或（若绕过租约）
违反单写者边界。

> **已裁决（2026-10-28，D-56）：选 (c)，与 §2.1 同裁决。** maint 保留 9 个工具（含写工具与
> `run_worker_once`），写租约只由这**一个进程**持有；worker 只在 ops 入口运行一次，maint 的
> `run_worker_once` 与 ops worker 共用 `WorkerGate`，不能并行执行。maint 是仅本机的
> operator 入口，不暴露隧道。写边界由「socket 名必须叫 mcp」改为「同一数据库只有一个进程
> 持有 `DatabaseLease` 与写协调器」。写边界与对应测试写在
> [generic-data-v8](generic-data-v8.md) §2.1（`tests/service/local_runtime/write_boundary.rs`）。

### 2.3 AI 客户端实测（#387 的 Q3）

原定 Devin 实测；Devin 不可用，owner 2026-10-28 决定改用 pi 1.3.0 在私有栈实测（不碰生产）。
实测分两条客户端路径：pi 默认 MCP exposure `codemode`（XDM-87 生产日更走的路径）与
`exposure:"direct"` + `--tools` 的直接工具路径。

**结论：`ops` 入口保持 `full`，`summary` 不能作为缺省**（D-36 的「实测不通过就退回」分支）。
依据：

- (a) 摘要字段：直接工具路径下 `summary` 只给 ≤300 B 指针；`get_task_progress`（page_size=1000）
  看不到 16 个 `REVISION_UNVERIFIED` 实体与 `failed_by_reason`，`list_data_versions` 看不到
  `version_id`，`get_data_availability`/`query_data` 即使 page_size=1 也拿不到行数据。
  同一组日更需求用 `summary` 要 40 次工具调用（靠 ≤2,048 B 小页回退），行数据仍不可得；
  `full` 为 8 次且全部可见。
- (b) 摘要与 `structuredContent` 同时进上下文：直接工具路径**不会**（有 `content` 时只用文本）；
  codemode 路径**会**——脚本收到完整 `CallToolResult`（`content` + `structuredContent`），
  实测同时打印时两者都进入模型上下文。`summary` 在 codemode 路径不减少上下文。
- (c) 大响应截断：两条路径都**不静默**。直接工具超过 20 KiB 时给
  `Warning: truncated output (original token count: …)` 与完整输出临时文件路径（实测 4.69 MB
  响应截到约 20.6 KiB）；codemode 把 4.69 MB 返回值截到约 40,235 字符（10,000 tokens），
  同样带 warning、`…N tokens truncated…` 与完整输出文件路径，可恢复。

证据等级：上述 (a)(b)(c) 均为 pi 1.3.0 实测（服务端 preview.38、私有 socket、只读调用；
完整报告在仓库外）。「MCP 工具即使服务端没有 `outputSchema` 也走 `structuredContent` 分支」
是读 bundle 源码的推断，与实测一致。未验证：其他客户端、生产 socket、并发/长时间行为。

建议：要缩小上下文应在 Skill/脚本层做有界投影，不要依赖 `text=summary`；重开 `summary`
评估前，摘要至少携带 Skill 依赖字段（`unfinished` 实体与 reason、`version_id` 等），
并在两条路径复测。

### 2.4 下游 `x` 仓库核对（P-02 已撤销，剩两项）

`P-02`（版本 pin 改为 `latest_per_entity`）已于 2026-10-26 撤销：只读核对确认 x 侧不 pin、
不缓存、不持久化 `version_id`（[decisions](../decisions.md) §8、D-40）。部署前仍需核对：

1. **6dp 兼容**（D-32）：x 侧按数值解析 6 位小数、不按 2 位格式化。只读核对已做过一次，
   部署日再确认一次代码未变。
2. **调用面收缩**：`mcp.sock` 的工具从 23 变 15。x 只用 `query_data`（v1，字节不变），
   所以不受影响——但要在部署日核对 x 实际调用的工具名都在 ops 的 15 个之内。若它调用了
   已移到 maint 的工具（`backfill_data`/`retry_task`/`cancel_task` 等），会得到 `unknown_tool`。

两项都要留证据（Issue 记录）。改连 `research.sock` 不在本次范围（D-40）。

### 2.5 schema 23 与 M1 重建

见 §0.4。部署窗口内必须：归档 schema 21 库 → 全新 init 到 24（M1 的 022 + M2 PR-1 的 023 + M2 PR-2 的 024）→ M1 重建 → 才有可读数据。
v8 不新增 migration，但发布二进制读不了旧库。

### 2.6 三个单元的常驻成本

XDM-59 实测（研究侧，单进程）：冷启动 4.449/4.958/4.424 s、热启动 1.161/0.558/1.220 s，
库 1.691 GiB。三进程各自打开同一个库会放大页缓存与冷启动；部署前按实测给每个单元设
`MemoryMax`，并确认总内存余量。未实测不得凭估算设限。

> **已裁决（2026-10-28，D-56）：先测量再定值。** 三进程方案已排除（最坏合计约 2.0 GiB，
> 超过 1.9 GB 主机内存），改为单进程三 socket；私有栈上按 XDM-59 的方法测三件事——
> 空闲 RSS、一次真实 v2 日更的 `MemoryCurrent` 峰值、一次 research 大查询峰值——测完再把
> `MemoryHigh`／`MemoryMax` 写进 unit。在测出之前，本节与设计里的 256M／512M／1.25G 只是
> 测量起点，不是部署值。

## 3. socket 与 systemd 单元（每条需 owner 批准）

**已裁决（2026-10-28，D-56）**：**一个 `x-data-mcp-data` 单元、三个 socket**，由同一个
`data_service` 进程创建（§2.1 案 (a)）。工具面仍按入口拆分，写租约只有这一份，worker 只有
一个。`x-data-mcp-acquire` 不变、仍是独立单元。

| 入口 | `--entry` | socket | 工具数 | 隧道暴露 |
|---|---|---|---|---|
| 研究 | `research` | `run/research.sock`（新增） | 5 | **是（唯一）** |
| 编排 | `ops` | `run/mcp.sock`（沿用） | 15 | 否 |
| 运维/取证 | `maint` | `run/maint.sock`（新增） | 9 | 否 |
| 来源接入 | — | `run/acquire.sock`（不变，独立单元 `x-data-mcp-acquire`） | 10 | 否 |

`mcp.sock` 保持编排入口：日更 Skill、部署脚本与下游读取都连它，换成研究入口会静默让
Skill 失去 plan/run。研究入口是加法，回滚只需把隧道指回 `mcp.sock`。

单元定义（`ExecStartPre` 沿用 D-17 的唯一定义；三个 socket 各一条就绪探针）：

| 单元 | `ExecStart` | `ExecStartPre` |
|---|---|---|
| `x-data-mcp-data` | `data_service <db> run/mcp.sock --entry ops run/research.sock --entry research run/maint.sock --entry maint` + 生产 drop-in 的尾部参数 `--poll-ms 1000` | `wait_service_ready.py run/mcp.sock --timeout 120 --expect-tool plan_market_update`；`wait_service_ready.py run/research.sock --timeout 120 --expect-tool query_data`；`wait_service_ready.py run/maint.sock --timeout 120 --expect-tool compute_metrics` |
| `x-data-mcp-acquire` | 不变 | 不变 |

描述符与生成器必须保留 `ExecStart` 的尾部参数：生产 drop-in `poll-interval.conf` 在 socket
列表之后追加 `--poll-ms 1000`（owner 批准，降空闲 CPU）。`scripts/data_unit_layout.py` 解析
新旧两种单元形状并原样保留尾部；`scripts/deploy_phase_g.sh` 与 `scripts/switch_binary.sh`
的就绪探针按解析出的每个 `${entry}=${socket}` 逐个检查（入口→工具映射由
`scripts/wait_service_ready.py` 维护：`research`→`query_data`、`ops`→`plan_market_update`、
`maint`→`compute_metrics`）。

执行顺序（每步确认后再下一步）：

1. **[批准]** 确认 §2.1／§2.2 已裁决（D-56）、§2.6 的 `MemoryMax` 已实测，且 §2.5 的库已
   重建到 23；记录 catalog/daily/yearly head 计数与 `sync_status` 基线（后面每步都与它比对）。
2. **[批准]** 给 `run/` 目录补三个 socket 的属主/权限（与 data 单元同用户）；旧 socket 文件
   先确认没有进程在用（`ss -xl` 只读查看），不 `rm` 活 socket。
3. **[批准]** 以一个进程同时绑定三个 socket 起 `x-data-mcp-data`；三条
   `wait_service_ready.py` 均通过后逐个核对：`research.sock` 恰好 5 个工具、`mcp.sock` 15 个
   （`plan_market_update`/`run_market_update` 的 `implemented:true`）、`maint.sock` 9 个
   （`compute_metrics`/`run_worker_once` 的 `operator_only:true`），三者
   `capability_revision == generic-data-v8`，`get_data_capabilities({})` ≤1,500 B，且 head
   计数与第 1 步基线一致。
4. **[批准]** 只读核对单写者与 worker：同一库上另起第二个 `data_service` 必须因租约拒绝启动；
   `maint.sock` 的 `run_worker_once` 在 ops worker 忙碌时返回 `worker_busy`（共用一个 gate）。
5. **[批准]** 记录三个 socket 的 `tools/list` 字节数与工具名，写入部署记录。
6. **回滚**：只停/退无效的那个入口不在本次粒度内——三 socket 同一进程，回滚是退掉新 socket
   参数（回到单 `mcp.sock` 旧 unit）或按 §7 退二进制＋库。

启动检查是 fail-closed：allowlist 与注册表不一致、`X_DATA_MCP_TEXT_MODE` 非法值时进程
拒绝启动（`src/data_supply/registry.rs:114-136`，`src/bin/data_service.rs:81-86`），
不会静默降级为更宽或更窄的工具面。

## 4. 隧道（每条需 owner 批准）

隧道配置不在本仓库，下面只写目标状态与顺序。

1. **[批准]** 并行观察：隧道同时连 `run/mcp.sock` 与 `run/research.sock`，只读比对
   `tools/list` 与研究侧 `query_data` 的一次调用，观察至少一次真实日更。
2. **[批准]** 切换：隧道 `ExecStart` 与 drop-in 的 `ExecStartPre` 从 `run/mcp.sock` 改为
   `run/research.sock`；`--expect-tool` 改为 `query_data`。
3. **[批准]** 切换后复核：隧道侧 `tools/list` 恰好 5 个工具；研究侧 `query_data`
   返回 v2 列式信封；`capability_revision` 为 `generic-data-v8`。
4. **回滚**：drop-in 指回 `run/mcp.sock` 并重启隧道。此时客户端看到的是 15 个工具
   （编排入口），能工作但比研究入口宽，属可接受的临时状态。研究入口若暴露了不该给的
   工具，先停隧道再停 `x-data-mcp-research`。

## 5. 部署日翻转的常量与文档（同一批提交）

与 v6/v7 清单同一模式：实现阶段只改代码常量与代码事实，部署日同一提交翻转部署事实。
**§5.1 的第一行是代码改动**，它和 §5.2 的文档翻转必须同提交，否则
`check_current_facts.py` 自相矛盾。

### 5.1 代码与脚本常量

| 位置（锚点） | 旧 | 新 |
|---|---|---|
| `src/data_supply/mcp.rs` `capabilities()` 返回体的 `capability_revision` | `"generic-data-v7"` | `"generic-data-v8"` |
| `scripts/check_current_facts.py` `EXPECTED_DATA_CAPABILITY_REVISION` | `"generic-data-v7"` | `"generic-data-v8"` |
| 同上 `EXPECTED_SKILL_CAPABILITY_REVISION` | `"generic-data-v7"` | `"generic-data-v8"` |
| 同上 `EXPECTED_DEPLOYED_DATA_TOOLS` | `23` | `24` |
| 同上 `REQUIRED_FACTS` 的 `23 data tools` 正则 | 23 | 24（与 §5.2 文档当前区同提交） |
| 同上 `EXPECTED_PACKAGE_VERSION` | 当前 preview 版本 | 部署 tag 对应版本 |
| `scripts/smoke_release.py` data 断言 | `24 tools` + `generic-data-v7`，检查名 `initialize_discovery_24_tools_generic_data_v7` | `generic-data-v8`，检查名同步；按入口断言 5/15 更好，但需脚本支持 `--entry` |
| `scripts/switch_binary.sh` S7 提示行 | `tools 23/10`（陈旧 echo） | `tools 15/10`（`mcp.sock` 是编排入口），顺手改成从响应读计数 |
| `scripts/deploy_preview.sh` `EXPECTED_TOOLS` | 19 个 v3 时代工具名 | 先确认是否仍在用；在用则改为 ops 入口集合 |

### 5.2 文档「当前状态」区域（`CURRENT_DOCS` 覆盖的 5 个文件 + P1-2 区域）

各区域把 `generic-data-v7`→`generic-data-v8`、「23 data tools」→「24 data tools」，
schema 表述改成 23：

- `README.md`（`## 当前状态`）
- `docs/daily-sync.md`（`## 当前状态`）
- `docs/mcp-tools.md`（`## 版本与阅读方式`）
- `docs/generic-data-tools.md`（`## 决策与适用范围`）
- `docs/source-closure.md`（文件头到第一个 `##`）
- `docs/STATUS.md`：「部署」表版本/时间行；「数据库 schema」行之后的代码事实段
  （已部署 23 → 24）；「服务」表 2 行 → 4 行（research / data / maint / acquire）；
  「数据」表工具数
- `docs/contracts/generic-data-v8.md` 状态行：「计划，未部署」→「已部署」，并把
  「当前部署仍是 v7 / 23 / schema 21」改为部署后事实
- `docs/contracts/v8-switch-checklist.md`（本文）状态行：同上
- `docs/contracts/generic-data-v7.md` 状态行：「已部署」→「历史快照」，指向 v8
  （不改则 `contract_snapshot_claims` 会把 v7 当成当前契约）
- `docs/mcp-tools.md` 的工具目录补 `list_catalog_facets`、三个新 dataset 与「v2 详见
  [generic-data-v8](generic-data-v8.md)」的指针（不复制契约）；`docs/generic-data-tools.md`
  的入口表补 research/ops/maint 三行
- `docs/design.md`「当前能力与目标差距」表的「MCP」行（`data socket 23 data tools` → 24、
  三入口）与「服务」行（双 systemd 单元 → 四单元）。这两行不在 `CURRENT_DOCS` 里，
  漏改不会被脚本抓住，但会与 STATUS 自相矛盾

### 5.3 Skill 与 eval（部署日，独立提交）

- `.devin/skills/x-data-daily-sync/SKILL.md`：`capability_revision` 断言 v7→v8；
  allowlist 收缩到 ops 的 15 个工具（`mcp.sock` 现在只有这些）；无过滤
  `get_data_capabilities({})` 只用于 revision 握手，`schema_digest`/`active_generation_ref`/
  `runtime_readiness`/`blocking_reasons` 改从详版（`dataset` + `operation`）取。
- `.devin/skills/x-data-status/SKILL.md`：revision 断言 v7→v8。
- `.devin/evals/daily-sync-config.json`：allowlist == Skill allowlist（15 个）。
- `.devin/evals/daily-ai-sync-v2.txt`：prompt 若改，`check_daily_sync_eval.py` 的
  `EXPECTED_PROMPT_SHA256` 同提交更新；不改则不动。
- `.devin/evals/v8-baseline.json` 与 `fixtures/v8/*.json` 已是实现阶段产物，部署日不改；
  只有实测结果进部署记录。

## 6. 部署日验证（命令原样贴进部署记录）

```bash
python3 scripts/check_current_facts.py
python3 scripts/check_deploy_facts.py
python3 scripts/check_daily_sync_eval.py
python3 scripts/check_public_hygiene.py
git diff --check
cargo fmt --check
```

加上 §3 每步的只读核对（`tools/list` 工具名与字节数、`capability_revision`、head 计数）。
Rust 测试由 CI 证明；部署主机上不编译。

## 7. 回滚（按粒度）

| 粒度 | 触发 | 动作 | 数据影响 |
|---|---|---|---|
| 隧道 | 研究入口异常、客户端不兼容 | drop-in 指回 `mcp.sock`，重启隧道 | 无 |
| 单元 | 研究/maint socket 起不来或吃内存 | `systemctl stop` 整个 `x-data-mcp-data` 退回单 socket 旧 unit；三个 socket 同进程，不能单独停一个 | 无 |
| 入口 | ops 入口 `text=summary` 不可用 | `X_DATA_MCP_TEXT_MODE=full` 重启 `x-data-mcp-data` | 无 |
| 二进制 + 库 | v8 形状有 P1 事故 | 上一版 release + 归档的 schema 21 库一起退 | 库回滚到归档点；期间的采集需重放 |

只切回二进制会因 schema 不匹配拒绝启动（§0.4），**不能**只退二进制。

## 8. 未决事项（需 owner 裁决后才能执行 §3）

| # | 事项 | 影响 | 现状 |
|---|---|---|---|
| B-1 | §2.1 租约模型：单进程多 socket / 只读租约 / 分时启停 | 决定 §3 能否上线 | 已裁决（2026-10-28，D-56）：单进程多 socket |
| B-2 | §2.2 maint 写工具归属 | `x-data-mcp-maint` 是否可上线 | 已裁决（2026-10-28，D-56）：与 2.1 同裁决，maint 保留 9 工具、写租约只由该进程持有 |
| B-3 | §2.3 AI 客户端实测（Q3） | `text=summary` 能否作为缺省 | 已完成（2026-10-28，pi 实测）：**不能**；`ops` 保持 `full` |
| B-4 | §2.4 `x` 仓库两项核对 | 下游兼容 | 只读核对已做一次，部署日复测 |
| B-5 | `market.daily` 是否补 `adjustment` 选项（[generic-data-v8](generic-data-v8.md) §12.2） | 研究入口只能读前复权 | 未裁决 |
| B-6 | `metadata.adjustment` 是否补齐 `algorithm_version`/`price_scale`（§12.3） | 下游无法从响应判断精度与算法 | 未裁决 |
| B-7 | `deploy_preview.sh` 的 `EXPECTED_TOOLS` 是否仍在用 | 部署脚本自断言 | 未核实 |

## 9. 最小切换路线（2026-10-27）

owner 2026-10-27 裁决：v8 先走**最小切换**，只保留现有 `x-data-mcp-data` 单元，以
`--entry ops`（缺省）运行 `mcp.sock` 上的 15 个工具，文本模式设
`X_DATA_MCP_TEXT_MODE=full`。本清单的完整三入口方案留作第二阶段。

### 9.1 本次做

1. 版本 bump 到 `v0.2.0-preview.36`，`capability_revision` 翻为 `generic-data-v8`。
2. `scripts/check_current_facts.py` 的部署常量：`EXPECTED_DEPLOYED_DATA_TOOLS = 15`
   （`mcp.sock` 的 `ops` 入口）、`EXPECTED_DATA_CAPABILITY_REVISION` /
   `EXPECTED_SKILL_CAPABILITY_REVISION` = `generic-data-v8`；`REQUIRED_FACTS` 的
   「N data tools」正则随之改为 15。
3. 当前状态区写部署后事实：preview.36、schema 24、`generic-data-v8`、15 data tools、
   文本模式 `full`；生产只有 `ops` 入口，research/maint 未部署；generation 与数据量
   写「部署后重建」，实测值由部署后的单独小 PR 补入。
4. 日更 Skill / status Skill 的 revision 断言改 v8；Skill allowlist 核对为 `ops` 的
   15 个工具（原 allowlist 已恰好是这 15 个，无需收缩）；eval 配置与 Skill 同提交。
5. 新增 D-53（v8 最小切换）与 D-54（SLO 第 1 行临时豁免）。

### 9.2 推迟到第二阶段

- `x-data-mcp-research` / `x-data-mcp-maint` 单元与 `research.sock` / `maint.sock`。
- 隧道改指 `research.sock`。
- ~~Devin 实测（§2.3）~~：已改为 AI 客户端（pi）实测（2026-10-28），见 §2.3。
- §2.1 单写者租约、§2.2 maint 写工具归属、§2.6 三单元常驻成本。

> **已裁决（2026-10-28，D-56）**：§2.1／§2.2／§2.6 三项已定——§2.1 选单进程多 socket；
> §2.2 与它同裁决（maint 九个工具保留、写租约只由这一个进程持有、worker 只在 ops）；
> §2.6 先测量再定 `MemoryMax`。因此不再起 research/maint 两个单元，改为 §3 的「一个 data 单元、三个
> socket」；第二阶段从 PR-1 开始按 §9.6 执行。隧道改指 `research.sock` 仍需另行批准，不在 PR-1 内；实测已改用 pi（见 §2.3）。

### 9.3 第二阶段的前提条件

1. **已完成（2026-10-28，D-56）**：§2.1 与 §2.2 已由 owner 裁决——单进程多 socket，
   maint 与 2.1 同裁决（9 工具保留，写租约只由这一个进程持有，worker 只在 ops 入口）。
2. §2.6 的三单元常驻内存实测完成并给出 `MemoryMax`（已裁决为「先测量再定值」）。
3. **已完成（2026-10-28）**：AI 客户端（pi）实测确认 `text=summary` 不可作缺省，`ops` 保持 `full`，见 §2.3。
4. 最小切换在 `ops` 入口上连续运行，日更 Skill 无 `SKILL_CONTRACT_MISMATCH`。

### 9.5 下游 `x`（XDM-66 核对结论）

XDM-66 的 4 项核对都不阻塞，`x` 只调用 `query_data`；**连接入口不变**，仍然连
`run/mcp.sock`，该 socket 在本次部署里是 `ops` 入口（D-40）。部署后用真实实例跑一次
`query_data` 断言，内容取 #387 上的核对评论：

- `version_manifest` 长度为 1；
- 序列 `status=present`；
- `metadata.adjustment={confirmed, qfq}`；
- `close` 是 6 位小数字符串。

以上任一项不符即按 §7 的入口粒度回滚，不静默适配。

### 9.4 与 §5.1 的冲突项（本次按最小切换执行）

| 项 | §5.1 原目标 | 本次实际 | 原因 |
|---|---|---|---|
| `EXPECTED_DEPLOYED_DATA_TOOLS` / `REQUIRED_FACTS` | 24（三入口总注册数） | **15**（`mcp.sock` 的 `ops` 入口） | 生产只有 `ops` 入口；24 是代码注册面，不是部署面 |
| `scripts/smoke_release.py` | 按入口断言 5/15 | dev stdio 仍断言 24；`data_service` 运行时断言 15 + v8 | 脚本未加 `--entry`，运行时入口就是 `ops` |
| `scripts/deploy_preview.sh` `EXPECTED_TOOLS` | 在用则改为 ops 集合 | **未改** | README 已标注该脚本是 preview.11 的单 data-service 历史证据，本次部署不用它 |
| §3 三入口单元表 | research/ops/maint 四单元 | 只 `x-data-mcp-data`（ops）+ acquire | 第二阶段（D-56 已裁定为单进程三 socket） |

### 9.6 第二阶段启动（2026-10-28）

XDM-73 备忘（仓库外，2026-10-28）§6 的实施顺序已获批准，从本小节起生效：

| 步骤 | 内容 | 状态 |
|---|---|---|
| 0 | 裁决记录：本清单 §2／§3／§9 与 D-56 | 已落地（D-56） |
| 1 | PR-1：`data_service` 重复 `<socket> --entry <kind>`、一次启动检查、N 个 endpoint、共享 lease／backend／generation／gate、每个 listener 独立 allowlist、worker 只在 ops 跑一次、任一 bind 失败整体不启动 | 已合并（#434） |
| 2 | PR-2：写边界策略（maint 9 工具、worker 只在 ops、共用 gate）的文档与测试 | 已合并（#437，见 [generic-data-v8](generic-data-v8.md) §2.1） |
| 3 | PR-3：部署／探针／文档（`deploy_phase_g.sh`、`switch_binary.sh`、`wait_service_ready.py` 多 socket） | 已合并（#438） |
| 4 | Ops-A：私有栈单进程三 socket 的内存实测（§2.6） | 已完成（XDM-84，2026-10-28；报告在仓库外） |
| 5 | Ops-B：生产同进程三 socket，隧道不动 | 已执行（2026-10-28，随 preview.38，见 §9.7） |
| 6 | Ops-C：AI 客户端实测（§2.3；Devin 不可用，改用 pi） | 已完成（2026-10-28；结论：`ops` 保持 `full`） |
| 7 | Ops-D：隧道改指 `research.sock`（另行批准） | 未开始 |
| 8 | Ops-E：maint 启用（仅本机 operator） | 未开始 |

PR-1 的旧单 socket 调用方式（`data_service <db> <socket> [--entry k]`）保持可用，回退只需用旧
unit 与旧二进制；隧道的 runbook（§4）与生产 unit 仍按 §3 的「每条需 owner 批准」执行。

### 9.7 Ops-B 实际执行记录（2026-10-28，preview.38）

owner 2026-10-28 批准：发布 preview.38（main 已含 #434 单进程多 socket、#437 写边界、
#438 部署/探针、#439 archive 索引复用、#435 acquire 流式），**schema 仍是 24**，用
`scripts/switch_binary.sh` 只换二进制（不重建库），之后给 data 单元加 drop-in 改成三 socket，
并加内存上限。**隧道不动**（仍指 `mcp.sock`）。

实际步骤（每一步的证据写进切换记录）：

1. `scripts/switch_binary.sh` 先 dry-run（确认目标 `ExecStart`、报告路径可写），再 `--execute`
   切到 preview.38，data/acquire 重启。**先换二进制再改单元**：三 socket `ExecStart` 要让
   preview.38 的 `data_service` 才认识（2026-10-28T10:04Z 的事故即由此而来，见下）。
2. `systemctl daemon-reload` 后装两个 data 单元 drop-in：
   - `exec-start.conf`：`ExecStart` 改为一个进程绑三个 socket（顺序 ops → research → maint），
     尾部保留 `--poll-ms 1000`：
     `data_service <db> run/mcp.sock --entry ops run/research.sock --entry research run/maint.sock --entry maint --poll-ms 1000`；
   - `memory.conf`：`MemoryHigh=384M`、`MemoryMax=640M`、`MemorySwapMax=512M`（依据 XDM-84
     的 w=1 独占测量；这组上限不覆盖 w=2）。
   `poll-interval.conf`、`X_DATA_MCP_GENERATION_ROOT`、`X_DATA_MCP_RUN_PREPARE_WORKERS=1`、
   `X_DATA_MCP_TEXT_MODE=full` 保持不动。
3. 重启 `x-data-mcp-data`，逐 socket 就绪探针：
   `scripts/wait_service_ready.py run/research.sock --entry research --expect-tool query_data`、
   `run/mcp.sock --entry ops --expect-tool plan_market_update`、
   `run/maint.sock --entry maint --expect-tool compute_metrics`。
4. 工具面断言：`research` 5、`ops` 15、`maint` 9 个工具，三者 `capability_revision` 均为
   `generic-data-v8`；`maint` 的 `run_worker_once` 在 ops worker 忙时返回 `worker_busy`
   （同一进程内共用一个 gate）。
5. x 契约断言（§9.5 四项，隧道仍连 `mcp.sock`）。
6. 记录 `systemctl show -p MemoryHigh -p MemoryMax -p MemorySwapMax` 与 `MemoryCurrent` 观测值。

回滚（按粒度，详见 §7）：

- 单元形状：删 `exec-start.conf` 与 `memory.conf` → `daemon-reload` → 重启 data，恢复单
  `mcp.sock`（`ops`，由 `poll-interval.conf` 提供 `--poll-ms 1000`）的旧 `ExecStart`；库不变。
- 二进制：`switch_binary.sh` 切回 preview.37——同一个 schema 24 库，不需要换库。
- 先退单元形状再退二进制：旧单 socket `ExecStart` 在 preview.38 上仍然有效，顺序不会把
  服务卡在「新单元 + 旧二进制」的组合上。

事故记录（2026-10-28T10:04–10:09Z）：在**已装三 socket drop-in、但二进制仍是 preview.37**
的单元上重启，data 服务约 5 分钟反复启动失败（preview.37 的 `data_service` 只认
`<db> <sock> [--entry k]` 的单 socket 形状，多 socket 参数被拒）；已删 drop-in 回滚到单 socket，
服务恢复。**教训：改单元参数前先确认已部署的二进制支持它**——§9.7 把「换二进制」放在
「改单元」之前的顺序，就是这条事故的直接结论。
