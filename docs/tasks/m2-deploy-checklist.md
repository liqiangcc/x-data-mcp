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
