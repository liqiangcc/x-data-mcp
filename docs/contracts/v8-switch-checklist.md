# generic-data-v8 部署日切换清单（入口拆分部分）

**状态**：计划，未执行。本文只记录部署日要做的机械切换；v8 的契约文档与
其余常量由 PR-6 收口，本文先固定 PR-2 已落地的入口、socket、单元与隧道口径。

实现已合并、部署未批准的部分：`data_service --entry research|ops|maint`
（默认 `ops`）、三张静态 allowlist、`tools/list` 不再内联 `outputSchema`、
capabilities 紧凑目录 + 详版。仓库内**不**创建 systemd 单元，也**不**改隧道；
下列步骤全部在生产主机上另行批准执行。

## 1. 入口与 socket

| 入口 | `--entry` | socket | systemd 单元 | 工具数 | 隧道暴露 |
|---|---|---|---|---|---|
| 研究 | `research` | `run/research.sock`（新增） | `x-data-mcp-research`（新增） | 5 | **是（唯一）** |
| 编排 | `ops` | `run/mcp.sock`（沿用） | `x-data-mcp-data`（沿用） | 15 | 否 |
| 运维/取证 | `maint` | `run/maint.sock`（新增） | `x-data-mcp-maint`（新增） | 9 | 否 |
| 来源接入 | — | `run/acquire.sock`（不变） | `x-data-mcp-acquire`（不变） | 10 | 否 |

`mcp.sock` 保持编排入口：日更 Skill、部署脚本与下游读取都连它，换成研究入口会
静默让 Skill 失去 plan/run。研究入口是**加法**（新增 socket），回滚只需把隧道指回
`mcp.sock`。

### 1.1 部署前必须解决的阻塞（本 PR 未改）

1. **`DatabaseLease` 是独占 flock**：同一数据库文件同时只能有一个 `data_service`
   进程（`src/local_runtime/paths.rs` 的 `try_lock`）。三入口同时启动前，研究入口需要
   只读租约/只读后端，或改成同一进程多 socket；这属于存储与租约模型改动，不在
   PR-2 的范围。
2. **maint 的写工具与「只有 ops 受理写请求」冲突**：设计 §3.2 的 maint 清单包含
   `backfill_data`/`republish_data`/`retry_task`/`cancel_task`/`verify_data_gap`/
   `run_worker_once`，而 §3.4 写「只有 ops 入口能受理写请求」。部署前必须裁决
   maint 与 ops 的写租约归属（当前实现仍只允许一个写进程）。

PR-2 只落入口过滤、`data_service --entry`、`tools/list` 去 `outputSchema`、
capabilities 紧凑/详版与 `list_catalog_facets`；不改变租约、存储模型或单写者边界。

## 2. systemd 单元（部署日执行）

三个 data 单元共用同一个二进制与同一个数据库文件；单写者边界不变（只有
`ops` 入口受理写请求，worker 只跑在 `ops`）。

| 单元 | `ExecStart` | `ExecStartPre`（D-17 唯一定义） |
|---|---|---|
| `x-data-mcp-research` | `data_service <db> run/research.sock --entry research` | `wait_service_ready.py run/research.sock --timeout 120 --expect-tool query_data` |
| `x-data-mcp-data` | `data_service <db> run/mcp.sock --entry ops` | `wait_service_ready.py run/mcp.sock --timeout 120 --expect-tool plan_market_update` |
| `x-data-mcp-maint` | `data_service <db> run/maint.sock --entry maint` | `wait_service_ready.py run/maint.sock --timeout 120 --expect-tool cancel_task` |

启动检查：`--entry` 与 registry 归属表不一致时进程拒绝启动（fail-closed），
不会静默降级为全量工具。

## 3. 隧道（部署日执行，需 owner 批准）

1. 并行观察：隧道同时连 `run/mcp.sock` 与 `run/research.sock`，只读比对
   `tools/list` 与研究任务，观察至少一次真实日更。
2. 切换：`ExecStart` 与 drop-in 的 `ExecStartPre` 从 `run/mcp.sock` 改为
   `run/research.sock`；`ExecStartPre` 的 `--expect-tool` 改为 `query_data`。
3. 回滚：drop-in 指回 `run/mcp.sock` 并重启隧道（此时它是编排入口，客户端会看到
   15 个工具；能工作但比研究入口更宽，属可接受的临时状态）。研究入口若暴露了
   不该给的工具，先停隧道再停 `x-data-mcp-research`。

## 4. 部署日翻转的常量与文档

与 v7 清单同一模式：实现阶段只改代码常量，部署日同一提交翻转部署事实。

| 位置（锚点） | 旧 | 新 | 何时 |
|---|---|---|---|
| `scripts/check_current_facts.py` `EXPECTED_DATA_TOOLS` | 24（v8 PR-2 已改） | 24 | 已随实现合并 |
| 同上 `EXPECTED_DEPLOYED_DATA_TOOLS` | 23 | 24 | 部署日 |
| 同上 `REQUIRED_FACTS` 的 `23 data tools` 正则 | 23 | 24 | 部署日（与文档当前区同提交） |
| 同上 `EXPECTED_DATA_CAPABILITY_REVISION` / `EXPECTED_SKILL_CAPABILITY_REVISION` | `generic-data-v7` | `generic-data-v8` | 契约 bump 与部署日（PR-6 收口） |
| `docs/STATUS.md`「数据库 schema」行之后的代码事实段 | 已部署 23 / 代码 24 | 已部署 24 | 部署日 |
| `docs/STATUS.md`「服务」表 | 2 行 | 4 行（research / data / maint / acquire） | 部署日 |
| `scripts/smoke_release.py` 的 data 断言 | 23 tools | 按入口（研究 5 / 编排 15） | 随实现合并 |
| `scripts/deploy_preview.sh` 的 `EXPECTED_TOOLS` | 19 个 v3 时代工具名 | 先确认是否仍在用；在用则改为入口集合 | 部署前确认 |
| `scripts/switch_binary.sh` S7 提示行 | `tools 23/10` | `tools 15/10`（`mcp.sock` 是编排入口） | 部署日 |
| 两个 Skill 的 revision 断言 | `generic-data-v7` | `generic-data-v8` | 部署日 |
| `.devin/evals/` 的 allowlist 与 prompt sha256 | v7 | v8 | 部署日 |
| 日更 Skill §0 的事实记录 | 无过滤 capabilities 取 `schema_digest`/`active_generation_ref`/`runtime_readiness`/`blocking_reasons` | 改从详版（`dataset` + `operation`）取；无过滤只用于 revision 握手 | 部署日 |

## 5. 回滚

- 未部署：回退 PR 即恢复单入口。
- 部署后入口有问题：隧道 drop-in 指回 `mcp.sock`，或停 `research`/`maint` 单元。
- 响应形状未变（v8 PR-2 仍是 v1 行式），回滚不需要还原数据库。
