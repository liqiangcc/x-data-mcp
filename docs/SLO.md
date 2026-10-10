# 性能与容量目标（SLO）

本文件是性能与容量目标的唯一入口，取代 README、`docs/daily-sync.md` 和各 Issue 中
零散的阈值表述（5min/16MiB、15min 等）。部署事实见 [STATUS](STATUS.md)。

每一行都标明状态：**hard**（owner 签字的硬门禁）、**baseline**（只记录实测，不作门禁），或 **待定**。

## 1. 目标表

| # | 场景 | 门禁 | 状态 | 最新实测（口径见备注） |
|---|---|---|---|---|
| 1 | 同代无变化，全市场 `update_data` | write-loop <600s；checkpoint 后净增长 ≤4KiB/attempted item；`data_versions +0` | **hard**。时间项 2026-10-10 签字，取代 2026-09-28 的 ≤15min；增长和版本两项沿用 2026-09-28（#259） | preview.27 / ecs：5,222 items，616s，+18.5MB，0 个新版本。perf 分支 / aws 私有栈：write-loop 433.8s（该构建当时因正确性问题被挂起，仅供参考） |
| 2 | 跨代有变化，全市场 `update_data` | write-loop <600s | **hard**（2026-10-10 签字） | preview.30 / aws 私有栈：E2E 1,484.6s。ecs 生产 2026-10-08：1,613s |
| 3 | 全市场年线 `update_data` | write-loop <600s | **hard**（2026-10-10 签字） | aws 私有栈：E2E 4,021s，其中服务端窗口合计 702s。aws 生产重建：服务端合计 603s，E2E 约 45min |
| 3a | 年线服务端窗口子目标 | — | **待定**，等年线服务端分阶段计时完成后再定 | — |
| 4 | 首次全市场采集 | ≤20min；首采后 DB ≤2.5GiB | hard（沿用） | aws 私有栈：346.4s。aws 生产重建：服务端合计 452s |
| 5 | 部署体积 | ≤4GiB | hard（沿用） | — |
| 6 | 已完成 100 条任务的 `get_task_progress` | 中位 <1s，且每个样本 <2s | 回归小门（不是 SLO） | perf 分支 `ef4fea4`：中位 0.108s / 0.091s，最大 0.894s，PASS（未部署） |

备注：
- 第 1–3 行的实测来自不同主机（ecs / aws）、不同代码版本和不同负载，**不能直接相互比较**。
- 第 1–3 行的门禁对生产生效的前提是：满足门禁的代码已经合并并部署。私有栈上的结果不等于生产结果。

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

待定：第 3a 行，即年线服务端窗口的子目标。
