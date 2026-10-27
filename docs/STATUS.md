# 当前状态（STATUS）

本文件是**部署与运行事实**的唯一入口。README、AGENTS.md、`docs/daily-sync.md`
等文档只链接本文件，不再复述版本、主机、generation 或数据量。性能与容量目标见
[SLO](SLO.md)。

每次部署、迁移或 generation 激活后，执行者在同一个 PR 内更新本文件。

最后核实：2026-10-24。除标注「历史」的段落外，本文件的数值均为 2026-10-24 只读实测：
`python3` + `sqlite3`（`file:…?mode=ro`）读 `state/preview.sqlite` 与 `generations/acquire.sqlite`、
`systemctl show`、`journalctl`、`curl -sI`、live MCP 只读工具（`current/bin/mcp_connect <sock>` 的
`initialize`/`tools/list`/`get_data_capabilities`/`get_data_availability`/`sync_status`）、
`du`/`df`/`free`/`lscpu`/`stat`。数据段为 **O2 补采结束后的 17:19Z 终态**（17:26Z 复核，preview.35 切换后数据未变），
部署段已含 2026-10-24T17:25Z 的 preview.35 切换。
「推断」标记表示由实测数据推出、但没有独立测量的结论。

## 部署

| 项 | 事实 | 来源 |
|---|---|---|
| 主机 | aws-node（EC2；systemd 启动日志记录 `Detected virtualization amazon`） | 运维记录、`journalctl` 启动段 |
| 系统 | Ubuntu 26.04 LTS，kernel 7.0.0-1014-aws | `/etc/os-release`、`uname -r` |
| 资源 | 2 vCPU（1 个物理核 × 2 超线程，Xeon Platinum 8259CL；`/proc/cpuinfo` 无 `sha_ni`），MemTotal 1,905,872 kB（约 1.82GiB）+ 8GiB swap（2×4GiB，实测已用 2.5GiB），根盘 77G（已用 50G、余 27G）；`/tmp` 是 953MB 的 tmpfs（占内存，实测已用 625MB） | `lscpu`、`grep -c processor /proc/cpuinfo`、`grep sha_ni /proc/cpuinfo`（无匹配）、`free -m`、`swapon --show`、`df -h / /tmp` |
| 本机工具链 | 本机禁止本地 Rust 编译：执行 `rustc --version` 被主机策略拦截并返回「Local Rust compilation is forbidden on this host (1.9 GB RAM)」，仅允许 `cargo fmt`。因此本机工具链版本未复核；部署二进制的构建版本见「构建」行 | 实测拦截提示 |
| 部署版本 | v0.2.0-preview.35 @ `23edeaf648049463a80d1f20584945e738f532c5` | `/opt/x-data-mcp-preview/current/manifest.json`、`readlink current`、live MCP `initialize` 返回 `serverInfo.version=0.2.0-preview.35` |
| 部署时间 | 2026-10-24T17:25Z，二进制热切换（`current` 符号链接与两个单元的重启均在 17:25:22，新 `mcp.sock` 17:25:35）；切换用的脚本是分支 `fix/service-readiness @ d2608e7`，该就绪修复已随 #370（`8e83f22`，2026-10-24 合入 main）进入仓库：`scripts/switch_binary.sh` 的 S6 调用 `scripts/wait_service_ready.py`，默认超时 120s（`XDM_READY_TIMEOUT` 可覆盖）。本次报告 `reports/preview35-switch-20261024.json` 是切换后整理的（mtime 2026-10-24T17:30:30Z，比切换晚约 5 分钟）：内容只有 7 条 check 名称字符串（S1–S7），没有逐项证据；当时仓库内没有任何脚本能产出它（`switch_binary.sh` 自 #296 P1-3 起才写机器报告）。就绪探针 data 13,461ms、acquire 8ms；DB 未迁移未重建（schema 21，`db_bytes` 1,185,640,448 与切换前相同）。上一个部署 preview.34 @ `a77d0901…` 保留在 `releases/` 作回滚点 | 报告 JSON、`stat`/`readlink`、`journalctl -u x-data-mcp-tunnel`、`scripts/switch_binary.sh:110-129`、PR #370 |
| 数据库 schema | 已部署：schema 21（`pragma user_version=21`）；代码：schema 23（未部署；M1 的 schema 22 已按 §3/§7 扩展：口径/精度列、双序列写入、因子事件表与派生数据集版本登记、指数序列与逐指数口径元数据、交易日历、日更内部派生 item、研究快照导出；M2 PR-1 追加 023：证券主表、代码迁移、简称历史、股本事件、风险警示区间、停牌区间与两张规则表，并扩展 `derived_dataset_versions.dataset_kind`，只建表、无写入路径） | `pragma user_version`（只读）、`src/sqlite_tasks.rs` 的 `COMPACT_SCHEMA_VERSION`、#383 M1 §14、#391 M2 §9 |
| 构建 | rustc 1.99.1 (58cf8f9c3 2026-10-13)，ubuntu-24.04，最低 glibc 2.39 | `current/manifest.json` |
| 部署目录 | `/opt/x-data-mcp-preview/current` → `releases/23edeaf…`（6 个版本目录共存，共 348MiB） | `readlink`、`du -sh releases/*` |
| 迁移 | 2026-10-09 从 ecs-node 迁到 aws-node；全新部署后重新采集，未迁移 DB | `/opt/x-data-mcp-preview/reports/migration-to-aws-20261009.json` |
| ecs-node | 服务已 stop 并 disable，数据保留为冷备（含 generation `a49c314b…` 的字节） | 同上 |
| production_ready | `false`：owner 个人研究、本地使用，未授权对外分发（#71） | `get_data_capabilities` |

v7 自 preview.33 起运行；preview.34（2026-10-17T18:09Z）与 preview.35（2026-10-24T17:25Z）都是二进制热切换，数据库沿用（schema 21），部署时刻无数据重建、无契约变更（`capability_revision` 仍为 `generic-data-v7`，23/10 工具不变）。
preview.34 上线后 `x-data-mcp-data` 连续运行 5d12h39m 直到 2026-10-23 06:49 主机重启
（`systemctl show` 的 `Consumed … wall clock time` 与 `journalctl --list-boots` 一致）；preview.35 切换时两个单元在 17:25:22 重启。
preview.34 那次没有落盘报告（见「已知问题」），preview.35 这次有。回滚到 preview.34 不涉及契约差异；回滚到 preview.32（v6）的已知代价：v7 运行过 plan/run 之后，v6 对 plan/run 任务的 `get_task_progress` 会报未知契约，存在被重启恢复收尾（`interrupted`）的 plan/run 任务时 v6 的 `list_tasks` 会整体失败。

M1（schema 22）代码已全部合并、**未部署**：PR #389/#390/#394/#397/#400/#402/#406/#408（CI 绿）。部署按 D-08（归档旧库 + 全新 init），发布包暂不含 `export_research`（`scripts/build_release.sh` 未加 `--features export`，属 owner 批准项）。部署前实测清单见 [M1 部署清单](tasks/issue-383-m1-deploy-checklist.md)；实现记录见 [M1 实现记录](tasks/issue-383-m1-implementation.md)。

M2（[#391](https://github.com/liqiangcc/x-data-mcp/issues/391)）的 PR-1 把迁移链推到 schema 23（`migrations/tasks/023_security_master.sql`）：只建表、不写数据、不改任何读写行为，所以部署事实仍是 schema 21；schema 22 的库既不会被新二进制原地改写，也不会被 023 回填（`check_schema` 直接拒绝非当前 schema）。设计见 [M2 设计](tasks/issue-391-m2-security-master.md) §9。

## 服务

| systemd 单元 | 进程 | 入口 | 工具数 | 备注 |
|---|---|---|---|---|
| `x-data-mcp-data` | `data_service` | `run/mcp.sock` | 23 | capability `generic-data-v7`（含只读 `get_yearly_base_years`）；drop-in 设置 `X_DATA_MCP_GENERATION_ROOT` |
| `x-data-mcp-acquire` | `acquire_service` | `run/acquire.sock` | 10 | 设置了 `X_DATA_MCP_TDX_FETCH=current/scripts/tdx_fetch.py` |
| `x-data-mcp-tunnel` | OpenAI `tunnel-client`（profile `x-data-mcp`） | 转发到 `run/mcp.sock` | — | 只转发 data server，不转发 acquire；`ExecStartPre` 用 `/opt/x-data-mcp-preview/wait-data-service.py`（该文件不在仓库内），就绪等待上限 **120s**（2026-10-24T16:56:58Z 由 10s 改为 120s，`deadline=time.monotonic()+120`；但单元未设 `TimeoutStartSec`，systemd 默认 90s 也覆盖 `ExecStartPre`，所以实际生效上限是 **90s**），单元为 `Restart=always` / `RestartSec=10`；data 服务停止时隧道会因 stdio 子进程退出而自行退出 |

访问边界与认证细节属于运维信息，不记录在公开仓库。当前边界是否可接受，尚待 owner 明确决定。

工具数与能力版本为 2026-10-24T17:26Z（preview.35 切换后）live MCP 只读复核：data socket `tools/list` 返回 23 个工具、
`get_data_capabilities` 返回 `capability_revision=generic-data-v7`、`active_generation_ref=c184e9c4…`；
acquire socket `tools/list` 返回 10 个工具。两个 socket 的 `serverInfo.version` 均为 `0.2.0-preview.35`。

代码（main）为 24 data tools（v8 PR-2，未部署）：三个入口 `research`/`ops`/`maint` 由 `data_service --entry` 选择，`mcp.sock` 继续作编排入口，`tools/list` 不再内联 `outputSchema`。隧道改指 `research.sock`、新增 `x-data-mcp-research`/`x-data-mcp-maint` 单元的步骤见 [v8 切换清单](contracts/v8-switch-checklist.md)（未执行）。已部署实例仍为 23 data tools、单入口。

## 数据

以下为 **2026-10-24T17:19Z 终态**（O2 补采结束后实测；17:26Z 在 preview.35 切换后复核，数据未变）；补采过程见「日更」段的 2026-10-24 时间线。

| 项 | 事实 | 来源 |
|---|---|---|
| 当前 generation | `c184e9c4b816dfc068d4d6e2eaedcf5e1742ad2979b0e54be2af798b54995d91`，2026-10-24T17:01:34Z 激活（`activation_request_id=ops-20261024-act-1`，previous `9d9e38d7…`）；代目录 572,634,818 B（三个官方成员合计 572,630,763 B + 清单/决策/证据） | `generations/active-generation.json`、live `sync_status`、`du -sb`、`snapshot-manifest.json` |
| 热 generation | **5/6**（2026-10-24T17:52Z 清理后）：1 active + 4 superseded（`9d9e38d7`、`f289c612`、`8b8614c3`、`54553656`）；另有 10 个 evidence_only。下一次 acquire 可直接受理（5 < 6）；activate 成功后 preview.35 的自动保留会退役最旧的 superseded（预期 `8b8614c3`、`54553656`），保留最新 2 个加刚被取代的前一代 | `generations/acquire.sqlite`（按 `state` 计数）、`src/acquire/service.rs:39,456,1058-1084` |
| 退役记录 | `87322997…` 2026-10-19 退役（经用户确认）；`2d5851a4…` 2026-10-24T16:57:16Z 退役，**退役前 548M、退役后 16K，释放约 546MB**；`a119629b…` 2026-10-24T17:52Z 退役，**退役前 574M、退役后 16K，释放约 574MB**；更早 4 个已无字节的代（84b3f15d、c360967a、2e918330、a49c314b）2026-10-13 退役为 evidence_only（只改状态，清单/决策/证据保留） | `generations/acquire.sqlite`（`updated_at`、`request_outcomes` 的 `ops-20261024-retire-2d58`/`ops-20261024-retire-a119`）、`du -sb generations/generations/*`；退役前后 `generations` 总量 3.3G→2.7G→2.1G 与两次释放一致 |
| 数据库 | `state/preview.sqlite`，fresh compact schema 21（`pragma user_version=21`），1,185,640,448 B；WAL 0 B（17:25:22 停服务时 checkpoint）、SHM 32,768 B | `ls -l`、`pragma user_version` |
| 目录 | 5,228 个实体（`catalog-1791873196258239513`） | `catalog_head`/`catalog_members` 只读查询、live `get_data_availability`（`record_count=5228`） |
| 日线 | 已发布 5,224 个；**5,216 个 head 为 2026-10-23**（`last_day=20749`）；8 个更早（fail-closed，`coverage_status=1`）：sz.000016 2026-09-03、sh.688496 2026-09-10、sh.601059 2026-09-14、sh.601198 2026-09-14、sz.002717 2026-10-12、sz.301314 2026-10-12、sz.301468 2026-10-16、sh.601326 2026-10-20 | `market_series`（`period=1`）`last_day` 分布只读查询、live `get_data_availability` |
| 年线 | 5,224 个 published，**5,216 个 head 为 2026-10-23**，落后集合与日线逐实体相同（同上 8 个） | `market_series`（`period=2`）只读查询 |
| 缺失 | `sz.001381`、`sz.301569`、`sz.301660`、`sz.301718`（官方 hsjday 缺成员，预期）；实测这 4 个实体在 catalog 中存在但日线、年线均无 series | `catalog_members` 反连接 `market_series` 只读查询 |
| 任务与事件 | 579 个任务：529 succeeded、49 partially_failed、1 failed；`market_events` 83,545 行 | `tasks`、`market_events` 只读查询 |
| 与迁移报告的差异 | 迁移报告记录 `sz.001246` 为空、年线 5,223 个 succeeded；本次重建该实体有 2 条日线（2026-09-30、2026-10-08）和 1 条年线，年线共 5,224 个。读回的这两项预期值（5,223 与 `sz.001246` 为空）因此不符，判断为预期值过时而非故障，待 owner 确认 | `deploy31-readback.json` |
| 重建耗时 | 全新重建基于 generation 87322997（数据截止 2026-10-08）：collect write-loop 308.4s（catalog + 日线 53 块，含 4 个缺成员条目）+ 年线首次 write-loop 208.9s；预览演练对照 209.7s / 224.3s | 重建驱动 JSON |

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

运维事实：热 generation 上限 6（`MAX_HOT_GENERATIONS`，`src/acquire/service.rs:39`；判定见 `:456`，hot = 状态不是 evidence_only 的代），超限后 `acquire_generation` 被拒 `generation_capacity`；Skill 无权退役 generation；退役历史：2026-10-13 由操作员退役 4 个已无字节的旧代，2026-10-19 退役 87322997，2026-10-24T16:57:16Z 退役 2d5851a4（释放约 546MB）。**历史：10-24 补采后一度回到 6/6**（新代 `c184e9c4` 激活，`9d9e38d7` 转为 superseded）；17:52Z 退役 `a119629b…` 后当前为 **5/6**（2026-10-26 只读实测：1 active + 4 superseded），下一次 acquire 可直接受理。保留策略（`RETAINED_SUPERSEDED=2`，`src/acquire/service.rs:43`）已随 preview.35 **于 2026-10-24T17:25Z 部署**，但只在 activate 提交成功后触发（`src/acquire/service.rs:1058-1084`），不能解除 acquire 之前的容量拒绝。Skill 在有更新日的路径仍未完整验证（枚举过慢一次、部分自行追加一次）；#304 的有效 PASS 仍为 3/10（均为 unchanged）。批量更新工具（plan/run，generic-data-v7）实现中。

## 新鲜度

| 项 | 事实（2026-10-24T17:19Z 终态，17:26Z 复核） | 来源 |
|---|---|---|
| 最后任务 | 2026-10-24T17:18:42Z 受理、17:18:43.9Z 完成：`market.yearly` 的 `update_data`（`explicit-yearly-task-v1`），41/41 succeeded；库内共 579 个任务，`market_series.max(updated_at)=2026-10-24T17:18:43.9Z` | `tasks`、`market_series` |
| 落后交易日 | **无**：日线/年线 head 均为 2026-10-23（`last_day=20749`），即最近一个已收盘交易日。补采前 head 为 2026-10-19（10-20..10-23 四个交易日缺口），已由 10-24 补采补齐；这四天是交易日已由官方数据直接确认（`sh.600000` 末五个 bar = 10-19、10-20、10-21、10-22、10-23；未另用交易日历核对） | `market_series`、`series_bars`（`sh.600000`）、`date -d <date> +%A` |
| 官方输入 | 2026-10-24T17:26Z `curl -sI` 与 active generation `c184e9c4` 的 `snapshot-manifest.json` **逐项相同**：base 16,245,958 B / `"6adc3a88-f7e4c6"` / `Sat, 24 Oct 2026 04:56:40 GMT`；gbbq 6,288,317 B / `"6adc3a88-5ff3bd"` / 同上；hsjday 550,096,488 B / `"6adb13be-20c9ce68"` / `Fri, 23 Oct 2026 07:58:54 GMT`。上一个 active `9d9e38d7` 记录的是 16,058,358 / `"6ad5ef6d-f507f6"`、6,161,423 / `"6ad5ef6d-5e040f"`、549,367,720 / `"6ad5cdba-20beafa8"`，三成员当时均已变 | `curl -sI <三个官方 URL>`、`generations/generations/c184e9c4…/snapshot-manifest.json` |
| 容量 | hot **5/6**（2026-10-26 只读实测：1 active + 4 superseded；见「数据」表的「热 generation」行）：下一次 acquire 可直接受理（5 < 6）。历史：10-24 补采期间曾回到 6/6，17:52Z 退役 `a119629b…` 后为 5/6。自动保留只在 activate 后回收 | `generations/acquire.sqlite`、`src/acquire/service.rs:39,456` |

## 2026-10-23 重启与就绪（实测）

- 主机于 2026-10-23 06:49:20 重启（`journalctl --list-boots` 的 boot `d462ed36c2ad42aba34aed2031018be3`）；上一轮 uptime 5d12h39m，恰好从 preview.34 切换（10-17T18:09Z）开始。
- `x-data-mcp-data` 06:49:25 启动，`run/mcp.sock` 于 06:49:56.531 建立（`stat` 的 Birth）→ 冷启动到可服务 **31.5s**（库内有 574 个历史任务）。
- `x-data-mcp-tunnel` 06:49:25 首次启动，`ExecStartPre`（`wait-data-service.py`，当时 deadline 为 10s；10-24T16:56:58Z 已改为 120s）于 06:49:37 报 `data service IPC readiness failed`、`Control process exited, code=exited, status=1/FAILURE`；systemd 按 `Restart=always`/`RestartSec=10` 在 06:49:47 重试，06:49:56 `Started`，同一秒 tunnel-client 打出 `🟢 tunnel-client started`。**隧道在重启后不可用约 31s**。
- 10-24 06:43:16 的那次单元重启不是「恢复」：06:43:09 `apt-daily-upgrade` 触发 `systemd` daemon-reexec，随后 needrestart 重启了 tunnel、sshd、nginx 等单元；此前该单元自 10-23 06:49:56 起连续运行 23h24m（systemd 的 `Consumed … wall clock time`）。同样的模式在 10-23 07:42:51 也发生过一次（停机约 1s）。
- 因此重启本身只造成约 31s 的隧道不可用；10-20..10-23 的数据缺口与重启无关（10-20T00:07Z 之后就没有任何任务）。`scripts/switch_binary.sh` 的 S6 在旧版本只等 20s 且只判 socket 文件存在，按 31.5s 冷启动会判失败并回滚（推断，未在真实部署中触发）。仓库脚本已在 #370（`8e83f22`，2026-10-24 合入 main）改为 S6 调用 `scripts/wait_service_ready.py`、默认超时 120s（`XDM_READY_TIMEOUT` 可覆盖），不再只判 socket 文件存在；按 31.5s 冷启动，当前脚本可在超时内等到就绪（推断）。
- **残留风险（部分缓解，未消除）**：`wait-data-service.py` 的 deadline 已于 2026-10-24T16:56:58Z 从 10s 改为 120s，因此「冷启动比等待长」这条路径在当前库规模下不会再触发；剩下的风险是就绪超过 120s、或单元连续失败触到 `StartLimitBurst=5` 时停在 `failed` 不再重试，需人工介入。这两条都未实测（推断）。

## 足迹

| 项 | 事实（2026-10-24T17:26Z 实测，preview.35 切换之后） | 来源 |
|---|---|---|
| `/opt/x-data-mcp-preview` 全树 | 4.2GiB（`du -sh`，2026-10-24T17:55Z 清理后；清理前 6.2GiB） | `du -sh .`、`du -sb .` |
| generations | 2.7GiB：5 个带字节代（active `c184e9c4` 572,634,818；superseded `9d9e38d7` 571,591,556、`f289c612` 571,549,313、`8b8614c3` 574,645,314、`54553656` 574,839,345）；另 10 个代目录各约 4KB（只有清单/决策/证据，无代字节） | `du -sb generations/generations/*` |
| state | 1.2GiB：`preview.sqlite` 1,185,640,448 B + WAL 0 B + SHM 32,768 B。两份 v20 归档库已移出 `/opt` 到 `/root/x-data-archive/` 并 gzip（1,620,348,928 B → 742,656,972 B，`gzip -t` 通过，原库 SHA-256 保存在 `SHA256SUMS.v20`） | `du -sh state/*`、`ls -l`、`gzip -t` |
| releases | 348MiB，6 个版本目录（49–96MiB/个；新增 `23edeaf…` 50MiB） | `du -sh releases/*` |
| reports | 2.8MiB（含新的 `preview35-switch-20261024.json`） | `du -sh reports`、`ls -l reports/` |
| `/tmp` | tmpfs 953MB，已用 19MB（2%）；2026-10-24T17:49Z 清理了 10-23 的调试残留（strace 日志、probe DB、`one-read-tmp`、`bp`/`bp_prev`、`reading-mcp-*` 等，约 606MB） | `df -h /tmp`、`du -sh /tmp/*` |
| 根盘 | 77G，已用 49G，余 28G（`/tmp` 是内存盘，不计入） | `df -h /` |

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
| 只换二进制（不再每次全新 init）的部署：启动 registry sweep 的耗时随历史任务线性增长（推断），10-23 时已超过 `x-data-mcp-tunnel` 当时 10 秒的就绪等待（`wait-data-service.py`；现为 120s）。两个实测点：**2026-10-23 主机重启后（preview.34，574 个任务，冷 page cache）31.5s**；**2026-10-24T17:25Z preview.35 切换（579 个任务，热 page cache）13.5s**。两点无法区分「任务数」与「缓存冷热」的贡献（推断：后者影响更大），因此「线性增长」本身未得到证实，需专门测量。10-23 那次是首次 `ExecStartPre`（当时 deadline 10s）失败后靠 `Restart=always` 重试成功；10-24T16:56:58Z 起该 deadline 已改为 120s，17:25 切换时一次等到 socket（13.5s），空窗来自 `RestartSec=10` 加就绪时间。缓解（只 sweep 最近 N 天、先开 socket 后台 sweep）会改变「sweep 失败即拒绝启动」的语义，需另行批准。schema 21 的启动检查已改为逐 series 索引点查（S2b F3，不再 loose-scan） | 每次重启/切换首轮必失败，多花一个 `RestartSec`；若重试也失败则长时间不可用 | `journalctl -u x-data-mcp-data/-tunnel`、`stat run/mcp.sock`、`reports/preview35-switch-20261024.json`（`readiness_probe.data_elapsed_ms=13461`） |
| tunnel 启动竞态（仓库脚本已修，tunnel unit 仍用 /opt 旁路脚本）：2026-10-23 06:49 那次是 `wait-data-service.py` 的 10s deadline 短于 31.5s 冷启动，首轮 `ExecStartPre` 失败、靠 `Restart=always` 在 10s 后重试成功；`/opt` 内该脚本已于 2026-10-24T16:56:58Z 把 deadline 改为 120s，因此 10-24T17:25 切换时 `ExecStartPre` 一次等到 socket（13.5s）。#370（`8e83f22`，2026-10-24 合入 main）新增 `scripts/wait_service_ready.py`，`scripts/switch_binary.sh` 与两个部署脚本已调用（默认 120s）；但 tunnel 的 `ExecStartPre` 仍调用 `/opt` 上的 `wait-data-service.py`，该文件不在仓库内、也不受版本控制（D-17 只部分落地） | 重启/切换时的隧道空窗现在由 `RestartSec=10` + 就绪时间决定（17:25 实测约 13s）；若单元连续失败触到 `StartLimitBurst=5`，会停在 `failed` 需人工介入（未实测，推断）；仓库脚本已能等到 13.5–31.5s 冷启动（默认 120s），不再在 S6 判失败 | `journalctl -u x-data-mcp-tunnel`（10-23、10-24T17:25）、`grep deadline /opt/x-data-mcp-preview/wait-data-service.py`、`stat` 该文件 mtime、`scripts/switch_binary.sh`、`systemctl cat x-data-mcp-tunnel`、`systemctl show -p StartLimitBurst`、PR #370 |
| 报告覆盖不均：preview.33 / preview.34 没有报告（`reports/` 在 preview.32 之后直到 preview.35 才有条目）；preview.35 的报告是切换后整理的（mtime 2026-10-24T17:30:30Z，切换 17:25:22），只有 7 条 check 名称、没有逐项证据；#370（`8e83f22`）合入的是就绪探针。`switch_binary.sh` 自 #296 P1-3 起在 `--execute` 时写机器报告（`--report <path>`，默认 `reports/<short-tag>-switch-<UTC>.json`，含 P 与 S1–S7 逐项 `{step,status,elapsed_ms,evidence}`、探针 JSON 与回滚结果），preview.35 那份仍是人工整理的 | preview.33/34 的 S1–S6 逐项结果永久不可复核，只能引用可观测结果（链接 mtime、单元 uptime、live version）；下一次切换起有机器产出的逐项报告 | `ls -l reports/`、`cat reports/preview35-switch-20261024.json`、`scripts/switch_binary.sh`、`scripts/test_switch_binary.py` |
| 任务簿记（`frozen_work_json`、`requests.command_json`、`shared_evidence`）每年约增长 10–16GB（每天约 106 个任务） | 只换二进制的长期运行会逼近「部署体积 ≤8GiB（目录口径，2026-10-26 签字）」；降低需要改存储格式并定留存策略 | S7 |
| `market_events` 事件日志无界增长（RULINGS Q9 已接受）：估计 ~5,224 行/日 ≈ 1MB/日（S5 实测 26,118 行 / dbstat ~16MB 含索引） | 长期运行累积；与任务簿记增长分开记录 | RULINGS Q9、S5 |
| **schema 20 历史**（现行 schema 21 无此机制）：rollover 校验放宽后，回滚到旧二进制时，其对早 rollover 结果行的完整校验会报「requires depth-256 base」；已有 >127 深的链升级后首日会集中 rollover 一次 | 只影响仍运行 schema 20 的旧库/旧二进制回滚 | XP-051；`grep -rn rollover src/` 无命中，`src/sqlite_market/manifest.rs` 只服务 legacy backend（`src/sqlite_market/backend.rs:31-51`） |
| **schema 20 历史**（现行 schema 21 无版本链/链深）：年线最坏链深余量薄（约 2–9%），宿主为 t3.small，噪声约 ±10%，冷缓存首批明显偏慢 | 仅适用于 schema 20 版本链；schema 21 年线按当前日线序列重派生 | XP-045b/047/048/052；`src/sqlite_market/backend.rs:31-51` |
| aws `acquire.sqlite` 中有 10 个 evidence_only 代现已无代字节（2026-10-24T17:55Z 实测各约 4KB：868f4fa1、804c5a63、04c28d88、84b3f15d、c360967a、2e918330、a49c314b、87322997、2d5851a4、a119629b）。其中 `2d5851a4`（16:57Z）与 `a119629b`（17:52Z）是退役时才清掉字节；其余 8 个在更早退役时就已无字节 | 采集台账有残留；这些代已是 evidence_only，不计入热容量，但目录与清单仍占少量空间 | `du -sb generations/generations/*`、`generations/acquire.sqlite` |
| `/opt/x-data-mcp-preview/mcp.json` 的 `x-data-mcp-demo` 指向一个不存在的 sqlite | 配置过时 | `ls` |
| `src/acquire/fetch.rs` 在未设置 `X_DATA_MCP_TDX_FETCH` 时，回退到编译期的 `CARGO_MANIFEST_DIR` | 不带该变量启动的实例无法下载。main 已修（#375）：未设置时只认包内 `<exe dir>/../scripts/tdx_fetch.py`，找不到即启动失败并报出变量名与路径；已部署的 preview.35 仍是旧回退，下次部署后可删掉此行 | 源码、#375 |
| 生产服务、私有性能栈和多个 agent 会话共用 1 个物理核和 1.9GiB 内存；`/tmp` 是内存盘 | 2026-10-10 约 04:34Z 过载重启；测量结果有噪声 | 运维观察 |
| 热 generation 上限 6：超限后 `acquire_generation` 被拒（`generation_capacity`）。2026-10-24 实测路径：16:56Z 为 6/6（1 active + 5 superseded）→ 16:57:16Z 退役 `2d5851a4…` 降到 5/6 → 16:57:26Z acquire 被受理 → 17:01:34Z 新代 `c184e9c4` 激活，`9d9e38d7` 转为 superseded，**回到 6/6** → 17:52Z 再退役 `a119629b…`，**当前 5/6**，下一次 acquire 无需再手工腾位。容量是**计数**，但退役一个有字节的代同时释放空间（`2d5851a4` 约 546MB、`a119629b` 约 574MB）。自动保留策略（保留最新 2 个 superseded）**已随 preview.35 部署**（2026-10-24T17:25Z），在 activate 提交成功后生效 | 跨代日更会消耗热代名额；当前 5/6，activate 后由自动保留回收到约 4/6 | 2026-10-13 / 2026-10-24 实测（`generations/acquire.sqlite`、`du -sh`、`df /`）、`src/acquire/service.rs:43,925,1058-1084` |
| Skill 日更依赖 AI 会话逐页枚举目录实体（页大小受限、随目录规模线性增长；5,228 实体实测 27 分钟未提交一条更新） | 有更新那天的同步在 Skill 口径下不可用；已改由程序化追加驱动完成，批量更新工具（generic-data-v7）设计中 | 2026-10-13 首次增量实测 |
| AGENTS.md 的环境段、README「当前状态」、`docs/daily-sync.md`「当前状态」仍写 ecs-node / preview.29 / generation `2e9183…` | 读取这些文档的 agent 会被误导 | 本次核对；这三处已加指向本文件的说明；AGENTS.md 的 ecs-node / preview.29 段已在 2026-10-24 标为「历史」 |

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
