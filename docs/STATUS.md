# 当前状态（STATUS）

本文件是**部署与运行事实**的唯一入口。README、AGENTS.md、`docs/daily-sync.md`
等文档只链接本文件，不再复述版本、主机、generation 或数据量。性能与容量目标见
[SLO](SLO.md)。

每次部署、迁移或 generation 激活后，执行者在同一个 PR 内更新本文件。

最后核实：TODO(DEPLOY)（部署日更新；现行为 2026-10-12）。部署与数据相关数值为部署并重建后读取 live 服务的结果；其余标注「来源」的数值来自所列报告或命令。

## 部署

| 项 | 事实 | 来源 |
|---|---|---|
| 主机 | aws-node | 运维记录 |
| 系统 | Ubuntu 26.04 LTS，kernel 7.0.0-1014-aws | `/etc/os-release`、`uname -r` |
| 资源 | EC2 t3.small（可突发）：2 vCPU（1 个物理核加 2 个超线程，没有 SHA-NI），1.9GiB RAM + 8GiB swap，根盘 77G；`/tmp` 是 953MB 的 tmpfs（占内存） | `lscpu`、`/proc/cpuinfo`、`free -h`、`df -h` |
| 本机工具链 | rustc / cargo 1.99.0 | `rustc --version` |
| 部署版本 | TODO(DEPLOY)：<新 tag> @ `<代码 sha>` | `/opt/x-data-mcp-preview/current/manifest.json` |
| 部署时间 | TODO(DEPLOY)：<部署时间>，`scripts/deploy_phase_g.sh <新 tag>`；旧库归档于 `state/preview.sqlite.v21-archived-<部署时间>`（归档名里的 schema 号按被归档库事实填写）；previous deployment v0.2.0-preview.31 (schema 20，`releases/83d323f…`) 保留作回滚点 | 部署报告 |
| 构建 | rustc 1.99.0，ubuntu-24.04，最低 glibc 2.39 | 同上 |
| 部署目录 | `/opt/x-data-mcp-preview/current` → `releases/83d323f…` | `ls -l` |
| 迁移 | 2026-10-09 从 ecs-node 迁到 aws-node；全新部署后重新采集，未迁移 DB | `/opt/x-data-mcp-preview/reports/migration-to-aws-20261009.json` |
| ecs-node | 服务已 stop 并 disable，数据保留为冷备（含 generation `a49c314b…` 的字节） | 同上 |
| production_ready | `false`：owner 个人研究、本地使用，未授权对外分发（#71） | `get_data_capabilities` |

## 服务

| systemd 单元 | 进程 | 入口 | 工具数 | 备注 |
|---|---|---|---|---|
| `x-data-mcp-data` | `data_service` | `run/mcp.sock` | 21 | capability `generic-data-v6`（含只读 `get_yearly_base_years`）；drop-in 设置 `X_DATA_MCP_GENERATION_ROOT` |
| `x-data-mcp-acquire` | `acquire_service` | `run/acquire.sock` | 10 | 设置了 `X_DATA_MCP_TDX_FETCH=current/scripts/tdx_fetch.py` |
| `x-data-mcp-tunnel` | OpenAI `tunnel-client`（profile `x-data-mcp`） | 转发到 `run/mcp.sock` | — | 只转发 data server，不转发 acquire |

访问边界与认证细节属于运维信息，不记录在公开仓库。当前边界是否可接受，尚待 owner 明确决定。

## 数据

| 项 | 事实 | 来源 |
|---|---|---|
| 当前 generation | `87322997b21ee492224b42984c7fa5df865840d26f04bee628e54ce74a0e0de6`，2026-10-08T09:06:02Z 激活 | `generations/active-generation.json` |
| 前一 generation | `a49c314b8f910aa68d0a66b9f3b3d08bf0d57f5b215a8b77a973786391e512a1`（字节只在 ecs 冷备中） | 同上、迁移报告 |
| 数据库 | `state/preview.sqlite`，fresh compact schema 21，TODO(DEPLOY)：<部署日 `ls -l` 实测> B | `ls -l` |
| 目录 | 5,228 个实体 | 重建驱动读回 |
| 日线 | 已发布 5,224 个；head 5,209 个为 2026-10-08，其余为更早的最后交易日（停牌/退市类） | 重建驱动读回 |
| 年线 | 5,224 个 published，0 失败，0 unknown | 重建驱动读回 |
| 缺失 | `sz.001381`、`sz.301569`、`sz.301660`、`sz.301718`（官方 hsjday 缺成员，4 个日线任务条目 fail-closed，预期） | 重建驱动读回 |
| 与迁移报告的差异 | 迁移报告记录 `sz.001246` 为空、年线 5,223 个 succeeded；本次重建该实体有 2 条日线（2026-09-30、2026-10-08）和 1 条年线，年线共 5,224 个。读回的这两项预期值（5,223 与 `sz.001246` 为空）因此不符，判断为预期值过时而非故障，待 owner 确认 | `deploy31-readback.json` |
| 重建耗时 | 全新重建基于 generation 87322997（数据截止 2026-10-08）：catalog 10.0s + 日线 53 块约 4 分钟 57 秒（含 4 个缺成员条目）+ 年线首次 write-loop 304.7s | 重建驱动 JSON |

部署后首次增量更新（2026-10-12 收盘后，跨代 Append，验证 GOAL-008 关闭条件）：**待执行**。

## 进行中

- **GOAL-008 性能**：PR #323 已于 2026-10-11 squash 合并（`8a446b1`），发布 v0.2.0-preview.31，2026-10-12 部署。
  - 全市场验收（XP-015，perf `3b11231`，aws 私有栈，浅链，53 批，trace-OFF，每负载单次）：年线首次 310.6s、日线跨代 Append 104.2s（合成截断夹具）、年线重聚合 328.3s、日线同代 NoChange 78.0s，write-loop 均 <600s，读回全部 PASS（见 [SLO](SLO.md)）。第三类「保留的真实 mixed Append/Full/fail-closed」**未运行**（retained gen-1 字节永久不可得）；`AGENTS.md` 记载的离线 harness 结果仅作旁证，不作替代。
  - **最坏链深未做全量实测**：错开的提前全量重写（每实体阈值 64–127）后最坏链深 ≤127，年线批中位 10.3s，按 53 批外推约 548–588s，余量约 2–9%；日线在深链上的有效实测缺失。
  - 校验分层变化（均经 owner 批准，2026-10-10 / 11）：accept 只检查在途；`get_data_availability` 统计与校验走已提交层（含年线）；`get_yearly_base_years` 叶节点完整校验、祖先已提交层；年线发布的先验年线 head 读取降到已提交层；rollover 校验由「父深度必须正好 256」放宽为 64..=256。全链完整校验仍在发布提交、服务启动 sweep、`task_sqlite check`、`query_data`。
  - **已于 2026-10-12 上线到生产（preview.31，全新重建）**，生产读回：catalog 5,228、日线 5,224、年线 5,224、absent 4 个、5 个抽样实体日线/年线可读。
- **#304**：连续 10 个交易日仅用 Skill 完成同步的验收。已记录的 PASS 运行（Run 3、Run 5、2026-10-08）全部是 `day=unchanged`，有更新那天的路径尚未覆盖。有效计数以 #304 的评论为准；2026-10-08 那次使用的是提示词 `daily-ai-sync-v2`。

## 已知问题

| 问题 | 影响 | 来源 |
|---|---|---|
| 只换二进制（不再每次全新 init）的部署：启动 registry sweep 的耗时随历史任务线性增长，约运行 70 天后超过 `x-data-mcp-tunnel` 的 10 秒等待（`wait-data-service.py`）；缓解（只 sweep 最近 N 天、先开 socket 后台 sweep）会改变「sweep 失败即拒绝启动」的语义，需另行批准。schema 21 的启动检查已改为逐 series 索引点查（S2b F3，不再 loose-scan） | 目前每次部署全新 init，未触发 | XP-043 |
| 任务簿记（`frozen_work_json`、`requests.command_json`、`shared_evidence`）每年约增长 10–16GB（每天约 106 个任务） | 只换二进制的长期运行会逼近「部署体积 ≤4GiB」；降低需要改存储格式并定留存策略 | S7 |
| `market_events` 事件日志无界增长（RULINGS Q9 已接受）：估计 ~5,224 行/日 ≈ 1MB/日（S5 实测 26,118 行 / dbstat ~16MB 含索引） | 长期运行累积；与任务簿记增长分开记录 | RULINGS Q9、S5 |
| rollover 校验放宽后，回滚到旧二进制时，其对早 rollover 结果行的完整校验会报「requires depth-256 base」；已有 >127 深的链升级后首日会集中 rollover 一次 | 只影响长期运行的库 | XP-051 |
| 年线最坏链深余量薄（约 2–9%），宿主为 t3.small，噪声约 ±10%，冷缓存首批明显偏慢 | 个别批次可能离群 | XP-045b/047/048/052 |
| aws `acquire.sqlite` 中有 4 个没有字节的 generation，normalized-retire 待处理 | 采集台账有残留 | 迁移报告 |
| `/opt/x-data-mcp-preview/mcp.json` 的 `x-data-mcp-demo` 指向一个不存在的 sqlite | 配置过时 | `ls` |
| `src/acquire/fetch.rs` 在未设置 `X_DATA_MCP_TDX_FETCH` 时，回退到编译期的 `CARGO_MANIFEST_DIR` | 不带该变量启动的实例无法下载 | 源码 |
| 生产服务、私有性能栈和多个 agent 会话共用 1 个物理核和 1.9GiB 内存；`/tmp` 是内存盘 | 2026-10-10 约 04:34Z 过载重启；测量结果有噪声 | 运维观察 |
| AGENTS.md 的环境段、README「当前状态」、`docs/daily-sync.md`「当前状态」仍写 ecs-node / preview.29 / generation `2e9183…` | 读取这些文档的 agent 会被误导 | 本次核对；这三处已加指向本文件的说明 |

已关闭：#324（stopped-write 重写 `outcome_json`，随 PR #323 修复）；受理体检扫描全部历史（随 PR #323 修复，2026-10-12 部署后生产已包含）。

## 维护规则

1. **谁更新**：部署或迁移的执行者，在记录本次部署的同一个 PR 内更新。
2. **其他文档**：README 只保留「项目是什么」并链接本文件；AGENTS.md 的环境段只写开发约束，主机事实链接本文件。旧段落的清理由后续 PR 完成。
3. **待办：扩展 CI 检查。** `scripts/check_current_facts.py` 目前只检查包版本、schema、工具数量这类构建事实，所以文档写错主机或部署版本时也能通过。建议增加部署事实的检查：
   - 本文件中的部署版本必须与最近一份已提交的部署记录一致；
   - `CURRENT_DOCS` 的当前区域不得把已退役主机写成当前主机；
   - 「最后核实」日期距今超过 N 天时给出警告。
