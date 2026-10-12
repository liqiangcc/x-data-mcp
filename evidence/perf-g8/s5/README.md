# S5 验收驱动套件（schema-21 latest-only）

**状态：未跑；Part 1 已对照源码修订（见末节）**（TASK-PAR-1 B）。作者在窗口内不运行任何驱动、
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

## 未验证项核实记录（c-worker1，Part 1，仅对照源码，未启动任何服务）

1. **acquire 调用形状** — 对照 `src/acquire/wire.rs` / `mcp.rs` / `service.rs`：
   参数形状（`contract_version:"v1"`、`request_id`、`generation_id`、
   `expected_previous`）正确；**返回字段原写错**：受理体只有
   `request_id/task_id/state/reused`；task 视图用 `state`（非 `status`）；
   verify 的判定在 `state == "consistency_verified"`；activate 成功为
   `state == "active"`（否则 `refused`+`reason_code`）；`sync_status` 的活动
   指针在 `active.generation_id`（非 `active_generation`）。已改并加假客户端单测。
2. **终态集合 / generation_id** — 终态是 `succeeded`/`failed`（字段 `state`），
   `generation_id` 在 **task 视图**里（受理体没有）；原代码会永远轮询到 2700 s
   超时。已修。
3. **年份口径** — 原实现把「未来年」静默交集掉；已移植 yearly_g8 的
   `resolve_years`（Asia/Shanghai 当前年之后记 `future_years_in_response`，
   未请求项/未解析项/缺 present_years 都进 `unavailable`），抽成纯函数加单测。
4. **mirror** — `Mirror` fetcher 只认 `base.zip/gbbq.zip/hsjday.zip` 三个名字，
   且 `std::fs::read` 整个 hsjday（~550MB 堆）再写 staging：服务 RSS 峰值
   ≥ 550MB，窗口前 MemAvailable 须留足。原 `mirror_inputs` 会把输入目录里的
   evidence JSON 也拷进去，已限定三个角色文件。
5. **`--end-date`** — 原实现在 collect/yearly1 数小时之后才在 append 阶段报错；
   已改为启动时对 append/nochange 阶段先校验。
6. **服务启停** — `data_service <db> <sock>`、
   `acquire_service <gen-root> <sock> --mirror <dir>` 参数与 `src/bin/*.rs`
   一致；两者都装了 SIGTERM 处理；`X_DATA_MCP_GENERATION_ROOT` 由 data_service
   读取（`src/data_supply/generation.rs`）。

### 试跑前必须知道的阻断项（新发现）

- **recency 探针**：`verify_generation` 取 `hsjday` 里 64 个确定性抽样成员的
  最大末根 bar，与 `acquisition_completed_at`（= 服务器当前时刻）比较，
  **> 14 天即 NOT_VERIFIED**（`service.rs` `probe_coverage`）。k=5 截断后抽样最大 bar
  = 2026-09-23，今天（10-12）已 19 天 → gen-A 无法 verify/activate。
  处理：`make_trunc_k.py --keep-member sh/lday/sh880312.day` 让一个**不在
  目录范围内**的板块指数成员保持未截断（抽样最大 bar 回到 10-08）；该成员
  不进入任何采集/追加负载。gen-B（10-08）自身在 10-22 之后同样会过期。
- 新增 `make_trunc_k.py`（参数化 K，替代 `drivers/make_trunc_hsjday.py` 的
  硬编码路径）。
- `startup_probe.py`：冷启动前对副本先 `fsync` 再 `DONTNEED`（脏页不会被丢弃）。
- 禁区新增 `/root/x-data-mcp-owner`。
- 仍未对 live server 验证：schema 21 构建下 data 工具的 wire（availability /
  `update_data` / `get_yearly_base_years` 沿用 XP-015 形状）；演练（`--max-chunks 3`）负责确认。
