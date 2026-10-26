# 决策记录（DECISIONS）

本文件记录**已经由 owner 拍板**的产品与工程决策，一条决策一段，只追加、不改写。被取代的决策保留原文，
状态改为「已取代」并指向取代它的条目。部署与运行事实见 [STATUS](STATUS.md)，性能与容量目标见 [SLO](SLO.md)，
本文件只回答「谁在什么时候定了什么、为什么」。

状态取值：**生效**、**实施中**（已决定，代码未合并或未部署）、**部分落地**（部分组件已合并或已部署，其余仍是目标）、**已取代**。`scripts/check_current_facts.py` 校验每个决策行的状态列以这四个取值之一开头（`docs/decisions.md` 的状态列规则，P1-2/#296）。

## 1. 性能与门禁

| # | 日期 | 决策 | 原因 | 状态 | 来源 |
|---|---|---|---|---|---|
| D-01 | 2026-10-10 | 全市场同代无变化日更、跨代有变化日更、年线三个场景的 **write-loop <600s 是硬门禁** | 统一原先分散的 5min / 15min 等阈值，以可重复的口径判定 | 生效 | [SLO](SLO.md) 第 1–3 行 |
| D-02 | 2026-10-10 | 门禁测量期间暂停 owner 自己的其他 agent 会话；测量用 trace-OFF；结果注明样本数与主机噪声 | 单核 t3.small 上并发负载会使耗时失真（噪声约 ±10%） | 生效 | [SLO](SLO.md) 备注 |
| D-03 | 2026-10-10 | **accept 只检查在途数据**；历史由服务启动 sweep 和 `task_sqlite check` 校验；启动 sweep 失败则拒绝启动 | accept 门禁随历史线性变慢，是日更超时的主因 | 生效 | [STATUS](STATUS.md)「校验分层变化」、PR #323 |
| D-04 | 2026-10-10 ~ 11 | `related_tasks` 有界（进行中 + 最近 5 条）；capability 升到 `generic-data-v5`，Skill 同步更新 | 控制进度类调用的响应时间与 payload | 生效 | PR #323 |
| D-05 | 2026-10-11 | `get_yearly_base_years` 叶节点完整校验、祖先已提交层；年线发布时先验年线 head 读取降到已提交层；全链完整校验保留在发布提交、启动 sweep、`task_sqlite check`、`query_data` | 年线分批耗时的主要成分是逐节点重复校验 | 生效 | [STATUS](STATUS.md)、PR #323 |
| D-06 | 2026-10-11 | 错开的提前全量重写（每实体阈值 64–127），校验放宽为父深度 64..=256 | 避免同一天所有实体同时 rollover 的耗时悬崖 | 已取代（D-09 已落地：schema 21 无 rollover/链深，自 preview.32 起部署） | PR #323 |
| D-07 | 2026-10-11 | 合并 PR #323（squash，`8a446b1`）并发布 v0.2.0-preview.31；接受第三类「保留的真实 mixed 负载」记为**未运行**；不改写已推送的历史 | retained gen-1 字节永久不可得；改写历史会破坏已有引用 | 生效 | [STATUS](STATUS.md)、[SLO](SLO.md) |
| D-19 | 2026-10-26 | #295 **先测量再定方向**：由看板任务 XDM-14（P2-4）在私有 perf 栈上做 schema 21 的耗时分解，再由 XDM-11（P3-4）按实测瓶颈在四个候选方向中选一个（窄投影 / 放宽批量和分页粒度 / 服务端筛选（随 #268）/ 降为低优先级）；本条只记录「先测量」，不预设方向。本地全市场分析的需求由 [#373](https://github.com/liqiangcc/x-data-mcp/issues/373) 的分析脚本解决：直读 SQL 的旁路只限分析脚本（`scripts/analysis/boll.py`，`mode=ro`），不进入 MCP 产品路径；schema 21 只保存每个实体的当前序列，脚本读到的是读取时刻的 head，结论必须连同读取时刻与 generation 一起记录，不能当作固定版本重放 | #295 的 383s 是 preview.28（schema 20）上测的，当时每次读取都要校验整条版本链（[STATUS](STATUS.md)「校验分层变化」：全链完整校验保留在 `query_data`）；schema 21 换了读路径，每页校验只读表头（`src/sqlite_market.rs:538-547`），这个数字已不能代表现状，「瓶颈是传输」的判断也没有分段测量支撑；未重新测量前选方向等于猜 | 实施中（测量未做，方向未定） | [#295](https://github.com/liqiangcc/x-data-mcp/issues/295)、owner 2026-10-26 同意「先测量」、[#373](https://github.com/liqiangcc/x-data-mcp/issues/373) |

## 2. 部署

| # | 日期 | 决策 | 原因 | 状态 | 来源 |
|---|---|---|---|---|---|
| D-08 | 2026-10-12 | 生产采用**归档旧库 + 全新初始化重建**，不做原地迁移；保留上一版 release 作回滚点；部署用 `scripts/deploy_phase_g.sh` | 只换二进制时启动 sweep 耗时随历史任务线性增长，约 70 天后超过 tunnel 的 10s 等待 | 生效 | [STATUS](STATUS.md)、PR #328 |
| D-17 | 2026-10-24 | **服务就绪只有一个定义**：`scripts/wait_service_ready.py` —— socket 上完成 MCP `initialize` 且 `serverInfo.name == "x-data-mcp"`，需要时再要求 `tools/list` 含指定工具。部署脚本与隧道启动必须调用它，**禁止固定 `sleep`、固定秒数轮询或仅判断 socket 文件存在**；等待预算默认 60s（探针）/ 120s（部署脚本），可用 `XDM_READY_TIMEOUT` 覆盖 | 2026-10-23 冷启动 31s 超过 tunnel 硬编码的 10s 与部署脚本的 20s；socket 文件存在不等于可服务，`data_service` 在 `registry_sweep` + `series_startup_check` 之后才 bind，成本随历史线性增长（RESULT-XP-043）。tunnel 因此约 24h 不可用 | **部分落地**：`scripts/wait_service_ready.py` 已随 #370（`8e83f22`，2026-10-24 合入 main）进入 main，`scripts/switch_binary.sh` 与两个部署脚本已调用（默认 120s）；隧道 drop-in 模板已进仓库（`scripts/deploy_phase_g.sh` 的 `tunnel_dropin_template()`：`ExecStartPre=<deploy root>/current/scripts/wait_service_ready.py <data sock> --timeout 120 --expect-tool plan_market_update`），探针随发布包分发（`scripts/build_release.sh`）且 `scripts/smoke_release.py` 断言其存在且可执行——**仓库侧已就绪**；生产 `/etc/systemd/system/x-data-mcp-tunnel.service.d/data-service.conf` 仍调用 `/opt` 上不受版本控制的 `wait-data-service.py`（790 B，mtime 2026-10-24T16:56:58Z），**生产隧道切换到仓库探针待 owner 批准**（2026-10-26 未执行） | TASK-OPS-T1、PR #370、#296 P1-3 |

## 3. 存储模型

| # | 日期 | 决策 | 原因 | 状态 | 来源 |
|---|---|---|---|---|---|
| D-09 | 2026-10-12 | **每个 (实体, 周期) 只保存当前最新序列**，不再保存版本链；`version_id` 改为当前内容的摘要；变更记录改为只追加的事件日志；除权由原子替换加年线重算表达；年线为独立步骤，日线变化后按事件日志判为 fresh / behind / invalid | 去掉链深对性能的依赖和整条版本链的正确性面（rollover、逐节点校验、分段共享） | 生效（PR #329 于 2026-10-13 合并 `87bf5aa`；schema 21 随 preview.32 于 2026-10-13 部署） | PR #329、[STATUS](STATUS.md) |
| D-10 | 2026-10-12 | D-09 的实施授权范围：在侧分支经 CI 完成实现，并在私有库上验收；**合并、打 tag、发布、部署到 `/opt` 生产、修改下游 x 仓库，均须另行批准** | 生产与对外契约变更不可由实现过程隐式带出 | 生效 | PR #329 |

## 4. 工程与仓库

| # | 日期 | 决策 | 原因 | 状态 | 来源 |
|---|---|---|---|---|---|
| D-11 | — | 工程约束（非产品决策）：本机不运行 cargo 编译和测试（仅 `cargo fmt --check`），编译与测试只走 CI | 主机只有 1 个物理核和 1.9GiB 内存，编译会拖垮生产服务和测量 | 生效 | [STATUS](STATUS.md)「资源」 |
| D-12 | 2026-10-12 | 仓库由私有改为**公开**；同时清理了最旧的一批 Actions artifact | 私有仓库的 Actions artifact 额度耗尽，预览包无法上传 | 生效 | 仓库设置 |
| D-13 | 2026-10-12 | 新提交的身份使用 `Liqiangcc`，**不写邮箱** | 邮箱写入提交后无法撤回 | 生效 | — |
| D-14 | 2026-10-12 | 仓库治理清理：删除已完整进入 main 的远端旧分支（167 个）；已放弃的 `migration/node-*` 分支先打 `archive/*` tag 再删分支；关闭过期草稿 PR #326（分支保留）；保留 `perf/g8-daily-update-perf`（含合并后的重建驱动提交） | 旧分支与过期 PR 掩盖了真实的在途工作；删除前均核对过可恢复性 | 生效（已执行） | #296 |
| D-15 | 2026-10-12 | 公开仓库信息卫生：`docs/STATUS.md` 不再记录主机名与访问边界细节；新增 CI 门禁拒绝 tracked 文本中的邮箱、内部主机名、私网地址 | 仓库公开后这些信息无法撤回 | 生效（PR #331 于 2026-10-13 合并；`check_public_hygiene.py` 已在 CI） | PR #331、`.github/workflows/ci.yml:35-38` |
| D-16 | 2026-10-12 | GOAL-008 收尾后清理性能测试夹具与构建产物（约 30G）；开放 Issue 按验收清单逐条对账后再收口，证据不足不关闭 | 夹具已无复用价值，开放 Issue 不得不经对账就关闭 | 生效（夹具清理待 GOAL-008 收尾后执行；Issue 对账进行中） | #296 |

## 5. 受理协议与编排

| # | 日期 | 决策 | 原因 | 状态 | 来源 |
|---|---|---|---|---|---|
| D-18 | 2026-10-26 | #294 选**方向 C**：`update_data` 显式 items **保持调用级 fail-closed 受理**，协议不改（不返回 per-item 受理结果、不加 dry-run 校验端点）。推荐编排是日更走 `plan_market_update`/`run_market_update`（`generic-data-v7`）：plan 阶段已把可预知的单项不合格（无 head、`STALE_BASE`、年份非子集、未来窗口）提前分类，写路径不会因单项整批被拒；只在显式 items 场景下使用**分裂重试**，规则写在 [`.devin/skills/x-data-daily-sync/SKILL.md`](../.devin/skills/x-data-daily-sync/SKILL.md) §3.8 | 调用级 fail-closed 是既有契约（[generic-data-v7](contracts/generic-data-v7.md) §6），改协议需要独立 contract review 并改动 wire 形状；v7 已把日更主路径挪到服务器侧 plan/run，plan/run 内部没有整批拒绝模式。2026-10-24 日更实测：日线 plan/run 5,224 项，5,199 succeeded、25 `REVISION_UNVERIFIED`，25 项显式前向窗口补推 25/25 succeeded，全程未出现整批被拒 | 生效 | [#294 最后一条状态评论（2026-10-13）](https://github.com/liqiangcc/x-data-mcp/issues/294#issuecomment-6148089096)、[STATUS](STATUS.md)「日更」段、owner 2026-10-26 同意按 Claude 推荐裁决 |

## 6. 数据来源

| # | 日期 | 决策 | 原因 | 状态 | 来源 |
|---|---|---|---|---|---|
| D-20 | 2026-10-26 | 生产来源原则修订（关联 #55）：为达到首期严谨回测级，允许引入**一手官方来源**——交易所与证监会的官方披露，以及 TDX 官方文件；每个新来源按 [#308](https://github.com/liqiangcc/x-data-mcp/issues/308) 的「离线输入准入」模式过证据门禁；**不接入任何第三方数据商或聚合源**；具体来源逐个由 owner 裁决。BaoStock 等第三方仍只能作交叉验证证据。README「唯一目标链路」段与[官方文件架构](production-source.md)中冲突的表述保留原文、标为历史并指向本条 | 只靠 TDX 衍生文件锚不住交易所层面的官方事实（ETF 身份、ST 时间线都要交易所名单与公告），首期要的是严谨回测级而不是「能跑」；证据门禁本身不变，放开的只是来源类别 | 生效 | [#55](https://github.com/liqiangcc/x-data-mcp/issues/55)、[#308](https://github.com/liqiangcc/x-data-mcp/issues/308)、owner 2026-10-26 批准 |

## 7. 待决（尚无结论，不得当作已决定执行）

| # | 事项 | 需要谁决定 | 备注 |
|---|---|---|---|
| P-01 | D-09 完成后是否合并、发布并部署到生产 | owner | 已落地：PR #329 于 2026-10-13 合并，schema 21 随 preview.32 部署（2026-10-13） |
| P-02 | 下游 x 仓库对 `KlineReader` 的版本 pin 逻辑改为 `latest_per_entity` | owner | 另一个仓库，须另行批准 |
| P-03 | 启动检查的冷缓存代价（随机读，约数秒到十余秒）是否接受，或改为启动后异步校验 | owner | 以 S5 在真实库上的复测为依据 |
| P-04 | 任务簿记与事件日志的保留策略 | owner | 先观察一周真实日更的增长 |
| P-05 | GOAL-008 的关闭标准（建议：生产有更新的一天，仅用 Skill 完成同步并验证通过，见 #304） | owner | — |
