# generic-data-v8 部署日切换清单

**状态**：已执行（2026-10-27，最小切换）。当前部署为 `generic-data-v8` /
15 data tools（`mcp.sock` 的 `ops` 入口）/ compact schema 24（见 [STATUS](../STATUS.md)）；
research/maint 单元、隧道切换与 §2.1/§2.2/§2.6 推迟到第二阶段，见 §9。
契约本身见 [generic-data-v8](generic-data-v8.md)。

实现已合并、部署未批准的部分：`data_service --entry research|ops|maint`（缺省 `ops`）、
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
4. **schema 依赖**：v8 的发布二进制是 `COMPACT_SCHEMA_VERSION = 24`，**不能服务 schema 21
   库**（`src/sqlite_tasks.rs`）。所以 v8 部署必须与 M1 的归档 + 全新 init
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

## 2. 部署前必须完成的六项（阻塞，逐项需 owner 裁决）

### 2.1 单写者租约：三个进程不能同时持有同一个库

`DatabaseLease::acquire` 对库文件本身取独占 flock（`try_lock`，`src/local_runtime/paths.rs:75-105`），
`data_service` 启动即取（`src/bin/data_service.rs:92`）。三个单元指向同一个库文件时，
第二个进程拒绝启动。**必须先裁决**：

- (a) 一个进程多 socket（改动最小，但要改 `data_service` 的参数与生命周期）；
- (b) research/maint 用只读后端、不取写租约（要确认只读连接看不到半提交状态）；
- (c) 三进程按需启停（隧道要研究入口常驻，不可接受）。

§3 的单元表按「一个入口一个进程」写（即 (b)）；若裁决为 (a)，§3 合并成一个单元、三个 socket
由同一进程创建，其余步骤不变。

### 2.2 maint 的写工具与「只有 ops 受理写」冲突

maint 清单含 `backfill_data`/`republish_data`/`retry_task`/`cancel_task`/`verify_data_gap`/
`run_worker_once`（`src/data_supply/registry.rs:70-80`），而 worker 只在 ops 入口跑
（`src/bin/data_service.rs:90`），写租约也只有 ops 持有。**必须先裁决**：

- (a) maint 只做取证（去掉写工具，`retry_task`/`cancel_task` 回 ops）；
- (b) 写请求从 maint 转投 ops（新增一层转发，v8 不做）；
- (c) 单进程多 socket（与 2.1 同一个裁决）。

在裁决前，`x-data-mcp-maint` 不能上线：它启动后会因为没有租约而失败，或（若绕过租约）
违反单写者边界。

### 2.3 Devin 实测（#387 的 Q3）

编排入口 `text=summary` 偏离 MCP 规范的 SHOULD。部署前必须用 Devin 跑一次真实日更，
确认：(a) 摘要字段够 Skill 判断；(b) 客户端不会把 `structuredContent` 与摘要同时塞进
上下文；(c) 大响应不会被客户端静默截断。**未实测不得部署**——若结论是摘要不够用，
把 `ops` 的缺省改回 `full` 是部署日可做的开关（`X_DATA_MCP_TEXT_MODE=full`），
但要在部署记录里写明。

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

见 §0.4。部署窗口内必须：归档 schema 21 库 → 全新 init 到 24 → M1 重建 → 才有可读数据。
v8 不新增 migration，但发布二进制读不了旧库。

### 2.6 三个单元的常驻成本

XDM-59 实测（研究侧，单进程）：冷启动 4.449/4.958/4.424 s、热启动 1.161/0.558/1.220 s，
库 1.691 GiB。三进程各自打开同一个库会放大页缓存与冷启动；部署前按实测给每个单元设
`MemoryMax`，并确认总内存余量。未实测不得凭估算设限。

## 3. socket 与 systemd 单元（每条需 owner 批准）

| 入口 | `--entry` | socket | 单元 | 工具数 | 隧道暴露 |
|---|---|---|---|---|---|
| 研究 | `research` | `run/research.sock`（新增） | `x-data-mcp-research`（新增） | 5 | **是（唯一）** |
| 编排 | `ops` | `run/mcp.sock`（沿用） | `x-data-mcp-data`（沿用） | 15 | 否 |
| 运维/取证 | `maint` | `run/maint.sock`（新增） | `x-data-mcp-maint`（新增） | 9 | 否 |
| 来源接入 | — | `run/acquire.sock`（不变） | `x-data-mcp-acquire`（不变） | 10 | 否 |

`mcp.sock` 保持编排入口：日更 Skill、部署脚本与下游读取都连它，换成研究入口会静默让
Skill 失去 plan/run。研究入口是加法，回滚只需把隧道指回 `mcp.sock`。

单元定义（`ExecStartPre` 沿用 D-17 的唯一定义）：

| 单元 | `ExecStart` | `ExecStartPre` |
|---|---|---|
| `x-data-mcp-research` | `data_service <db> run/research.sock --entry research` | `wait_service_ready.py run/research.sock --timeout 120 --expect-tool query_data` |
| `x-data-mcp-data` | `data_service <db> run/mcp.sock --entry ops` | `wait_service_ready.py run/mcp.sock --timeout 120 --expect-tool plan_market_update` |
| `x-data-mcp-maint` | `data_service <db> run/maint.sock --entry maint` | `wait_service_ready.py run/maint.sock --timeout 120 --expect-tool cancel_task` |

执行顺序（每步确认后再下一步）：

1. **[批准]** 确认 §2.1/§2.2 已裁决，且 §2.5 的库已重建到 23；记录 catalog/daily/yearly head
   计数与 `sync_status` 基线（后面每步都与它比对）。
2. **[批准]** 给 `run/` 目录补 socket 的属主/权限（与 data 单元同用户）；旧 socket 文件先
   确认没有进程在用（`ss -xl` 只读查看），不 `rm` 活 socket。
3. **[批准]** 起 `x-data-mcp-research`；`wait_service_ready.py` 通过后只读核对
   `tools/list` 恰好 5 个工具名、`capability_revision == generic-data-v8`、
   `get_data_capabilities({})` ≤1,500 B。
4. **[批准]** 改 `x-data-mcp-data` 的 `ExecStart` 加 `--entry ops`（旧单元不带 `--entry`
   时缺省也是 `ops`，所以这一步是显式化，不是行为变化）；重启后核对 15 个工具、
   `plan_market_update`/`run_market_update` 的 `implemented:true`、head 计数与第 1 步的基线一致。
5. **[批准]** 起 `x-data-mcp-maint`（前提是 §2.2 裁决为「maint 可独立持租约」）；核对 9 个
   工具与 `compute_metrics`/`run_worker_once` 的 `operator_only:true`。
6. **[批准]** 记录三个 socket 的 `tools/list` 字节数与工具名，写入部署记录。

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
| 单元 | 研究/maint 单元起不来或吃内存 | `systemctl stop` 该单元；`x-data-mcp-data` 不动 | 无 |
| 入口 | ops 入口 `text=summary` 不可用 | `X_DATA_MCP_TEXT_MODE=full` 重启 `x-data-mcp-data` | 无 |
| 二进制 + 库 | v8 形状有 P1 事故 | 上一版 release + 归档的 schema 21 库一起退 | 库回滚到归档点；期间的采集需重放 |

只切回二进制会因 schema 不匹配拒绝启动（§0.4），**不能**只退二进制。

## 8. 未决事项（需 owner 裁决后才能执行 §3）

| # | 事项 | 影响 | 现状 |
|---|---|---|---|
| B-1 | §2.1 租约模型：单进程多 socket / 只读租约 / 分时启停 | 决定 §3 能否上线 | 未裁决 |
| B-2 | §2.2 maint 写工具归属 | `x-data-mcp-maint` 是否可上线 | 未裁决 |
| B-3 | §2.3 Devin 实测（Q3） | `text=summary` 能否作为缺省 | 未实测 |
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
- Devin 实测（§2.3）。
- §2.1 单写者租约、§2.2 maint 写工具归属、§2.6 三单元常驻成本。

### 9.3 第二阶段的前提条件

1. §2.1 与 §2.2 已由 owner 裁决（单进程多 socket、只读租约或分时启停三者之一）。
2. §2.6 的三单元常驻内存实测完成并给出 `MemoryMax`。
3. Devin 实测确认 `text=summary` 是否可作缺省；在此之前 `ops` 保持 `full`。
4. 最小切换在 `ops` 入口上连续运行，日更 Skill 无 `SKILL_CONTRACT_MISMATCH`。

### 9.4 与 §5.1 的冲突项（本次按最小切换执行）

| 项 | §5.1 原目标 | 本次实际 | 原因 |
|---|---|---|---|
| `EXPECTED_DEPLOYED_DATA_TOOLS` / `REQUIRED_FACTS` | 24（三入口总注册数） | **15**（`mcp.sock` 的 `ops` 入口） | 生产只有 `ops` 入口；24 是代码注册面，不是部署面 |
| `scripts/smoke_release.py` | 按入口断言 5/15 | dev stdio 仍断言 24；`data_service` 运行时断言 15 + v8 | 脚本未加 `--entry`，运行时入口就是 `ops` |
| `scripts/deploy_preview.sh` `EXPECTED_TOOLS` | 在用则改为 ops 集合 | **未改** | README 已标注该脚本是 preview.11 的单 data-service 历史证据，本次部署不用它 |
| §3 三入口单元表 | research/ops/maint 四单元 | 只 `x-data-mcp-data`（ops）+ acquire | 第二阶段 |
