# M1 部署前实测清单（私有栈）

关联 Issue：[#383](https://github.com/liqiangcc/x-data-mcp/issues/383)。
**这是清单，不是结果。** 每一项都要在私有栈上真实跑一遍并把数字贴回 Issue；没有跑过的项写
"未运行"，不得写 PASS。判定由 owner 按原始数据做（[SLO](../SLO.md) §3），执行者不自行判定通过。

设计依据：[M1 设计](issue-383-m1-research-data.md) §9.3（耗时/存储估算）、§10（迁移与部署）、
§15（验证方式）；门槛：[SLO](../SLO.md) 第 1–5 行；实现：[实现记录](issue-383-m1-implementation.md)。

## 0. 前置与边界

- **批准**：部署、打 tag、发布、`scripts/build_release.sh` 加 `--features export`、`/root` 下
  建 venv 装 pyarrow/duckdb，都要 owner 单独批准（设计 §10.3）。
- **测量窗口独占**（SLO §3，2026-10-10 签字）：窗口内暂停 agent 会话，不跑 cargo/编译/其他重负载；
  出现其他重负载的样本作废并如实记录。
- **二进制来源**：必须是 CI 或 Release 构建（SLO §3 判定）；记录 rustc 版本、二进制 digest
  （`/proc/<pid>/exe` 与安装包一致）、代码 SHA。
- **停止线**（H18 / SLO §3）：MemAvailable < 300MB 或磁盘可用 < 5GB 立即停止；本机 1.8GiB 内存，
  重型步骤前先看 `MemAvailable`。
- **生产只读**：`/opt/x-data-mcp-preview` 不改文件、不 stop/start/restart 单元、不调用写工具；
  读 sqlite 用 `file:…?mode=ro`。
- **私有栈**：独立 `--stack-root`，不复用生产目录；复用现有 harness
  `evidence/perf-g8/s5/s5_run.py`（写阶段驱动）与 `evidence/perf-g8/s5/startup_probe.py`
  （冷/热启动探测）。两者是 schema-21 时期的工具，`--init-subcommand init21` 现在建的是
  **schema 24**（`COMPACT_SCHEMA_VERSION`，`src/sqlite_tasks.rs:45`：M1 的 022 + M2 PR-1 的 023 +
  M2 PR-2 的 024；M2 的表都是空的、无写入路径，024 只是把 `capital_events.category` 的取值
  放宽到 2,3,5,7,8,9,10）；冷启动用 `posix_fadvise(DONTNEED)` 对**私有副本**降温，不 drop 全局 page cache，
  不影响生产。
- **记录格式**：每个阶段一个 JSON（字段与 `s5_run.py` 的 phase 输出同名），加一份汇总；
  汇总里必须同时给 considered / attempted / published / no_change / failed / absent 计数
  （SLO §2：unknown 不算成功）。

## 1. 构建与产物

| 项 | 做法 | 记录 | 预期/门槛 |
|---|---|---|---|
| 默认构建 | 用 CI 产物或 `cargo build --release --locked`（在 CI/发布流水线，不在本机） | 二进制字节数、构建时间、rustc 版本 | 体积门禁见 §4；没有历史硬门槛 |
| 导出 feature | `cargo build --release --features export --locked --bin export_research` | 二进制字节数、构建时间 | 参考值：CI 记录 release 构建 110s（PR #408）；**编译时间与增量体积要贴 Issue**（U-3） |
| 发布包 | `scripts/build_release.sh` **默认带 `export_research`**（#413：脚本固定 `--features export`，打包后断言该命令存在，`scripts/build_release.sh:11-17,39`） | 包内 bin 清单与 `manifest.json` 的 `features` | 包内必须有 `bin/export_research`；重跑 `scripts/smoke_release.py` |

## 2. 冷启动（U-4，设计 §9.3 的风险项）

1. 全量重建出 schema 24 库后（M2 的 023 只建表、024 只放宽 `capital_events.category` 的 CHECK，都无写入路径，表是空的，不影响本节的序列数），确认序列数：`select period, count(*) from market_series group by period`。
   **预期**：`period=1` 10,457（5,224 不复权 + 5,224 前复权 + 9 指数）、`period=2` 5,224，
   合计 **15,681**。设计 §9.3 的"5,224 → 10,457"只数了日线，年线也参与启动检查，所以估算的
   冷启动应按 15,681 而不是 10,457 复核（本清单的差异说明）。
2. 用 `startup_probe.py` 对私有库跑冷/热各 ≥3 次，记录"到 socket 可服务"的耗时；再用
   `scripts/wait_service_ready.py`（D-17：MCP `initialize` + `serverInfo.name`）复测一次端到端。
3. 记录 `task_sqlite check` 深检耗时（不在启动路径，但要知道代价）。

| 量 | 基线（实测） | 门槛 |
|---|---|---|
| 冷启动到可服务 | 2026-10-23 主机重启后 **31.5s**（preview.34，574 个历史任务，冷 page cache） | **硬上限 90s**：`x-data-mcp-tunnel` 的 `ExecStartPre` 虽设 120s，但单元未设 `TimeoutStartSec`，systemd 默认 90s 覆盖它（[STATUS](../STATUS.md) 已知问题） |
| 热启动到可服务 | 2026-10-24T17:25Z 切换 **13.5s**（preview.35，热 page cache） | 同上 |
| 启动检查本身 | 合成夹具 10,400 序列：冷 11.38s / 热 0.36s（[latest-only-storage](../latest-only-storage.md) §8） | 序列数约 ×1.5，按 O(#series) 外推冷 ≈17s；**实测为准** |

## 3. write-loop（SLO 第 1–3 行，hard <600s）

每个阶段用单调时钟：从提交第一个写请求到最后一个任务终态，任务之间的探测与轮询都计入
（SLO §2）。preflight 与 readback 单独报告，不混进 write-loop。

| 阶段 | 基线（v6/schema 21） | 设计估算（§9.3） | M1/v2 实测（私有栈，2026-10-27） | 记录项 |
|---|---|---|---|---|
| 首次全市场采集 | collect write-loop 200.3s（k=5）/ 208.8s（k=1），5,228 considered / 5,224 succeeded / 4 failed（官方缺成员，预期） | 260–320s | **664s**（双序列；基线约 200s） | 阶段 JSON + 4 个 absent 实体清单 |
| 同代 no-change 日更 | 70.9s（k=5）/ 74.5s（k=1），5,224 no_change，`data_versions` +0 | 85–100s | **169s** | 净增长（≤4KiB/item）、`data_versions` +0、10 个内部 item 的 item 级结果 |
| 跨代 append 日更 | 97.4s（k=5）/ 93.5s（k=1），5,224 published | 120–145s | **735s**（v2 plan/run；**超 SLO 第 2 行的 600s，按 D-52 临时豁免**） | 追加周期库增量（基线约 +27MB） |
| 年线重聚合 | 233.3s / 234.8s | 不变（±5%） | **460s** | 每个日更周期库增量（基线约 54MB） |

最后一列的预期值来自 2026-10-27 的私有栈实测（代码 `84b6657`，2 vCPU；跨代日更那次测量时
load average 约 6，**不是独占窗口**，按 [SLO](../SLO.md) §3 不能当门禁判定）。本清单仍要求按 §0
的独占窗口重测：重测结果与这一列不一致时以重测为准，并记录偏差。跨代日更 735s 高于第 2 行的
600s，owner 2026-10-27 给了临时豁免（[SLO](../SLO.md) 第 2 行、[DECISIONS](../decisions.md) D-52），
到期条件是「v2 日更分块并发」完成并重测 <600s。

额外必测（设计 §9.3 与 PR-7 的偏差项）：

- **日历 coverage 阶段**：全量 `GROUP BY trade_day` 的 CPU 成本没有历史基线，PR-7 只在合成夹具
  （120 序列 × 40 bar）上断言 < 30s。全市场单次耗时必须实测并记录。
- **内部 item 计数**：一次全市场 run 的 `item_count` = plan eligible + 10；内部 item 失败时
  run 收尾 `partially_failed`、不回滚已提交行情（用一次人为失败验证，再恢复）。
- **两条序列的一致性**：run 结束后同一实体 `adjustment=0` 与 `adjustment=1` 的日期范围一致
  （PR-2 的约束），随机抽 20 个实体核对。

## 4. 体积（SLO 第 5 行，hard ≤8GiB 目录口径）

| 量 | 基线/估算 | 记录 |
|---|---|---|
| 全量重建后的 `state/*.sqlite` | 现库 1,185,640,448 B（schema 21）；设计估算 schema 24 约 2.7GB（+745MB 不复权 +745MB 前复权 +约 7MB 因子 +日历/指数；M2 的 023/024 表为空，不增字节） | `du -sb`、`pragma page_count*page_size` |
| `/opt` 部署树全树 | 2026-10-24 基线 4.2GiB；估算部署后约 5.7GiB | `du -sh`（含 releases/generations/state/reports） |
| 事件日志增速 | 10,448 行/日 → 15,672 行/日（+5,224；设计 §9.3 估 +1.8MB/日、+650MB/年） | `market_events` 行数与 `dbstat` 增量 |
| 首采后 DB（SLO 第 4 行） | 硬门槛 ≤2.5GiB（2.684GB）；M1 设计估算库约 2.7GB ≈ 2.51GiB，**正好压线、按估算略超** | 实测 DB 字节。若超门槛，由 owner 决定改门槛还是砍体积；本清单不自行判通过 |
| 导出体积 | 夹具实测约 1.5GB（不复权 32.3 B/行、6dp 前复权 55.9 B/行；设计 §7.7 已从 340MB 修正）；小夹具 5,030,424 B | 全量导出的字节数、耗时、压缩率；`/root/x-data-exports` 的保留策略未定（D-29 之后另定） |

## 5. 真实 generation 上的验收（AC）

AC 定义见 Issue #383 的验收表。CI 只覆盖小夹具，下面这些必须在真实 generation 上跑一次：

| AC | 做法 | 判据 |
|---|---|---|
| AC-M1-1 | 抽 ≥20 个实体（含 1 个手日 bar 的实体），把 `adjustment=0` 序列与同一 generation 的 `.day` 成员逐根比对（4 个价格字段 + volume/amount/`r7_raw`） | 逐根相等；volume 按 #381 归一（手日 ×100、无法确认 `null`） |
| AC-M1-2 | 用库里的不复权 bar + `adjustment_events` 重跑 `qfq-exact-v1`，与库内 6dp 序列比对 | 逐位相等 |
| AC-M1-4 | 对全部 A 股不复权 bar 检查 `trade_day ∈ trading_days` | 落在日历外的比例 ≤0.01%（1/10000），逐条列出（1991 年深市边界） |
| AC-M1-7 | 同一 active generation、无日更，导出两次 | 两次 `snapshot_id` 与全部文件 sha256 相同 |
| AC-M1-10 | `query_data` 取 `sh.000001`、取 raw/因子/日历/指数 | `market.daily(sh.000001)` → absent；`market.index`/`market.calendar`/`market.adjustment_events` 自 #410 起由 `query_data` 直接返回（v8 的 dataset 分支：`src/sqlite_supply.rs:411-423`、`docs/contracts/generic-data-v8.md:129-131`）；只有 `market.daily` 的 `adjustment=0`（raw）序列仍只在导出里可见 |

**下游 `x` 仓库的部署前置（D-32）**——两个上游契约点，必须用真实 `query_data` 验一次：

1. 日线读路径仍只解析 `adjustment=1` 的序列（`query_data` 返回的 `version_id` 与库里前复权序列
   一致，且不出现两条序列混读）。
2. 序列清单的 `metadata.adjustment.value` 仍是字面 `"qfq"`（`KlineReader` 依赖这个值判断口径）。

`x` 侧代码已只读核对为"可以直接兼容"（无 2dp 假设、无 `version_id` pin），但**数值结果会变**：
所有用到价格的计算（回撤边界、BOLL、区间收益、模拟成交价）都会变。建议 `x` 仓库补精度说明，
属另一仓库改动，需另行批准。

## 6. 回滚演练

- 按 D-08/D-33：归档 schema 24 库，回到上一版 release（preview.35）+ 归档的 schema 21 库，确认服务可启动、
  `query_data` 可读、导出命令不影响库。
- **副本要先改成 WAL**：用 backup API 做的副本是 `journal_mode=delete`，而 preview.35 的启动检查要求 WAL
  （`src/sqlite_market/compact_runtime.rs:452-455`），直接启动会拒绝；先跑 `PRAGMA journal_mode=WAL` 再启动，
  实测改完 **22.8 s** 就绪。
- 正式回滚用部署时改名归档的**原库**：它的 WAL 模式随文件保留，不需要这一步；backup API 的副本只用于演练。
- 记录回滚耗时与任何手工步骤；回滚脚本用 `scripts/rollback_preview.sh`（若与本次部署方式匹配）。注意脚本会
  `cp` 副本并删 `-wal`/`-shm`，所以副本的 `journal_mode` 必须先改成 WAL（见上一条）。

## 7. 汇总（贴回 Issue #383）

1. 每个阶段的 JSON 路径与关键数字（write-loop、item 计数、净增长、DB/目录体积）。
2. 冷/热启动各 3 次的耗时与最小值/中位数。
3. AC-M1-1/2/4/7/10 的原始输出或脚本路径。
4. 两个 `x` 契约点的实测结果。
5. 未运行项清单与原因；任何与设计估算的偏差（含本清单 §2 的序列数差异）。

## 8. 部署窗口（2026-10-27 裁决）

- **窗口**：2026-10-27 收盘后；部署 tag 为 `v0.2.0-preview.36`。
- **部署期间暂停开发 `pi`**，并**冻结 main**：窗口内只接受本次部署日提交，不合入其他改动。
- **重建步骤**（每步确认后再下一步）：
  1. 归档 `state/preview.sqlite`（schema 21）到窗口外，全新 `task_sqlite init` 到 schema 24。
  2. 全量采集：目录 + 日线 + 年线首建，**约 11–16 min**。
  3. v2 `plan_market_update`/`run_market_update` 派生，**约 10 min**。
  4. 年线重聚合，**约 8 min**。
  5. 只读读回与 §5 的 AC 抽查，写部署记录。
- **不可读时间**：重建期间 `x` 侧读不全，约 **40–50 min**（各步耗时之和加上切换与就绪）。
  期间不跑其他重负载，保持独占窗口（SLO §3）。
- 本清单 §2/§3/§4 的实测数字在部署后按实际重跑结果回填；没有跑过的项仍写“未运行”。
