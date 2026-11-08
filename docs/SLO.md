# 性能与容量目标（SLO）

本文件是性能与容量目标的唯一入口，取代 README、`docs/daily-sync.md` 和各 Issue 中
零散的阈值表述（5min/16MiB、15min 等）。部署事实见 [STATUS](STATUS.md)。

每一行都标明状态：**hard**（owner 签字的硬门禁）、**baseline**（只记录实测，不作门禁），或 **待定**。

## 1. 目标表

| # | 场景 | 门禁 | 状态 | 最新实测（口径见备注） |
|---|---|---|---|---|
| 1 | 同代无变化，全市场 `update_data` | write-loop <600s；checkpoint 后净增长 ≤4KiB/attempted item；`data_versions +0` | **hard**。时间项 2026-10-10 签字，取代 2026-09-28 的 ≤15min；增长和版本两项沿用 2026-09-28（#259）；**2026-10-28 已解除（D-54 解除，见 XDM-84 实测）**（见备注与 D-57） | preview.27 / ecs：5,222 items，616s，+18.5MB，0 个新版本。perf 分支 / aws 私有栈：write-loop 433.8s（该构建当时因正确性问题被挂起，仅供参考）。**perf `3b11231` / aws 私有栈（t3.small），2026-10-11，浅链，53 批：write-loop 78.0s，5,224 no_change，`data_versions` +0，净增长 +18.55MB（≈3.47KiB/item），读回 PASS（XP-015，单次）**。**v6/schema-21（feat e22bafc，generation 87322997，aws 私有栈 t3.small，n=1，噪声约 ±10%）：write-loop 70.9s（k=5）/ 74.5s（k=1 对照），5,224 no_change；no_change 周期库增量 +17.8MB ≈ 3.4KiB/item**。**v2/M1 + O3（main `0f3fe25`，单进程三 socket，2026-10-28，2 vCPU，load average 4–8 最高 8.12，非完全独占，w=1，含 research 并发）：同代无待更新 write-loop 87.6s；同代全量 5,238 item（显式 `end_date` 构造，口径见备注）write-loop 573.4s（<600s），`succeeded` 5,238/5,238，data_service VmHWM 304.3 MiB，MemAvailable 最低 419.4 MiB** |
| 2 | 跨代有变化，全市场 `update_data` | **G1** 日更 write-loop <600s（`run_market_update`，**不含来源刷新任务**）；**G2** 来源刷新：`tail` <300s（仅限未触发整表的 tail）、触发整表时**每张被触发的表各自 <600s**（run 总量不设门禁）、`full` 按来源组计量：原有 7 来源合计 <60min、ETF 4 来源合计 <40min、run 总量不设门禁（**D-73**）（**G2 在 R2 上线前不可测**；每表口径由 **D-67** 于 2026-11-01 裁定，读 `source_refresh_run_sources.elapsed_ms`） | **hard**（门槛 2026-10-10 签字，G1 沿用；**G2 2026-11-01 由 D-66 签字，R2 上线前不可测**）；**2026-10-27 临时豁免，2026-10-28 独占实测后保留**（见备注与 D-52）；**2026-10-28 解除条件改为看生产：连续 3 次生产跨代日更（有新 generation 激活）G1 write-loop 均 <600s，单次 ≥600s 计数清零**（见 D-63）；**2026-11-01 拆 G1/G2（D-66），D-52 豁免与 D-63 计数规则不变，从 preview.41 上线起重新计数**；**2026-11-05 生产机换成 vm2-126（D-74），D-63 的连续 3 次计数从 0 重新开始，从 vm2 第一次生产跨代日更算起**；**计数 2/3（2026-11-05 vm2 第一次 175.3s、2026-11-06 vm2 第二次 154.3s，G1 write-loop 均 <600s）** | preview.30 / aws 私有栈：E2E 1,484.6s。ecs 生产 2026-10-08：1,613s。**perf `3b11231` / aws 私有栈，2026-10-11，浅链，53 批，合成截断夹具：write-loop 104.2s，5,224 published，读回 PASS（XP-015，单次）**；日线 rollover 单条约 0.26–0.30s（错开后约 21–25s/日）。**v6/schema-21（同上口径）：write-loop 97.4s（k=5）/ 93.5s（k=1），5,224 published；追加周期库增量约 +27MB**。**v2/M1（代码 `84b6657`，私有栈，2026-10-27，2 vCPU，load average 约 6，非独占）：`plan_market_update`/`run_market_update` 跨代日更 write-loop 735s（超 600s，按 2026-10-27 裁决临时豁免）**。**v2/M1 + O3（同第 1 行环境与口径：main `0f3fe25`，单进程三 socket，w=1，2026-10-28，load average 最高 7.86，非完全独占）：跨代全量 5,238 item write-loop 734.0s（仍超 600s，24 项 `RevisionUnverified`），data_service VmHWM 293.2 MiB；acquire 切代（删决策工件后真跑 verify，12.88s）VmHWM 33.3 MiB，复用决策工件时 5.9 MiB（XDM-64 同口径 479.8 MiB，#435 后 −93%）**。**vm2-126 / preview.45 生产跨代日更（2026-11-05，`4c17de16…`→`c642468f…`，2 vCPU，非独占、窗口内没有其他数据任务）：G1 `run_market_update`（不带 `scope`，1 个 task `task-dea5f97c…`）5,241 项，5,235 成功、6 个 `REVISION_UNVERIFIED`，write-loop 175.3s（<600s）；6 项补漏 5.1s，6/6 成功**（Claude 2026-11-05 只读核实）。**vm2-126 / preview.45 生产跨代日更（2026-11-06，`c642468f…`→`2731a664…`，2 vCPU，非独占、窗口内没有其他数据任务）：G1 `run_market_update`（不带 `scope`，1 个 task `task-e7c1a907…`）5,241 项，5,231 成功、10 个 `REVISION_UNVERIFIED`，write-loop 154.3s（<600s）**（VM2 只读查询 2026-11-08） |
| 3 | 全市场年线 `update_data` | write-loop <600s | **hard**（2026-10-10 签字）；**2026-11-02 临时豁免（D-69）**：有效到方案 A 上线后连续 3 次生产日更的年线 write-loop 均 <600s 为止，任何一次 ≥600s 计数清零并附原因分析；门槛本身不改（见备注与 D-69）；计数规则不变、只算生产日更（2026-11-05 起生产日更在 vm2-126，D-74）；**计数 2/3（2026-11-05 vm2 第一次 127.9s、2026-11-06 vm2 第二次 89.6s，年线 write-loop 均 <600s）** | aws 私有栈：E2E 4,021s，其中服务端窗口合计 702s。aws 生产重建：服务端合计 603s，E2E 约 45min。**perf `3b11231` / aws 私有栈，2026-10-11，浅链，53 批驱动（5,224 项分 53 个任务、每任务约 100 项）：年线首次 310.6s、日线变化后重聚合 328.3s，读回 PASS（XP-015，单次）。最坏链深（≤127）为分布外推：批中位 10.3s，×53 ≈ 548–588s，未做全量实测**。**v6/schema-21（同 53 批驱动口径）：年线首次 222.4s（k=5）/ 217.2s（k=1），重聚合 233.3s / 234.8s，5,224；重聚合周期库增量约 +27MB——每个日更周期库增量约 54MB（追加 27MB + 年线重聚合 27MB）**。**生产单任务形态（一个 task 装 5,223 项，带 `comparison` + `expected_yearly_head`）：preview.41 2026-11-01 write-loop **729.2s**；preview.42/schema 26 2026-11-02 **791.89s**（均超 600s；两次数据工作量与 2026-11-02 部署重建相同：5,223 项 / 74,504 年 / 逐项年份直方图相同；根因是 `tasks.frozen_work_json` 每项整行重写 2 次，见 XDM-169 报告 §3(e)/§4）**。**vm2-126 / preview.45 生产日更（2026-11-05，跨代，单任务形态：1 个 task `task-85edf8f8…`，5,205 项，非独占、窗口内没有其他数据任务）：5,205/5,205 成功，write-loop 127.9s（<600s）；25 项补推 1.0s，25/25 成功**（Claude 2026-11-05 只读核实）。**vm2-126 / preview.45 生产日更（2026-11-06，跨代，单任务形态：1 个 task `task-6970fa8e…`，5,196 项，非独占、窗口内没有其他数据任务）：5,196/5,196 成功，write-loop 89.6s（<600s）**（VM2 只读查询 2026-11-08） |
| 3a | 年线服务端窗口子目标 | — | **待定**，等年线服务端分阶段计时完成后再定 | — |
| 4 | 首次全市场采集 | ≤20min；首采后 DB ≤2.5GiB | hard（沿用） | aws 私有栈：346.4s。aws 生产重建：服务端合计 452s。**v6/schema-21（同上口径）：collect write-loop 200.3s（k=5）/ 208.8s（k=1），5,228 considered / 5,224 succeeded / 4 failed（官方缺成员，预期）** |
| 5 | 部署体积 | ≤**8GiB**（目录口径） | **hard**（2026-10-26 签字；口径 = `/opt/x-data-mcp-preview` 完整部署目录）。原「≤4GiB」为历史值，保留在 §4 与备注 | 2026-10-24T17:55Z baseline（preview.35 切换 + 清理后）：`/opt/x-data-mcp-preview` 全树 4.2GiB（清理前 6.2GiB）= generations 2.7GiB（5 个带字节代）+ state 1.2GiB（当前库 1,185,640,448 B + WAL 0 B）+ releases 348MiB（6 个版本目录）+ reports 2.8MiB；两份 v20 归档库已移出到 `/root/x-data-archive/` 并 gzip（709MiB），`/tmp` tmpfs 另用 19MB，均不计入。分项见 [STATUS](STATUS.md)「足迹」。历史：按 2026-10-26 之前的 ≤4GiB 门禁，这 4.2GiB 属于超标（当时口径待定） |
| 6 | 已完成 100 条任务的 `get_task_progress` | 中位 <1s，且每个样本 <2s | 回归小门（不是 SLO） | perf 分支 `ef4fea4`：中位 0.108s / 0.091s，最大 0.894s，PASS（未部署） |

备注：
- 2026-10-11 的全市场验收（XP-015）每个负载只跑了单次（n=1）；测量主机为 t3.small，噪声约 ±10%。第三类「保留的真实 mixed Append/Full/fail-closed」**未运行**（retained gen-1 字节永久不可得），见 `FINAL-G8-PERFORMANCE.md`。
- 第 1–3 行的实测来自不同主机（ecs / aws）、不同代码版本和不同负载，**不能直接相互比较**。
- 第 1–3 行的门禁对生产生效的前提是：满足门禁的代码已经合并并部署。私有栈上的结果不等于生产结果。
- **2026-10-27 第 2 行临时豁免**：v2 的 `plan_market_update`/`run_market_update` 跨代日更在私有栈上实测 write-loop **735s**（代码 `84b6657`，2 vCPU，测量时 load average 约 6，**不是独占测量**），高于 600s 门槛。豁免只覆盖第 2 行、**不改门槛**，有效到「v2 日更分块并发」这项性能任务完成并重测低于 600s 为止；到期后按重测结果判定（[DECISIONS](decisions.md) D-52）；**2026-10-28 独占实测后豁免保留，解除条件见下条**。
- **2026-10-27 第 1 行临时豁免**：v2 的 `plan_market_update`/`run_market_update` 同代日更在私有栈上实测 write-loop **632.5s**（代码 `6e78888`，2 vCPU，测量时 load average 6–10，**不是独占测量**），高于 600s 门槛。豁免只覆盖第 1 行、**不改门槛**，到期条件与第 2 行相同：有效到「XDM-64 v2 分块并发」完成并重测低于 600s 为止（[DECISIONS](decisions.md) D-54）；**2026-10-28 独占实测后豁免保留，解除条件见下条；当日随后按 XDM-84 解除，见第 1 行豁免解除那条与 D-57**。
- **2026-10-28 独占实测（XDM-64，preview.37，2 vCPU；测量时 load average 4–8，按 §2 不算完全独占窗口）**：owner 裁决生产保持 `X_DATA_MCP_RUN_PREPARE_WORKERS=1`（1 线程），第 1、2 行的 2026-10-27 临时豁免**都保留**，门槛本身不变；解除条件改为「跨代日更准备侧优化（O3，见 issue-383-xgen-write-path-analysis.md §3）后独占重测 <600 s」。write-loop 实测：第 1 行场景（同代、无待更新，即生产日常）w1 **93.8s** / w2 **91.4s**；同代、全量 5,213 项 w1 **619.5s** / w2 **515.5s**（强制全量不属于第 1 行的「无变化」场景，仅作参考）；第 2 行场景（跨代、全量）w1 **822.4s** / w2 **661.0s**，其中 24 项 `RevisionUnverified` 与 worker 数无关。内存：w2 的 data_service VmHWM 469–554 MiB、MemAvailable 最低 322 MiB（距 §2 的 300MB 停止线很近）；acquire 切代峰值约 480 MiB，贴到其 480 MiB cgroup 上限（[DECISIONS](decisions.md) D-52、D-54）。
- **2026-10-28 第 1 行豁免解除（XDM-84，main `0f3fe25`，单进程三 socket，`X_DATA_MCP_RUN_PREPARE_WORKERS=1`，2 vCPU；测量时 load average 4–8、窗口最高 8.12，**不是完全独占**，且同时跑 research 全市场查询）**：第 1 行场景同代无待更新 write-loop **87.6s**；同代全量 **5,238 item（5,224 entity + 14 派生）573.4s < 600s**，终态 `succeeded` 5,238/5,238，data_service VmHWM **304.3 MiB**，MemAvailable 最低 419.4 MiB（未触发 300MiB 停止线）。owner 裁决**解除 D-54**：第 1 行恢复无豁免的 hard 门禁，门槛本身不变（[DECISIONS](decisions.md) D-57）。口径差异：本次全量用显式 `end_date=2026-10-23` 把 5,224 个实体变成 eligible（XDM-64 的 1b 为 `eligible=5,199`、5,213 item），**573.4s 不能直接替换 XDM-64 的 619.5s**；测量时生产仍是 preview.37 的**单 socket** 部署（历史：三 socket 在私有栈上测，随 preview.38 才上生产）。同次跨代重测见下。
- **2026-10-28 XDM-84 第 2 行重测（与上条同一环境与口径）**：跨代全量 5,238 item write-loop **734.0s**（仍高于 600s；24 项 `REVISION_UNVERIFIED`，与 XDM-64 同类；XDM-64 为 822.4s，#439 后 −10.8%，改善不能归因于代码，报告 §3.3 标注为库/缓存/负载差异），data_service VmHWM 293.2 MiB、MemAvailable 最低 364.3 MiB。**D-52 豁免保留**，解除条件于 2026-10-28 改为**「连续 3 次生产跨代日更（有新 generation 激活的日更）的 `run_market_update` 任务记录 write-loop 均 <600 s」**（[DECISIONS](decisions.md) D-63）。**acquire 切代峰值 33.3 MiB**（删决策工件后真 verify，12.88s；复用决策工件时 5.9 MiB），XDM-64 同口径 479.8 MiB，#435 后 −93%。
- **2026-10-28 第 2 行解除条件改为看生产**（[DECISIONS](decisions.md) D-63，owner 裁决）：同日三组数据——**生产**当日跨代日更任务记录 write-loop **526.75s**（preview.38，`c184e9c4`→`0f1623b0`，5,238 item / 5,222 成功 / 16 `REVISION_UNVERIFIED`）；**私有栈对照两臂 535.23s / 535.43s**（同一代际对，非完全独占，load average 中位约 8.7）；同期私有栈**另两次 734.0s 与 750.61s**（同一代际对，非完全独占）**成因未查明**（两次报告均为仓库外报告，2026-10-28）。解除条件由「跨代日更准备侧优化后独占重测 <600s」改为**连续 3 次生产跨代日更（有新 generation 激活的日更）的 `run_market_update` 任务记录 write-loop（`created_at`→`last_progress_at`）均 <600s**，任何一次 ≥600s 计数清零并附原因分析；计数从 2026-10-29 起的下一次生产跨代日更算第 1 次，**2026-10-28 的 526.75s 只作背景、不计入**。门槛本身不变。
- **2026-11-01 第 2 行拆 G1/G2**（[DECISIONS](decisions.md) D-66，owner 裁决）：**G1 日更写循环** =
  `run_market_update` 从 `created_at` 到终态、**不含来源刷新任务**，门槛 **<600s**，D-63 的连续 3 次
  生产计数规则不变（从 preview.41 上线起重新计数）；**G2 来源刷新** = 独立任务
  `refresh_exchange_sources`：`tail` **<300s**、触发整表刷新 **<600s**、`full` **<60min**
  （1,941 请求 × 1.42s ≈ 46min；1 req/s 理论下界约 32min）。**`full` 自 R2b 起是异步的**，
  所以 <60min 量的是 run 记录的 `finished_at − started_at`（或日志里的真实墙钟），不是 MCP
  调用的耗时：调用本身只校验并返回 `running`。**`tail` 自 #582 起也是异步的**，同理 <300s 量的是
  run 记录的 `finished_at − started_at`，不是 MCP 调用耗时；门槛数字不变。
- **2026-11-01 G2 的触发整表改按表计量**（[DECISIONS](decisions.md) D-67，owner 裁定）：
  探针触发整表时，**每张被触发的表各自 <600s**（`szse_listing`、`szse_short_name` 各算一张），
  run 总时长不设门禁。计量列是 schema 26 的 `source_refresh_run_sources.elapsed_ms`（该来源
  从第一个请求到结束的墙钟毫秒，向上取整）。依据：设计 §3 实测 `szse_short_name` 375 页
  377.4s、`szse_listing` 146 页 181.3s，各自都在 600s 内；两表同日触发合计可到 549 请求、
  按 1.42s/请求约 780s，run 总量按 600s 记账会把正常叠加判成违规。
  **G2 在 R2 实现上线前不可测**，
  不参与当前判定；D-52 豁免保留。即使来源获取归零，本代 G1 仍约 609s（实体写入 551.8s +
  日历 44s + 启动 9.8s = 605.6s 下限），G1 转绿依赖实体路径优化，不因拆分而变绿。
- **2026-11-05 `full` 按来源组计量**（[DECISIONS](decisions.md) D-73，owner 2026-11-05
  裁定）：原有 7 个来源（`sse_listing`、`sse_delisting`、`sse_bulletin`、
  `sse_suspension`、`szse_listing`、`szse_delisting`、`szse_short_name`）的
  `source_refresh_run_sources.elapsed_ms` 之和 **<60min**；M3-2 新增的 4 个 ETF 来源
  （`sse_etf_list`、`szse_etf_list`、`sse_fund_delist_ann`、`szse_etf_notice_ann`）之和
  **<40min**；run 总时长不设门禁。两组都是 hard，只读 `status='succeeded'` 的 `full`。
  计量列是 `source_refresh_run_sources.elapsed_ms`，**取代**「`full` 读
  `finished_at − started_at`」这一计量方式（ETF 来源加入后 `full` 共 3,874 请求，
  1 req/s 下理论下界约 64.6min，原 <60min 无法通过；总量放宽会掩盖 A 股来源变慢，
  故分组）。
- **2026-11-05 生产机切换与 vm2 实测（D-74，owner 裁定）**：日更和主数据搬到
  vm2-126（preview.45，`f6de633`，schema 27，`generic-data-v10`；2 vCPU AMD EPYC
  9754，约 7.6 GiB，时区 Asia/Shanghai，单元用 `TZ=UTC`）；tunnel 暂不部署，
  客户端仍走 aws-node 的 preview.43（数据停在 11-04，不再跑日更），aws-node 作
  fallback。**以下为 2026-11-05 在 vm2、preview.45 上的单次实测，同代数据，
  非生产日更样本**（S2–S12 期间只有少量只读查询）：

  | 项 | 结果 |
  |---|---|
  | 首采 A 股 53 批 5,225 实体 | 两段合计 199 s（中间因等裁定停约 7 分 52 秒） |
  | 全市场年线 5,224 项 | write-loop 110 s |
  | 日更形态 A 股 5,241 项（两次） | write-loop 194.0 s、196.0 s |
  | A 股 + ETF 6,872 项 | write-loop 280.0 s |
  | ETF only 1,631 项 | write-loop 37.0 s |
  | 资源 | `MemAvailable` 最低 5.65 GB，data `VmHWM` 394,776 kB，WAL 峰值 16 MiB |

  同脚本单次测量 vm2 优于 aws-node（随机 4K 读 2,834 vs 1,619 IOPS、写入
  364 vs 201 MB/s、`MemAvailable` 约 6.1 GB vs 0.75 GB）。上面是同代样本；
  跨代 G1 的 vm2 读数见下一条（2026-11-05 第一次生产日更）；D-63 的连续 3 次计数从 0 重新开始（aws-node 上
  11-02 那 1 次不带过来），从 vm2 第一次生产跨代日更算起（D-74）；D-69 的
  计数规则不变，也只算生产日更。vm2 是试用机（约 2027-01-04 到期），到期前
  要定去向。
- **2026-11-05 vm2 第一次生产跨代日更（D-63 计数 1/3，D-69 计数 1/3；Claude 2026-11-05 只读核实）**：
  vm2-126、preview.45，generation `4c17de16…` → `c642468f…`（2026-11-05T08:40:27Z 激活，
  激活请求 `daily-20261105-xdm-accept-bg95dq-activate`），2 vCPU，**非独占，窗口内没有其他数据任务**。
  这是**跨代生产读数**（有新 generation 激活），不是前面「同代数据、非生产日更样本」的重复。
  按 §3 任务形态：G1 与年线都是各 1 个 task。

  | 项 | task | 项数 | 结果 | 门禁 |
  |---|---|---|---|---|
  | G1：A 股 `run_market_update`（不带 `scope`） | `task-dea5f97c…` | 5,241 | 5,235 成功，6 个 `REVISION_UNVERIFIED`；write-loop **175.3 s** | 第 2 行 G1 <600s，D-63 计数 **1/3** |
  | 补漏 | `task-2306dc95…` | 6 | 6/6 成功，5.1 s | 不单独计门禁 |
  | ETF 批次 | `task-9dad45b1…` | 1,631 | 1,631/1,631 成功，36.1 s | 不单独计门禁 |
  | 年线 | `task-85edf8f8…` | 5,205 | 5,205/5,205 成功，write-loop **127.9 s** | 第 3 行 <600s，D-69 计数 **1/3** |
  | 年线补推 | `task-3b9f0a6f…` | 25 | 25/25 成功，1.0 s | 不单独计门禁 |
  | `sz.158081` 首采 | `task-37a06a8e…` | 1 | 成功，1 根 bar | — |

  当天没跑 `tail`（05:00–06:13Z 已跑 `full`），所以本次没有 G2 读数。判定仍由 owner 根据原始数据
  计算；这里只记录计数，D-52 与 D-69 的豁免继续有效，直到各自连续 3 次达成。
  运行观察：执行这次日更的模型（SWE 2 Flash）提前结束并漏了步骤，经提示后走完；这是一次运行观察，
  不是对该模型的结论。
- **2026-11-06 vm2 第二次生产跨代日更（D-63 计数 2/3，D-69 计数 2/3；VM2 只读查询 2026-11-08）**：
  vm2-126、preview.45，generation `c642468f…` → `2731a664…`
  （`2731a664546921b8c43b81744ba53dd0a86682c89668f2be640a63e8c65bfc54`，2026-11-06T08:49:30Z 激活，
  激活请求 `daily-sync-20261106-activate-g2pqjr`，`expected_previous=c642468f…`，`VERIFIED_COMPATIBLE`），
  2 vCPU，**非独占，窗口内没有其他数据任务**（当天只有下面 8 个 task，G1 窗口 08:49:46–08:52:20Z 内只有它自己）。
  跨代生产读数（有新 generation 激活）。按 §3 任务形态：G1 与年线都是各 1 个 task，
  write-loop 取 `created_at` → `last_progress_at`。

  | 项 | task | 项数 | 结果 | 门禁 |
  |---|---|---|---|---|
  | G1：A 股 `run_market_update`（不带 `scope`） | `task-e7c1a907…` | 5,241 | 5,231 成功，10 个 `REVISION_UNVERIFIED`；write-loop **154.3 s** | 第 2 行 G1 <600s，D-63 计数 **2/3** |
  | 年线 | `task-6970fa8e…` | 5,196 | 5,196/5,196 成功，write-loop **89.6 s** | 第 3 行 <600s，D-69 计数 **2/3** |
  | 10 只 `REVISION_UNVERIFIED` 重推 | `task-0471bbcf…` | 33 | 23 成功，其余 10 项仍 `REVISION_UNVERIFIED`，38.0 s | 不单独计门禁 |
  | 同 10 只显式区间补写 | `task-45e859d8…` | 10 | 10/10 成功，4.1 s | 不单独计门禁 |
  | ETF 批次 | `task-c8fee63e…` | 1,632 | 1,632/1,632 成功，26.5 s | 不单独计门禁 |
  | 年线补推 | `task-bf22a99a…` | 6 | 6/6 成功，0.5 s | 不单独计门禁 |
  | 年线显式补推 | `task-19c08ffd…` | 28 | 28/28 成功，1.1 s | 不单独计门禁 |
  | `sz.158076` 首采 | `task-670debf9…` | 1 | 成功，0.5 s | — |

  G1 的 10 个失败项都是 `INVALID_MARKET_COMMAND: NEEDS_VERIFICATION: RevisionUnverified([…])`，
  symbols：`sh.600894`、`sh.603279`、`sh.605338`、`sz.000650`、`sz.002275`、`sz.002864`、
  `sz.300741`、`sz.301099`、`sz.301215`、`sz.301300`（与 11-05 的 6 项同类，是改写历史窗口的 fail-closed，
  不是写入失败）。
  当天跑了 3 段 `tail`（09:17:02–09:36:05Z，均 `succeeded`），**没有跑 `full`**，所以本次没有 G2 的
  `full` 读数。3 个 run 的 `finished_at − started_at`（VM2 只读查询 2026-11-08）：

  | run | 起止（Z） | 时长 | requests | changed | 判定 |
  |---|---|---|---|---|---|
  | `refresh-tail-1793956622169890299` | 09:17:02→09:27:44 | 642 s | 602 | 380 | 触发整表（`szse_short_name` 380.0 s <600 s，通过）；run 总量 642 s 不设门禁 |
  | `refresh-tail-1793957264088571449` | 09:27:44→09:31:51 | 247 s | 225 | 1 | 未触发整表，`tail` <300 s 通过 |
  | `refresh-tail-1793957511942187918` | 09:31:51→09:36:05 | 254 s | 225 | 1 | 未触发整表，`tail` <300 s 通过 |

  三段按第 2 行原文判定均达标：第一段触发了整表，适用 D-67 的「每张被触发的表各自 <600 s」——
  `source_refresh_run_sources` 中 `szse_short_name` 该 run 有 376 条成员（页）、`elapsed_ms=379969`
  （即 380.0 s，<600 s）；`szse_listing` 无成员、`elapsed_ms=4214`（仅探针）；run 总量 642 s
  按规则不设门禁。另两段的 `szse_short_name`、`szse_listing` 均无成员（仅探针），即未触发整表，
  适用 `tail` <300 s：247 s、254 s 均达标。
  出现 3 次是因为客户端 60 s 超时后重试，而 worker 一直被占着（#582 的描述；
  会话日志证实：09:16Z 的 daily-sync 会话里 3 次 `refresh_exchange_sources({"mode":"tail"})`
  均为 `error` / `MCP request timed out after 60000ms`，`durationMs` 60,003/60,002/60,002 ms）。
  **G2 不进 D-63 计数**——D-63 只看 G1 write-loop，所以 D-63 2/3 与 D-69 2/3 的结论不变。
  **推断（未验证）**：三段 `tail` 里 `szse_etf_notice_ann` 每次约 181 s（`elapsed_ms`
  181052 / 181018 / 180966，各 181 个请求，按 1 s 出站间隔即固定开销）；未触发整表的 `tail`
  实测 247–254 s，离 300 s 门槛只剩约 50 s 余量，该来源变慢即可能超门禁。
  判定仍由 owner 根据原始数据计算；这里只记录计数，D-52 与 D-69 的豁免继续有效，直到各自连续 3 次达成。
  运行观察：当天第一次启动时 pi 1.3.0 没展开 `/skill:`，提示词缺 Skill 正文，09:16Z 续跑补完 §3.3、
  ETF 批次和 §3.6（#586）；这是编排问题，不影响 G1 的任务记录。
- **G2 的测量入口（runbook）**：G2 只读 schema 26 的 `source_refresh_runs` 与
  `source_refresh_run_sources`，不另设计时器。未触发整表的 `tail` 读
  `source_refresh_runs.finished_at − started_at`；`full` 按来源组读 `source_refresh_run_sources.elapsed_ms` 之和（D-73）；触发整表时读
  `source_refresh_run_sources.elapsed_ms`（`source_id ∈ {szse_listing, szse_short_name}`
  且该来源 `complete=1` 或 `snapshots>1` 的行，每行 <600000 ms，run 总量不设门禁）。
  `requests` 是 `client.attempts()`、包含重试，不能用来判断触发。**门禁只用
  `status='succeeded'` 的 run 判定**；`failed`/`process_restarted` 的 run 不参与门禁判定，
  但测量记录必须逐条列出每个这类 run 的耗时与失败码，不得省略（`running` 尚无
  `finished_at`，只记录状态与 `started_at`，且不推进 `verified_through`）。查询语句、
  如何判断“触发”与失败分诊见[来源刷新 runbook](source-refresh-runbook.md)。**schema 25 的
  生产库没有这三张表，部署 v9 前 G2 没有读数**。
- **2026-11-02 第 3 行临时豁免（D-69，owner 2026-11-02 批准）**：生产日更的单任务全市场年线 write-loop **729.2s（2026-11-01，preview.41）/ 791.89s（2026-11-02，preview.42，schema 26）**，均超 600s。两次数据工作量与 2026-11-02 部署重建相同（5,223 项、74,504 年、逐项年份直方图相同），根因是 `tasks.frozen_work_json`（3.95–5.13 MB）在每项执行时被整行重写 2 次（XDM-169 报告 §3(e)/§4）。豁免**有效到方案 A（年线分块任务簿记，XDM-171）上线后连续 3 次生产日更的年线 write-loop 均 <600s 为止**；任何一次 ≥600s 计数清零并附原因分析。门槛本身不改。**口径**：表中 310.6 / 328.3 / 222.4 / 233.3s 都是 **53 批驱动形态**（5,224 项分 53 个任务、每任务约 100 项），与生产单任务形态（一个 task 装 5,223 项）不可比——单任务下 `tasks` 行的整行重写成本按项数放大，正是本次超标的机制。
- 第 5 行口径已于 2026-10-26 定为**完整部署目录**（`/opt/x-data-mcp-preview` 全树，不含已移出的归档库与 `/tmp`），门禁 **≤8GiB**；上调原因：复权模型选方案 (c) 要物化一份前复权序列，并新增因子/日历/指数/导出元数据，M1 设计复核后目录口径从 4.2GiB 到**约 5.7GiB**（+约 1.5GB，[#383](https://github.com/liqiangcc/x-data-mcp/issues/383) §17 第 5 条）。同日先定的 6GiB 余量太薄（距估算值仅约 0.3GiB，一个代目录就超过），当天改为 8GiB；原 4GiB 为历史值（[DECISIONS](decisions.md) D-21）。导出文件落 `/root/x-data-exports/`，不计入本门禁。
- **历史（2026-10-24，口径未定的时期；以下为原文）：** 第 5 行「部署体积」的测量边界此前未定义，已有记录也不一致：唯一的 PASS 证据 `docs/daily-sync.md:275`
  写的是「preview.28 部署包」（发布包口径），而 `docs/tasks/issue-259-final-storage-design.md:67` 写的是
  「完整部署目录」≤3.5GiB / hard ≤4GiB（目录口径）。发布包与目录相差一个数量级
  （preview.35 的 tarball 为 20,317,354 B，而 2026-10-24T17:26Z 的 `/opt/x-data-mcp-preview` 全树为 6.2GiB）。
  因此当时把口径标为**待定**：2026-10-24 只记录两种口径的 baseline，不自行判定 PASS/FAIL。
  按目录口径当时为 4.2GiB（相对当时的 4GiB 门禁略超）：约 2.7GiB 是 5 个带字节代（active + 4 superseded，各约 572–575MB），
  其中 4 个 superseded 约 2.3GiB 属可回收项（activate 后自动保留会退役最旧的 2 个）；当前库约 1.2GB 与 active 代约 573MB 是工作集。
  两份 v20 归档库（原 1.62GB）已于 2026-10-24T17:51Z 移出 `/opt` 并 gzip 到 709MiB，不再计入目录口径；按发布包口径仍远低于 4GiB。

## 2. 计时口径

| 段 | 定义 | 是否计入门禁 |
|---|---|---|
| write-loop | 用单调时钟计时，从客户端提交第一个写请求，到收到最后一个任务的终态。**任务之间的探测和结果轮询都计入** | **是** |
| preflight | catalog、`get_data_availability` 等写入前的读取 | 否，单独报告 |
| readback | 抽样 `query_data`，与 pinned 版本、年线 lineage 比对 | 否，单独报告 |
| driver total | 驱动的总墙钟 | 否，单独报告 |

- 计数必须全部报告：considered、attempted、published、no_change、failed、needs_verification、absent/empty。unknown 不得算作成功。
- 重放和无变化不得算作有变化的聚合。

## 3. 测量环境

- **必须记录：**
  - 主机、代码 SHA；
  - 二进制 digest（`/proc/<pid>/exe` 与安装包一致）、rustc 版本；
  - DB 大小、页缓存的冷热状态；
  - uptime、free、MemAvailable 最小值，以及按 RSS 排序前 10 的进程。
- **独占（2026-10-10 签字）：** 门禁测量期间暂停 agent 会话。测量窗口内，不运行 cargo、编译或其他重负载，只保留生产服务、私有栈和测量驱动。窗口内出现其他重负载时，对应样本作废并如实记录，不得静默剔除。
- **停止线：** MemAvailable 低于 300MB 或磁盘可用低于 5GB 时，立即停止。
- **样本：**
  - 全市场门禁至少完整运行 1 次，不预热、不过滤、不重试；
  - 小门每个任务 10 个串行样本，冷启动的第 0 个样本计入。
- **任务形态（2026-11-02，D-69）：** 任何 write-loop 记录必须写明 **task 数与每个 task 的项数**。53 批驱动（每 task 约 100 项）与生产单任务（一个 task 装全部约 5,223 项）不可直接横比：`tasks` 行的整行重写成本随单 task 项数放大，单任务形态才是生产日更的年线口径。
- **判定：** 由 owner 根据原始数据计算；报告不得自行判定通过。只有二进制来自 CI 或 Release 构建时，测量结果才有效。

## 4. 签字记录

| 日期 | 决定 | 决定人 | 记录 |
|---|---|---|---|
| 2026-09-28 | 同代无变化 hard ≤15min、≤4KiB/item、`data_versions +0` | owner | #259 |
| 2026-10-10 | 第 1–3 行统一为 write-loop <600s hard，15min 退役；门禁测量期间暂停 agent | owner（用户当面决定，由 Claude Code Owner 会话记录） | 本 PR |

| 2026-10-11 | 接受验收第三类（真实 mixed Append/Full/fail-closed）为 NOT_RUN，以 `AGENTS.md` 记载的离线 harness 结果作旁证、不作替代 | owner（用户当面决定，由 Claude Code Owner 会话记录） | FINAL-G8-PERFORMANCE.md |
| 2026-10-11 | 批准错开的提前全量重写（每实体阈值 64–127；rollover 校验放宽为父深度 64..=256）；部署继续采用全新重建 | owner（同上） | PR #323 |
| 2026-10-26 | 第 5 行部署体积门禁由 ≤4GiB 上调为 **≤8GiB（目录口径）**；原 ≤4GiB 标为历史并保留。理由：复权模型选方案 (c) 物化前复权序列并新增因子/日历/指数/导出元数据，M1 设计估算部署后约 5.7GiB（+约 1.5GB），先定的 6GiB 余量太薄 | owner（同意按 Claude 推荐，以本 PR 合并为准） | 本 PR；[DECISIONS](decisions.md) D-21、[#383](https://github.com/liqiangcc/x-data-mcp/issues/383) §17 第 5 条 |
| 2026-10-27 | 第 2 行**临时豁免**：v2 跨代日更 write-loop 实测 **735s**（私有栈，代码 `84b6657`，2 vCPU，load average 约 6，非独占）高于 600s；豁免到「v2 日更分块并发」完成并重测 <600s 为止，**门槛本身不变** | owner | [DECISIONS](decisions.md) D-52、[#383](https://github.com/liqiangcc/x-data-mcp/issues/383) |
| 2026-10-28 | 第 1、2 行临时豁免**保留**：XDM-64 独占实测（preview.37，2 vCPU，load average 4–8，非完全独占，按 §2 不算独占窗口）第 1 行场景 w1 93.8s / w2 91.4s、第 2 行场景 w1 822.4s / w2 661.0s；生产维持 `X_DATA_MCP_RUN_PREPARE_WORKERS=1`，解除条件改为「跨代日更准备侧优化（O3，见 issue-383-xgen-write-path-analysis.md §3）后独占重测 <600 s」 | owner | [DECISIONS](decisions.md) D-52、D-54；[#383](https://github.com/liqiangcc/x-data-mcp/issues/383) |
| 2026-10-28 | 第 1 行**解除豁免**：XDM-84 实测（main `0f3fe25`，单进程三 socket，w=1，2 vCPU，load average 4–8、最高 8.12，非完全独占，含 research 并发）同代无待更新 87.6s、同代全量 5,238 item **573.4s <600s**（VmHWM 304.3 MiB；显式 `end_date` 构造，口径与 XDM-64 的 1b 不同，不能直接替换 619.5s）；第 1 行恢复无豁免的 hard 门禁，门槛不变。第 2 行跨代重测 **734.0s** 仍超 600s，**D-52 豁免保留**；acquire 切代 VmHWM 33.3 MiB | owner | [DECISIONS](decisions.md) D-57、D-52；[#383](https://github.com/liqiangcc/x-data-mcp/issues/383) |
| 2026-11-02 | 第 3 行**临时豁免**（D-69，照 D-52 先例）：生产单任务全市场年线 write-loop **729.2s**（11-01）/ **791.89s**（11-02）超 600s；有效到方案 A（XDM-171）上线后连续 3 次生产日更的年线 write-loop 均 <600s 为止，任何一次 ≥600s 计数清零并附原因分析；门槛不改；310.6 / 328.3 / 222.4 / 233.3s 均标注为 53 批驱动形态、与生产单任务不可比 | owner | [DECISIONS](decisions.md) D-69、[SLO](SLO.md) 第 3 行；XDM-169 报告（仓库外，2026-11-02） |

待定：第 3a 行，即年线服务端窗口的子目标。
