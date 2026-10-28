# preview.37 部署清单（M2 上线）

部署日提交的清单，形状沿用 [M1 部署清单](issue-383-m1-deploy-checklist.md) §8。部署事实以
[STATUS](../STATUS.md) 为准；本文件只写**窗口内要做的事**与**验证项**，不写未测量的数字。

## 0. 边界

- 版本：`v0.2.0-preview.37`；`capability_revision` **不变**（`generic-data-v8`）、工具面不变
  （`ops` 入口 15 个 data 工具）、schema 不变（24）。依据：M2 设计 §12.5 判定 M2 不加工具、
  不升 revision，新 dataset 随 v8 进；本部署只是把 v8 已声明的 6 个 M2 数据集真正上线
  （见 [generic-data-v8](../contracts/generic-data-v8.md) 状态行）。
- 部署期间**冻结 main**：窗口内只接受本次部署日提交与必要的回填。
- 部署由 operator 执行；开发 agent 不动 `/opt`、不打 tag、不发布。

## 1. 部署方式

`scripts/deploy_phase_g.sh --force-git-transport`（本机 gh 读不到 release asset digest 时走
`refs/dist` 的 git transport，与历次一致）：

1. 停 `x-data-mcp-data` / `x-data-mcp-acquire`。
2. 归档 `state/preview.sqlite`（schema 24）到窗口外，文件名带时间戳
   `state/preview.sqlite.v24-archived-<UTC>`，**保留不迁移**。
3. 从 release 提交读取期望的 fresh-init schema（#423 起部署脚本自己读，不再写死 21），
   `task_sqlite init` 建全新库——**不做原地升级**。
4. **data 单元的 drop-in 加 `X_DATA_MCP_RUN_PREPARE_WORKERS=1`**（与
   `text-mode.conf` 并列，新的 `prepare-workers.conf`；`daemon-reload` 后重启单元）。
   #429 起 v2 日更的准备线程数由它控制：`1` 走原来的串行 `run_items` 路径，不设时按
   CPU 取 `clamp(2,4)`，生产 2 vCPU 主机默认 **2**，其保守最坏内存估算约 **1.0 GiB**
   （`2*workers + 1` 个 ≤100-item chunk × 约 200 MiB；`src/market_update/runner.rs`
   的 `prepare_workers_from_env` 注释）。本机 1.9 GiB RAM，部署窗口先按 1 跑。
   放开条件：**独占窗口**（[SLO](../SLO.md) §3）实测内存与耗时，owner 确认后再改回不设
   或 2；非整数/超范围的值会让 `data_service` 拒绝启动，不是静默降级。
5. 起两个 systemd 单元，检查 socket、`tools/list`（15 / 10）、`pragma user_version=24`、
   单元环境里有 `X_DATA_MCP_RUN_PREPARE_WORKERS=1`。
6. 写部署报告到 `reports/preview37-phaseg-deployment.json`。

运维事实（2026-10-28 owner 批准）：data 单元另加 drop-in `poll-interval.conf`，`ExecStart` 追加 `--poll-ms 1000`——原默认 100 ms 的空闲轮询约占 8% CPU，改后约 1.4%；新任务接手延迟 ≤1 s。删除该文件并 `daemon-reload` 后重启即回退默认值。

## 2. 重建步骤与预计耗时

预计值与 preview.36 同量级（采集 11–16 min、plan/run 约 10 min、年线约 8 min，不可读窗口约
40–50 min）；preview.36 实测 844s / 41s / 302s。**新增的 4 个 M2 内部 item 的耗时未测量**
（preview.36 的 41s 只含 9 个指数 + 日历），按「部署后回填」处理，不作门禁。

本次窗口的 plan/run 以 `X_DATA_MCP_RUN_PREPARE_WORKERS=1` 运行（串行准备）：**耗时预期高于
默认 2 线程**，内存预期低于 1 GiB 上限；这两项都只记录实测，不当门禁——2 线程的收益与内存
代价要等独占窗口实测后由 owner 决定（见 §1 第 4 步）。

1. 全量采集：目录 + 日线（不复权/前复权）+ 年线首建。
2. v2 `plan_market_update` / `run_market_update`：实体 item 之后追加 **14** 个内部 item
   （9 指数 + 日历 + `security_master`/`risk_warnings`/`suspensions`/`trading_rules`），
   M2 数据集在这一步产生版本。
3. 年线重聚合。
4. 只读读回 + §3 的验证项，写部署记录。

## 3. 验证项

M1 回归（与 preview.36 相同）：

- `get_data_capabilities`：`capability_revision=generic-data-v8`，15 个 data 工具。
- `securities.catalog` / `market.daily` / `market.yearly` / `market.index` / 交易日历可读，
  实体数与 STATUS「数据」段回填值一致。
- `export_research` 能跑：`manifest.json` 的 `schema` 是 `x-data-mcp.research-snapshot.v2`，
  `files` 12 项（M1 的 5 + M2 的 7）。

M2 读回（`query_data`，`contract_version:"v2"`，`filter.kind:"daily"`）：

- `market.security_master`：有登记版本、行数 > 0；`selection.kind=universe` 的
  `as_of` 返回当日主表成员。
- `market.capital_events`：有登记版本、行数 > 0（按实体读与 whole-market 读各一次）。
- `market.suspensions`：`confirmed` 行数为 0，`unresolved_gap` 行数 > 0（写入非空才登记版本；
  空写入不登记，读取保持 `absent`）。
- `market.risk_warnings` / `market.price_limit_rules` / `market.security_names`：
  `manifest.status=absent` 或 0 行，**且** run 的内部 item evidence 里有对应 `pending_sources`。
  这不等于失败，不要把 0 行当成缺陷。
- 研究快照导出里 7 个 M2 文件列名/类型与 `files[].dataset`/`temporal` 声明一致；
  `daily_raw` 不含涨跌停/可交易性派生列（AC-M2-11）。

窗口内额外记录（不发门禁结论）：

- plan/run 期间的峰值 RSS（`systemctl show -p MemoryPeak x-data-mcp-data` 或
  `/proc/<pid>/status` 的 `VmHWM`）与墙钟耗时，以及当时是否独占（SLO §3）。
- 部署报告里写明 `X_DATA_MCP_RUN_PREPARE_WORKERS=1`；回填 PR 把它记进
  [STATUS](../STATUS.md) 的「服务」表。

**已知限制（XDM-74 补测，2026-10-27；不是本次部署引入的回归，验证时不要当失败）**：

- **退市日只有 TDX 整理期口径**：重建后 `market.security_master` 里 `delisting_date` 有值
  **152** 行（来源全部 `tdx_arrangement`），其余 `unknown`。交易所上市/退市名单（A-1/A-2）未准入，
  主表比 catalog **多 279 个实体**（沪主板 118 / 深主板 116 / 创业板 42 / 科创板 3）——它们当日
  没有 bar 且退市日未知，被当成仍在册；`universe(as_of=2026-10-23)` 因此是 5,351 而 catalog 是
  5,226。导出快照的成员带 `delisting_unknown` 标记（`src/research/universe.rs:508`），严格口径
  可据此排除。
- **代码复用/拆分仍 pending**：`600018` 只有 1 行（2006-10-26 起），没有拆分，
  `ambiguous_code_reuse=0`；A-1/A-2 接入前，这不算「已验证无复用」。
- **`market.suspensions` 只返回覆盖 `as_of` 的缺口**：`as_of` 落在区间内才出该行
  （`row.covers(as_of)`，`src/sqlite_supply/m2.rs:150-157`）；按日期范围验证时**必须给 `as_of`**，
  范围内的其它缺口只在 `temporal.unknown_known_at_rows` 里计数，返回 0 行不代表没有缺口。

部署后按实测回填 [STATUS](../STATUS.md)（版本 SHA、部署时刻、数据量、耗时、M2 行数），
并跑 `python3 scripts/check_current_facts.py`。

## 4. 回滚

- 回滚到 **preview.36**：切回 `releases/fcb86ab…` 的二进制（`readlink current` 指回它）。
  库同为 schema 24，**不需要换库**；preview.36 不读 M2 数据集，残留的表与
  `derived_dataset_versions` 行不影响读面。要让库也回到部署前状态，用本窗口归档的
  `state/preview.sqlite.v24-archived-<UTC>`。
- 再往回到 **preview.35**（v7/schema 21）：二进制与 schema 21 归档库
  `state/preview.sqlite.v21-archived-20261027T121543Z` 必须一起退，只退二进制会因 schema
  不匹配拒绝启动（[v8 切换清单](../contracts/v8-switch-checklist.md) §9）。
- 回滚后同样要重跑 §3 的验证，并在同一个 PR 里更新 STATUS。

## 5. 第二阶段运维步骤（草案，未执行）

v8 第二阶段（D-56 单进程多 socket）的运维顺序草案，来源是 XDM-73 备忘 §6；本清单只记录
**窗口内要做的事**，不是部署日提交，也不改变 [STATUS](../STATUS.md) 的当前事实。执行前每个
`[批准]` 项都需 owner 单独确认；生产 data 单元在 Ops-A 完成前保持单 `ops` socket。

### 5.1 Ops-A：私有栈三 socket 内存实测

- 在私有栈（不是 `/opt` 生产根）用与生产相同的发布目录与库副本起**一个** `data_service`，
  `ExecStart` 为 `<db> run/mcp.sock --entry ops run/research.sock --entry research
  run/maint.sock --entry maint --poll-ms 1000`；`X_DATA_MCP_RUN_PREPARE_WORKERS=1`。
- 三条就绪探针逐 socket 通过：`wait_service_ready.py run/mcp.sock --entry ops`（15 工具）、
  `--entry research`（5）、`--entry maint`（9）；同一进程只取一次写租约。
- 按 XDM-59 的方法记录：空闲 `MemoryCurrent`、一次真实 v2 日更的 `MemoryCurrent` 峰值、
  一次 research 大查询的峰值、`MemAvailable` 最低值、冷/热启动耗时。测量环境、数据规模、
  并发与代码版本一并记录，结果不带入生产预算。
- 只有实测给出每个入口峰值后，才写 `[批准]` 的 `MemoryHigh`/`MemoryMax`；测完停私有栈，
  释放端口、socket 与租约。生产不受影响。

### 5.2 Ops-B：生产同进程三 socket，隧道不动

- 前置：Ops-A 完成并批准内存值；`scripts/deploy_phase_g.sh` / `switch_binary.sh` 的
  PR 已合并（它们解析并生成多 socket `ExecStart`，保留 `--poll-ms` 等尾部参数）。
- `[批准]` 改 data 单元（或 drop-in）为三个 socket：`mcp.sock` 仍是 `ops` 入口，
  新增 `research.sock`/`maint.sock`；三条 `ExecStartPre` 就绪探针各断言自己入口的工具。
  `run/` 下 socket 的属主/权限与 data 单元同用户；旧 socket 先确认无进程使用，不 `rm` 活 socket。
- `[批准]` 以单进程绑定三个 socket 后逐个核对：`research.sock` 恰好 5 个工具、
  `mcp.sock` 15 个、`maint.sock` 9 个，三者 `capability_revision == generic-data-v8`，
  `get_data_capabilities({})` ≤1,500 B，head 计数与窗口前基线一致；同一库上第二个
  `data_service` 必须因租约拒绝启动，`maint.sock` 的 `run_worker_once` 在 worker 忙时返回
  `worker_busy`。
- **隧道不动**：`x-data-mcp-tunnel` 仍指 `run/mcp.sock`，研究面外部行为不变；Devin 实测
  （Ops-C）与隧道改指 `research.sock`（Ops-D）另行批准，不在本步骤。
- 回滚：退掉新增 socket 参数（回到单 `mcp.sock` 的旧 `ExecStart`）并重启 data；库不变。
  多 socket 是同一进程，任一 bind 失败进程整体不启动，不会有半个入口在服务。

### 5.3 记录

- 每一步在部署记录里写：实际 `ExecStart`、每条探针的 JSON（`entry`/`tool`/`tools`/
  `elapsed_ms`）、`MemoryCurrent` 观测值、回滚点。
- 部署日提交才把 [STATUS](../STATUS.md)「服务」表与
  [v8 切换清单](../contracts/v8-switch-checklist.md) §3/§9.6 的「已部署入口」翻成三入口；
  本草案与脚本改动都不翻。
