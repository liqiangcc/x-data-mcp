# S5 验收驱动套件（schema-21 latest-only）

**状态：只写未跑**（TASK-PAR-1 B）。作者在窗口内不运行任何驱动、
不启动服务、不连 socket；c-worker1 在 S5 窗口审并试跑。

## 流程（与 XP-015 负载链同构）

在**私有栈**上一次跑通（每步产物写入 `--out`）：

| 步 | 动作 | 计时口径 |
|---|---|---|
| 0 | 解包 release 包到私有目录（人工） | — |
| 1 | `task_sqlite init20 <db>` 全新空库（v21 二进制打印 `task schema 21 ready`） | init wall |
| 2 | 起 `data_service` + `acquire_service`（trace OFF，`X_DATA_MCP_STAGE_TRACE` unset） | socket 可连等待 |
| 3 | gen-A：mirror ← `--input-old` → `acquire_generation` → `verify_generation` → `activate_generation` | 步内 wall |
| 4 | collection：`securities.catalog` 一遍 + `market.daily` ≤100 实体/批串行 | **write-loop**（首个写提交 → 最后任务终态，含轮询） |
| 5 | yearly 首次：每批 availability(daily+yearly) + `get_yearly_base_years` + `update_data(market.yearly)` | write-loop |
| 6 | gen-B：mirror ← `--input-new` → acquire/verify/activate | 步内 wall |
| 7 | 日线跨代 append：`update_data` items=[last+1, end] | write-loop |
| 8 | 日线 no-change：`update_data` items=[last, last] | write-loop |
| 9 | 年线重聚合（同 5） | write-loop |
| 10 | 读回：`s5_readback.py`（样本 query/availability/`task_sqlite check`/`integrity_check`） | 步内 wall |

判定门禁（`docs/SLO.md`）：各写负载 write-loop <600s；计数
（considered/published/no_change/failed/needs_verification/unknown）
全报，unknown 不并入成功；PASS/FAIL 由 Owner 依原始 JSON 判定，
套件不自判。

## 文件

- `s5_common.py` — 禁区路径守卫（`/opt/x-data-mcp-preview`、
  `/root/x-perf-g8*` 一律拒绝，含 `..`/符号链接绕行）、Mcp 客户端、
  `wait_task`、`Phase`（write-loop 记录）、outcome 汇总。
- `s5_run.py` — 链式驱动，全部路径走 CLI（无指向禁区的默认值，
  启动即断言）：`--stack-root --pkg-bin --gen-root --input-old
  --input-new --out [--init-subcommand init20] [--end-date DATE]
  [--phases …] [--max-chunks N] [--no-service-mgmt] [--request-prefix]`。
  `--no-service-mgmt` 时只驱动已运行的 socket，不 spawn/kill 服务。
- `s5_readback.py` — 读回。`--sock --db --task-sqlite --out
  [--sample-list FILE] [--end-date]`；`--db` 用**已停服务的快照**
  （check 只读扫，活库会撞 WAL）。
- `startup_probe.py` — 冷/热启动检查耗时：`--db --work-dir
  --data-service --gen-root --sock --out [--runs N]`；冷启动对私有
  库副本 `posix_fadvise(DONTNEED)` 后测到 socket 可连。回答「冷启动
  ≈11–13s 是否超隧道 10s 等待」。
- `test_s5_kit.py` — 不连服务的单测（守卫、计时、汇总、分类复用）：
  `python3 -B -m unittest discover -s evidence/perf-g8/s5 -v`。

## 已知未验证项（试跑前看）

1. acquire 侧调用形状按 `.devin/skills/x-data-daily-sync/SKILL.md`
   步骤 2 写（`acquire_generation/verify_generation/activate_generation/
   get_acquisition_task/sync_status`，均带 `contract_version:"v1"`）；
   wire 字段名已对照 `src/acquire/mcp.rs` 的 ENTRIES 与 wire 结构，
   但未对 live server 验证（verify 的 decision 值接受
   `consistency_verified`/`VERIFIED_COMPATIBLE` 两种写法，若实际不同
   需要按真实字段改）。
2. `get_acquisition_task` 终态集合按 `succeeded/failed/cancelled`
   判定；`generation_id` 取 `acquire_generation` 受理返回字段——
   若受理体不直接返回，需改从 `sync_status`/`get_acquisition_request`
   取（未核实）。
3. `phase_yearly` 的年份交集口径沿用 yearly_g8.py（present_years ∩
   [first_record.year, last_record.year]），未加「未来年异常」分支——
   XP-015 的 Asia/Shanghai 当前年判定未移植（按 ≤2026 常量过滤，
   S5 时如跨年需改）。
4. `mirror_inputs` 用 `copy2` 整目录替换 mirror（PLAN-XP-015 §2
   口径）；镜像文件 ~570MB，试跑注意磁盘余量（停止线 df<5GB）。
5. `s5_run.py` 的 `--end-date` 必填于 append/nochange 阶段（= gen-B
   快照最后交易日，操作者给）。
6. 服务启停只发 SIGTERM 并 wait（30s 超时后 SIGKILL）；栈目录权限
   0700/0600 沿用 XP-015 约定。
