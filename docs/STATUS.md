# 当前状态（STATUS）

本文件是**部署与运行事实**的唯一入口。README、AGENTS.md、`docs/daily-sync.md`
等文档只链接本文件，不再复述版本、主机、generation 或数据量。性能与容量目标见
[SLO](SLO.md)。

每次部署、迁移或 generation 激活后，执行者在同一个 PR 内更新本文件。

**两台主机（D-74，2026-11-05 owner 裁定）**：日更和主数据的生产机是 **vm2-126**
（preview.47，schema 27，`generic-data-v11`，无 tunnel）；**aws-node** 保留 tunnel 与
fallback（preview.43，schema 26，`generic-data-v9`，数据冻结、不再跑日更），既有客户端仍连
aws-node 的 tunnel；新客户端可经 ssh + `mcp_connect` 连 vm2 的只读 `research` 入口（[客户端连接](client-connect.md)）。
vm2 是试用机（约 2027-01-04 到期，到期前要定去向）。

最后核实：2026-11-09（VM2-11 日更复核与 VM2-28 preview.47 部署验收，两份报告在仓库外，均经 Claude 审核）：
11-09 生产跨代日更（`2731a664…`→`afab103b…`，08:49:24Z 激活）、8 个 task 与 write-loop、当日一次重复 `full`
与 41.0 / 30.6 min 的 G2 分组读数、「2026-11-09T10:45Z preview.47 部署（r3 PASS，停服约 0.6 s）」与部署后只读验收。
更早的 2026-11-08 VM2 只读查询、2026-11-07 preview.46 只读验收（XDM-198）、2026-11-05 部署与第一次生产跨代日更读数
保留在「数据」段。

**preview.47 已部署（2026-11-09T10:45Z）**：vm2-126 已切到 `v0.2.0-preview.47` /
`de1f84a503e44005183f0998b01f3b8ac7989566`（switch 报告
`/opt/x-data-mcp-preview/reports/preview47-switch-20261109T104521Z.json`：`status=PASS`、
`rollback_outcome=not_needed`、`previous_release=c35b425f…`；P/S1–S7 全 PASS，S2 私有 smoke 32,840 ms，
S4+S5+S6 停服窗口约 **0.6 s**）。相对 preview.46（`c35b425`）含三处代码改动：#596 ETF 简称提取器 v2
（`etf-short-name-v2`）、#601 bind 之前安装 SIGTERM/SIGINT 处理器（Fixes #599）、#606 D-87 `tail` 复用
窗口类来源的已关闭窗口（Refs #604）。schema 仍为 27，capability revision 仍为 `generic-data-v11`，工具数不变
（ops 16 / research 17 / maint 9 / acquire 10），只换二进制。**D-87 的 `tail` 复用已随本版部署，生产实测留待
2026-11-10 日更**（本次部署刻意未跑 `tail`，见「已知问题」）。

**历史：preview.47 待部署（2026-11-08）**：当时的 PR 只做版本号与文档，未打 tag、未发 release、未部署。

**代码面已到 `generic-data-v12`（未部署，2026-11-08）**：#607 PR-D 把
`market.daily` + ETF + `adjustment=qfq` 从「必然 `adjustment_unavailable`」改成按实体核对门槛
放行或带 `details.reason` 阻断（D-89/D-92），`get_data_capabilities` 新增 ETF `qfq` 的
`series` 行与 `algorithm_versions`（`qfq`、`etf_factor_checks`），放行的响应带
`etf_checks_digest`。schema 仍为 27，工具数与工具签名不变，只换二进制。**部署面仍是
`generic-data-v11`**；部署顺序与部署前的影响见 [generic-data-v12 §7](contracts/generic-data-v12.md)
（先部署 → 全部 ETF `qfq` 报 `etf_factor_unverified`，owner 批准导入核对记录后放行，不需要重启）。

**历史：preview.46 已部署（2026-11-07T16:52Z）**：vm2-126 已切到 `v0.2.0-preview.46` /
`c35b425f727fa74ad2cca0f6c9c683213e256b21`（switch 报告
`/opt/x-data-mcp-preview/reports/preview46-switch-20261107T165152Z.json`：`status=PASS`、
`rollback_outcome=not_needed`、`previous_release=f6de633d…`）。内存上限生效值
`MemoryHigh=1G`（1,073,741,824 B）/`MemoryMax=1536M`（1,610,612,736 B）/`MemorySwapMax=0`
（`systemctl show` 读回），启动日志：
`read memory budget: refuses new reads above 870 MiB (85% of the cgroup limit 1024 MiB:
memory.high 1024 MiB, memory.max 1536 MiB)`。`MemorySwapMax=0` 时超过 `MemoryMax`
会被 cgroup OOM 杀掉 data 服务，不是只拒绝读取；1.9 GiB 机器的档位（`448M`/`640M`/`256M`）仍未实测。

本轮实测来源：

- VM2-11 报告（2026-11-09T08:44–10:30Z，vm2-126 生产日更复核，报告在仓库外，经 Claude 审核）：11-09 日更的
  generation 切换、8 个 task 的项数与 write-loop、3 条偏差（轮询误判、无间隔轮询、重复 `full`）、
  `replay --verify-only` 的沪 0 漂移 / 深派生存量旧读数、备份与 `check_daily_run.py` 复跑读数。
  本节标为「VM2-11」的行全部照该报告；数字一律不从报告以外推算。
- VM2-28 报告（2026-11-09T10:33–10:50Z，vm2-126 preview.47 只换二进制部署与部署后只读验收，报告在仓库外，
  经 Claude 审核）：r1（S1 失败）、r2（S4 失败）与 **r3 PASS** 三次尝试、部署后版本/单元/工具数/读面冒烟。
  部署本身来自 `reports/preview47-switch-20261109T104521Z.json`（`status=PASS`）。本节标为「VM2-28」的行照该报告。
- Claude 2026-11-05 只读核实的部署事实（查询与日志在仓库外；owner 同日只做了 D-74 裁定，
  未核实这些数字）：vm2-126 的版本、schema、revision、工具数、active generation、
  `market_series`/`series_bars`/库体积、ETF 缺口、generations 克隆来源、部署窗口/构建/
  WAL/capability/回滚点读数，以及 aws-node 的冻结时点读数，均照该核实记录；
  仍未列出的项（catalog id、官方输入、内存分项）下文标注「本轮未列出」。
- Claude 2026-11-05 只读核实的**第一次生产日更**（vm2-126，跨代）：active generation 切到
  `c642468f…`（2026-11-05T08:40:27Z）、各任务结果与 write-loop、热 generation、
  `market_series`/`series_bars`/库体积/`/opt` 体积，见「数据」段的 vm2 表与
  「2026-11-05 vm2 第一次生产日更」小节；任务记录与查询在仓库外，owner 未复核这些数字。
- VM2 只读查询（2026-11-08，`python3 sqlite3` 的 `file:…?mode=ro`，含 WAL，不停服务不写库）核实的
  **第二次生产跨代日更**（vm2-126，跨代）：active generation 切到 `2731a664…`
  （2026-11-06T08:49:30Z，上一代 `c642468f…`，激活请求 `daily-sync-20261106-activate-g2pqjr`）、
  当天 8 个 task 的项数/结果分类/write-loop、当日 `tail`（3 段，无 `full`）、11-07 无日更记录，
  见「数据」段的 vm2 表与「2026-11-06 vm2 第二次生产日更」小节；SQL 与原始输出摘要见本任务的 PR 正文，
  owner 未复核这些数字。
- XDM-198 报告（UTC 2026-11-07T16:52–17:05Z，vm2-126 只读验收，报告在仓库外）：preview.46 的版本/单元/
  内存档位、`source_store replay --verify-only` 对库快照的比较、`export_research --dry-run` 预检与读面冒烟。
  部署本身来自 `reports/preview46-switch-20261107T165152Z.json`（`status=PASS`）；该报告与本节
  标为「XDM-198」的行是本轮新增事实。
- 2026-11-05 vm2 单次实测（preview.45、同代数据、非生产日更样本）：首采/年线/日更形态/
  ETF 批次的 write-loop 与资源读数，全文见 [SLO](SLO.md) 备注。
- 历史来源（aws-node preview.43 时期，2026-11-02 实测）：
  `/opt/x-data-mcp-preview/reports/preview42-phaseg-deployment.json`（`deploy_phase_g.sh
  v0.2.0-preview.42 --force-git-transport`，`status=DEPLOYED`、`before.schema=25`、
  `source_store_replay.status=ok`、逐 socket 就绪探针）。
- XDM-165 报告（preview.42 发布/部署/重建/验收，报告在仓库外）：部署窗口
  2026-11-02T06:04:22→06:04:42Z、重建耗时、`MemAvailable`/WAL 采样、豁免记录。
- `/opt/x-data-mcp-preview/reports/preview43-switch-20261102T104817Z.json`（`switch_binary.sh
  v0.2.0-preview.43 c0ae295… --execute`，`status=PASS`、`rollback_outcome=not_needed`、P/S1–S7
  逐项耗时与四入口就绪探针）与 XDM-170 报告（preview.43 发布/只换二进制/人工 smoke，报告在仓库外）。
- 历史：`reports/preview41-switch-20261101T101825Z.json`（preview.41 只换二进制）与 XDM-159 报告。
- `/opt/x-data-mcp-preview/reports/preview40-switch-20261101T003814Z.json` 与
  `preview39-phaseg-deployment.json`
  （历史对照：preview.40 只换二进制、preview.39 fresh init）。
- `python3` 的 `sqlite3`（`file:…?mode=ro`，**含 WAL**：未 checkpoint 的提交只在 `-wal` 里，
  读 `immutable=1` 会看到 checkpoint 前的旧计数）读 `state/preview.sqlite`（schema 25）与
  `generations/acquire.sqlite`；来源库清单 `state/source-store/manifest.jsonl`。
- `readlink`/`stat`/`ls -l`/`du -sb`/`df -h`/`free -m`、`systemctl show -p ActiveState`/
  `systemctl show -p ExecStart --value`、`current/manifest.json`。
- XDM-146 报告（旧代退役 gate 的副本实测、`incon.dat` 检查，报告在仓库外）与 XDM-139 的日更任务记录
  （`tasks`/`task_items` 只读查询）。

preview.40 及更早的实测值保留为「历史」对照；本轮未重新测量的行（主机虚拟化、本机工具链、官方输入
`curl -sI` 比对）标注为「历史」。部署与重建在生产机上执行但**非独占**，下述耗时不作门禁证据。
「推断」标记表示由实测数据推出、但没有独立测量的结论。

## 部署

### 两台主机总览（D-74，2026-11-05）

| 项 | vm2-126（日更和主数据） | aws-node（fallback + tunnel） |
|---|---|---|
| 角色 | 生产：日更与主数据 | fallback 与 tunnel/客户端入口；数据冻结、不再跑日更 |
| 版本 | `v0.2.0-preview.47`，git_sha `de1f84a503e44005183f0998b01f3b8ac7989566` | 历史（冻结）：`v0.2.0-preview.43`，git_sha `c0ae29500b56a754a5f2d3304b8584f551b94a89` |
| schema / capability | schema 27 / `generic-data-v11` | 历史（冻结）：schema 26 / `generic-data-v9` |
| active generation | `afab103be4a776a98587d05a9c83a04022295f65ce606ee2fb42e36d48d90569`（2026-11-09T08:49:24Z 激活，上一代 `2731a664…`；完整 sha、激活时间与上一代见「数据」段，来源 VM2-11）；最新行情日 2026-11-09 | `4c17de16…`（2026-11-04T10:19:18Z 激活，与 vm2 同一代）；数据停在 2026-11-04 |
| tunnel | 无（外部读路径是 ssh + `mcp_connect`，见[客户端连接](client-connect.md)） | tunnel/data/acquire 三单元仍 active；既有客户端仍连这里，看不到 ETF 与 11-05 之后的数据 |
| 到期 | 试用机，约 2027-01-04 到期，到期前要定去向 | — |

### vm2-126 明细（当前生产：日更和主数据）

| 项 | 事实 | 来源 |
|---|---|---|
| 主机 | vm2-126（试用机，约 2027-01-04 到期，到期前要定去向；D-74） | owner 2026-11-05 裁定 |
| 系统/资源 | Ubuntu 24.04.4，kernel 6.8.0-101-generic；2 vCPU AMD EPYC 9754，约 7.6 GiB；时区 Asia/Shanghai，systemd 单元用 `TZ=UTC` | Claude 2026-11-05 只读核实 |
| 部署版本 | `v0.2.0-preview.47`，二进制 `git_sha de1f84a503e44005183f0998b01f3b8ac7989566` | VM2-28 |
| 部署时间 | **2026-11-09T10:45Z**（switch 报告窗口 10:45:21→10:45:57Z，共 **36 s**），`scripts/switch_binary.sh v0.2.0-preview.47 de1f84a50… --execute`：`status=PASS`、`rollback_outcome=not_needed`、`previous_release=c35b425f…`、P/S1–S7 全 PASS（S1 1,885 ms、S2 私有 smoke 32,840 ms、S3 2 ms、S4 101 ms、S5 6 ms、S6 487 ms）。**停服 window = S4+S5+S6 ≈ 0.6 s**（S2 的 32.8 s 在生产 socket 之外，不占停服时间）。本次是第三次尝试：r1 10:33:38Z 因 `systemd-run` 环境里 `gh` 未认证失败在 S1，r2 10:38:33Z 因本机无 `x-data-mcp-tunnel.service` 失败在 S4（回滚后服务恢复），r3 用 `XDM_TUNNEL_SERVICE=xdm180-tunnel-stub.service` 占位单元一次通过；r1/r2 均未改动生产 | `reports/preview47-switch-20261109T104521Z.json`（r1/r2 为 `…T103338Z.json` / `…T103833Z.json`）、VM2-28 |
| 数据库 schema | 已部署：schema 27（026 = R2 的 `source_refresh_runs`/成员表与快照索引主键 `(snapshot_id, request_fingerprint)`，027 = M3-2 PR-B 把 `adjustment_events.category` 扩到 1/11 并加逐行 `basis_status`）；本次只换二进制，库未改。库 `state/preview.sqlite` **2,313,494,528 B**（2026-11-07 只读快照），WAL 当时 0 B；11-09 日更后体积本轮未列出 | XDM-198、`src/sqlite_tasks.rs` 的 `COMPACT_SCHEMA_VERSION` |
| cgroup 内存 | data 单元 `MemoryHigh=1G`（1,073,741,824 B）/`MemoryMax=1536M`（1,610,612,736 B）/`MemorySwapMax=0`；启动日志 `read memory budget: refuses new reads above 870 MiB (85% of the cgroup limit 1024 MiB: memory.high 1024 MiB, memory.max 1536 MiB)`。acquire 单元本轮读回三个值都是 `infinity`（本任务只要求核 data 的三个值） | XDM-198 |
| 构建 | 本轮未列出（preview.45 为 rustc 1.99.1 / `x86_64-unknown-linux-gnu`，历史）；features `export`、`production NOT_PRODUCTION_READY` 未重读 | — |
| 部署目录 | 本轮未列出（11-05 部署后 3.8 GB，历史） | — |
| 回滚点 | switch 报告的 `previous_release=c35b425f…`（preview.46）保留；同 schema 27，回滚只需换回二进制。r2 遗留的 `releases/de1f84a…` 已移到 `/root/x-data-mcp-backups/p47-r2-release-de1f84a-20261109T104439Z`（未删除），r3 重新装到 `releases/de1f84a…`。客户端本来就连 aws-node，vm2 出问题时 aws-node 的 preview.43 继续服务（数据停在 11-04） | VM2-28、D-74 |
| 迁移 | 来源库、generations、`acquire.sqlite`、行业快照目录从 aws-node 克隆；行情库在 vm2 上 fresh init（schema 27），再由 replay 和首采重建；aws-node 的库原地保留 | Claude 2026-11-05 只读核实 |
| capability | `get_data_capabilities` 读回 `capability_revision=generic-data-v11`、`production_ready=false`、`items=16` | VM2-28 |
| production_ready | `false`：owner 个人研究、本地使用，未授权对外分发（#71） | `current/manifest.json` 的 `production: NOT_PRODUCTION_READY` |

### 2026-11-09 preview.47 部署后只读验收（VM2-28）

来源为 VM2-28 报告（UTC 2026-11-09T10:33–10:50Z，只读；报告在仓库外，经 Claude 审核）。本次是**只换二进制**，
库、WAL、generations 均未动；未跑 `tail`、未跑写任务、未跑 `replay`、未改 systemd。

| 项 | 结果 |
|---|---|
| 版本与单元 | `0.2.0-preview.47` / `de1f84a…`；data、acquire、`xdm180-tunnel-stub` 均 `active`；data 的三项 cgroup 上限仍为 1G / 1536M / 0，启动日志有 `read memory budget: refuses new reads above 870 MiB (85% of the cgroup limit 1024 MiB: memory.high 1024 MiB, memory.max 1536 MiB)` |
| 能力与工具 | ops **16** / research **17** / maint **9** / acquire **10**（与 preview.46 一致，工具数不变）；`capability_revision=generic-data-v11`、`production_ready=false` |
| 读面冒烟 | `sh.600000` 日线 `isError=false`，**5 条** bar（2026-11-03→11-09）；`sh.510050` `adjustment=raw` **5 条**；`adjustment=qfq` → `isError=true`、`code=adjustment_unavailable`（fail-closed，与预期一致：ETF 前复权仍未交付） |
| #601 | `journalctl --since 10:45:00` 中无 killed/signal/OOM 记录（本次启动无被信号杀死记录） |
| 未做 | 第 9 步「不跑 `tail`」已遵守；D-87 的 `tail` 复用**待 2026-11-10 日更实测**（#604） |

### 2026-11-07 preview.46 部署后只读验收（XDM-198，历史）

来源为 XDM-198 报告（执行时间 UTC 2026-11-07T16:52–17:05Z，只读；报告在仓库外）。`source_store replay`
的 `--db` 路径会对库文件取排他 flock（`database lease`），服务在线时不能直接对生产库跑，本轮的比较
是在库的只读一致性快照上做的（见[新机器部署手册](new-host-runbook.md) §4/§6）。

| 项 | 结果 |
|---|---|
| 版本与单元 | `0.2.0-preview.46` / `c35b425…`；data、acquire 均 `active`/`running`；data 的三项 cgroup 上限见上表「cgroup 内存」行 |
| 只读重放 | exchange `match`（5,882）；security_names `drift`（ETF 名称 1,720 行尚未入库，预期）；suspensions `drift`/`inputs_changed`（预期）；risk_warnings `drift`/`rows_changed`（20/20 条为主键日期 +1–2 个交易日，D-84 R2；见「已知问题」） |
| 导出预检 | `export_research --dry-run`：`snapshot_id=snapshot-d6280a707ea1b079472eb81c884b2489`、`schema=x-data-mcp.research-snapshot.v5`、`coverage.delisted_series=337`、catalog `catalog-1793852554590130457`（5,225 成员）、14 files planned / 0 written |
| 能力与工具 | ops 16 / research 17 / maint 9 / acquire 10；`capability_revision=generic-data-v11`、`production_ready=false`、`items=16` |
| 读面冒烟 | `sh.600000` 7 根 bar；`sh.510050` 不带 `adjustment` → `adjustment_required_for_etf`，`raw` → 7 根 bar，`qfq` → `adjustment_unavailable`（fail-closed） |
| 未验证 | #533 的同主键 sha 差异（被 D-84 R2 的日期平移掩盖，需 11-09 日更后重跑）；suspensions 的 14 条行级 drift 未逐条归因；acquire 的 cgroup 内存；最新集中行情日与 `series_bars` 本轮未列出 |

### aws-node 明细（fallback + tunnel；preview.43 冻结，不再跑日更）

以下各行是 aws-node 在 2026-11-05 冻结时点的记录；标「历史」的行是更早对照值。
vm2 上线后，本节不再随部署更新，更新见上节与「数据」段的 vm2 表。

| 项 | 事实 | 来源 |
|---|---|---|
| 主机 | aws-node（EC2；systemd 启动日志记录 `Detected virtualization amazon`） | 运维记录、`journalctl` 启动段 |
| 系统 | Ubuntu 26.04 LTS，kernel 7.0.0-1014-aws | `/etc/os-release`、`uname -r` |
| 资源 | 2 vCPU（1 个物理核 × 2 超线程，Xeon Platinum 8259CL @ 2.50GHz；`/proc/cpuinfo` 无 `sha_ni`），MemTotal 1,905,872 kB（约 1.82GiB）+ 8GiB swap；本轮采样 used 1,278–1,350MB / available 554–626MB、swap used 3,668–3,705MB，根盘 77G（已用 54G、余 24G），`/tmp` 953MB tmpfs（已用 101MB） | `lscpu`、`grep -c processor /proc/cpuinfo`、`grep sha_ni /proc/cpuinfo`（无匹配）、`free -m`、`swapon --show`、`df -h / /tmp` |
| 部署版本（aws-node） | 历史（冻结）：`v0.2.0-preview.43`，二进制 `git_sha c0ae29500b56a754a5f2d3304b8584f551b94a89`（`current/manifest.json`：包 0.2.0-preview.43、`features [export]`、`production NOT_PRODUCTION_READY`、`target x86_64-unknown-linux-gnu`） | `current/manifest.json`、`readlink -f current` |
| 部署版本（历史 preview.42） | `v0.2.0-preview.42`，二进制 `git_sha 803b35cf7b17e13d6bc90d5b11b5b487bd013e65`（fresh init 到 schema 26；目录保留作回滚点） | `releases/803b35cf…`、XDM-165 报告 |
| 本机工具链 | 本机禁止本地 Rust 编译：执行 `rustc --version` 被主机策略拦截并返回「Local Rust compilation is forbidden on this host (1.9 GB RAM)」，仅允许 `cargo fmt`。因此本机工具链版本未复核；部署二进制的构建版本见「构建」行 | 实测拦截提示 |
| 部署版本（历史 preview.41） | `v0.2.0-preview.41`，`git_sha 98a4dbe2a6ca4c44233da0d7fcf7bcd8f65a88a1`（只换二进制，schema 仍 25；目录保留作回滚点） | `releases/98a4dbe…`、XDM-159 报告 |
| 部署版本的 tag/release 对应（aws-node，历史冻结） | tag `v0.2.0-preview.43`、GitHub release（prerelease，assets `*-linux-x86_64.tar.gz` + `.sha256` + `smoke-report.json`）与 `current/manifest.json` 三者指向同一个提交 `c0ae295…`（main #504 squash）；资产来自 workflow_dispatch run **41430776287**（headSha `c0ae295…`），tarball sha256 `75ae07cc0f920c4471e3175cda32d04a78f8f35bd12600fe4cab4582d5945909`，包内 `manifest.git_sha=c0ae295…` 与 release 下载复核一致；本机 `scripts/smoke_release.py` 14 项 PASS（33.1 s，`binary_sha256=70b6d3c1…`，与 CI 一致）。release 创建触发的 `Release` workflow run 41431474101 在 build 步骤被取消（上传/发布步骤 skipped），未覆盖资产 | `git ls-remote refs/tags/v0.2.0-preview.43`、`gh release view v0.2.0-preview.43`、`gh run view 41430776287`、下载后 `sha256sum -c` 与包内 `manifest.json` |
| 部署版本的 tag/release 对应（历史 preview.42） | tag `v0.2.0-preview.42`、GitHub release（prerelease，assets `*-linux-x86_64.tar.gz` + `.sha256` + `smoke-report.json`）与 `current/manifest.json` 三者指向同一个提交 `803b35c…`（main #501 squash）；资产来自 workflow_dispatch run **41396601977**（headSha `803b35c…`），tarball sha256 `a855697622954d8c5c374af61a95a6188f3a9617767d3dce279a6571b578c3ba`，包内 `manifest.git_sha=803b35c…` 与 release 下载复核一致；本机 `scripts/smoke_release.py` 14 项 PASS（34.3 s，`binary_sha256=61564de0…`，与 CI 一致）。release 创建触发的 `Release` workflow run 41397022938 在上传步骤前被取消，未覆盖资产；`refs/dist/v0.2.0-preview.42`（`eb4be455…`）绑定同一 tag_sha + tarball sha256 | `git ls-remote refs/tags/v0.2.0-preview.42`、`gh release view v0.2.0-preview.42`、`gh run view 41396601977`、下载后 `sha256sum -c` 与包内 `manifest.json` |
| 部署版本的 tag/release 对应（历史 preview.41） | tag `v0.2.0-preview.41` → `98a4dbe…`，run 41296420664，tarball sha256 `39097de5…`；`Release` run 41296695105 已在上传前取消 | 历史行，XDM-159 报告 |
| 部署时间 | **2026-11-02T10:48:17→10:49:14Z**，`scripts/switch_binary.sh v0.2.0-preview.43 c0ae295… --execute`：P 291 ms、S1 5,749 ms、S2 32,994 ms（私有 smoke）、S3 5,545 ms、S4 6,515 ms、S5 11 ms、S6 5,115 ms、S7 42 ms；`status=PASS`、`previous_release=803b35cf…`、`current_release=c0ae295…`、`rollback_outcome=not_needed`。就绪探针：research 4,273 ms / 5、ops 25 ms / 16、maint 14 ms / 9、acquire 15 ms / 10。**只换二进制**：库、WAL、generations 不动，`user_version` 仍 **26**。停机：data/acquire/tunnel 全 inactive 10:49:03→10:49:09（**6 s**），经隧道的外部面 10:49:03→10:49:14（11 s） | `reports/preview43-switch-20261102T104817Z.json`、`journalctl` 启动段 |
| 部署时间（历史 preview.42 fresh init） | **2026-11-02T06:04:22→06:04:42Z**（全程 **20 s**），`scripts/deploy_phase_g.sh v0.2.0-preview.42 --force-git-transport`：停 tunnel→acquire→data → 归档 schema 25 库为 `state/preview.sqlite.v25-archived-20261102T060432Z` → `task_sqlite init21` 全新 schema 26 库 → `source_store replay --scope all`（`status=ok`：industry 7 快照 / 5,235 assignments、exchange 1,836、generation_facts 12、risk_warnings 1,289、security_names 7,344，五个 scope 全部 `database=match`）→ 切 `current` → 起 data→acquire→tunnel。就绪探针：research 219ms / 5、ops 23ms / 16、maint 16ms / 9、acquire 10ms / 10。**fresh init，不原地升级**；`user_version` = **26** | `reports/preview42-phaseg-deployment.json`、XDM-165 报告、`journalctl` 启动段 |
| 部署时间（历史 preview.41） | 2026-11-01T10:18:25→10:18:55Z，`scripts/switch_binary.sh v0.2.0-preview.41 98a4dbe… --execute`，P/S1–S7 全 PASS，停机约 3 s；只换二进制，schema 仍 25。该报告的 S6 只探了 `ops`（XDM-160 已修），research 5 / maint 9 由 XDM-159 手工探针核对 | `reports/preview41-switch-20261101T101825Z.json` |
| 部署时间（历史 preview.40） | 2026-11-01T00:38:14→00:38:55Z，`scripts/switch_binary.sh --execute`：P/S1–S7 全 PASS，停机约 3 s（00:38:52→00:38:55Z），探针 `ops` 15 / `research` 5 / `maint` 9、acquire 10。只换二进制，schema 仍 25。同窗口的第一次运行失败在 S2（私有 smoke 退出非 0，`rollback_outcome: not_needed`）。当时部署的二进制是 PR 分支提交 `441185b` 经 workflow_dispatch 构建的包，release tag `v0.2.0-preview.40` 指向 main `7a48749`，`git diff --stat 441185b 7a48749` 仅 `docs/tasks/issue-446-m3-delisted-industry.md` 不同（1 file changed, 189 insertions(+)） | `preview40-switch-20261101T003814Z.json`、`preview40-switch-20261101T003800Z.json` |
| 维护窗口（历史 preview.41 退役） | 2026-11-01T10:25:19→10:26:06Z（**47 s**，含 tunnel 临时停服），三单元全 inactive → 全 active；未改任何 systemd 单元。窗口内先做 `source_store replay --scope all --verify-only` 基线，再 `register-generation-member` + `replay --scope generation_facts` 重建 `generation_facts`（18 行），再经 acquire MCP `retire_generation` 退役两代 | XDM-159 报告、`retire_exemptions`/`generation_facts` 只读查询 |
| preview.40 及更早 | 历史：preview.40 @ `441185bb3dcaf5bfa8a821db7914f2c38a39e379`（目录保留作回滚点）、preview.39 @ `8173991d6780032c43038bc1922bf6a042a4b904`（2026-10-31 phase-g fresh init）、preview.38 @ `6ac5c963fdb6fb2e0f20b3963faf1fcfdf9bb3a5`、preview.37 @ `49422db26be2da79ff39c7522897d25fcb6055bb`、preview.36 @ `fcb86ab065e87f9ef7655fd87b8e224517bd8f1b` | 各自部署报告与 `releases/` 目录 |
| 部署时间（历史 preview.39） | 2026-10-31T18:27:55Z（data/acquire 同时 stop）→ 18:28:07Z 起单元，`scripts/deploy_phase_g.sh`：归档 schema 24 库到 `state/preview.sqlite.v24-archived-20261031T182806Z` → 从 release 提交读期望 schema 后 `task_sqlite init` 全新 schema 25 库 → 起两个单元 → 来源库重放（`--scope all`：industry `ok`，7 快照 / 5,235 assignments / 7 boundaries / 9 entries，其余 scope `store_empty`）→ 写部署报告（`status: DEPLOYED`、`before.schema=24`、`previous_release=releases/6ac5c963…`、`production: NOT_PRODUCTION_READY`）。**fresh init，不做原地升级**；停机 ≈12 s。部署后当日晚间重建（catalog + 5,223 实体日线 + 5,223 年线 + 338 条 listed-ever 序列）见「数据」段 | `preview39-phaseg-deployment.json`、`journalctl -u x-data-mcp-data/-acquire`、`releases/`/`state/` 目录 |
| 部署时间（历史 preview.37） | 2026-10-28T00:21:26Z（UTC），`scripts/deploy_phase_g.sh --force-git-transport`：停 data/acquire → 归档旧 schema 24 库 → 从 release 提交读取期望 schema 后 `task_sqlite init` 全新库 → 起两个单元 → socket/工具数/schema 检查 → 写部署报告 `/opt/x-data-mcp-preview/reports/preview37-phaseg-deployment.json`（`status: DEPLOYED`，`before.schema=24`，`production: NOT_PRODUCTION_READY`）。**fresh init，不做原地升级**；部署窗口的设置：`X_DATA_MCP_TEXT_MODE=full`、`X_DATA_MCP_RUN_PREPARE_WORKERS=1`（v2 日更的准备线程串行，放开条件见 [M2 部署清单](tasks/m2-deploy-checklist.md) §1，待独占实测） | 部署报告 JSON、`journalctl -u x-data-mcp-data/-acquire`、`readlink`/`stat` |
| 部署时间（历史 preview.36） | 2026-10-27T12:15:43Z，`scripts/deploy_phase_g.sh --force-git-transport`（本机 gh 2.62 读不到 release asset digest，改走 `refs/dist` 的 git transport，与历次一致）：停 data/acquire → 归档 schema 21 库 → `task_sqlite init21` 全新 init 到 schema 24 → 起两个单元 → socket/工具数/schema 检查 → 写报告 `reports/preview36-phaseg-deployment.json`。每个已部署的发布目录按完整 SHA 命名，旧目录保留作回滚点。本次是全新库部署，不是热切换 | 部署报告 JSON、`readlink`/`stat`、`journalctl` |
| 数据库 schema（aws-node，历史冻结） | 历史（冻结）：schema 26（2026-11-02 的 phase-g 部署从 release 提交读期望 schema 后 fresh init；`pragma user_version` 读回 **26**，`journal_mode=wal`）；aws-node 上代码 schema 27 未部署，vm2-126 已部署 schema 27（见上节「vm2-126 明细」；026 为 R2 的 `source_refresh_runs`/成员表与快照索引主键 `(snapshot_id, request_fingerprint)`，027 为 M3-2 PR-B 把 `adjustment_events.category` 扩到 1/11 并加逐行 `basis_status`；M1 的 022 加口径/精度列、双序列写入、因子事件表与派生数据集版本登记、指数序列与逐指数口径元数据、交易日历、日更内部派生 item、研究快照导出；M2 的 023 建证券主表/代码迁移/简称历史/股本事件/风险警示区间/停牌区间与两张规则表并扩展 `derived_dataset_versions.dataset_kind`，024 重建 `capital_events` 把 `category` CHECK 扩到 2/3/5/7/8/9/10；M3 的 `025_m3_index_tables.sql` 建来源库 5 张索引表 `industry_snapshots`/`industry_assignments`/`exchange_source_snapshots`/`generation_facts`/`retire_exemptions` 并给 `price_limit_rules` 加可空列 `min_band_ticks`） | `pragma user_version`（只读）、`src/sqlite_tasks.rs` 的 `COMPACT_SCHEMA_VERSION` 常量、phase-g 报告 `preview39-phaseg-deployment.json`、#383/#391/#446 设计段 |
| 构建 | rustc 1.99.1 (58cf8f9c3 2026-10-13)，ubuntu-24.04，最低 glibc 2.39 | `current/manifest.json` |
| 部署目录 | `/opt/x-data-mcp-preview/current` → `releases/c0ae29500b56a754a5f2d3304b8584f551b94a89`；7 个发布目录共 **506,960,939 B**：`c0ae295`（当前，77,130,067 B）、`803b35c`（preview.42，77,188,834 B）、`98a4dbe`（preview.41，75,490,085 B）、`441185b`（preview.40，75,083,365 B）、`8173991`（preview.39，75,178,293 B）、`6ac5c963`（preview.38，63,501,494 B）、`49422db`（preview.37，63,388,801 B）；未删任何目录 | `readlink -f current`、`ls -l releases/`、`du -sb releases` |
| 回滚点 | 回滚 preview.43→preview.42：**同一个 schema 26 库，只换回二进制，不换库**——停 tunnel→acquire→data，`current` 指回 `releases/803b35cf7b17e13d6bc90d5b11b5b487bd013e65`，再起 data→acquire→tunnel；等价命令 `scripts/switch_binary.sh v0.2.0-preview.42 803b35cf7b17e13d6bc90d5b11b5b487bd013e65 --execute`（会重新下载 release 资产并做私有 smoke）。再往回到 preview.41（`releases/98a4dbe…`，schema 25）：**必须同时换库**——停 tunnel→acquire→data，把 schema 26 库 `mv` 到 `state/preview.sqlite.v26-failed-<UTC>`（留取证，不 `rm`），再把 `state/preview.sqlite.v25-archived-20261102T060432Z`（2,013,822,976 B）`mv` 回 `state/preview.sqlite`，然后起 data→acquire→tunnel。只退二进制会因 schema 26 不可读拒绝启动（`check_schema` 对 `version > COMPACT_SCHEMA_VERSION` 直接拒绝）。再往回到 preview.38（`releases/6ac5c963…`，schema 24）：同样要换库（`state/preview.sqlite.v24-archived-20261031T182806Z`，1,878,384,640 B）并移除 acquire 的 `market-db.conf`（preview.38 的二进制不认 `--market-db`，见 [M3 设计](tasks/issue-446-m3-delisted-industry.md) §7A）。**退役不可逆**：各代 base/gbbq/hsjday 字节已删，回滚只恢复软硬件面 | `releases/`、`state/` 目录、`preview43-switch-20261102T104817Z.json`、`preview42-phaseg-deployment.json` 的 `archived_db`、XDM-159/XDM-165/XDM-170 报告 |
| 迁移 | 2026-10-09 从 ecs-node 迁到 aws-node；全新部署后重新采集，未迁移 DB | `/opt/x-data-mcp-preview/reports/migration-to-aws-20261009.json` |
| ecs-node | 服务已 stop 并 disable，数据保留为冷备（含 generation `a49c314b…` 的字节） | 同上 |
| production_ready | `false`：owner 个人研究、本地使用，未授权对外分发（#71） | `current/manifest.json` 的 `production: NOT_PRODUCTION_READY`（本轮未重新调用 `get_data_capabilities`） |

以下为 aws-node（fallback）的部署历史与 M2/M3 说明，均为历史记录：

历史：preview.38（2026-10-28 切换窗口，Op-B）只换二进制：schema 仍是 24、库不重建（沿用
preview.37 的 `state/preview.sqlite`），`capability_revision` 仍是 `generic-data-v8`，工具面不变。
那次变的是 data 单元的形状：**一个 `data_service` 进程服务三个 socket**——`run/mcp.sock`（`ops`，
编排入口，x 与日更 Skill 的连接不变）、`run/research.sock`（`research`，只读研究面）、
`run/maint.sock`（`maint`，仅本机 operator 用，不暴露隧道）。三入口的工具数为 ops 15、
research 5、maint 9（权威值见「服务」表）。文本模式 `full`、`--poll-ms 1000`、
`X_DATA_MCP_RUN_PREPARE_WORKERS=1` 都沿用；cgroup 上限
`MemoryHigh=384M`/`MemoryMax=640M`/`MemorySwapMax=512M` 按 XDM-84 的 w=1 独占测量设值
（报告在仓库外；`memory.conf` 删除后即回退默认无上限）。**隧道仍指 `run/mcp.sock`**
（Ops-D 未做），研究面外部行为不变。

preview.39（2026-10-31）是 M3 的 fresh init 上线：schema 24 → 25，库重建，两个单元各加一个
drop-in（data 的 `source-store.conf` 设 `X_DATA_MCP_SOURCE_STORE`；acquire 的 `market-db.conf` 给退役
gate 加只读 `--market-db`），工具面、`capability_revision` 与三 socket 形状不变。preview.40（2026-11-01）
只换二进制：schema、generation 与数据量当时不变，变的是行为——日更的 SSE 公告索引 URL 百分号编码
（#482）已在内。同日 04:21–05:49Z 的第 1 代日更（XDM-139）激活了新 generation
`f2fd6fcb…`，数据量随该轮更新（见「数据」段）。

preview.41（2026-11-01T10:18Z）是空季度 `pageCount=0` 的修复（#485）与 `risk_warnings` 解析口径
（#489）上线：只换二进制，schema 25、generation 与数据量不变，工具面、`capability_revision`、
三 socket 形状也不变（本窗口手工探针：ops 15 / research 5 / maint 9 / acquire 10）。同日 10:25–10:26Z
的 47 s 维护窗口把两个遗留代退役为 `evidence_only`（D-65，见「旧代回收」行），热代 4 → 2。
库里的 `risk_warnings` 仍为 0 行：修复随下次日更（11-02，XDM-139）才写入，见「数据」段。

preview.43（2026-11-02T10:48Z）是 D-68 沪市风险警示公告标题分类修复（#503）上线：只换二进制，
schema 26、`generic-data-v9`、工具面（ops 16 / research 5 / maint 9 / acquire 10）与数据量都不变，
库、WAL、generations 未动（XDM-170）。`derived:risk_warnings` 尚未重跑；11-03 日更会按 D-68
首次发布沪市区间（见「数据」段）。

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
一起退（只退二进制会因 schema 不匹配拒绝启动）；**该归档库已不在 `state/`（已删），所以现在只剩「重新采集」一条路**。回滚到 preview.36 不需要换库（同为 schema 24），
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

M3（[#446](https://github.com/liqiangcc/x-data-mcp/issues/446)）PR-1 把迁移链推到
schema 25（`migrations/tasks/025_m3_index_tables.sql`）：只建来源库的 5 张索引表
（`industry_snapshots`/`industry_assignments`/`exchange_source_snapshots`/
`generation_facts`/`retire_exemptions`）并给 `price_limit_rules` 追加可空列
`min_band_ticks`；该 schema **已随 preview.39 部署**（2026-10-31 fresh init，生产库为 25；代码已到 26，R2 的 026 未部署）。
设计见 [M3 设计](tasks/issue-446-m3-delisted-industry.md) §9。

M2 随 preview.37 部署（PR #412/#415/#419/#420/#422/#424/#427/#428/#425，CI 绿）。六个
解析事实数据集 `market.security_master`、`market.security_names`、`market.capital_events`、
`market.risk_warnings`、`market.suspensions`、`market.price_limit_rules` 在 v8 代码上可读
（`contract_version:"v2"` + `filter.kind:"daily"`，未登记版本返回 `manifest.status=absent` 与
0 行）。写入路径不变；下表右列是本轮（2026-11-01，第 1 代日更后）的库内实测值，M3 的
PR-6/8/10/12 与 A-1/A-2、A-3、S-6 已把大部分 `pending_source` 接上，剩余缺口照实写：

| 数据集 | 写入路径 | 2026-11-01 实测（只读计数） |
|---|---|---|
| `market.security_master` | 日更 run 的 `derived:security_master` item（TDX `base.dbf` + `infoharbor_spec.cfg` + `gbbq`，再叠加 A-1/A-2 交易所名单） | **5,565** 行；`listed_ever` 去重实体 **5,564**；`listing_date` unknown 0（`listing_source` exchange_list 5,559 / tdx_ssdate 6）；`delisting_date` 有值 **337**（来源全 `exchange_list`）；`listing_conflict` 2 / `delisting_conflict` 15；`issuer_resolution` code_date 5,564 / ambiguous 1；退市原因分类仍全 `unknown`（未接入）；`duplicates_collapsed` 2 组（`000020`、`200020`） |
| `market.capital_events` | 日线提交的同事务写入（`gbbq` 非 cat1 类别）；全量采集与日更都走这条路径 | **128,338** 行 / 5,558 实体；`category` 2: 4,497、3: 3,741、5: 112,063、7: 18、8: 146、9: 7,569、10: 304 |
| `market.suspensions` | 日更 run 的 `derived:suspensions` item（A-3 停复牌公告） | **70,626** 段（2026-11-02 重建 run）；历史（preview.41）：79,442 段 = `confirmed` 20,420 + `unresolved_gap` 59,022，另有 4 条 `deferred_beyond_calendar`，耗时 1,839,810 ms |
| `market.risk_warnings` | 日更 run 的 `derived:risk_warnings` item（#308 历史 ST 写入器） | **2,174 行**（2026-11-07 只读快照：沪 **884** `exchange_announcement` + 深 **1,290** `szse_name_history`）；`derived_dataset_versions` 的 `risk_warnings` 登记写于 **2026-11-06T09:36:32Z**（preview.45 日更），部署后尚无日更重写该表。**D-68** 分类修复已上线；preview.46 起 **D-84 R2** 把沪市事件生效日改为「公告日当天或其后第一个交易日的下一个交易日」（实施与撤销同规则）。历史：2026-11-02 fresh init 的 `source_store replay --scope risk_warnings` 写入 1,289 行（全部 `known_at=2026-11-01`，SSE 覆盖只到 2000Q1）；`derived:risk_warnings` 自 preview.42 首次 `full` 起 fail-closed（`sh.600734` 2022-01-29 的 `SameDayActionConflict`，旧规则把预审计说明误判为实施，由 D-68 修复）；第 1 代日更 0 行、`source_invalid: page_count is 0`（#485/#489 随 preview.41 部署） |
| `market.price_limit_rules` | 日更 run 的 `derived:trading_rules` item | **27** 行（`sh`/`sz` 主板、创业板、科创板；`basis_status` 全 `verified`，最早生效日 9,846）+ `min_tick_rules` **2** 行（交易所成对，`verified`） |
| `market.security_names` | 日更 run 的 `derived:security_names` item（S-6，D-61：仅深交所 `SSGSGMXX`/`tab2` 准入） | **7,344** 行：`interval` 4,261 / `unbounded` 3,083；覆盖窗口 3、`not_observed` 窗口 2；沪市/北交所仍 `NOT_AVAILABLE`；耗时 377,444 ms |

六个数据集由**日更 plan-run** 的内部 item 维护（`derived:security_master`、
`derived:risk_warnings`、`derived:suspensions`、`derived:trading_rules`，另加 M3 的
`derived:security_names`/`derived:industry_snapshot`/`derived:delisted_series_check`，见
[日更说明](daily-sync.md) 与 `src/task_control/derived.rs`）。`security_master`/`suspensions`/
`security_names`/`price_limit_rules` 各登记 1 个版本（`derived_dataset_versions` 共 **11,024** 行，其中
`capital_events` 5,558、`adjustment_events` 5,461 是 per-entity 版本）；`derived:risk_warnings` 与
`derived:security_names` 的登记由 2026-11-02 的 fresh init 重放写入（`derived_dataset_versions`
`('risk_warnings','a_share')`、`('security_names','a_share')` 各 1 行）。历史（第 1 代日更）：`derived:risk_warnings`
写 0 行、不登记版本，读取为 `manifest.status=absent`。研究快照导出
（`research-snapshot.v3`，含 `industry_assignments.parquet`/`code_migrations.parquet`）已随 M3 PR-9 上线。

**M2/M3 已知限制（本轮实测；括号里是历史 XDM-74 补测值）**：

- **退市日已由交易所名单给出**：`delisting_date` 有值的 **337** 行，来源全部 `exchange_list`
  （A-1/A-2 已接入）；其余 5,228 行 `unknown`。主表 **5,565 行 / 5,564 个实体**（`sh.600018` 有 2 行），
  catalog **5,225** 个成员：341 个主表实体不在 catalog，其中 338 个有日线序列（listed-ever 采集，见
  「数据」段）、3 个是迁移别名（`sh.601313`、`sz.000022`、`sz.000043`，无序列）。历史（2026-10-27）：
  只有 TDX 整理期口径 152 行、主表比 catalog 多 279。
  导出快照的成员仍带 `delisting_unknown` 标记（`src/research/universe.rs:508`），严格口径可据此排除。
- **代码复用/拆分已部分落地**：`code_migrations` **109** 行（108 `candidate` + 1 `confirmed`），
  `issuer_resolution` code_date 5,564 / ambiguous 1。历史（2026-10-27）：`600018` 只有 1 行（2006-10-26 起），
  `ambiguous_code_reuse=0`。
- **`market.suspensions` 按 `as_of` 返回**：只有 `as_of` 落在缺口区间内才返回该行
  （`row.covers(as_of)`，`src/sqlite_supply/m2.rs:150-157`）；按日期范围查时**必须给 `as_of`**，
  否则只会看到默认时点覆盖的缺口，范围内的其它缺口只在 `temporal.unknown_known_at_rows` 里计数。

## 服务

### vm2-126（当前生产：日更和主数据）

| systemd 单元 | 进程 | 入口 | 工具数 | 备注 |
|---|---|---|---|---|
| `x-data-mcp-data` | `data_service` | `run/mcp.sock`（`--entry ops`）、`run/research.sock`（`--entry research`）、`run/maint.sock`（`--entry maint`） | 16 / 17 / 9 | capability `generic-data-v11`（ops 16 / research 17 / maint 9 / acquire 10；VM2-28 2026-11-09 部署后只读探针，`get_data_capabilities` 读回 `generic-data-v11`；drop-in 与 socket 布局沿用 preview.38 起的单进程三 socket 形态，逐项配置本轮未核实） |
| `x-data-mcp-acquire` | `acquire_service` | `run/acquire.sock` | 10 | 同上 |
| `x-data-mcp-tunnel` | — | — | — | **无 tunnel**（D-74：tunnel 暂留 aws-node；客户端经 ssh + `mcp_connect` 连 `research.sock`，见[客户端连接](client-connect.md)） |

### aws-node（fallback + tunnel；preview.43 冻结）

以下为 aws-node 三单元在冻结时点的记录；tunnel 仍是既有客户端的入口，新客户端走 vm2 的 ssh 路径（见[客户端连接](client-connect.md)）。

| systemd 单元 | 进程 | 入口 | 工具数 | 备注 |
|---|---|---|---|---|
| `x-data-mcp-data`（aws-node，历史冻结） | `data_service`（单进程、三 socket） | `run/mcp.sock`（`--entry ops`）、`run/research.sock`（`--entry research`）、`run/maint.sock`（`--entry maint`） | 16 / 5 / 9 | 历史（aws-node 冻结，preview.43/schema 26/`generic-data-v9`；preview.42 实测 16 / 5 / 9，ops 的 `tools/list` 实测 **30,159 B** ≤ 30,800 B 预算。代码 allowlist ops 16、三入口去重 25）；6 个 drop-in 全列：`exec-start.conf`（三 socket `ExecStart`，尾部 `--poll-ms 1000`）、`generation.conf`（`X_DATA_MCP_GENERATION_ROOT=/opt/x-data-mcp-preview/generations`，并把该目录加进 `ReadWritePaths`）、`memory.conf`（`MemoryHigh=384M`/`MemoryMax=640M`/`MemorySwapMax=512M`，`systemctl show` 读回 402,653,184/671,088,640/536,870,912 B）、`prepare-workers.conf`（`X_DATA_MCP_RUN_PREPARE_WORKERS=1`，XDM-64/#429）、`source-store.conf`（`X_DATA_MCP_SOURCE_STORE=/opt/x-data-mcp-preview/state/source-store`）、`text-mode.conf`（`X_DATA_MCP_TEXT_MODE=full`）；`poll-interval.conf` 已不存在（`--poll-ms 1000` 由 `exec-start.conf` 带到）；`maint` 只在本机使用、不经隧道 |
| `x-data-mcp-acquire`（aws-node，历史冻结） | `acquire_service` | `run/acquire.sock` | 10 | 历史（aws-node 冻结）：单元级 `X_DATA_MCP_TDX_FETCH=/opt/x-data-mcp-preview/current/scripts/tdx_fetch.py`；drop-in `market-db.conf` 把 `ExecStart` 换成追加 `--market-db /opt/x-data-mcp-preview/state/preview.sqlite`（退役 gate 只读连接）并把 `state` 加进 `ReadWritePaths`；回滚到 preview.38 前必须移除它 |
| `x-data-mcp-tunnel`（aws-node） | OpenAI `tunnel-client`（profile `x-data-mcp`） | 转发到 `run/research.sock`（`--entry research`） | — | **2026-11-02 起（D-35 的 Ops-D 已执行，XDM-166 方案 (a)）**：profile 的 `mcp.commands[0].command` 与 drop-in 的 `ExecStartPre` 都指向 `run/research.sock`，drop-in 同时设 `TimeoutStartSec=180` 并用仓库探针 `current/scripts/wait_service_ready.py --entry research --expect-tool query_data`（原 `/opt` 旁路脚本 `wait-data-service.py` 退役）。启动摘要 `mcp_target_value` 读回 `mcp_connect …/run/research.sock`；远端工具面由 ops 的 16 项降到 research 的 5 项。回滚：两个文件改回（备份 `x-data-mcp.yaml.bak-20261102T060408Z`、`data-service.conf.bak-20261102T060408Z`）→ `daemon-reload` → 重启 tunnel。只转发 data server，不转发 acquire；drop-in 给该单元加上 `Wants=`/`After=x-data-mcp-data.service`，单元为 `Restart=always` / `RestartSec=10`。**2026-11-01 实测**：只停 data+acquire 的窗口里，tunnel 的 stdio 子进程随 data 退出、tunnel-client 请求重启，systemd 10 s 后重启 tunnel 并由 `Wants=` 把 data 拉起（10:22:19Z），所以停服窗口必须把 tunnel 一起停，且先停 tunnel、最后起它（`scripts/switch_binary.sh`/`deploy_phase_g.sh` 的停/起顺序与回滚路径已按此改，XDM-160）；历史（未复核）：`ExecStartPre` 用 `/opt/x-data-mcp-preview/wait-data-service.py`（该文件不在仓库内），就绪等待上限 120s（2026-10-24T16:56:58Z 由 10s 改为 120s；但单元未设 `TimeoutStartSec`，systemd 默认 90s 也覆盖 `ExecStartPre`，所以实际生效上限是 90s） |

启动日志（`journalctl -u x-data-mcp-data`，2026-11-02T06:04:39Z 那次启动；preview.39 起输出相同）：
`source store: /opt/x-data-mcp-preview/state/source-store`，
`exchange source outbound request interval: 1000 ms (per client; adjacent request starts are spaced by
this interval)`（XDM-143 的出站最小间隔）。三个 systemd 单元本轮实测都是 `active`：data 与 acquire 自
2026-11-02T06:04:39/06:04:40Z、tunnel 自 06:04:40Z（preview.42 部署窗口结束后启动；窗口前
三者均被 stop，`ActiveEnterTimestamp` 证明没有遗留重启）。

每次 `tools/call` 结束都会在 stderr 留一行 `XDM_CALL {json}`（`entry`/`tool`/`request_id`/
`task_id`/`outcome`/`elapsed_ms`；参数值与返回数据不落日志），由 journald 收集；字段含义、
`journalctl` 过滤与日志量估算见[调用日志](call-log.md)。代码已实现（XDM-167），随下个 release
部署——已部署实例暂无该行。

访问边界与认证细节属于运维信息，不记录在公开仓库。当前边界是否可接受，尚待 owner 明确决定。

运行面（2026-10-28 owner 批准，现行）：`ExecStart` 带 `--poll-ms 1000`——原默认 100 ms 的空闲轮询
约占 8% CPU（2 vCPU），改后约 1.4%；新任务接手延迟 ≤1 s。preview.38 起三 socket `ExecStart` 写在
`exec-start.conf` 里并以 `--poll-ms 1000` 结尾（`scripts/data_unit_layout.py` 原样保留尾部参数）。
回滚步骤见 [v8 切换清单](contracts/v8-switch-checklist.md) §9.7。

工具数与能力版本的 live 只读复核（2026-11-09，preview.47，VM2-28）：`ops` **16**、`research` **17**、
`maint` **9**、acquire **10**（只读探针的 `initialize` → `tools/list`，不建任务、不写状态）；
`get_data_capabilities({})` 的 `capability_revision=**generic-data-v11**`、`production_ready=false`。
preview.47 线上 `tools/list` 字节数本轮未单独测量（代码面 `research` 入口 17 项工具为 19,264 B，
见 [mcp-tools](mcp-tools.md)）。隧道仍只在 aws-node 上连 `research.sock`（Ops-D 已执行）。

代码（main）注册 37 data tools（含 D-75 的 10 个只读工具与 D-82 的 `get_tradability`/`get_market_cap`，research 入口 17，已随 preview.46 部署）：三个入口 `research`/`ops`/`maint` 由
`data_service --entry` 选择，`mcp.sock` 继续作编排入口，`tools/list` 不再内联
`outputSchema`。preview.38 起生产同时部署三个入口（单进程三 socket，见「服务」表）；
隧道改指 `research.sock`（Ops-D）已于 2026-11-02 preview.42 部署窗口执行（见「服务」表）；Ops-C 已由 AI 客户端（pi）实测（2026-10-28），
结论是 `summary` 不能作为 `ops`/`maint` 缺省。代码（PR #447）已按 D-36 的
「实测不通过就退回」把所有入口的文本模式缺省改为 `full`，`summary` 只作显式选项；
当前生产仍靠上表的 `X_DATA_MCP_TEXT_MODE=full` 覆盖；代码缺省改动在 main 上（PR #447），
preview.46 是否已改用代码缺省、drop-in 是否删除属部署事项，本轮未核实，见
[v8 切换清单](contracts/v8-switch-checklist.md) §2.3 与 §9.6。

## 数据

### vm2-126（当前生产：日更和主数据）

XDM-198 2026-11-07 只读验收（查询与报告在仓库外）；标「历史」的行是 2026-11-05 第一次生产日更后的读数：

| 项 | 事实 | 来源 |
|---|---|---|
| active generation | 完整 sha `afab103be4a776a98587d05a9c83a04022295f65ce606ee2fb42e36d48d90569`，2026-11-09T08:49:24.593Z 激活，上一代 `2731a664…`（现 `superseded`）；采集请求 `acq-231273a5360b5528408fa6d7`（`succeeded`，`bytes_done=575,006,339`；base 16.5 MB、gbbq 6.36 MB）。历史（11-06 代）：完整 sha `2731a664546921b8c43b81744ba53dd0a86682c89668f2be640a63e8c65bfc54`，2026-11-06T08:49:30.017197549Z 激活，上一代 `c642468f…`；该代 manifest：base 16,612,084 B / gbbq 6,152,170 B / hsjday 551,918,442 B（`Last-Modified` Fri, 06 Nov 2026 07:58:47 GMT） | VM2-11；历史行来自 VM2 只读查询 2026-11-08、XDM-198 |
| 最新行情日 | **2026-11-09**（`max(trade_day)` = 20766；当日更新序列 17,296 条） | VM2-11 |
| 热 generation | **5/6**（1 active `afab103b…` + 4 superseded），`evidence_only` 21，未到容量上限；历史：2026-11-05 时点 4/6（1 active `c642468f…` + 3 superseded）。`5ac3ef2b…` 没被自动退役：`derived_facts_missing`，原因是 vm2 的库是 fresh init，缺这个旧代的派生事实 | VM2-11；`src/acquire/service.rs:40,44` |
| `market_series` | **18,118** 条（2026-11-07 只读快照，11-09 未重测） | XDM-198 |
| `series_bars` | 本轮未列出（11-05 为 37,831,020 行，历史） | — |
| 数据库 | `state/preview.sqlite` **2,313,494,528 B**（schema 27；WAL 0 B；2026-11-07 快照，11-09 日更后体积未列出） | XDM-198 |
| 部署目录 | 本轮未列出（11-05 部署后 3.8 GB、日更后 4.5 GB，历史） | — |
| catalog | `catalog-1793852554590130457`，**5,225** 个成员（导出预检读回，2026-11-07） | XDM-198 |
| ETF 缺口 | 本轮未列出（11-05 读数：主表 1,846 行 ETF、128 个退市日、86 只无日线，历史） | — |
| 第一次生产日更 | 见下节「2026-11-05 vm2 第一次生产日更」（历史） | 同上 |
| 第二次生产日更（跨代） | 见下节「2026-11-06 vm2 第二次生产日更（跨代）」（历史） | VM2 只读查询 2026-11-08 |
| 第三次生产日更（跨代） | 见下节「2026-11-09 vm2 第三次生产跨代日更」 | VM2-11 |
| 年线 / 日线明细 / 官方输入 / 内存分项 | 本轮未列出 | — |

### 2026-11-05 vm2 第一次生产日更（跨代）

来源均为 Claude 2026-11-05 只读核实（任务记录在仓库外）。这是 vm2-126 上第一次有新 generation 激活的
生产日更：`4c17de16…` → `c642468f…`（08:40:27Z 激活）。当天没有跑 `tail`（05:00–06:13Z 已跑过 `full`）。

| 项 | 任务 | 结果 |
|---|---|---|
| G1：A 股 `run_market_update`（不带 `scope`） | `task-dea5f97c…` | 5,241 项，5,235 成功，6 个 `REVISION_UNVERIFIED`；write-loop **175.3 s** |
| 补漏 | `task-2306dc95…` | 6/6 成功，5.1 s |
| ETF 批次 | `task-9dad45b1…` | 1,631/1,631 成功，36.1 s |
| 年线 | `task-85edf8f8…` | 5,205/5,205 成功，write-loop **127.9 s** |
| 年线补推 | `task-3b9f0a6f…` | 25/25 成功，1.0 s |
| `sz.158081` 首采 | `task-37a06a8e…` | 成功，1 根 bar |

运行观察：执行这次日更的模型（SWE 2 Flash）提前结束并漏了步骤，经提示后走完。这是一次运行观察，
不是对该模型的结论。

### 2026-11-06 vm2 第二次生产日更（跨代）

来源均为 VM2 只读查询（2026-11-08，`python3 sqlite3` 的 `file:…?mode=ro`，含 WAL；不停服务、不写库；
SQL 与原始输出摘要在本任务的 PR 正文）。这是 vm2-126 上第二次有新 generation 激活的生产日更：
`c642468f…` → `2731a664…`（08:49:30Z 激活），数据到 **2026-11-06**。当天共 8 个 task，窗口内没有其他数据任务。
write-loop 取 D-63 口径的 `created_at` → `last_progress_at`。

| 项 | 任务 | 项数 | 结果 | write-loop |
|---|---|---|---|---|
| G1：A 股 `run_market_update`（不带 `scope`） | `task-e7c1a907…` | 5,241 | 5,231 成功，10 个 `REVISION_UNVERIFIED`（`partially_failed`） | **154.3 s** |
| 年线 | `task-6970fa8e…` | 5,196 | 5,196/5,196 成功 | **89.6 s** |
| 同 10 只重推 | `task-0471bbcf…` | 33 | 23 成功，10 项仍 `REVISION_UNVERIFIED`（`partially_failed`） | 38.0 s |
| 同 10 只显式区间补写 | `task-45e859d8…` | 10 | 10/10 成功 | 4.1 s |
| ETF 批次 | `task-c8fee63e…` | 1,632 | 1,632/1,632 成功 | 26.5 s |
| `sz.158076` 首采 | `task-670debf9…` | 1 | 1/1 成功 | 0.5 s |
| 年线补推 | `task-bf22a99a…` | 6 | 6/6 成功 | 0.5 s |
| 年线显式补推 | `task-19c08ffd…` | 28 | 28/28 成功 | 1.1 s |

G1 的 10 个失败项均为 `INVALID_MARKET_COMMAND: NEEDS_VERIFICATION: RevisionUnverified([…])`：
`sh.600894`、`sh.603279`、`sh.605338`、`sz.000650`、`sz.002275`、`sz.002864`、`sz.300741`、
`sz.301099`、`sz.301215`、`sz.301300`；同 10 只的显式区间任务与年线任务均成功。
当天跑了 3 段 `tail`（09:17:02–09:36:05Z，均 `succeeded`），**没有跑 `full`**，所以没有 G2 的
`full` 读数。3 个 run 的时长（`source_refresh_runs.finished_at − started_at`，VM2 只读查询 2026-11-08）：

| run | 起止（Z） | 时长 | requests | changed | 判定 |
|---|---|---|---|---|---|
| `refresh-tail-1793956622169890299` | 09:17:02→09:27:44 | 642 s | 602 | 380 | 触发整表（`szse_short_name` 380.0 s <600 s，通过）；run 总量 642 s 不设门禁 |
| `refresh-tail-1793957264088571449` | 09:27:44→09:31:51 | 247 s | 225 | 1 | 未触发整表，`tail` <300 s 通过 |
| `refresh-tail-1793957511942187918` | 09:31:51→09:36:05 | 254 s | 225 | 1 | 未触发整表，`tail` <300 s 通过 |

三段按 SLO 第 2 行原文均达标，G2 本次无超标读数。出现 3 次是因为客户端 60 s 超时后重试、
worker 一直被占着（#582；会话日志证实 09:16Z 的 daily-sync 会话里 3 次
`refresh_exchange_sources({"mode":"tail"})` 均为 `MCP request timed out after 60000ms`）。
是否触发整表由 `source_refresh_run_sources` 的成员行判定：第一段 `szse_short_name` 有 376 条
`source_refresh_run_members`（`snapshots=376`、`changed=376`、`elapsed_ms=379969`），另两段的
`szse_short_name`/`szse_listing` 均无成员行（仅页 1 探针）；`source_refresh_runs.detail` 为空，
不以它判定。G2 不进 D-63 计数，所以下面的 2/3 结论不变。
**推断（未验证）**：三段 `tail` 里 `szse_etf_notice_ann` 每次约 181 s（`elapsed_ms` 181052/181018/180966，
各 181 个请求），是固定开销；未触发整表的 `tail` 实测 247–254 s，离 300 s 门槛只剩约 50 s 余量。
计数结论：生产主机、有新 generation 激活、G1 不带 `scope`、write-loop 154.3 s <600 s 四条均满足，
D-63 计数 → **2/3**；年线 89.6 s 同样满足 D-69 的条件，计数 → **2/3**（细节见 [SLO](SLO.md)）。

运行观察：当天第一次启动时 pi 1.3.0 没展开 `/skill:`，提示词缺 Skill 正文，09:16Z 续跑补完 §3.3、
ETF 批次和 §3.6（#586）；这是编排问题，不影响 G1 的任务记录。

### 2026-11-09 vm2 第三次生产跨代日更

来源为 VM2-11 报告（UTC 2026-11-09T08:44–10:30Z，vm2-126 生产日更复核，报告在仓库外，经 Claude 审核）。
这是 vm2-126 上第三次有新 generation 激活的生产日更：`2731a664…` → `afab103b…`
（08:49:24.593Z 激活），最新行情日 **2026-11-09**。日更由 Claude 于 08:42:45Z 启动，
核对窗口 `--since 2026-11-09T08:42:45Z`。write-loop 取 D-63 口径的 `created_at` → `last_progress_at`。

| 项 | task | 项数 | 结果 | write-loop |
|---|---|---|---|---|
| G1：A 股 `run_market_update`（不带 `scope`） | `task-013253605e4c81cc30fe6738877b2af6` | 5,241 | 5,233 成功，8 个 `REVISION_UNVERIFIED`（`partially_failed`） | **231.2 s** |
| §3.3 补漏 | `task-d0abf4497338b89283e86aa2abbb918f` | 8 | 8/8 成功 | 2.2 s |
| ETF 批次 | `task-2da11139544e0562080d54c503e7a340` | 1,633 | 1,633/1,633 成功 | 31.7 s |
| 年线 | `task-10ba99f16707bca718b1165db2b2e15a` | 5,187 | 5,187/5,187 成功 | **120.5 s** |
| §3.6 年线显式重推 | `task-1880f006ed993ab88d3ef4446a8951ce` | 37 | 37/37 成功 | 0.8 s |
| `sh.688827` 首采 | `task-a17cf104091cb0ed9bed19f8df0c2029` | 1 | 0/1，官方 `.day` 无成员（预期 fail-closed） | 0.1 s |

G1 的 8 个失败项均为 `INVALID_MARKET_COMMAND: NEEDS_VERIFICATION: RevisionUnverified([…])`：
`sh.603998`、`sh.688777`、`sz.000426`、`sz.002091`、`sz.300972`、`sz.301282`、`sz.301559`、`sz.301608`。
该 task 的 17 个 derived item 全部成功（含 9 条 `derived:index:*`），所以不存在「derived 同源失败」。
`check_daily_run.py` 10:22Z 复跑：「结果：通过（每个必做步骤都已执行或已检查无需动作）」、「会话结论：声称完成」。

当天的来源刷新：1 个 `tail` run（`refresh-tail-1794214168855827718`）+ 一次**重复的 `full`**
（`refresh-full-1794215128411620246`，09:05:28→10:17:05Z，`succeeded`，71m37s，3,878 请求 /
reused 3,198 / changed 521 / failed 0 / unobserved 146）。本周的 `full` 已于 2026-11-08 由 VM2-18 跑完，
所以这次是重复的（成因与建议见「已知问题」）。按 D-73 的分组读数：原有 7 来源合计 **41.0 min**
（2,072 请求，<60 min 通过）、ETF 4 来源合计 **30.6 min**（1,806 请求，<40 min 通过），两组
`all_complete=1`、`failed_windows=0`；run 总时长按 D-73 不设门禁。

计数结论：生产主机、有新 generation 激活、G1 不带 `scope`、write-loop 231.2 s <600 s 四条均满足，
D-63 计数 → **3/3**；年线 120.5 s 同样满足 D-69 的条件，计数 → **3/3**（细节见 [SLO](SLO.md)）。

### aws-node（fallback；数据冻结于 2026-11-04，不再跑日更）

**preview.42 的 fresh init**（2026-11-02T06:04:22→06:04:42Z）与随后的**重建**
（07:13Z–07:59Z，XDM-165；顺序 `full` → catalog → market.daily → 首次 tail → plan/run → listed_ever → 年线）
已完成；下表是 aws-node 2026-11-02 的只读实测（库 + WAL）。aws-node 在冻结前日更持续到
2026-11-04（数据停在 11-04，见上方总览），下表 generation/序列计数是 11-02 时点值、
不是冻结时点值；冻结时点读数见下。重建在生产机上执行但**非独占**（多个开发 agent
同时在跑），下述耗时不作门禁证据。标注「历史」的行是更早的对照值。

**aws-node 冻结时点（2026-11-05 只读实测；历史，冻结后不再变化）**：schema 26；`current` → `releases/c0ae295…`；
active generation `4c17de16…`（2026-11-04T10:19:18Z 激活，与 vm2 同一代）；历史热 generation
**3/6**（superseded `5ac3ef2b…`、`f41c8935…`，11-04 时与 vm2 相同）；`evidence_only` 17；
`market_series` 16,354 条；最新行情日 **2026-11-04**；库 2,193,297,408 B，WAL 16 MiB。

| 项 | 事实 | 来源 |
|---|---|---|
| active generation（aws-node，11-02 实测） | `f2fd6fcbded1fc142c0e0b1aba998a7de72117a58c3bcc8815e80943046d20a6`（2026-11-01T01:14:57Z 激活，`previous_generation_id` = `0f1623b0ec0430baa94cbc2686b781d6400f92eecbbe4ddd862911e295322683`，`activation_request_id` = `daily-20261101-xdm139-activate`）；11-02 时点最新集中行情日 **2026-10-30**（`last_day=20756`）；**不是冻结时点值**（冻结时点见上） | `generations/active-generation.json`、`market_series` 只读查询 |
| 热 generation（aws-node，11-02 实测） | 历史（aws-node 2026-11-02 实测）：**2/6**：1 active（`f2fd6fcb`）+ 1 superseded（`0f1623b0`）；另有 15 个 evidence_only（13 个原有 + 本轮退役的 `9d9e38d7`、`c184e9c4`）；`pins/` 16 个目录，`inflight_tasks` 无 | `generations/acquire.sqlite`（按 `state` 计数）、`generations/pins/`、`src/acquire/service.rs:40,44,1118-1140` |
| 旧代回收 | **已处理（2026-11-01T10:25–10:26Z，XDM-159）**：两个遗留代 `9d9e38d7`、`c184e9c4` 按方案 A（D-65，门禁规则不改）完成——登记 `tdxhy.cfg`/`zhb.zip!incon.dat` 为 generation 成员 → `replay --scope generation_facts` 把 `generation_facts` 重建为 **18** 行 → 每代对 3 类行情派生事实记豁免（共 **6** 条 `retire_exemptions`，`recorded_by=Liqiangcc`，未用 `--owner-approve-evidence`）→ acquire MCP `retire_generation`（`request_id=retire-20261101-<gen8>`）。两个回复均为 `state=evidence_only`、`reason_code=null`，未追加豁免。退役不可逆（base/gbbq/hsjday 字节已删）；回滚只恢复软硬件面 | XDM-159 报告、`retire_exemptions`/`generation_facts`/`manifest.jsonl` 只读查询、[DECISIONS](decisions.md) D-65 |
| 数据库 | `state/preview.sqlite`，fresh compact schema 26（`pragma user_version=26`，`journal_mode=wal`），**1,932,939,264 B** + WAL **16,777,216 B**（`-shm` 163,840 B；`mode=ro` 含 WAL 读）；归档：preview.41 的 schema 25 库 `state/preview.sqlite.v25-archived-20261102T060432Z`（2,013,822,976 B，保留作回滚点）与 preview.38 的 schema 24 库 `state/preview.sqlite.v24-archived-20261031T182806Z`（1,878,384,640 B，保留） | `stat`、`pragma user_version` 读回、phase-g 报告 `preview42-phaseg-deployment.json` |
| catalog | `catalog-1793603620505554045`，**5,225** 条（fresh init 后的首采，`task-9bca64db…`，07:13Z） | `catalog_head`/`catalog_members` 只读查询 |
| 日线 | 不复权 **5,570** 条序列 + 前复权 **5,561** 条序列（`qfq-exact-v1`，6dp）：5,223 条是 catalog 实体、338 条是 listed-ever 退市序列、另 **9** 条指数（仅不复权）；最新集中行情日 **2026-10-30**。5,217/5,223 个 catalog 实体 head=2026-10-30，6 个在之前：`sh.688496`(20706)、`sz.002717`(20738)、`sh.601326`(20746)、`sh.605258`(20749)、`sz.002827`(20749)、`sh.688121`(20754)（重建 run 里它们 6 项都 `success`，窗口内没有新 bar，head 不变） | `market_series`（`period=1`）只读查询、重建 run 的 `task_items.outcome_json` |
| 年线 | **5,223** 个 published（`yearly_series_lineage` 同数），其中 5,217 个 head=2026-10-30 | `market_series`（`period=2`）只读查询 |
| 基准指数 | **9** 条，全部 head=2026-10-30；`sh.000001` 剔除 16 根、`sz.399001` 剔除 9 根 OHLC 矛盾的 bar（D-51） | 指数序列只读查询、D-51 |
| 交易日历 | **8,798** 天（`trade_day` 7657 … 20756 = 1990-12-19 … 2026-10-30） | `trading_days` 只读查询 |
| 复权事件 | **59,361** 行，`(entity, ordinal)` 组合也是 **59,361** 个 | `adjustment_events` 只读查询 |
| 缺失 | `sh.688827`、`sz.001381`（官方 hsjday `.day` 缺成员，预期）保持 absent：catalog 中存在，日线/年线均无 series | `catalog_members` 反连接 `market_series` 只读查询 |
| M3 来源库索引表 | `industry_snapshots` **7** 行、`industry_assignments` **5,235** 行（全部 `name_status=unknown_dictionary`，成因见[M3 设计](tasks/issue-446-m3-delisted-industry.md) AC-M3-3）、`exchange_source_snapshots` **2,590** 行（`sse_suspension` 1,300 / `szse_short_name` 750 / `szse_listing` 292 / `sse_bulletin` 230 / `szse_delisting` 12 / `sse_listing` 4 / `sse_delisting` 2；首次 `full` 把整表与窗口类来源全量重抓后每页两个 sha 都在）、`generation_facts` **15** 行（4 代 × 3 项行业事实，重放重建；活跃代补齐的 3 项行情派生事实由重建 run 写入）、`retire_exemptions` **3** 行（`0f1623b0` × 3 类派生事实，2026-11-02T08:06:30Z，`recorded_by=Liqiangcc`，理由「preview.42 fresh init 丢失市场派生事实（D-65 先例）」）。来源库 `state/source-store` **68,731,247 B**、清单 **2,607** 行、blob **2,595** 个，其中 `origin` 形如 `generation:<sha>` 的有 **8** 行（4 代 × 2：`tdxhy.cfg` 与 `zhb.zip!incon.dat`） | 各表只读计数、`manifest.jsonl`、`du -sb` |
| M2 数据集 | 库内实测值见上面「M2 随 preview.37 部署」表的右列；`derived_dataset_versions` 共 **11,024** 行 | 只读查询 |
| 重建耗时（生产，非独占） | **preview.42 当轮（2026-11-02）**：首次 `full` 06:22:20→07:08:06Z（2,746 s，**2,072 请求 / 1,307 复用 / 523 变更**，`failed_windows=0`，G2 full <60min（当时口径；D-73 起 `full` 按来源组计量：原有 7 来源 <60min、ETF 4 来源 <40min）通过）+ catalog（1 项，07:13:33→07:13:41Z）+ 日线全量 collect（53 块 = 52×100 + 1×25，07:15:59→07:25:09Z，**553.5 s**，2 块 `partially_failed`）+ 首次 `tail`（07:27:22→07:28:27Z，**65 s**，36 请求 / 28 复用，`verified_through=2026-11-02`，未触发整表）+ 日线 plan/run（23 项，写循环 **131.5 s**，22 成功，`derived:risk_warnings` 失败）+ listed_ever（338 项，07:48:02→07:48:55Z，**53.9 s**）+ 年线 plan/run（5,223 项，07:52:04→07:59:13Z，写循环 **429.2 s**，全成功）。历史（preview.39，2026-10-31）：catalog 7 s + 日线 541 s + v2 run 1,977 s + listed_ever 48 s + 年线 566 s | `tasks`/`task_items`（`created_at`/`last_progress_at`）、`source_refresh_runs` 只读查询 |
| 任务与事件 | 本轮（fresh 库）：库内共 **57** 个任务（54 succeeded、3 partially_failed：2 个日线 100 项块 + 1 个日线 plan/run），全部结束，无 running；`market_events` **16,366** 行；`series_bars` **36,145,465** 行。历史（preview.35 时点）：579 个任务、`market_events` 83,545 行 | `tasks`、`market_events`、`series_bars` 只读查询 |
| 退役记录 | **2026-11-01T10:26Z**：`9d9e38d7…`、`c184e9c4…` 退役为 evidence_only（各带 3 条豁免，热代 4→2，代目录 2,291,141,645 → 1,146,923,381 B，释放 ≈1.065 GiB，根盘可用 +1,143,803,904 B）。历史：`87322997…` 2026-10-19 退役；`2d5851a4…` 2026-10-24T16:57:16Z（548M→16K）；`a119629b…` 2026-10-24T17:52Z（574M→16K）；更早 4 个 2026-10-13 退役为 evidence_only；2026-10-28T11:56Z `0f1623b0` 激活后的自动保留一次退役了 `8b8614c3`、`54553656`、`f289c612` | XDM-159 报告、`generations/acquire.sqlite`、`df`/`du -sb` |

### 2026-11-02 preview.42 fresh init 与重建（XDM-165）

时间均为 `tasks.created_at`/`last_progress_at` 或 `source_refresh_runs` 的 UTC：

1. 06:04:22→06:04:42Z 部署（`deploy_phase_g.sh v0.2.0-preview.42 --force-git-transport`）：停 tunnel→acquire→data，
   schema 25 库归档为 `state/preview.sqlite.v25-archived-20261102T060432Z`，`init21` 建 schema 26，`replay --scope all` 五个 scope
   全部 `ok`/`database=match`（industry 7 / 5,235、exchange 1,836、generation_facts 12、risk_warnings 1,289、security_names 7,344），
   起 data→acquire→tunnel；就绪探针 research 219 ms/5、ops 23 ms/16、maint 16 ms/9、acquire 10 ms/10。停机 20 s。
2. 06:22:20→07:08:06Z **首次 `full`**（`refresh-full-1793600540254628747`）：2,746 s，2,072 请求 / 1,307 复用 / 523 变更，
   `failed_windows=0`、`unobserved_windows=101`，7 个来源全部 `complete=1`；`verified_through=2026-11-02`。
3. 07:13:33→07:13:41Z catalog 首采（`catalog-1793603620505554045`，5,225 条）。
4. 07:15:59→07:25:09Z 日线全量 collect：53 块（52×100 + 1×25，v1 explicit），553.5 s，51 块 succeeded、2 块 partially_failed。
5. 07:27:22→07:28:27Z 首次 `tail`（`refresh-tail-1793604442021279556`）：65 s，36 请求 / 28 复用，`changed=0`，未触发整表。
6. 07:28:43→07:30:54Z 日线 plan/run（`plan_id=1e282b03…`，23 项，写循环 131.5 s）：22 成功；`derived:risk_warnings` 失败
   （`SameDayActionConflict`，`sh.600734` 2022-01-29，见「数据」表该行与 XDM-165 报告）。
7. 07:48:02→07:48:55Z `sh_sz_a_listed_ever` 首采（338 项，53.9 s，succeeded）。
8. 07:52:04→07:59:13Z 年线 plan/run（5,223 项，写循环 429.2 s，全成功）。
9. 08:06:24→08:06:45Z 短窗口（停 tunnel→acquire→data）：给 `0f1623b0` 记 3 条 `retire_exemptions`（08:06:30Z，`recorded_by=Liqiangcc`），
   起回 data→acquire→tunnel。

### 2026-11-01 第 1 代日更（XDM-139）

时间均为 `tasks.created_at`/`last_progress_at`（数据库写入时间，UTC）：

1. 01:14:57Z 激活 `f2fd6fcb…`（request `daily-20261101-xdm139-activate`，带 previous `0f1623b0…`）。
2. 04:21:56→05:12:05Z 日线 plan/run（`task-8128ffb4…`，**5,240** 项 = 5,223 个 catalog 实体 + 17 个派生 item），写循环 **3,009.6s**（`created_at` 04:21:56.224 → `last_progress_at` 05:12:05.864）：
   **5,215 成功、25 失败**（24 项 `REVISION_UNVERIFIED` + `derived:risk_warnings`）。
3. 05:32:28→05:33:04Z 日线显式前向窗口补推（`explicit-range-task-v1`，**24/24 成功**，实体集合即上一步的 24 个失败项）。
4. 05:34:40→05:46:50Z 年线 plan/run（**5,130/5,130 成功**；5,223 − 5,130 = 93 个 ineligible 实体）。
5. 05:49:46→05:49:54Z 年线显式补推（**93/93 成功**）。

派生 item 的实际结果（同一 task 的 `task_items.outcome_json`）：`derived:calendar` 8,798 天、
18,010,773 根 bar 在历内、67 根在外；`derived:delisted_series_check` checked **337**、unchanged 337、
`tail_extended` 空（80 ms）；`derived:security_master` 5,565 行（181,311 ms，`duplicates_collapsed` 2 组）；
`derived:suspensions` 79,442 段（1,839,810 ms，`confirmed` 20,420、`deferred_beyond_calendar` **4**：
`sh.601326` 10-21→10-31、`sh.605258` 10-26→10-31、`sh.688121` 10-29→10-31 等，区间尾越过已知交易日历 10-30）；
`derived:security_names` 7,344 行（377,444 ms，深市覆盖 3 个窗口 / `not_observed` 2、沪市 `NOT_AVAILABLE`）；
`derived:industry_snapshot` `no_change=true`（5,666 行 / 5,235 assignments / scope 5,231 / 7 快照 / 330 ms，
`source_kind=generation`、evidence grade A）；`derived:trading_rules` `no_change=true`（27 条价格限制 +
2 条最小变动 / 29 行 / 19 ms）；9 个指数 item 全部成功。`derived:risk_warnings` **失败**：
`INVALID_MARKET_COMMAND: historical ST/risk-warning source: source_invalid: source_invalid:
source_invalid: page_count is 0`，库内 `risk_warning_intervals` **0 行**（未登记版本，读取 `manifest.status=absent`）。

验证（本轮只读）：

- preview.41 切换窗口与维护窗口后的探针：`ops`/`research`/`maint` 工具数 15/5/9，acquire 10（见「服务」段）；
  机器报告只记了 `ops`，research/maint 是手工探针（XDM-160 已修）。
- `market.risk_warnings` 在 11-01 交接时读回 `manifest.status=absent`、`row_count=0`
  （`sz.000004`，2020-01-01…2026-11-01）；该结论来自 `preview40-risk-warnings-backfill.json`，不是推断。
- 库内读回（只读 SQL，含 WAL）与导出/缓存无关的计数：catalog 5,225、日线 5,561（不复权/前复权各一）、
  年线 5,223、日历 8,798、复权事件 59,361、指数 9、主表 5,565、停牌 79,442。
- 历史（preview.37 部署窗口）：x 消费侧契约断言通过（`sh.600006` 2025 年数据）、`market.index` 返回 9 个实体、
  6 个 M2 数据集在 ops 入口可读回；preview.36 的全量 daily 导出为 119s / 900MB（`daily_raw` 16,737,243 行），
  本轮未重跑导出。


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

运维事实：热 generation 上限 6（`MAX_HOT_GENERATIONS`，`src/acquire/service.rs:40`；hot = 状态不是
evidence_only 的代），超限后 `acquire_generation` 被拒 `generation_capacity`；Skill 无权退役 generation；
退役历史：2026-10-13 由操作员退役 4 个已无字节的旧代，2026-10-19 退役 `87322997`，
2026-10-24T16:57:16Z 退役 `2d5851a4`（释放约 546MB），2026-10-28T11:56Z 一次退役 `8b8614c3`、
`54553656`、`f289c612`，2026-11-01T10:26Z 退役 `9d9e38d7`、`c184e9c4`（释放 ≈1.065 GiB）。
**本轮实测 2/6**（aws-node 2026-11-02 实测：1 active + 1 superseded，见「数据」表 aws-node 行；历史：aws-node 冻结时点为 3/6；历史：vm2-126 第一次生产日更后为 4/6，见「数据」表各自的「热 generation」行）：下一次 acquire 可直接受理。
保留策略（`RETAINED_SUPERSEDED=2`，`src/acquire/service.rs:44`）已随 preview.35
**于 2026-10-24T17:25Z 部署**，但只在 activate 提交成功后触发（`retire_expired_superseded`，`:1118-1140`），不能解除 acquire 之前的容量拒绝。
Skill 在有更新日的路径仍未完整验证
（枚举过慢一次、部分自行追加一次）；#304 的有效 PASS 仍为 3/10（均为 unchanged）。批量更新工具
（`plan_market_update`/`run_market_update`）自 `generic-data-v7`（preview.33，2026-10-16）起已实现并随
二进制上线；XDM-139 的第 1 代日更即由它驱动（5,240 项，写循环 **3,009.6s**，见「数据」段）。

## 新鲜度

### vm2-126（当前生产：日更和主数据）

| 项 | 事实 | 来源 |
|---|---|---|
| 最新集中行情日 | **2026-11-09**（`max(trade_day)` = 20766；当日更新序列 17,296 条） | VM2-11 |
| 数据规模 | `market_series` **18,118** 条（2026-11-07 只读快照）；`series_bars` 本轮未列出 | XDM-198 |
| 官方输入 / 任务计数 / 落后交易日明细 | 本轮未单独列出 | — |

### aws-node（fallback；冻结时点 2026-11-05 实测 + 2026-11-01/02 历史实测）

| 项 | 事实 | 来源 |
|---|---|---|
| 冻结时点（2026-11-05，历史） | schema 26；active `4c17de16…`（2026-11-04T10:19:18Z 激活，与 vm2 同一代）；历史热 generation **3/6**（superseded `5ac3ef2b…`、`f41c8935…`）；`market_series` 16,354 条；最新行情日 **2026-11-04**；库 2,193,297,408 B，WAL 16 MiB | Claude 2026-11-05 只读核实 |

以下为 aws-node 2026-11-01/02 实测（**不是冻结时点值**）：

| 项 | 事实（aws-node，2026-11-01/02 实测） | 来源 |
|---|---|---|
| 最后任务 | 2026-11-01T05:49:54Z（`explicit-yearly-task-v1`，93/93 成功），之后无新任务；库内共 62 个任务（见「数据」表） | `tasks` 只读查询 |
| 落后交易日 | **无**（2026-11-02 时点）：日线/年线最新集中行情日为 2026-10-30（`last_day=20756`，即当时最近一个已收盘交易日）。5,217/5,223 个 catalog 实体 head=10-30；6 个停在之前（名单见「数据」表），其中 4 个正是 `derived:suspensions` 的 `deferred_beyond_calendar` 样本（`sh.688496`、`sh.601326`、`sh.605258`、`sh.688121`，区间尾 10-31 越过已知日历 10-30），另 2 个（`sz.002717`、`sz.002827`）未逐项归因 | `market_series`、`task_items` 只读查询 |
| 官方输入 | active generation `f2fd6fcb` 的 `snapshot-manifest.json`（2026-11-01T01:07:35→01:09:59Z 下载）：base_industry 16,612,587 B / `"6ae58eb5-fd7ceb"` / `Sat, 31 Oct 2026 06:46:13 GMT`；gbbq 6,152,973 B / `"6ae58eb5-5de30d"` / 同上；day（hsjday）551,001,909 B / `"6ae44e3a-20d79f35"` / `Fri, 30 Oct 2026 07:58:50 GMT`。本轮**未**用 `curl -sI` 与源站重新比对（上次比对：历史 2026-10-24T17:26Z，基线 `c184e9c4`，逐项相同） | `generations/generations/f2fd6fcb…/snapshot-manifest.json`、历史 `curl -sI <三个官方 URL>` |
| 容量 | 历史（aws-node 2026-11-02 实测，非冻结时点）：hot **2/6**（1 active + 1 superseded；权威值见「数据」表 aws-node 行的「热 generation」行） | `generations/acquire.sqlite`、`src/acquire/service.rs:40,44` |

## 2026-10-23 重启与就绪（实测）

- 主机于 2026-10-23 06:49:20 重启（`journalctl --list-boots` 的 boot `d462ed36c2ad42aba34aed2031018be3`）；上一轮 uptime 5d12h39m，恰好从 preview.34 切换（10-17T18:09Z）开始。
- `x-data-mcp-data` 06:49:25 启动，`run/mcp.sock` 于 06:49:56.531 建立（`stat` 的 Birth）→ 冷启动到可服务 **31.5s**（库内有 574 个历史任务）。
- `x-data-mcp-tunnel` 06:49:25 首次启动，`ExecStartPre`（`wait-data-service.py`，当时 deadline 为 10s；10-24T16:56:58Z 已改为 120s）于 06:49:37 报 `data service IPC readiness failed`、`Control process exited, code=exited, status=1/FAILURE`；systemd 按 `Restart=always`/`RestartSec=10` 在 06:49:47 重试，06:49:56 `Started`，同一秒 tunnel-client 打出 `🟢 tunnel-client started`。**隧道在重启后不可用约 31s**。
- 10-24 06:43:16 的那次单元重启不是「恢复」：06:43:09 `apt-daily-upgrade` 触发 `systemd` daemon-reexec，随后 needrestart 重启了 tunnel、sshd、nginx 等单元；此前该单元自 10-23 06:49:56 起连续运行 23h24m（systemd 的 `Consumed … wall clock time`）。同样的模式在 10-23 07:42:51 也发生过一次（停机约 1s）。
- 因此重启本身只造成约 31s 的隧道不可用；10-20..10-23 的数据缺口与重启无关（10-20T00:07Z 之后就没有任何任务）。`scripts/switch_binary.sh` 的 S6 在旧版本只等 20s 且只判 socket 文件存在，按 31.5s 冷启动会判失败并回滚（推断，未在真实部署中触发）。仓库脚本已在 #370（`8e83f22`，2026-10-24 合入 main）改为 S6 调用 `scripts/wait_service_ready.py`、默认超时 120s（`XDM_READY_TIMEOUT` 可覆盖），不再只判 socket 文件存在；按 31.5s 冷启动，当前脚本可在超时内等到就绪（推断）。
- **残留风险（部分缓解，未消除）**：`wait-data-service.py` 的 deadline 已于 2026-10-24T16:56:58Z 从 10s 改为 120s，因此「冷启动比等待长」这条路径在当前库规模下不会再触发；剩下的风险是就绪超过 120s、或单元连续失败触到 `StartLimitBurst=5` 时停在 `failed` 不再重试，需人工介入。这两条都未实测（推断）。

## 足迹

### vm2-126（当前生产：日更和主数据）

| 项 | 事实 | 来源 |
|---|---|---|
| 数据库 | `state/preview.sqlite` **2,313,494,528 B**（schema 27；2026-11-07 只读快照，WAL 0 B；11-09 日更与 preview.47 部署均未重测） | XDM-198 |
| `/opt` | 本轮未列出（11-05 部署后 3.8 GB、日更后 4.5 GB，历史） | — |
| generations / releases / reports / 内存分项 | 本轮未单独列出 | — |

### aws-node（fallback；2026-11-01/02 实测，非冻结时点）

| 项 | 事实（aws-node，2026-11-01/02 实测；未整树重测的单项标「历史」；冻结时点库 2,193,297,408 B、WAL 16 MiB 见「数据」段 aws-node 冻结时点块） | 来源 |
|---|---|---|
| `/opt/x-data-mcp-preview` 全树 | **5,419,972,074 B**（约 5.05GiB）；根盘 77G 已用 54G / 余 24G（70%） | `du -sb .`、`df -h /` |
| generations | **1,147,087,655 B**：2 个带字节代（active `f2fd6fcb` 573,771,524；superseded `0f1623b0` 573,093,101）；另 15 个代目录各 4,055–4,119 B（只有清单/决策/证据，代字节已删）；共 17 个代目录；`pins/` 16 个目录。本轮维护窗口后相比退役前减少 **1,144,218,264 B**（≈1.065 GiB） | `du -sb generations`、`du -sb generations/generations/*`、`ls generations/pins` |
| state | `preview.sqlite` **2,013,822,976 B**（WAL 已 checkpoint 为 0，`-shm` 65,536 B）+ 归档 `preview.sqlite.v24-archived-20261031T182806Z`（1,878,384,640 B）+ `source-store` 25,287,617 B；整个 `state/` **3,917,529,519 B**（另有一些历史 `config-before-*`/`mcp.json.before-*` 小目录）。历史：旧 STATUS 引用的 `v24-archived-20261028T002126Z`（1,732,116,480 B）与 `v21-archived-20261027T121543Z`（1,185,640,448 B）**已不在** `state/`（已删）；两份 v20 归档库已移出 `/opt` 到 `/root/x-data-archive/` 并 gzip | `ls -l state/`、`stat`、`du -sb state` |
| releases | **352,642,038 B**，5 个发布目录：`98a4dbe`（当前，75,490,085 B）、`441185b`（preview.40）、`8173991`（preview.39）、`6ac5c963`（preview.38）、`49422db`（preview.37） | `du -sb releases`、`ls -l releases/` |
| reports | **2,693,784 B**；本轮新增：`preview41-switch-20261101T101825Z.json`（3,526 B，preview.41 切换）。历史（2026-11-01 上一轮）：`preview40-switch-20261101T003814Z.json`、失败的那次 `preview40-switch-20261101T003800Z.json`、`preview40-risk-warnings-backfill.json`、`preview39-phaseg-deployment.json` | `ls -l reports/`、`du -sb reports` |
| `/tmp` | tmpfs 953MB，已用 101MB（11%）；历史（2026-10-24T17:49Z）清理过 10-23 的调试残留约 606MB | `df -h /tmp`、历史 `du -sh /tmp/*` |
| 内存 | `free -m`：total 1,905 / used 1,278–1,350 / buff-cache 656 / available 554–626；swap 8,191 total / 3,668–3,705 used。历史（2026-10-24）：swap 已用 2.5GiB | `free -m` |

## 进行中

- **SLO 门禁**：第 1 行（同代无变化 `update_data`，write-loop <600s）**2026-10-28 已解除临时豁免**（D-54 解除，见 D-57）：XDM-84 实测同代无待更新 87.6s、同代全量 5,238 item **573.4s <600s**（main `0f3fe25`，单进程三 socket，w=1，2 vCPU，load average 4–8、最高 8.12，非完全独占，含 research 并发；全量口径用显式 `end_date` 构造，见 [SLO](SLO.md) 备注）。第 2 行（跨代有变化，<600s）的 D-52 豁免**已于 2026-11-09 按 D-63 解除**（见下）；历史：2026-10-28 同次实测跨代全量 **734.0s** 高于 600s，acquire 切代 VmHWM 33.3 MiB。两项门槛本身不变；这些时间来自非独占窗口，只作相对比较。**2026-11-05 起日更和主数据生产机换成 vm2-126（D-74，preview.45/schema 27/`generic-data-v10`，无 tunnel；2026-11-07 起 preview.46，**2026-11-09 起 preview.47**，均为 `generic-data-v11`），aws-node 的 preview.43 冻结为 fallback**；D-63 的连续 3 次生产跨代日更计数**从 0 重新开始**（aws-node 上 11-02 那 1 次不带过来），从 vm2 第一次生产跨代日更算起，D-69 计数规则不变、也只算生产日更。**2026-11-05 vm2 第一次生产跨代日更已完成：G1 write-loop 175.3 s（D-63 计数 1/3），年线 write-loop 127.9 s（D-69 计数 1/3）；2026-11-06 第二次：G1 write-loop 154.3 s（→ 2/3），年线 89.6 s（→ 2/3）；2026-11-09 第三次：G1 write-loop 231.2 s（→ **3/3**），年线 write-loop 120.5 s（→ **3/3**）——D-63 与 D-69 的解除条件已于 2026-11-09 达成（3/3），D-52 豁免按 D-63 解除**（VM2-11），细节见「数据」段与 [SLO](SLO.md)。****生产自 preview.38 起是单进程三 socket（`mcp.sock`/`research.sock`/`maint.sock`，见「服务」表），preview.40 与 preview.41 沿用同一形状**；第 1 代日更（XDM-139）是 preview.40 下的首个全量实测，但不作门禁证据（非独占，见「数据」段）。详见 [SLO](SLO.md) 备注与
  [DECISIONS](decisions.md) D-52/D-57/D-63/D-69/D-74。
- **GOAL-008 性能**：PR #323 已于 2026-10-11 squash 合并（`8a446b1`），发布 v0.2.0-preview.31，2026-10-12 部署。
  - 全市场验收（XP-015，perf `3b11231`，aws 私有栈，浅链，53 批，trace-OFF，每负载单次）：年线首次 310.6s、日线跨代 Append 104.2s（合成截断夹具）、年线重聚合 328.3s、日线同代 NoChange 78.0s，write-loop 均 <600s，读回全部 PASS（见 [SLO](SLO.md)）。第三类「保留的真实 mixed Append/Full/fail-closed」**未运行**（retained gen-1 字节永久不可得）；`AGENTS.md` 记载的离线 harness 结果仅作旁证，不作替代。
  - **最坏链深未做全量实测**：错开的提前全量重写（每实体阈值 64–127）后最坏链深 ≤127，年线批中位 10.3s，按 53 批外推约 548–588s，余量约 2–9%；日线在深链上的有效实测缺失。
  - 校验分层变化（均经 owner 批准，2026-10-10 / 11）：accept 只检查在途；`get_data_availability` 统计与校验走已提交层（含年线）；`get_yearly_base_years` 叶节点完整校验、祖先已提交层；年线发布的先验年线 head 读取降到已提交层；rollover 校验由「父深度必须正好 256」放宽为 64..=256。全链完整校验仍在发布提交、服务启动 sweep、`task_sqlite check`、`query_data`。
  - **已于 2026-10-12 上线到生产（preview.31，全新重建）**，生产读回：catalog 5,228、日线 5,224、年线 5,224、absent 4 个、5 个抽样实体日线/年线可读。
- **#304**：连续 10 个交易日仅用 Skill 完成同步的验收。已记录的 PASS 运行（Run 3、Run 5、2026-10-08）全部是 `day=unchanged`，有更新那天的路径尚未覆盖。有效计数以 #304 的评论为准；2026-10-08 那次使用的是提示词 `daily-ai-sync-v2`。

## 已知问题

| 问题 | 影响 | 来源 |
|---|---|---|
| `source_store replay --verify-only` 的 `risk_warnings` 报 `drift`（vm2-126，**#533**）：**2026-11-09 日更后重跑，沪市漂移 0 条（873 条区间 / 688 闭合 + 185 开 / 414 只代码），#533 的修复在生产上已确认生效**。当时全 scope 的 `status` 仍为 `drift`（`drift_cause=rows_changed`，`entries=2494`、`snapshots=7924`、`problems=[]`），**成因是派生数据旧于来源库、不是数据损坏**：`full` 之前同一 scope 为 `match`（`entries=2119`），重复 `full` 之后抓到 375 条新的风险警示记录，而库内 `derived:risk_warnings` 还是 08:5x 那一版。历史：**preview.46 已部署**（2026-11-07T16:52Z，`c35b425`），**D-84 R2 使 884 行沪市主键整体后移，部署后第一次日更前 `replay` 报 `rows_changed` 属预期**（2026-11-07 验收的 20/20 条 drift 都是主键日期 +1–2 个交易日的成对条目），#533 的同主键 `source_document_sha256` 差异当时被日期平移掩盖；replay 与日更的公告页顺序不同（replay 按请求指纹文本排序，日更按标题过滤器 → 窗口起点 → 页号），代码已改用日更页顺序（`BulletinOrder`） | 不是数据损坏；日更前不要在生产库上跑写模式 `replay`（会把 884 行改成新规则的值） | VM2-11、XDM-198、Claude 2026-11-05 只读核实 |
| 深市 `risk_warnings` 的 replay 漂移（vm2-126）：`full` 之后 20 条 drift 样例**全部在深市**，成因是**派生数据旧于 `full`**。按 `manifest.jsonl` 的 `acquired_at` 逐条核对：重放值全部来自 **2026-11-09T09:42:03Z**（`full` 窗口内）的 `szse_short_name` 抓取，库内值全部来自 **2026-11-06T09:18:22Z**（窗口前），**20/20 全中**。与两份备份的 manifest 差值（6,313 → 6,834 = +521）和 `full` 的 `changed=521` 完全吻合 | 不是数据损坏，也不是 #533 在深市仍然存在，而是**派生数据尚未重建**；**M3-9 / AC-M3-4 本轮不宣布通过，待 2026-11-10 日更重建派生数据后重跑 `replay --verify-only` 确认**（预期回到 `match`） | VM2-11 |
| **#308**（沪市风险警示写入器 `derived:risk_warnings`）：沪市 0 漂移，**可在 11-09 读数基础上关闭**（待 owner 裁定）；深市部分归上一条（待 11-10 重建后再验） | 沪市确认；深市未关闭 | VM2-11、XDM-198 |
| 11-09 日更 agent 的**轮询缺陷**（两处，均为编排问题，不影响数据）：①08:44:46Z 因 `bytes_done` 在首个文件完成前恒为 0（`base.zip` 16.5 MB 下载约 64 s），而轮询间隔只有 2–3 s，agent 误判超出轮询预算停下，Claude 08:49:16Z 用**同一会话与原 `task_id`** 恢复（未新建 acquisition）；②09:05–09:36Z 对 `status_of` 无间隔轮询，实测每分钟约 5,000 次，被 magpie 的防循环机制中止（会话涨到 291 条，无写库、无改状态） | 建议 Skill 把 `fetch:*` 阶段的等待下限设为单文件下载时长量级（如 ≥60 s 再判停），并给 `status_of` 轮询加间隔；不改代码（#625） | VM2-11 偏差 1/2 |
| 11-09 日更发起了**重复的 `full`**（`refresh-full-1794215128411620246`，09:05:28→10:17:05Z，`succeeded`，71m37s，3,878 请求 / changed 521 / failed 0）：本周的 `full` 已于 **2026-11-08** 由 VM2-18 跑完（`refresh-full-1794120245537117945`），而日更 agent 按 Skill §3.0「每周一次 `full`」照章发起，不知道本周已跑（会话里没有该状态，Skill 也没有前置判断） | 多花约 3,800 个来源请求与约 80 分钟窗口；数据本身无害（`full` 是幂等整表重抓）。**建议 Skill §3.0 加前置判断：读上次 `full` 的 `source_refresh_runs.started_at`，若距今天 <7 天则跳过并记录「本周已跑」**（#625；不改代码） | VM2-11 偏差 4 |
| **D-87 的 `tail` 复用已随 preview.47 部署（2026-11-09T10:45Z），但尚未在生产实测**：部署任务刻意未跑 `tail`，留待 **2026-11-10 日更**。预期 `szse_etf_notice_ann` 从约 181 个请求降到约 50 个（推断，未验证） | 未实测前不能宣布 G2 的 `tail` 余量改善 | VM2-28、[#604](https://github.com/liqiangcc/x-data-mcp/issues/604)、D-87 |
| 只换二进制（不再每次全新 init）的部署：启动 registry sweep 的耗时随历史任务线性增长（推断），10-23 时已超过 `x-data-mcp-tunnel` 当时 10 秒的就绪等待（`wait-data-service.py`；现为 120s）。两个实测点：**2026-10-23 主机重启后（preview.34，574 个任务，冷 page cache）31.5s**；**2026-10-24T17:25Z preview.35 切换（579 个任务，热 page cache）13.5s**。两点无法区分「任务数」与「缓存冷热」的贡献（推断：后者影响更大），因此「线性增长」本身未得到证实，需专门测量。10-23 那次是首次 `ExecStartPre`（当时 deadline 10s）失败后靠 `Restart=always` 重试成功；10-24T16:56:58Z 起该 deadline 已改为 120s，17:25 切换时一次等到 socket（13.5s），空窗来自 `RestartSec=10` 加就绪时间。缓解（只 sweep 最近 N 天、先开 socket 后台 sweep）会改变「sweep 失败即拒绝启动」的语义，需另行批准。schema 21 的启动检查已改为逐 series 索引点查（S2b F3，不再 loose-scan） | 每次重启/切换首轮必失败，多花一个 `RestartSec`；若重试也失败则长时间不可用 | `journalctl -u x-data-mcp-data/-tunnel`、`stat run/mcp.sock`、`reports/preview35-switch-20261024.json`（`readiness_probe.data_elapsed_ms=13461`） |
| tunnel 启动竞态与重启把 data 拉起（仓库脚本已修，tunnel unit 仍用 /opt 旁路脚本）：2026-10-23 06:49 那次是 `wait-data-service.py` 的 10s deadline 短于 31.5s 冷启动，首轮 `ExecStartPre` 失败、靠 `Restart=always` 在 10s 后重试成功；`/opt` 内该脚本已于 2026-10-24T16:56:58Z 把 deadline 改为 120s。**2026-11-01 实测的新一面**：tunnel 的 drop-in 带 `Wants=`/`After=x-data-mcp-data.service`，只停 data+acquire 的窗口里 systemd 10 s 后重启 tunnel，并把 data 拉起（10:22:19Z），导致正在跑的 replay 报 `database service already active`；该窗口已改为三单元全停重跑（XDM-159）。仓库脚本的停/起顺序与回滚路径已改为先停 tunnel、最后起它（XDM-160），但**运维文档的窗口步骤必须同步**，不要只按旧文档停 data+acquire。历史：#370（`8e83f22`，2026-10-24 合入 main）新增 `scripts/wait_service_ready.py`，两个部署脚本已调用；tunnel 的 `ExecStartPre` 仍调 `/opt` 上的 `wait-data-service.py`（不在仓库、不受版本控制，D-17 只部分落地） | 重启/切换时的隧道空窗由 `RestartSec=10` + 就绪时间决定；只停两个单元的 >10s 窗口会被 tunnel 重启破解（包括写库任务）；若单元连续失败触到 `StartLimitBurst=5`，会停在 `failed` 需人工介入（未实测，推断） | `journalctl -u x-data-mcp-tunnel`（10-22、10-23、10-24、11-01）、XDM-159 报告、`grep deadline /opt/x-data-mcp-preview/wait-data-service.py`、`systemctl show -p ExecStart --value x-data-mcp-tunnel`、PR #370 |
| 报告覆盖不均：preview.33 / preview.34 没有报告；preview.35 的是切换后人工整理的（7 条 check 名称、无逐项证据）。自 #296 P1-3 起 `switch_binary.sh --execute` 写机器报告（`--report`，默认 `reports/<short-tag>-switch-<UTC>.json`，含 P 与 S1–S7 逐项 `{step,status,elapsed_ms,evidence}`、探针 JSON 与回滚结果）。本轮已看到该机制生效：preview.40 窗口的 `…003800Z.json` 记了 `status: FAILED` / `failed_at: S2` / `rollback_outcome: not_needed`，重跑的 `…003814Z.json` 全 PASS；preview.39 的 phase-g 报告也是机器产出（含 `archived_db`） | preview.33/34 的 S1–S6 逐项结果永久不可复核，只能引用可观测结果（链接 mtime、单元 uptime、live version）；以后每次切换都有逐项报告 | `ls -l reports/`、`cat reports/preview40-switch-*.json`、`scripts/switch_binary.sh`、`scripts/test_switch_binary.py` |
| 任务簿记（`frozen_work_json`、`requests.command_json`、`shared_evidence`）每年约增长 10–16GB（每天约 106 个任务） | 只换二进制的长期运行会逼近「部署体积 ≤8GiB（目录口径，2026-10-26 签字）」；降低需要改存储格式并定留存策略 | S7 |
| `market_events` 事件日志无界增长（RULINGS Q9 已接受）：按当时 5,224 条序列估计 ~5,224 行/日 ≈ 1MB/日（S5 实测 26,118 行 / dbstat ~16MB 含索引）；当前 5,561 条序列，按同口径推断 ~5,561 行/日（本轮实测库内共 32,044 行） | 长期运行累积；与任务簿记增长分开记录 | RULINGS Q9、S5、本轮 `market_events` 只读计数 |
| **schema 20 历史**（现行 schema 21 无此机制）：rollover 校验放宽后，回滚到旧二进制时，其对早 rollover 结果行的完整校验会报「requires depth-256 base」；已有 >127 深的链升级后首日会集中 rollover 一次 | 只影响仍运行 schema 20 的旧库/旧二进制回滚 | XP-051；`grep -rn rollover src/` 无命中，`src/sqlite_market/manifest.rs` 只服务 legacy backend（`src/sqlite_market/backend.rs:31-51`） |
| **schema 20 历史**（现行 schema 21 无版本链/链深）：年线最坏链深余量薄（约 2–9%），宿主为 t3.small，噪声约 ±10%，冷缓存首批明显偏慢 | 仅适用于 schema 20 版本链；schema 21 年线按当前日线序列重派生 | XP-045b/047/048/052；`src/sqlite_market/backend.rs:31-51` |
| aws `acquire.sqlite` 中 15 个 evidence_only 代已无代字节（本轮实测各 4,055–4,119 B：`868f4fa1`、`804c5a63`、`04c28d88`、`84b3f15d`、`c360967a`、`2e918330`、`a49c314b`、`87322997`、`2d5851a4`、`a119629b`、`8b8614c3`、`54553656`、`f289c612`，以及 2026-11-01 退役的 `9d9e38d7`、`c184e9c4` 各 4,055 B）。其中 `2d5851a4`（10-24T16:57Z）与 `a119629b`（10-24T17:52Z）是退役时才清字节，`8b8614c3`/`54553656`/`f289c612` 是 2026-10-28T11:56Z 自动保留时清；其余在更早退役时就已无字节 | 采集台账有残留；这些代不计入热容量，但目录与清单仍占少量空间 | `du -sb generations/generations/*`、`generations/acquire.sqlite` |
| `/opt/x-data-mcp-preview/mcp.json` 的 `x-data-mcp-demo` 指向一个不存在的 sqlite | 配置过时 | `ls` |
| `src/acquire/fetch.rs` 在未设置 `X_DATA_MCP_TDX_FETCH` 时，回退到编译期的 `CARGO_MANIFEST_DIR` | 历史：preview.35 及更早不带该变量启动的实例无法下载。main 已修（#375）：未设置时只认包内 `<exe dir>/../scripts/tdx_fetch.py`，找不到即启动失败并报出变量名与路径；preview.36 构建自 `fcb86ab`（含 #375），本轮未实测该回退路径 | 源码、#375 |
| 生产服务、私有性能栈和多个 agent 会话共用 1 个物理核和 1.9GiB 内存；`/tmp` 是内存盘 | 2026-10-10 约 04:34Z 过载重启；测量结果有噪声 | 运维观察 |
| 热 generation 上限 6：超限后 `acquire_generation` 被拒（`generation_capacity`）。历史（aws-node 2026-11-02 实测）：本轮实测 **2/6**（1 active `f2fd6fcb` + 1 superseded `0f1623b0`；历史：aws-node 冻结时点为 3/6；历史：vm2-126 第一次生产日更后为 4/6，见「数据」表），下一次 acquire 可直接受理。稳态**预期**为 3/6（active + 2 个保留的 superseded），还剩 3 个余量。历史：2026-10-24 16:56Z 为 6/6 → 退役 `2d5851a4` 降到 5/6 → 17:01:34Z 新代 `c184e9c4` 激活回到 6/6 → 17:52Z 再退役 `a119629b` 到 5/6 → 2026-11-01 退役两代遗留代到 2/6。容量是**计数**，但退役一个有字节的代同时释放空间（约 570MB/代；本轮 2 代共释放 ≈1.065 GiB）。自动保留策略（`RETAINED_SUPERSEDED=2`）**已随 preview.35 部署**（2026-10-24T17:25Z），在 activate 提交成功后生效 | 跨代日更会消耗热代名额 | 2026-11-01 实测（`generations/acquire.sqlite`、`du -sb`、`df /`）、`src/acquire/service.rs:40,44` |
| **旧代回收被证据 gate 卡住**（**已处理**，2026-11-01T10:25–10:26Z，XDM-159）：`9d9e38d7`、`c184e9c4` 两个遗留代曾经 `source_bindings=0`、`generation_facts=0`，退役报 `source_store_evidence_missing`；按方案 A（D-65：补登记 generation 成员 → `replay --scope generation_facts` 重建 18 行 → 每代 3 类行情派生事实逐条记豁免，共 6 条 → `retire_generation`）现均为 `evidence_only`，未用 `--owner-approve-evidence`。门禁规则未改；`0f1623b0` 仍可用现有来源库通过 gate，按 `RETAINED_SUPERSEDED=2` 在后续激活时自动回收 | 已解除：热代 4/6 → 2/6，余量从 1 个恢复到 3 个。退役不可逆（代字节已删） | XDM-146 报告、XDM-159 报告、`generation_facts`/`retire_exemptions` 只读查询 |
| `market.risk_warnings` 历史缺口（**已解除**）：第 1 代日更的 `derived:risk_warnings` 曾失败（`source_invalid: page_count is 0`，空季度 `pageCount=0`）；修复 #485（XDM-148）与解析口径 #489（XDM-152）随 preview.41 部署后，11-02 fresh init 的 `replay --scope risk_warnings` 写入 **1,289 行**（`known_at=2026-11-01`），preview.42 首次 `full` 又暴露 `SameDayActionConflict`（`sh.600734`）；D-68 分类修复（#503）**已随 preview.43 上线**（只换二进制），11-03 日更首次发布沪市区间 | 已解除：读面自 11-02 fresh init 起不再 absent；新规则区间在 11-03 日更写入 | XDM-159/XDM-165/XDM-170 报告、`task_items.outcome_json`、D-68、`current/manifest.json` |
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
4. **CI 检查现状（2026-11-01 复核，P1-2/#296）。** `scripts/check_current_facts.py` 检查构建事实（包版本、两套 schema、
   data/acquire 工具数、capability revision）、5 个 `CURRENT_DOCS` 当前区域、8 个 `CURRENT_REGIONS`
   当前区域（含 `data-catalog.md`）、`docs/contracts/*.md` 快照、`docs/decisions.md` 状态列，以及本文件的内部一致性（热 generation 计数与拆分、
   data/acquire 工具数、部署版本）。快照检查用于文档的「已部署 schema」取本文件的权威行，
   不再写死在测试里（部署后改写本行即可）。`scripts/check_deploy_facts.py` 的覆盖情况：
   - 「本文件声明的版本须与最近一份已提交的记录一致」：**未实现**。目前只校验版本串形式（ `vX.Y.Z-preview.N` ）。
     preview.35 的报告在 `/opt/x-data-mcp-preview/reports/` 内、不进仓库，且是切换后人工整理的（#296 P1-3 之前
     `switch_binary.sh` 不写报告；现在 `--execute` 会写 `reports/<short-tag>-switch-<UTC>.json`）。就绪修复已随
     #370（`8e83f22`）合入 main；要实现这条检查，仍需先把报告是否随仓库提交定下来。
   - `CURRENT_DOCS` 的当前区域不得把已退役主机写成当前主机：**已实现**（`check_current_host`）。
   - 「最后核实」日期超过 14 天时告警：**已实现**（`STALE_DAYS = 14`）。
