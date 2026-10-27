# 性能与容量目标（SLO）

本文件是性能与容量目标的唯一入口，取代 README、`docs/daily-sync.md` 和各 Issue 中
零散的阈值表述（5min/16MiB、15min 等）。部署事实见 [STATUS](STATUS.md)。

每一行都标明状态：**hard**（owner 签字的硬门禁）、**baseline**（只记录实测，不作门禁），或 **待定**。

## 1. 目标表

| # | 场景 | 门禁 | 状态 | 最新实测（口径见备注） |
|---|---|---|---|---|
| 1 | 同代无变化，全市场 `update_data` | write-loop <600s；checkpoint 后净增长 ≤4KiB/attempted item；`data_versions +0` | **hard**。时间项 2026-10-10 签字，取代 2026-09-28 的 ≤15min；增长和版本两项沿用 2026-09-28（#259）；**2026-10-27 临时豁免**（见备注与 D-54） | preview.27 / ecs：5,222 items，616s，+18.5MB，0 个新版本。perf 分支 / aws 私有栈：write-loop 433.8s（该构建当时因正确性问题被挂起，仅供参考）。**perf `3b11231` / aws 私有栈（t3.small），2026-10-11，浅链，53 批：write-loop 78.0s，5,224 no_change，`data_versions` +0，净增长 +18.55MB（≈3.47KiB/item），读回 PASS（XP-015，单次）**。**v6/schema-21（feat e22bafc，generation 87322997，aws 私有栈 t3.small，n=1，噪声约 ±10%）：write-loop 70.9s（k=5）/ 74.5s（k=1 对照），5,224 no_change；no_change 周期库增量 +17.8MB ≈ 3.4KiB/item** |
| 2 | 跨代有变化，全市场 `update_data` | write-loop <600s | **hard**（2026-10-10 签字）；**2026-10-27 临时豁免**（见备注与 D-52） | preview.30 / aws 私有栈：E2E 1,484.6s。ecs 生产 2026-10-08：1,613s。**perf `3b11231` / aws 私有栈，2026-10-11，浅链，53 批，合成截断夹具：write-loop 104.2s，5,224 published，读回 PASS（XP-015，单次）**；日线 rollover 单条约 0.26–0.30s（错开后约 21–25s/日）。**v6/schema-21（同上口径）：write-loop 97.4s（k=5）/ 93.5s（k=1），5,224 published；追加周期库增量约 +27MB**。**v2/M1（代码 `84b6657`，私有栈，2026-10-27，2 vCPU，load average 约 6，非独占）：`plan_market_update`/`run_market_update` 跨代日更 write-loop 735s（超 600s，按 2026-10-27 裁决临时豁免）** |
| 3 | 全市场年线 `update_data` | write-loop <600s | **hard**（2026-10-10 签字） | aws 私有栈：E2E 4,021s，其中服务端窗口合计 702s。aws 生产重建：服务端合计 603s，E2E 约 45min。**perf `3b11231` / aws 私有栈，2026-10-11，浅链，53 批：年线首次 310.6s、日线变化后重聚合 328.3s，读回 PASS（XP-015，单次）。最坏链深（≤127）为分布外推：批中位 10.3s，×53 ≈ 548–588s，未做全量实测**。**v6/schema-21（同上口径）：年线首次 222.4s（k=5）/ 217.2s（k=1），重聚合 233.3s / 234.8s，5,224；重聚合周期库增量约 +27MB——每个日更周期库增量约 54MB（追加 27MB + 年线重聚合 27MB）** |
| 3a | 年线服务端窗口子目标 | — | **待定**，等年线服务端分阶段计时完成后再定 | — |
| 4 | 首次全市场采集 | ≤20min；首采后 DB ≤2.5GiB | hard（沿用） | aws 私有栈：346.4s。aws 生产重建：服务端合计 452s。**v6/schema-21（同上口径）：collect write-loop 200.3s（k=5）/ 208.8s（k=1），5,228 considered / 5,224 succeeded / 4 failed（官方缺成员，预期）** |
| 5 | 部署体积 | ≤**8GiB**（目录口径） | **hard**（2026-10-26 签字；口径 = `/opt/x-data-mcp-preview` 完整部署目录）。原「≤4GiB」为历史值，保留在 §4 与备注 | 2026-10-24T17:55Z baseline（preview.35 切换 + 清理后）：`/opt/x-data-mcp-preview` 全树 4.2GiB（清理前 6.2GiB）= generations 2.7GiB（5 个带字节代）+ state 1.2GiB（当前库 1,185,640,448 B + WAL 0 B）+ releases 348MiB（6 个版本目录）+ reports 2.8MiB；两份 v20 归档库已移出到 `/root/x-data-archive/` 并 gzip（709MiB），`/tmp` tmpfs 另用 19MB，均不计入。分项见 [STATUS](STATUS.md)「足迹」。历史：按 2026-10-26 之前的 ≤4GiB 门禁，这 4.2GiB 属于超标（当时口径待定） |
| 6 | 已完成 100 条任务的 `get_task_progress` | 中位 <1s，且每个样本 <2s | 回归小门（不是 SLO） | perf 分支 `ef4fea4`：中位 0.108s / 0.091s，最大 0.894s，PASS（未部署） |

备注：
- 2026-10-11 的全市场验收（XP-015）每个负载只跑了单次（n=1）；测量主机为 t3.small，噪声约 ±10%。第三类「保留的真实 mixed Append/Full/fail-closed」**未运行**（retained gen-1 字节永久不可得），见 `FINAL-G8-PERFORMANCE.md`。
- 第 1–3 行的实测来自不同主机（ecs / aws）、不同代码版本和不同负载，**不能直接相互比较**。
- 第 1–3 行的门禁对生产生效的前提是：满足门禁的代码已经合并并部署。私有栈上的结果不等于生产结果。
- **2026-10-27 第 2 行临时豁免**：v2 的 `plan_market_update`/`run_market_update` 跨代日更在私有栈上实测 write-loop **735s**（代码 `84b6657`，2 vCPU，测量时 load average 约 6，**不是独占测量**），高于 600s 门槛。豁免只覆盖第 2 行、**不改门槛**，有效到「v2 日更分块并发」这项性能任务完成并重测低于 600s 为止；到期后按重测结果判定（[DECISIONS](decisions.md) D-52）。
- **2026-10-27 第 1 行临时豁免**：v2 的 `plan_market_update`/`run_market_update` 同代日更在私有栈上实测 write-loop **632.5s**（代码 `6e78888`，2 vCPU，测量时 load average 6–10，**不是独占测量**），高于 600s 门槛。豁免只覆盖第 1 行、**不改门槛**，到期条件与第 2 行相同：有效到「XDM-64 v2 分块并发」完成并重测低于 600s 为止（[DECISIONS](decisions.md) D-54）。
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

待定：第 3a 行，即年线服务端窗口的子目标。
