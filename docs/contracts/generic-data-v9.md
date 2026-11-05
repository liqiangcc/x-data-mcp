# generic-data-v9 契约（来源刷新 ops 工具）

**状态**：已被 [generic-data-v10](generic-data-v10.md) 取代（2026-11-05，preview.45 on vm2-126，D-74）；
aws-node fallback 仍部署 `generic-data-v9` / compact schema 26（见 [STATUS](../STATUS.md)）。
本文件只记录 v9 相对 [generic-data-v8](generic-data-v8.md) 的增量，v8 的读取面、任务面、
错误码与部署约束继续有效。三个入口合计注册 25 项（`ops` 入口 16）。

## 1. 增量

### 1.1 `refresh_exchange_sources`

- 入口：`ops`（`requires_local`、写工具；research/maint 不可见）；不进入 `All` 之外的
  任何新 socket。
- 输入：`{"mode":"tail"|"full","sources":["sse_listing",...],"dry_run":false}`。
  `sources` 缺省为全部 7 个 HTTP 交易所来源；未知名字拒绝（`invalid_arguments`），
  不静默忽略。`mode` 只在 `status_of` 时可省略（见下）；不带 `mode` 也不是 `status_of`
  的调用是 `invalid_arguments`。
- `tail`：窗口类来源取当前开窗 + 上一自然季度；整表类来源取全表；`szse_listing` 与
  `szse_short_name` 只取第 1 页探针，记录数变化或新行含 `ST`/`*ST`/`退` 时当天补全量。
- `full`：覆盖所选来源的全部指纹（窗口类全部窗口，整表类全部页），是 `verified_through`
  推进与 H=14 天闸门的唯一完整证据。
- **同步只给 `tail`**：一次 tail 的计划请求是 28 个，实测 1.42 s/请求，墙钟约 40 s；
  探针命中当天的整表补全是这条同步路径的**上界例外**：`szse_listing` ≤146 页、
  `szse_short_name` ≤375 页，两者同时命中时最多 549 个请求（约 13 min，1 req/s 下界）。
  超出这个上界的工作不能靠 tail 完成（它不会扩大抓取范围）。**G2 的门禁因此按表计量**
  （D-67，owner 2026-11-01）：没有触发整表的 tail 适用 <300 s；一旦触发整表，改为
  **每张被触发的表各自 <600 s**，run 总时长不设门禁（实测 `szse_short_name` 375 页
  377.4 s、`szse_listing` 146 页 181.3 s，各自都在 600 s 内；两表同日合计约 780 s）。
  每表时长记在 `source_refresh_run_sources.elapsed_ms`（该来源从第一个请求到结束的墙钟
  毫秒，向上取整，所以只有“没有测量”才是 0）。
- **`full` 是异步的**：调用只做校验、写 `running` 记录，然后立即返回
  `{run_id, status:"running"}`（无 `finished_at`）；实际抓取在本进程的后台线程执行，
  写入仍走写协调器（同一写队列）。full 是 1,941 个请求 ≈ 46 min，因此**不能**在单次
  MCP 调用里等完。客户端断开不中断后台 run；服务停止会杀掉线程，该 run 在下一次启动
  时按“回收”规则变成 `failed`。
- **panic 当场结题**：`tail`（MCP 连接线程）与后台 full 走同一个执行入口，抓取过程
  `catch_unwind`。发生 panic 时该 run 立即写成 `failed`（失败码 `refresh_panicked`，
  detail 是 panic 文本，超长截到 200 字符，非 `&str`/`String` 载荷记 `unknown`），
  `verified_through` 不推进，单飞闸门立刻释放，后续 `start` 不必等重启。只有**进程退出**
  （含 `kill -9`、panic 落在写协调器内部导致无法写库）才依赖下一次启动回收。
- **查询**：`{"status_of":"<run_id>"}` 读取 run 记录（mode/status/scope/起止时间/
  requests/reused/changed/failed_windows/unobserved_windows/`verified_through`/失败列表），
  不启动任何 run；未知 `run_id` 是 `refresh_run_not_found`，在 `details.run_id` 里回带该
  值。`status_of` 不能与 `mode`/`sources`/`dry_run` 同时给出（`invalid_arguments`）。
  状态为 `running`/`succeeded`/`failed`；`running` 时 `finished_at` 缺省。计数器与
  `verified_through` 只在 run 结题时一次写入，所以 `running` 回包的计数恒为 0，它不是
  进度条：进行中的 run 只能看到计划范围、`started_at` 与状态。
- **单飞**：同一时刻只允许一个 run。开始新 run 时若已有 `running` 记录，返回
  `refresh_already_running`，`details.run_id` 指向那个 run；`tail`/`full` **互斥**，
  不按 mode 各放一个。检查与插入在同一个写事务里，两个并发开始不可能都成功。理由：
  两个 run 各按 1 req/s 各自限速，合起来就是 2 req/s，违反 XDM-143 的合规约束；
  单飞也让“单 Client 1 req/s”等于“全局 1 req/s”。
- **回收（仅进程退出留下的行）**：`data_service` 拿到数据库租约启动时，把上一个进程实例
  留下的 `running` 行标成 `failed`（detail 里的失败码 `process_restarted`，`source_id`
  为 `*`），不推进 `verified_through`。在跑的 run 不会走到这里：崩溃/停止之外的故障
  （含 panic）由上面的“当场结题”处理。归属判断只用
  `source_refresh_runs.owner_instance`（进程实例 id，即本进程的 run id），不用已过时间
  猜：同一进程里正在跑的 full 不会被自己的重启逻辑回收。
- `dry_run=true`：只返回计划，不发 HTTP、不写来源库、不写 run 记录。
- 输出：run id、状态、scope、真实请求数（含重试与页 1 复核）、复用/变更计数、
  `revision` 列表（旧/新 snapshot、旧/新 `as_of`、页数、行数、窗口）、失败列表。
  异步 full 的即时回包只带计划（`sources[].planned_units`/`planned`），计数全为 0；
  `status_of` 的回包不带 per-source 行与 `revision`（只有 run 行和失败列表是持久记录，
  per-source 行在库里由 R3 读取）。
- 它不发布任何 market 数据集；关闭窗口的修订留给下一次日更（D-66 Q7）。

### 1.2 运行记录（schema 26）

- `source_refresh_runs`：一次运行的 mode/status/scope/起止时间/计数器/
  `verified_through`/task_id/`owner_instance`/detail，进程重启后仍可查。
  `owner_instance` 是打开该 run 的进程实例 id，只用于启动时回收。
- `source_refresh_run_sources`：每个来源的 `complete`（计划的全部请求是否完成，含已观测
  的 0 行窗口）、请求/变更/未观测/失败计数与该来源的 `verified_through`。已知空窗口计入
  `unobserved_windows`，作为覆盖 gap 交给 R3，不算失败也不阻塞 `complete`；只有成功且
  `complete=1` 的来源才推进日期。tail 的两窗口、未触发的探针都不推进。
- `source_refresh_run_members`：`(run_id, source_id, request_fingerprint) → snapshot_id`，
  外加页数、行数、`as_of`、`fetched_at`、是否变更、窗口。R3 的重放选择按 run 成员
  取页；0 行窗口没有可选页，探针页与失败的表尝试也不是被选中的页，均不写入成员。
- `exchange_source_snapshots` 的主键由 `snapshot_id` 改为
  `(snapshot_id, request_fingerprint)`：相同字节的不同请求（例如多个空窗口）必须能
  各自复用；同一请求以不同 sha 仍保留多行历史。

### 1.3 读面的 `metadata.verified_through`（XDM-157）

`query_data`/`get_data_capabilities` 的 `manifest[].metadata` 增加可选 `verified_through`
（`YYYY-MM-DD`）：该数据集全部输入来源中**最旧**的“完整刷新确认日”，即
`source_refresh_run_sources` 里 `complete=1` 且 run `succeeded` 的最新日期取 `min`。
它与行的 `as_of`/`known_at` 是三个字段：`as_of` 说事实/字节到哪一天、`known_at` 说我们
何时知道、`verified_through` 说这组指纹被确认没变到哪一天。它只为四个 R3 只读 item 的
数据集定义（`security_master`/`security_names`/`risk_warnings`/`suspensions`）；一个来源没有
完整 run（fresh init）时**整个字段缺省**，不用页面的 `as_of` 猜。四个数据集的读取面都已实现
（`market.security_names` 自 XDM-163 起读取 `security_name_history` 区间，未登记版本仍回答
`absent`/0 行），所以 `metadata.verified_through` 在四个数据集上都可见。事实只存在于 run 记录
（§1.2），所以重放或整表重写不会让它消失。

### 1.4 整表一致性

整表采集在最后一页之后重读第 1 页；sha 变化时重试一次，仍变化则
`window_changed_mid_fetch` 且不选中该页集。选中前还逐页从来源库读回字节，核对页号、
页数、行数；不一致或中途失败为 `snapshot_incomplete`，不写成员、不推进
`verified_through`。

## 2. 部署与兼容

- 能力修订 `generic-data-v9`；部署实例（preview.42，2026-11-02）`ops` 的 `tools/list` 为 16 项
  （含本工具），线上实测 **30,159 B**。
- schema 26 仍按 D-08 全新 init，不做原地迁移；旧二进制拒绝 26。
- 回滚：回到 v8 二进制并保留旧库归档；schema 26 的 run 记录不参与 v8 的读写面。
- 工具面预算：`tools/list` 不内联 `outputSchema`。R2b 后 CI 实测 research 11,691 B、
  ops 30,114 B、maint 13,523 B（R2 基线为 11,691 / 29,814 / 13,523）；main 的
  15-tool ops payload 为 28,701 B。增量拆解：R2 新增工具 `refresh_exchange_sources`
  贡献 1,113 B（29,300 → 30,500）；R2b 在同一工具上把 `mode` 改为可省、加 `status_of`，
  输出加 `running` 状态与可选 `finished_at`，再加 300 B（无新增工具）。按 v8 §6.3
  「已批准上限 + 已记录面增长」的规则，ops 上限从 30,500 调为 30,500 + 300 = 30,800 B
  （`tests/mcp/v8_tool_surface.rs`）；部署后线上实测 30,159 B，距上限 641 B（owner 2026-11-01 已批准该预算）。
