# generic-data-v9 契约（来源刷新 ops 工具）

**状态**：代码契约（未部署）；当前部署仍为 `generic-data-v8` / compact schema 25。
本文件只记录 v9 相对 [generic-data-v8](generic-data-v8.md) 的增量，v8 的读取面、任务面、
错误码与部署约束继续有效。代码注册 25 data tools（三个入口；`ops` 入口 16），
代码 schema 26（未部署）。

## 1. 增量

### 1.1 `refresh_exchange_sources`

- 入口：`ops`（`requires_local`、写工具；research/maint 不可见）；不进入 `All` 之外的
  任何新 socket。
- 输入：`{"mode":"tail"|"full","sources":["sse_listing",...],"dry_run":false}`。
  `sources` 缺省为全部 7 个 HTTP 交易所来源；未知名字拒绝（`invalid_arguments`），
  不静默忽略。
- `tail`：窗口类来源取当前开窗 + 上一自然季度；整表类来源取全表；`szse_listing` 与
  `szse_short_name` 只取第 1 页探针，记录数变化或新行含 `ST`/`*ST`/`退` 时当天补全量。
- `full`：覆盖所选来源的全部指纹（窗口类全部窗口，整表类全部页），是 `verified_through`
  推进与 H=14 天闸门的唯一完整证据。
- `dry_run=true`：只返回计划，不发 HTTP、不写来源库、不写 run 记录。
- 输出：run id、状态、scope、真实请求数（含重试与页 1 复核）、复用/变更计数、
  `revision` 列表（旧/新 snapshot、旧/新 `as_of`、页数、行数、窗口）、失败列表。
- 它不发布任何 market 数据集；关闭窗口的修订留给下一次日更（D-66 Q7）。

### 1.2 运行记录（schema 26）

- `source_refresh_runs`：一次运行的 mode/status/scope/起止时间/计数器/
  `verified_through`/task_id/detail，进程重启后仍可查。
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

### 1.3 整表一致性

整表采集在最后一页之后重读第 1 页；sha 变化时重试一次，仍变化则
`window_changed_mid_fetch` 且不选中该页集。选中前还逐页从来源库读回字节，核对页号、
页数、行数；不一致或中途失败为 `snapshot_incomplete`，不写成员、不推进
`verified_through`。

## 2. 部署与兼容

- 代码能力修订 `generic-data-v9`；部署实例在 preview.41 前仍为 `generic-data-v8`，
  此时 `ops` 的 `tools/list` 没有本工具，16 只是代码 allowlist 计数。
- schema 26 仍按 D-08 全新 init，不做原地迁移；旧二进制拒绝 26。
- 回滚：回到 v8 二进制并保留旧库归档；schema 26 的 run 记录不参与 v8 的读写面。
- 工具面预算：`tools/list` 不内联 `outputSchema`；ops 新增一个输入 schema，预算以
  实现 PR 的实测值为准（`tests/mcp/v8_tool_surface.rs`）。
