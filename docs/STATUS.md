# 当前状态（STATUS）

本文件是**部署与运行事实**的唯一入口。README、AGENTS.md、`docs/daily-sync.md`
等文档只链接本文件，不再复述版本、主机、generation 或数据量。性能与容量目标见
[SLO](SLO.md)。

每次部署、迁移或 generation 激活后，执行者在同一个 PR 内更新本文件。

最后核实：2026-10-28（preview.37 实测值回填；preview.38 只换二进制、未重建库，schema、
generation 与数据量沿用该轮实测）。本轮实测来源：
`/opt/x-data-mcp-preview/reports/preview37-phaseg-deployment.json`（`deploy_phase_g.sh
--force-git-transport`）、`python3` + `sqlite3`（`file:…?mode=ro`，**含 WAL**：未 checkpoint 的
提交只在 `-wal` 里，读 `immutable=1` 会看到 checkpoint 前的旧计数）读 `state/preview.sqlite`、
`readlink`/`stat`/`ls -l`、部署窗口内的 MCP/x 消费侧验证（[M2 部署清单](tasks/m2-deploy-checklist.md) §3）。
preview.36 及更早的实测值保留为「历史」对照；本轮未重新测量的行（主机、系统、资源、足迹总量、
systemd 单元细节、热 generation 计数）标注为「历史」。部署后重建在生产机上执行但**非独占**，
下述耗时不作门禁证据。
「推断」标记表示由实测数据推出、但没有独立测量的结论。

## 部署

| 项 | 事实 | 来源 |
|---|---|---|
| 主机 | aws-node（EC2；systemd 启动日志记录 `Detected virtualization amazon`） | 运维记录、`journalctl` 启动段 |
| 系统 | Ubuntu 26.04 LTS，kernel 7.0.0-1014-aws | `/etc/os-release`、`uname -r` |
| 资源 | 2 vCPU（1 个物理核 × 2 超线程，Xeon Platinum 8259CL；`/proc/cpuinfo` 无 `sha_ni`），MemTotal 1,905,872 kB（约 1.82GiB）+ 8GiB swap（2×4GiB，实测已用 2.5GiB），根盘 77G（已用 50G、余 27G）；`/tmp` 是 953MB 的 tmpfs（占内存，实测已用 625MB） | `lscpu`、`grep -c processor /proc/cpuinfo`、`grep sha_ni /proc/cpuinfo`（无匹配）、`free -m`、`swapon --show`、`df -h / /tmp` |
| 本机工具链 | 本机禁止本地 Rust 编译：执行 `rustc --version` 被主机策略拦截并返回「Local Rust compilation is forbidden on this host (1.9 GB RAM)」，仅允许 `cargo fmt`。因此本机工具链版本未复核；部署二进制的构建版本见「构建」行 | 实测拦截提示 |
| 部署版本 | `v0.2.0-preview.38` | 部署窗口的合并提交；完整 SHA 与 `current/manifest.json` 由切换记录填入（本 PR 在切换窗口开始前保持 OPEN，由 Claude 合并后执行） |
| preview.38 之前 | preview.37 @ `49422db26be2da79ff39c7522897d25fcb6055bb`（历史；被 preview.38 取代，二进制目录保留作回滚点） | `reports/preview37-phaseg-deployment.json`、回填 PR 的 `readlink current`、历史 `releases/` 目录 |
| preview.37 之前 | v0.2.0-preview.36 @ `fcb86ab065e87f9ef7655fd87b8e224517bd8f1b`（历史；被 preview.37 取代） | `reports/preview36-phaseg-deployment.json`、历史 `readlink` |
| 部署时间 | **2026-10-28 切换窗口（Op-B）**，`scripts/switch_binary.sh` **只换二进制**（dry-run 后 `--execute`）：不重建库、不归档、不改 schema——库仍是 preview.37 的 schema 24 库（`state/preview.sqlite`，**1,790,316,544 B**）。切换后给 data 单元装两个 drop-in：`exec-start.conf` 把 `ExecStart` 换成一个进程绑定三个 socket（`mcp.sock`→`ops`、`research.sock`→`research`、`maint.sock`→`maint`，尾部保留 `--poll-ms 1000`）、`memory.conf` 设 cgroup `MemoryHigh=384M`/`MemoryMax=640M`/`MemorySwapMax=512M`（依据 XDM-84 独占测量，报告在仓库外）。逐 socket 就绪探针与回滚步骤见 [v8 切换清单](contracts/v8-switch-checklist.md) §9.7；实际时刻、探针 JSON 与报告路径由切换记录填入 | 切换窗口的 `switch_binary.sh` 报告、`journalctl -u x-data-mcp-data`、`systemctl cat`/`systemctl show -p MemoryHigh`、`stat state/preview.sqlite` |
| 部署时间（历史 preview.37） | 2026-10-28T00:21:26Z（UTC），`scripts/deploy_phase_g.sh --force-git-transport`：停 data/acquire → 归档旧 schema 24 库 → 从 release 提交读取期望 schema 后 `task_sqlite init` 全新库 → 起两个单元 → socket/工具数/schema 检查 → 写部署报告 `/opt/x-data-mcp-preview/reports/preview37-phaseg-deployment.json`（`status: DEPLOYED`，`before.schema=24`，`production: NOT_PRODUCTION_READY`）。**fresh init，不做原地升级**；部署窗口的设置：`X_DATA_MCP_TEXT_MODE=full`、`X_DATA_MCP_RUN_PREPARE_WORKERS=1`（v2 日更的准备线程串行，放开条件见 [M2 部署清单](tasks/m2-deploy-checklist.md) §1，待独占实测） | 部署报告 JSON、`journalctl -u x-data-mcp-data/-acquire`、`readlink`/`stat` |
| 部署时间（历史 preview.36） | 2026-10-27T12:15:43Z，`scripts/deploy_phase_g.sh --force-git-transport`（本机 gh 2.62 读不到 release asset digest，改走 `refs/dist` 的 git transport，与历次一致）：停 data/acquire → 归档 schema 21 库 → `task_sqlite init21` 全新 init 到 schema 24 → 起两个单元 → socket/工具数/schema 检查 → 写报告 `reports/preview36-phaseg-deployment.json`。每个已部署的发布目录按完整 SHA 命名，旧目录保留作回滚点。本次是全新库部署，不是热切换 | 部署报告 JSON、`readlink`/`stat`、`journalctl` |
| 数据库 schema | 已部署：schema 24（preview.38 与上一轮预览共用同一个库，**只换二进制、未重建也未归档**：`pragma user_version` 读回 24，库 **1,790,316,544 B**；该库由上一轮部署从 release 提交读期望 schema 后全新 init；preview.36 时同为 schema 24、库 1,732,116,480 B）；代码：schema 24（M1 的 schema 22 已按 §3/§7 扩展：口径/精度列、双序列写入、因子事件表与派生数据集版本登记、指数序列与逐指数口径元数据、交易日历、日更内部派生 item、研究快照导出；M2 PR-1 追加 023：证券主表、代码迁移、简称历史、股本事件、风险警示区间、停牌区间与两张规则表，并扩展 `derived_dataset_versions.dataset_kind`（当时只建表；写入路径由 M2 PR-3…PR-8 接线，preview.37 起部署）；M2 PR-2 追加 024：重建 `capital_events`，按 D-50 R-3 修订把 `category` CHECK 扩到 2/3/5/7/8/9/10，其他列、约束与索引原样保留） | `pragma user_version`（只读）、`src/sqlite_tasks.rs` 的 `COMPACT_SCHEMA_VERSION`、#383 M1 §14、#391 M2 §9 |
| 构建 | rustc 1.99.1 (58cf8f9c3 2026-10-13)，ubuntu-24.04，最低 glibc 2.39 | `current/manifest.json` |
| 部署目录 | `/opt/x-data-mcp-preview/current` → preview.38 的发布目录（按完整 SHA 命名，SHA 由切换记录填入）；上一个目录是 `releases/49422db…`（preview.37，保留作回滚点）。目录数量与总字节本轮未复核（历史 2026-10-24：6 个目录共 348MiB） | `readlink`（切换记录）、历史 `du -sh releases/*` |
| 迁移 | 2026-10-09 从 ecs-node 迁到 aws-node；全新部署后重新采集，未迁移 DB | `/opt/x-data-mcp-preview/reports/migration-to-aws-20261009.json` |
| ecs-node | 服务已 stop 并 disable，数据保留为冷备（含 generation `a49c314b…` 的字节） | 同上 |
| production_ready | `false`：owner 个人研究、本地使用，未授权对外分发（#71） | `get_data_capabilities` |

preview.38（2026-10-28 切换窗口，Op-B）只换二进制：schema 仍是 24、库不重建（沿用
preview.37 的 `state/preview.sqlite`），`capability_revision` 仍是 `generic-data-v8`，工具面不变。
变的是 data 单元的形状：**一个 `data_service` 进程服务三个 socket**——`run/mcp.sock`（`ops`，
编排入口，x 与日更 Skill 的连接不变）、`run/research.sock`（`research`，只读研究面）、
`run/maint.sock`（`maint`，仅本机 operator 用，不暴露隧道）。三入口的工具数为 ops 15、
research 5、maint 9（权威值见「服务」表）。文本模式 `full`、`--poll-ms 1000`、
`X_DATA_MCP_RUN_PREPARE_WORKERS=1` 都沿用；cgroup 上限
`MemoryHigh=384M`/`MemoryMax=640M`/`MemorySwapMax=512M` 按 XDM-84 的 w=1 独占测量设值
（报告在仓库外；`memory.conf` 删除后即回退默认无上限）。**隧道仍指 `run/mcp.sock`**
（Ops-D 未做），研究面外部行为不变。

v8 最小切换（preview.36，2026-10-27T12:15:43Z 部署完成；历史）：`x-data-mcp-data` 以 `--entry ops`
运行 `mcp.sock` 的 15 个工具（live `tools/list`），文本模式 `X_DATA_MCP_TEXT_MODE=full`
（systemd drop-in），`capability_revision=generic-data-v8`（live `get_data_capabilities`）；
库按 D-08 归档 schema 21 后全新 init 到 schema 24，并从 active generation `c184e9c4…`
重建（数据量见「数据」段）。preview.37（2026-10-27 收盘后的部署窗口）在**同一个 `generic-data-v8`** 下上线 M2：
工具面、`capability_revision`、schema 编号都不变，增加的是 6 个 dataset（§「M2 随
preview.37 部署」）与 `research-snapshot.v2` 导出。所以客户端不需要重新握手 revision，
但仍须按 `get_data_capabilities` 的 dataset 列表发现新数据（设计 §12.5 的裁决）。
历史：v7 自 preview.33 起运行；preview.34（2026-10-17T18:09Z）与 preview.35
（2026-10-24T17:25Z）都是二进制热切换，数据库沿用 schema 21，部署时刻无数据重建、
无契约变更（`capability_revision` 仍为 `generic-data-v7`，工具面不变）。
回滚到 preview.35 需二进制与 schema 21 归档库（`state/preview.sqlite.v21-archived-20261027T121543Z`）
一起退（只退二进制会因 schema 不匹配拒绝启动）。回滚到 preview.36 不需要换库（同为 schema 24），
但 preview.37 重建后写入的 M2 数据表与 `derived_dataset_versions` 行会留下；preview.36 二进制不读
这些 dataset，读面与服务不受影响，重新部署 preview.37 时会因 fresh init 归档旧库（见
[M2 部署清单](tasks/m2-deploy-checklist.md) §4）。

M1 已随 preview.36 部署（代码 schema 24：M1 的 022 + M2 PR-1 的 023 + M2 PR-2 的 024，
见上表「数据库 schema」行）：PR #389/#390/#394/#397/#400/#402/#406/#408 与修复 #416。
部署按 D-08（归档旧库 + 全新 init），发布包默认带 `export_research`（#413 起
`scripts/build_release.sh` 固定 `--features export`，并在打包后断言该命令存在）。
部署窗口、重建步骤与耗时见 [M1 部署清单](tasks/issue-383-m1-deploy-checklist.md) §8；
实现记录见 [M1 实现记录](tasks/issue-383-m1-implementation.md)。

M2（[#391](https://github.com/liqiangcc/x-data-mcp/issues/391)）的 PR-1 把迁移链推到
schema 23（`migrations/tasks/023_security_master.sql`），PR-2 追加 024
（`migrations/tasks/024_capital_categories.sql`）把迁移链推到 schema 24：023 只建表，
024 只重建 `capital_events` 以放宽 `category` CHECK（D-50 R-3 修订），都不改任何读写行为。
M1 重建后部署事实已是 schema 24；schema 22/23 的库既不会被新二进制原地改写，也不会被
024 回填（`check_schema` 直接拒绝非当前 schema）。设计见
[M2 设计](tasks/issue-391-m2-security-master.md) §9。

M2 随 preview.37 部署（PR #412/#415/#419/#420/#422/#424/#427/#428/#425，CI 绿）。六个
解析事实数据集 `market.security_master`、`market.security_names`、`market.capital_events`、
`market.risk_warnings`、`market.suspensions`、`market.price_limit_rules` 在 v8 代码上可读
（`contract_version:"v2"` + `filter.kind:"daily"`，未登记版本返回 `manifest.status=absent` 与
0 行）。producer 情况按来源分别如实记录：

| 数据集 | 写入路径 | 未接入的来源（`pending_source`） |
|---|---|---|
| `market.security_master` | 日更 run 的 `derived:security_master` item | 交易所上市/退市名单、日期化简称历史（#308）；主表用 TDX `base.dbf` + `infoharbor_spec.cfg` + `gbbq` 组装，缺的来源在大表里按 `unknown`/`unbounded` 表达 |
| `market.capital_events` | 日线提交的同事务写入（`gbbq` 非 cat1 类别，D-41/D-50 准入后）；全量采集与日更都走这条路径 | 无 |
| `market.suspensions` | 日更 run 的 `derived:suspensions` item | 停复牌公告（`confirmed` 行为 0，只出 `unresolved_gap`） |
| `market.risk_warnings` | 日更 run 的 `derived:risk_warnings` item | 历史 ST（#308 未准入）：写入 0 行，只记 `pending_source` |
| `market.price_limit_rules` | 日更 run 的 `derived:trading_rules` item | 规则原文（A-7 未准入）：写入 0 行，只记 `pending_source`；没有规则时涨跌停派生一律 `unknown` |
| `market.security_names` | 暂无写入器 | 日期化简称历史（#308）；表随 schema 23 存在，导出的 `security_names.parquet` 是合法的 0 行文件 |

四个全市场 M2 数据集由**日更 plan-run** 的内部 item 维护（`derived:security_master`、
`derived:risk_warnings`、`derived:suspensions`、`derived:trading_rules`，见
[日更说明](daily-sync.md) 与 `src/task_control/derived.rs`）：全新库重建时，第 3 步的 v2
`plan_market_update`/`run_market_update` 在实体 item 之后执行这四个 item。
`derived:security_master` 与 `derived:suspensions` 会登记版本（`derived_dataset_versions`）；
`derived:risk_warnings`/`derived:trading_rules` 在来源未准入时写 0 行、不登记版本，读取仍是
`manifest.status=absent` 而不是「空数据集」。`market.capital_events` 另有 per-entity 写入，
不依赖这四个 item。研究快照导出（`research-snapshot.v2`，7 个 M2 文件）随 preview.37 部署。

**M2 已知限制（XDM-74 补测，2026-10-27，私有栈）**：

- **退市日只有 TDX 整理期口径**：主表里 `delisting_date` 有值的 **152** 行（来源全部
  `tdx_arrangement`），其余 `unknown`。交易所上市/退市名单（A-1/A-2）未准入，因此主表比 catalog
  **多 279 个实体**（沪主板 118 / 深主板 116 / 创业板 42 / 科创板 3），这批当日都没有 bar 且退市日
  未知，被当成仍在册（`universe(as_of=2026-10-23)` 5,351 vs catalog 5,226）。导出快照的成员带
  `delisting_unknown` 标记（`src/research/universe.rs:508`），严格口径可据此排除。
- **代码复用/拆分仍 pending**：`600018` 只有 1 行（2006-10-26 起），没有拆分；
  `ambiguous_code_reuse=0`。A-1/A-2 接入前的复用判定与拆分都是待办，不是已验证结论。
- **`market.suspensions` 按 `as_of` 返回**：只有 `as_of` 落在缺口区间内才返回该行
  （`row.covers(as_of)`，`src/sqlite_supply/m2.rs:150-157`）；按日期范围查时**必须给 `as_of`**，
  否则只会看到默认时点覆盖的缺口，范围内的其它缺口只在 `temporal.unknown_known_at_rows` 里计数。

## 服务

| systemd 单元 | 进程 | 入口 | 工具数 | 备注 |
|---|---|---|---|---|
| `x-data-mcp-data` | `data_service`（单进程、三 socket） | `run/mcp.sock`（`--entry ops`）、`run/research.sock`（`--entry research`）、`run/maint.sock`（`--entry maint`） | 15 / 5 / 9 | capability `generic-data-v8`（含只读 `get_yearly_base_years`）；`X_DATA_MCP_TEXT_MODE=full`；drop-in：`exec-start.conf`（三 socket `ExecStart`，尾部保留 `--poll-ms 1000`）、`poll-interval.conf`（`--poll-ms 1000`）、`memory.conf`（`MemoryHigh=384M`/`MemoryMax=640M`/`MemorySwapMax=512M`）、`X_DATA_MCP_GENERATION_ROOT`、`X_DATA_MCP_RUN_PREPARE_WORKERS=1`（preview.37 起，v2 日更的准备线程串行；放开条件见 [M2 部署清单](tasks/m2-deploy-checklist.md) §1）；`maint` 只在本机使用、不经隧道 |
| `x-data-mcp-acquire` | `acquire_service` | `run/acquire.sock` | 10 | 设置了 `X_DATA_MCP_TDX_FETCH=current/scripts/tdx_fetch.py` |
| `x-data-mcp-tunnel` | OpenAI `tunnel-client`（profile `x-data-mcp`） | 转发到 `run/mcp.sock` | — | 只转发 data server，不转发 acquire；`ExecStartPre` 用 `/opt/x-data-mcp-preview/wait-data-service.py`（该文件不在仓库内），就绪等待上限 **120s**（2026-10-24T16:56:58Z 由 10s 改为 120s，`deadline=time.monotonic()+120`；但单元未设 `TimeoutStartSec`，systemd 默认 90s 也覆盖 `ExecStartPre`，所以实际生效上限是 **90s**），单元为 `Restart=always` / `RestartSec=10`；data 服务停止时隧道会因 stdio 子进程退出而自行退出 |

访问边界与认证细节属于运维信息，不记录在公开仓库。当前边界是否可接受，尚待 owner 明确决定。

运行面（2026-10-28 owner 批准）：data 单元新增 drop-in `poll-interval.conf`，`ExecStart` 追加 `--poll-ms 1000`——原默认 100 ms 的空闲轮询约占 8% CPU（2 vCPU），改后约 1.4%；新任务接手延迟 ≤1 s。删除该文件并 `daemon-reload` 后重启即回退默认值。preview.38 的三 socket `ExecStart` 写在 `exec-start.conf` 里，仍以 `--poll-ms 1000` 结尾（`scripts/data_unit_layout.py` 原样保留尾部参数），内存上限写在 `memory.conf`；两者的回滚步骤见 [v8 切换清单](contracts/v8-switch-checklist.md) §9.7。

工具数与能力版本的 live MCP 只读复核（preview.37 部署窗口，回填到部署记录）：`mcp.sock` 的
`tools/list` 返回 15 个工具，`get_data_capabilities` 返回
`capability_revision=generic-data-v8`；acquire socket 返回 10 个工具。preview.38 切换窗口的逐 socket
探针：`research.sock`→`query_data`、`mcp.sock`→`plan_market_update`、`maint.sock`→`compute_metrics`，
三者工具数 5/15/9、`capability_revision` 均为 `generic-data-v8`；隧道仍连 `mcp.sock`，x 契约断言与
preview.37 部署窗口一致。

代码（main）注册 24 data tools：三个入口 `research`/`ops`/`maint` 由
`data_service --entry` 选择，`mcp.sock` 继续作编排入口，`tools/list` 不再内联
`outputSchema`。preview.38 起生产同时部署三个入口（单进程三 socket，见「服务」表）；
隧道改指 `research.sock`（Ops-D）仍未做；Ops-C 已由 AI 客户端（pi）实测（2026-10-28），
结论是 `ops` 入口保持 `full`、`summary` 不作缺省，见
[v8 切换清单](contracts/v8-switch-checklist.md) §2.3 与 §9.6。

## 数据

**preview.37 部署后的重建已执行完成**（2026-10-28T00:22–00:40Z）：全新 schema 24 库，按
[M2 部署清单](tasks/m2-deploy-checklist.md) 从 active generation 重建（目录 + 日线 + 年线 +
v2 plan/run 派生 + 年线重聚合）。**preview.38 只换二进制、未重建库，下表的 generation 计数与字节未变（切换窗口未重测）。** 下表是本轮实测。重建在生产机上执行但**非独占**（多个开发
agent 同时在跑），下述耗时不作门禁证据。标注「历史」的行是更早的对照值（2026-10-24 补采或
preview.36 重建）。

| 项 | 事实 | 来源 |
|---|---|---|
| active generation | `c184e9c4b816dfc068d4d6e2eaedcf5e1742ad2979b0e54be2af798b54995d91`（2026-10-24T17:01:34Z 激活，重建后仍为 active）；最新集中行情日 **2026-10-23** | `generations/active-generation.json`、live `sync_status`、重建读回 |
| 热 generation | **5/6**：1 active + 4 superseded（`9d9e38d7`、`f289c612`、`8b8614c3`、`54553656`）；另有 10 个 evidence_only。本次部署只读该代做重建、未做 acquire，热计数沿用最近一次只读实测（2026-10-26），部署后未重测 | `generations/acquire.sqlite`（按 `state` 计数）、`src/acquire/service.rs:39,456,1058-1084` |
| 数据库 | `state/preview.sqlite`，fresh compact schema 24（`pragma user_version=24`），**1,790,316,544 B**；preview.36 的 schema 24 库改名归档为 `state/preview.sqlite.v24-archived-20261028T002126Z`（1,732,116,480 B，保留，不迁移），更早的 schema 21 归档 `state/preview.sqlite.v21-archived-20261027T121543Z`（1,185,640,448 B）也在 | `stat`、`pragma user_version`、部署报告 `archived_db` |
| catalog | `catalog-1793146923967808901`，**5,226** 条 | `catalog_head`/`catalog_members` 只读查询 |
| 日线 | 不复权 **5,224** 条序列 + 前复权 **5,224** 条序列（`qfq-exact-v1`，6dp）；最新集中行情日 2026-10-23 | `market_series`（`period=1`）只读查询、重建读回 |
| 年线 | **5,224** 个 published（每实体一条 yearly 序列，`yearly_series_lineage` 同数） | `market_series`（`period=2`）只读查询 |
| 基准指数 | **9** 条；`sh.000001` 剔除 16 根、`sz.399001` 剔除 9 根 OHLC 矛盾的 bar（D-51） | 指数序列只读查询、live `market.index` universe、D-51 |
| 交易日历 | **8,793** 天（1990-12-19 … 2026-10-23） | 日历表只读查询 |
| 复权事件 | **56,514** 行，**5,126** 个因子集 | 因子事件表只读查询 |
| 缺失 | `sh.688827`、`sz.001381`（官方 hsjday `.day` 缺成员，预期）保持 absent：catalog 中存在，日线/年线均无 series | `catalog_members` 反连接 `market_series` 只读查询 |
| M2 数据集 | preview.37 起由日更 run 维护：`security_master` **5,503** 行 / 5,503 实体（`security-master-assembly-v1`）、`capital_events` **122,370** 行 / 5,224 实体（category 2: 3,816、3: 3,268、5: 107,703、7: 17、8: 126、9: 7,166、10: 274）、`suspension_intervals` **65,516** 段（全部 `unresolved_gap`，`confirmed` 0，`suspension-gap-v1`）、`code_migrations` **1**（`candidate`）；`risk_warning_intervals`、`price_limit_rules`、`min_tick_rules`、`security_name_history` 均 **0** 行（来源未准入/未接入，只记 `pending_source`）。`security_master`/`suspensions` 在 `derived_dataset_versions` 里各登记 1 个版本；`base.dbf` 字节相同的重复行已按 D-55 合并（`000020`、`200020`），合并数记在 item evidence 的 `duplicates_collapsed` | 只读查询 `derived_dataset_versions`、各表计数 |
| 重建耗时（生产，非独占） | 本轮（preview.37）：全量采集 write-loop **667s**（53 个 `start_collection` 块任务，00:22:05→00:32:54Z；其中 2 个 `partially_failed`——5,226 considered / 5,224 succeeded / 2 absent）；v2 `plan_market_update`/`run_market_update` **84s**（00:33:01→00:34:18Z，5 个 eligible 实体 + 14 个派生 item——9 指数 + 日历 + 4 个 M2——全部成功）；年线重聚合 **320s**（53 个任务，00:34:22→00:39:38Z）。历史（preview.36）：全量采集 write-loop 844s；v2 plan/run 41s（5 eligible + 10 派生）；年线 302s | 重建驱动记录、`tasks`（`created_at`/`last_progress_at`）、回填 PR |
| 任务与事件 | 本轮：**108** 个任务——1 个 catalog 采集 + 53 个日线 collect（51 succeeded、2 `partially_failed`）+ 1 个 v2 日更 run + 53 个年线任务，全部结束，无 `running`；`market_events` **15,691** 行。历史（preview.35 时点）：579 个任务（529 succeeded、49 partially_failed、1 failed）、`market_events` 83,545 行 | `tasks`、`market_events` 只读查询 |
| 退役记录 | 历史：`87322997…` 2026-10-19 退役；`2d5851a4…` 2026-10-24T16:57:16Z 退役（548M→16K）；`a119629b…` 2026-10-24T17:52Z 退役（574M→16K）；更早 4 个 2026-10-13 退役为 evidence_only | `generations/acquire.sqlite`、历史 `du -sb` |

验证（preview.37 部署窗口实测）：

- x 消费侧契约断言通过：`sh.600006` 的 2025 年数据——manifest 1 条、status `present`、
  `metadata.adjustment = {confirmed, qfq}`、close 为十进制字符串（按 6dp 精度去尾零）。
- `market.index` universe 返回 9 个实体。
- 6 个 M2 数据集在 ops 入口可读回（`contract_version:"v2"` + `filter.kind:"daily"`）：
  `market.security_master 5,503` 行、`market.capital_events 122,370` 行、
  `market.suspensions 65,516` 段、`market.security_names 0`；
  `market.risk_warnings` / `market.price_limit_rules` 返回 `manifest.status=absent` + 0 行，
  并带各自的 `limitations`（来源未准入）。
- 库内读回（只读 SQL，含 WAL）与导出/缓存无关的计数：catalog 5,226、日线前复权 5,224、
  年线 5,224、日历 8,793、复权事件 56,514、指数 9、主表 5,503、停牌 65,516。
- 历史（preview.36）：全量 daily 导出 119s、900MB（`daily_raw` 16,737,243 行）；preview.37
  重建后未重跑导出。


部署后首次增量更新（2026-10-12 收盘后，跨代 Append，验证 GOAL-008 关闭条件）：2026-10-13 执行——Skill 会话先完成新 generation `2d5851a4…` 的 acquire→verify→activate（06:52Z），随后用 Skill 逐页枚举 5,228 个目录成员做增量，27 分钟内未提交任何 `update_data`，速度不足被叫停；10-09 与 10-12 的追加改由程序化驱动 `prod_append_v6.py`（PR #347，2026-10-13 合并，脚本在 `evidence/perf-g8/s5/prod_append_v6.py`）完成：daily write-loop 311.4s（53 块、5,224 项）+ 年线重聚合 239.9s（53 任务），读回零差异（published 5,224、落后恰 12 家 fail-closed、staged_records=0）。同日第二次同步（10-13 收盘后）：Skill 会话完成新 generation `a119629b…` 的 acquire→verify→activate（10:35Z）后，对 27 个实体自行提交了 10-13 的 `update_data`（成功），随后被操作员叫停；其余实体的追加仍由程序化驱动完成（幂等跳过已更新的 27 家）：daily write-loop 173.1s（52 块、5,197 项，published 5,184 + 13 fail-closed）+ 年线重聚合 259.1s（53 任务，含 Skill 已更新实体的年线补齐），读回 staged_records=0。事件计数：first_publish 10,448；append 10,331；replace 92；no_change 25；yearly_rederive 10,423。

历史 2026-10-19 日更（v7 plan/run 第二次真实运行，目标日 2026-10-19）：acquire 首次被 `generation_capacity` 拒绝，退役 87322997 后重新获取 generation `9d9e38d7…`（571,587,501 B，约 30 分钟），verify=consistency_verified，activate 带 expected_previous。日线 plan/run 5,224 项，成功 5,207、失败 17（均 `REVISION_UNVERIFIED`），run 189s；17 项用 `update_data` 前向窗口补漏，16 成功，`sh.603517` 窗口内无 bar（预期 fail-closed）。年线 plan eligible 5,186 / ineligible 37（`value_semantics_changed` 21、`daily_replaced_since_derivation` 16，`ineligible_entities` 首次在真实数据上完整列出、未截断），run 全部成功，417.5s；37 项显式重推后再 plan：already_current 5,224、eligible 0、ineligible 0、absent 4。进度响应最大 1,788 B；最低 MemAvailable 712MB。当时热 generation 为 6/6，下一次 acquire 前需要再退役一个（这一段描述的是 10-24 补采前的状态，已被下一段取代）。

2026-10-24 补采（O1 + O2，目标日 2026-10-23，一次补齐 10-20..10-23 四个交易日；时间均为数据库写入时间，与驱动端墙钟可能差数秒）：

1. 16:57:16Z 退役 `2d5851a4…`（`ops-20261024-retire-2d58`，superseded→evidence_only）：热 6/6→5/6，为 acquire 腾出计数；退役前 548M，释放约 546MB。
2. 16:57:26Z acquire `ops-20261024-acq-1` 受理（task `acq-82c4a0824ae393a2063ab0f8`），17:00:03Z 完成，共 572,630,763 B（base 16,245,958 + gbbq 6,288,317 + hsjday 550,096,488），产出 generation `c184e9c4…`。
3. 17:01:24Z verify `ops-20261024-ver-1` → `consistency_verified` / `VERIFIED_COMPATIBLE`（decision `efef9f2b…`）；17:01:34Z activate `ops-20261024-act-1`（带 expected_previous `9d9e38d7…`）。
4. 17:02:02→17:06:28Z 日线 plan/run（`ops-20261024-daily-run`，plan `08d878ba…`，5,224 项）：5,199 succeeded、25 `REVISION_UNVERIFIED`（除权股历史改写窗口，实体清单见 `task_items`）。
5. 17:07:30→17:07:49Z 日线显式前向窗口补推（`explicit-range-task-v1`，25 项，25/25 succeeded；实体集合与上一步失败项完全一致）。
6. 年线 plan：eligible 5,083 / ineligible 141（141 = 5,224 − 5,083，可反算；拆分为 25 `daily_replaced_since_derivation` + 116 `value_semantics_changed` 来自 plan 返回，plan 结果未落盘，未独立复核）。
7. 17:09:19→17:16:03Z 年线 run（`ops-20261024-yearly-run`，plan `a708259e…`，5,083 项，5,083/5,083 succeeded）。
8. 17:17:00→17:17:05Z 年线显式补推 100/100 succeeded；17:18:42→17:18:43Z 再补 41/41 succeeded。
9. 再 plan：日线 already_current 5,216 / eligible 8 / absent 4（合计 5,228）；年线 already_current 5,224 / eligible 0 / ineligible 0 / absent 4（合计 5,228）。同样未落盘，worker 只做了合计校验，结果与库内「5,216 个 head=10-23 + 8 个落后 + 4 个 absent」一致。

补采后的落后集合由 13 个变为 8 个：6 个原落后实体（sz.002731、sh.600293、sz.002388、sh.603517、sh.603759、sh.603838）已补到 2026-10-23，新增 1 个（sh.601326 停在 2026-10-20）。

运维事实：热 generation 上限 6（`MAX_HOT_GENERATIONS`，`src/acquire/service.rs:39`；判定见 `:456`，hot = 状态不是 evidence_only 的代），超限后 `acquire_generation` 被拒 `generation_capacity`；Skill 无权退役 generation；退役历史：2026-10-13 由操作员退役 4 个已无字节的旧代，2026-10-19 退役 87322997，2026-10-24T16:57:16Z 退役 2d5851a4（释放约 546MB）。**历史：10-24 补采后一度回到 6/6**（新代 `c184e9c4` 激活，`9d9e38d7` 转为 superseded）；17:52Z 退役 `a119629b…` 后当前为 **5/6**（2026-10-26 只读实测：1 active + 4 superseded），下一次 acquire 可直接受理。保留策略（`RETAINED_SUPERSEDED=2`，`src/acquire/service.rs:43`）已随 preview.35 **于 2026-10-24T17:25Z 部署**，但只在 activate 提交成功后触发（`src/acquire/service.rs:1058-1084`），不能解除 acquire 之前的容量拒绝。Skill 在有更新日的路径仍未完整验证（枚举过慢一次、部分自行追加一次）；#304 的有效 PASS 仍为 3/10（均为 unchanged）。批量更新工具（`plan_market_update`/`run_market_update`）自 `generic-data-v7`（preview.33，2026-10-16）起已实现并随二进制上线；preview.36 与 preview.37 的日更即由它驱动（preview.37 本轮 84s，见「重建耗时」行）；Skill 逐页枚举的旧路径已不是日更主路径。

## 新鲜度

| 项 | 事实（2026-10-28 preview.37 部署后回填） | 来源 |
|---|---|---|
| 最后任务 | 本轮：53 个年线任务中最后一个于 2026-10-28T00:39:38Z 完成（`explicit-yearly-task-v1`），之后无新任务；库内共 108 个任务（见「数据」表） | `tasks` 只读查询 |
| 落后交易日 | **无**：日线/年线 head 均为 2026-10-23（`last_day=20749`），即最近一个已收盘交易日。历史：补采前 head 为 2026-10-19，10-20..10-23 四个交易日的缺口已由 2026-10-24 补采补齐（补采时间线见下） | `market_series`、重建读回 |
| 官方输入 | 历史（2026-10-24T17:26Z 实测，该代仍为 active）：`curl -sI` 与 active generation `c184e9c4` 的 `snapshot-manifest.json` 逐项相同：base 16,245,958 B / `"6adc3a88-f7e4c6"` / `Sat, 24 Oct 2026 04:56:40 GMT`；gbbq 6,288,317 B / `"6adc3a88-5ff3bd"` / 同上；hsjday 550,096,488 B / `"6adb13be-20c9ce68"` / `Fri, 23 Oct 2026 07:58:54 GMT`。重建未重新 acquire，本轮未再 `curl -sI` 比对 | `curl -sI <三个官方 URL>`（历史）、`generations/generations/c184e9c4…/snapshot-manifest.json` |
| 容量 | hot **5/6**（1 active + 4 superseded；权威值见「数据」表的「热 generation」行）：下一次 acquire 可直接受理（5 < 6）。本次部署未做 acquire，计数未在部署后重测 | `generations/acquire.sqlite`、`src/acquire/service.rs:39,456` |

## 2026-10-23 重启与就绪（实测）

- 主机于 2026-10-23 06:49:20 重启（`journalctl --list-boots` 的 boot `d462ed36c2ad42aba34aed2031018be3`）；上一轮 uptime 5d12h39m，恰好从 preview.34 切换（10-17T18:09Z）开始。
- `x-data-mcp-data` 06:49:25 启动，`run/mcp.sock` 于 06:49:56.531 建立（`stat` 的 Birth）→ 冷启动到可服务 **31.5s**（库内有 574 个历史任务）。
- `x-data-mcp-tunnel` 06:49:25 首次启动，`ExecStartPre`（`wait-data-service.py`，当时 deadline 为 10s；10-24T16:56:58Z 已改为 120s）于 06:49:37 报 `data service IPC readiness failed`、`Control process exited, code=exited, status=1/FAILURE`；systemd 按 `Restart=always`/`RestartSec=10` 在 06:49:47 重试，06:49:56 `Started`，同一秒 tunnel-client 打出 `🟢 tunnel-client started`。**隧道在重启后不可用约 31s**。
- 10-24 06:43:16 的那次单元重启不是「恢复」：06:43:09 `apt-daily-upgrade` 触发 `systemd` daemon-reexec，随后 needrestart 重启了 tunnel、sshd、nginx 等单元；此前该单元自 10-23 06:49:56 起连续运行 23h24m（systemd 的 `Consumed … wall clock time`）。同样的模式在 10-23 07:42:51 也发生过一次（停机约 1s）。
- 因此重启本身只造成约 31s 的隧道不可用；10-20..10-23 的数据缺口与重启无关（10-20T00:07Z 之后就没有任何任务）。`scripts/switch_binary.sh` 的 S6 在旧版本只等 20s 且只判 socket 文件存在，按 31.5s 冷启动会判失败并回滚（推断，未在真实部署中触发）。仓库脚本已在 #370（`8e83f22`，2026-10-24 合入 main）改为 S6 调用 `scripts/wait_service_ready.py`、默认超时 120s（`XDM_READY_TIMEOUT` 可覆盖），不再只判 socket 文件存在；按 31.5s 冷启动，当前脚本可在超时内等到就绪（推断）。
- **残留风险（部分缓解，未消除）**：`wait-data-service.py` 的 deadline 已于 2026-10-24T16:56:58Z 从 10s 改为 120s，因此「冷启动比等待长」这条路径在当前库规模下不会再触发；剩下的风险是就绪超过 120s、或单元连续失败触到 `StartLimitBurst=5` 时停在 `failed` 不再重试，需人工介入。这两条都未实测（推断）。

## 足迹

| 项 | 事实（历史：2026-10-24T17:26Z 实测，preview.35 切换之后；preview.36 部署后未整树复核） | 来源 |
|---|---|---|
| `/opt/x-data-mcp-preview` 全树 | 历史 4.2GiB（2026-10-24T17:55Z 清理后）；preview.36 部署后已变化：新库 1,732,116,480 B + 旧 schema 21 归档库，本轮未重测全树 | 历史 `du -sh .`、`du -sb .` |
| generations | 历史 2.7GiB：5 个带字节代（active `c184e9c4` 572,634,818；superseded `9d9e38d7` 571,591,556、`f289c612` 571,549,313、`8b8614c3` 574,645,314、`54553656` 574,839,345）；另 10 个代目录各约 4KB（只有清单/决策/证据，无代字节） | 历史 `du -sb generations/generations/*` |
| state | 本轮实测：`preview.sqlite` **1,790,316,544 B**（fresh schema 24）+ preview.36 的库归档 `preview.sqlite.v24-archived-20261028T002126Z`（1,732,116,480 B）+ 更早的 `preview.sqlite.v21-archived-20261027T121543Z`（1,185,640,448 B）（均保留）。历史：两份 v20 归档库已移出 `/opt` 到 `/root/x-data-archive/` 并 gzip | `ls -l state/`、`stat`、部署报告 |
| releases | 历史 348MiB，6 个发布目录（2026-10-24）；本轮新增 `releases/49422db…`（preview.37），`releases/fcb86ab…`（preview.36）保留作回滚点；数量与总量本轮未复核 | 历史 `du -sh releases/*`、`readlink current` |
| reports | 历史 2.8MiB；本轮新增 `preview37-phaseg-deployment.json`（550 B，与 preview.36 同尺寸），总量未复核 | 历史 `du -sh reports`、`ls -l reports/` |
| `/tmp` | 历史：tmpfs 953MB，已用 19MB（2%）；2026-10-24T17:49Z 清理了 10-23 的调试残留（约 606MB） | 历史 `df -h /tmp`、`du -sh /tmp/*` |
| 根盘 | 历史：77G，已用 49G，余 28G（`/tmp` 是内存盘，不计入） | 历史 `df -h /` |

## 进行中

- **SLO 门禁**：第 1 行（同代无变化 `update_data`，write-loop <600s）**2026-10-28 已解除临时豁免**（D-54 解除，见 D-57）：XDM-84 实测同代无待更新 87.6s、同代全量 5,238 item **573.4s <600s**（main `0f3fe25`，单进程三 socket，w=1，2 vCPU，load average 4–8、最高 8.12，非完全独占，含 research 并发；全量口径用显式 `end_date` 构造，见 [SLO](SLO.md) 备注）。第 2 行（跨代有变化，<600s）仍按 D-52 **临时豁免中**：同次实测跨代全量 **734.0s** 高于 600s，acquire 切代 VmHWM 33.3 MiB。两项门槛本身不变；这些时间来自非独占窗口，只作相对比较。**生产已随 preview.38 换成单进程三 socket（`mcp.sock`/`research.sock`/`maint.sock`，见「服务」表）**。详见 [SLO](SLO.md) 备注与
  [DECISIONS](decisions.md) D-52/D-57。
- **GOAL-008 性能**：PR #323 已于 2026-10-11 squash 合并（`8a446b1`），发布 v0.2.0-preview.31，2026-10-12 部署。
  - 全市场验收（XP-015，perf `3b11231`，aws 私有栈，浅链，53 批，trace-OFF，每负载单次）：年线首次 310.6s、日线跨代 Append 104.2s（合成截断夹具）、年线重聚合 328.3s、日线同代 NoChange 78.0s，write-loop 均 <600s，读回全部 PASS（见 [SLO](SLO.md)）。第三类「保留的真实 mixed Append/Full/fail-closed」**未运行**（retained gen-1 字节永久不可得）；`AGENTS.md` 记载的离线 harness 结果仅作旁证，不作替代。
  - **最坏链深未做全量实测**：错开的提前全量重写（每实体阈值 64–127）后最坏链深 ≤127，年线批中位 10.3s，按 53 批外推约 548–588s，余量约 2–9%；日线在深链上的有效实测缺失。
  - 校验分层变化（均经 owner 批准，2026-10-10 / 11）：accept 只检查在途；`get_data_availability` 统计与校验走已提交层（含年线）；`get_yearly_base_years` 叶节点完整校验、祖先已提交层；年线发布的先验年线 head 读取降到已提交层；rollover 校验由「父深度必须正好 256」放宽为 64..=256。全链完整校验仍在发布提交、服务启动 sweep、`task_sqlite check`、`query_data`。
  - **已于 2026-10-12 上线到生产（preview.31，全新重建）**，生产读回：catalog 5,228、日线 5,224、年线 5,224、absent 4 个、5 个抽样实体日线/年线可读。
- **#304**：连续 10 个交易日仅用 Skill 完成同步的验收。已记录的 PASS 运行（Run 3、Run 5、2026-10-08）全部是 `day=unchanged`，有更新那天的路径尚未覆盖。有效计数以 #304 的评论为准；2026-10-08 那次使用的是提示词 `daily-ai-sync-v2`。

## 已知问题

| 问题 | 影响 | 来源 |
|---|---|---|
| 只换二进制（不再每次全新 init）的部署：启动 registry sweep 的耗时随历史任务线性增长（推断），10-23 时已超过 `x-data-mcp-tunnel` 当时 10 秒的就绪等待（`wait-data-service.py`；现为 120s）。两个实测点：**2026-10-23 主机重启后（preview.34，574 个任务，冷 page cache）31.5s**；**2026-10-24T17:25Z preview.35 切换（579 个任务，热 page cache）13.5s**。两点无法区分「任务数」与「缓存冷热」的贡献（推断：后者影响更大），因此「线性增长」本身未得到证实，需专门测量。10-23 那次是首次 `ExecStartPre`（当时 deadline 10s）失败后靠 `Restart=always` 重试成功；10-24T16:56:58Z 起该 deadline 已改为 120s，17:25 切换时一次等到 socket（13.5s），空窗来自 `RestartSec=10` 加就绪时间。缓解（只 sweep 最近 N 天、先开 socket 后台 sweep）会改变「sweep 失败即拒绝启动」的语义，需另行批准。schema 21 的启动检查已改为逐 series 索引点查（S2b F3，不再 loose-scan） | 每次重启/切换首轮必失败，多花一个 `RestartSec`；若重试也失败则长时间不可用 | `journalctl -u x-data-mcp-data/-tunnel`、`stat run/mcp.sock`、`reports/preview35-switch-20261024.json`（`readiness_probe.data_elapsed_ms=13461`） |
| tunnel 启动竞态（仓库脚本已修，tunnel unit 仍用 /opt 旁路脚本）：2026-10-23 06:49 那次是 `wait-data-service.py` 的 10s deadline 短于 31.5s 冷启动，首轮 `ExecStartPre` 失败、靠 `Restart=always` 在 10s 后重试成功；`/opt` 内该脚本已于 2026-10-24T16:56:58Z 把 deadline 改为 120s，因此 10-24T17:25 切换时 `ExecStartPre` 一次等到 socket（13.5s）。#370（`8e83f22`，2026-10-24 合入 main）新增 `scripts/wait_service_ready.py`，`scripts/switch_binary.sh` 与两个部署脚本已调用（默认 120s）；但 tunnel 的 `ExecStartPre` 仍调用 `/opt` 上的 `wait-data-service.py`，该文件不在仓库内、也不受版本控制（D-17 只部分落地） | 重启/切换时的隧道空窗现在由 `RestartSec=10` + 就绪时间决定（17:25 实测约 13s）；若单元连续失败触到 `StartLimitBurst=5`，会停在 `failed` 需人工介入（未实测，推断）；仓库脚本已能等到 13.5–31.5s 冷启动（默认 120s），不再在 S6 判失败 | `journalctl -u x-data-mcp-tunnel`（10-23、10-24T17:25）、`grep deadline /opt/x-data-mcp-preview/wait-data-service.py`、`stat` 该文件 mtime、`scripts/switch_binary.sh`、`systemctl cat x-data-mcp-tunnel`、`systemctl show -p StartLimitBurst`、PR #370 |
| 报告覆盖不均：preview.33 / preview.34 没有报告（`reports/` 在 preview.32 之后直到 preview.35 才有条目）；preview.35 的报告是切换后整理的（mtime 2026-10-24T17:30:30Z，切换 17:25:22），只有 7 条 check 名称、没有逐项证据；#370（`8e83f22`）合入的是就绪探针。`switch_binary.sh` 自 #296 P1-3 起在 `--execute` 时写机器报告（`--report <path>`，默认 `reports/<short-tag>-switch-<UTC>.json`，含 P 与 S1–S7 逐项 `{step,status,elapsed_ms,evidence}`、探针 JSON 与回滚结果），preview.35 那份仍是人工整理的 | preview.33/34 的 S1–S6 逐项结果永久不可复核，只能引用可观测结果（链接 mtime、单元 uptime、live version）；下一次切换起有机器产出的逐项报告 | `ls -l reports/`、`cat reports/preview35-switch-20261024.json`、`scripts/switch_binary.sh`、`scripts/test_switch_binary.py` |
| 任务簿记（`frozen_work_json`、`requests.command_json`、`shared_evidence`）每年约增长 10–16GB（每天约 106 个任务） | 只换二进制的长期运行会逼近「部署体积 ≤8GiB（目录口径，2026-10-26 签字）」；降低需要改存储格式并定留存策略 | S7 |
| `market_events` 事件日志无界增长（RULINGS Q9 已接受）：估计 ~5,224 行/日 ≈ 1MB/日（S5 实测 26,118 行 / dbstat ~16MB 含索引） | 长期运行累积；与任务簿记增长分开记录 | RULINGS Q9、S5 |
| **schema 20 历史**（现行 schema 21 无此机制）：rollover 校验放宽后，回滚到旧二进制时，其对早 rollover 结果行的完整校验会报「requires depth-256 base」；已有 >127 深的链升级后首日会集中 rollover 一次 | 只影响仍运行 schema 20 的旧库/旧二进制回滚 | XP-051；`grep -rn rollover src/` 无命中，`src/sqlite_market/manifest.rs` 只服务 legacy backend（`src/sqlite_market/backend.rs:31-51`） |
| **schema 20 历史**（现行 schema 21 无版本链/链深）：年线最坏链深余量薄（约 2–9%），宿主为 t3.small，噪声约 ±10%，冷缓存首批明显偏慢 | 仅适用于 schema 20 版本链；schema 21 年线按当前日线序列重派生 | XP-045b/047/048/052；`src/sqlite_market/backend.rs:31-51` |
| aws `acquire.sqlite` 中有 10 个 evidence_only 代现已无代字节（2026-10-24T17:55Z 实测各约 4KB：868f4fa1、804c5a63、04c28d88、84b3f15d、c360967a、2e918330、a49c314b、87322997、2d5851a4、a119629b）。其中 `2d5851a4`（16:57Z）与 `a119629b`（17:52Z）是退役时才清掉字节；其余 8 个在更早退役时就已无字节 | 采集台账有残留；这些代已是 evidence_only，不计入热容量，但目录与清单仍占少量空间 | `du -sb generations/generations/*`、`generations/acquire.sqlite` |
| `/opt/x-data-mcp-preview/mcp.json` 的 `x-data-mcp-demo` 指向一个不存在的 sqlite | 配置过时 | `ls` |
| `src/acquire/fetch.rs` 在未设置 `X_DATA_MCP_TDX_FETCH` 时，回退到编译期的 `CARGO_MANIFEST_DIR` | 历史：preview.35 及更早不带该变量启动的实例无法下载。main 已修（#375）：未设置时只认包内 `<exe dir>/../scripts/tdx_fetch.py`，找不到即启动失败并报出变量名与路径；preview.36 构建自 `fcb86ab`（含 #375），本轮未实测该回退路径 | 源码、#375 |
| 生产服务、私有性能栈和多个 agent 会话共用 1 个物理核和 1.9GiB 内存；`/tmp` 是内存盘 | 2026-10-10 约 04:34Z 过载重启；测量结果有噪声 | 运维观察 |
| 热 generation 上限 6：超限后 `acquire_generation` 被拒（`generation_capacity`）。2026-10-24 实测路径：16:56Z 为 6/6（1 active + 5 superseded）→ 16:57:16Z 退役 `2d5851a4…` 降到 5/6 → 16:57:26Z acquire 被受理 → 17:01:34Z 新代 `c184e9c4` 激活，`9d9e38d7` 转为 superseded，**回到 6/6** → 17:52Z 再退役 `a119629b…`，**当前 5/6**，下一次 acquire 无需再手工腾位。容量是**计数**，但退役一个有字节的代同时释放空间（`2d5851a4` 约 546MB、`a119629b` 约 574MB）。自动保留策略（保留最新 2 个 superseded）**已随 preview.35 部署**（2026-10-24T17:25Z），在 activate 提交成功后生效 | 跨代日更会消耗热代名额；当前 5/6，activate 后由自动保留回收到约 4/6 | 2026-10-13 / 2026-10-24 实测（`generations/acquire.sqlite`、`du -sh`、`df /`）、`src/acquire/service.rs:43,925,1058-1084` |
| Skill 日更依赖 AI 会话逐页枚举目录实体（页大小受限、随目录规模线性增长；5,228 实体实测 27 分钟未提交一条更新） | 有更新那天的同步在 Skill 口径下不可用；已改由程序化驱动完成——批量更新工具 `plan_market_update`/`run_market_update` 自 `generic-data-v7`（preview.33）起可用，preview.36 与 preview.37 日更即走该路径（preview.37 本轮 84s） | 2026-10-13 首次增量实测 |
| 历史：AGENTS.md 的环境段、README「当前状态」、`docs/daily-sync.md`「当前状态」曾写 ecs-node / preview.29 / generation `2e9183…` | 已修：#421 起 README 与 daily-sync 的当前态只链接本文件（本节为权威值），AGENTS.md 的 ecs-node / preview.29 段标为「历史」 | #421、本次核对 |

已关闭：#324（stopped-write 重写 `outcome_json`，随 PR #323 修复）；受理体检扫描全部历史（随 PR #323 修复，2026-10-12 部署后生产已包含）。

## 维护规则

1. **谁更新**：部署或迁移的执行者，在记录本次部署的同一个 PR 内更新。
2. **其他文档**：README 只保留「项目是什么」并链接本文件；AGENTS.md 的环境段只写开发约束，主机事实链接本文件。旧段落的清理由后续 PR 完成。
3. **格式约定（机器校验用，P1-2/#296）。** 同一事实只在一处给权威值，其余位置只复述：
   - 热 generation 计数的权威行是「数据」表的「热 generation」行（`N/M`），且必须与同一行的 `active` + `superseded`
     拆分自洽；其他位置的当前态复述必须与它一致。历史段、时间线（含 `→`）与预期值（「约」「预期」）不受约束。
   - 「服务」表的「工具数」列是 data / acquire 工具数的权威值，必须等于源码注册表
     （`src/data_supply/registry.rs` 的 `entries()`、`src/acquire/mcp.rs` 的 `ENTRIES`）。
   - 「部署版本」行是当前部署版本的权威值；其他当前态断言（含 `serverInfo.version`）必须与它一致。
   - 「数据库 schema」行是已部署 schema 的权威值（写成 `已部署：schema N`）。代码里的
     `COMPACT_SCHEMA_VERSION` 允许领先（未部署），其他当前态复述必须与已部署值一致。
   - `docs/contracts/*.md` 是历史快照：文件名不得含 `current`/`当前`，头部必须有 `**状态**：` 行且不得自称当前契约。
   - `docs/decisions.md` 的状态列只能取文件开头声明的取值。
4. **CI 检查现状（2026-10-26 复核，P1-2/#296）。** `scripts/check_current_facts.py` 检查构建事实（包版本、两套 schema、
   data/acquire 工具数、capability revision）、5 个 `CURRENT_DOCS` 当前区域、P1-1 校正过的 7 个 `CURRENT_REGIONS`
   当前区域、`docs/contracts/*.md` 快照、`docs/decisions.md` 状态列，以及本文件的内部一致性（热 generation 计数与拆分、
   data/acquire 工具数、部署版本）。`scripts/check_deploy_facts.py` 的覆盖情况：
   - 「本文件声明的版本须与最近一份已提交的记录一致」：**未实现**。目前只校验版本串形式（ `vX.Y.Z-preview.N` ）。
     preview.35 的报告在 `/opt/x-data-mcp-preview/reports/` 内、不进仓库，且是切换后人工整理的（#296 P1-3 之前
     `switch_binary.sh` 不写报告；现在 `--execute` 会写 `reports/<short-tag>-switch-<UTC>.json`）。就绪修复已随
     #370（`8e83f22`）合入 main；要实现这条检查，仍需先把报告是否随仓库提交定下来。
   - `CURRENT_DOCS` 的当前区域不得把已退役主机写成当前主机：**已实现**（`check_current_host`）。
   - 「最后核实」日期超过 14 天时告警：**已实现**（`STALE_DAYS = 14`）。
