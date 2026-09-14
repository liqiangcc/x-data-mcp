# #92 默认选择可实现性核对与设计冻结（第一阶段）

状态：设计冻结，供协调决策评审。**本文件不落任何代码、不改 wire/schema、不创建 PR。**
基线：`origin/main` = `01643ba092adc3c038dc6023108e4c7eb366f432`；分支 `issue-92-default-selection`。

协调口径（#92 评论）：
- 首次采集"已启动" = 至少一个对应 collection item 发生过真实执行 / 获得实际执行 claim；`accepted`/`pending` 不算；之后的 `failed`/`cancelled`/`interrupted` 不抹去 ever-started 历史。
- default 动态选择首次受理时必须冻结。
- gap 只能来自可靠持久证据，不能按日期不连续猜测。

## 1. 可实现性结论

### 1.1 ever-started：`EVER_STARTED_DURABLE_FACT_AVAILABLE`

当前模型存在**持久、跨重启、可复核**的事实来源，但它是组合事实，不能只看 `task_items.stage` 当前值。

**主证据：`task_items.stage ∈ {'running','finished'}`**（`migrations/tasks/001.sql`）

- `Change::StartItem` 提交 pending→running；`Change::FinishItem` 提交 running→finished 并带 outcome（`src/task_control/mod.rs` 状态转移；执行顺序 `src/task_control/service.rs`：Claim → StartItem → execute → FinishItem → FinishExecution）。
- `finished` 终态无任何重置路径。
- `running` 跨进程死亡保留：reconcile/失联检测将任务置 `needs_verification` 但**不重置 item stage**；`FinishExecution`/`FinishCancellation` 在任一 item 仍 running 时直接拒绝（`mod.rs` FinishExecution 分支 `NeedsVerification("execution has not safely ended")`），正常收尾路径不会静默改写 running item。

**唯一重置路径与独立兜底证据：`stopped_write_checks.audit_json`**（`migrations/tasks/007_stopped_checks.sql`）

- 对 collection item 而言，verified interruption 是唯一把 running item 改回 pending 的路径（`src/task_verification/decision.rs`："A previously running item is now unfinished, with its last known stage retained in audit"）。
- 该重置与 `stopped_write_checks` 审计行**同一原子提交**（`src/sqlite_verification.rs::commit_stop_check`），`audit_json` 内含完整 `Audit{target_before: TaskSnapshot, target_after}`——`target_before.items[].stage` 保留重置前的 `running`，`audit_sha256` 绑定；读取侧 `read_receipt`/`validate_audit` 重新校验快照一致性，篡改即 integrity 错误。
- `verification_inspections`（009，`src/verification_inspection/decision.rs`）的 `Interrupted` 结论同样重置 running→pending 并在 `audit_json.facts.target` 保留重置前快照，但 `verification_execution::supported` 限定其目标仅为两个 verification 契约族——**不作用于 collection item**，仅列为同类先例。
- `task_items` 无任何 DELETE 路径；`FinishExecution`/`FinishCancellation` 在任一 item running 时拒绝；reconcile 只置 `needs_verification` 不改 stage。除上述审计路径外不存在无审计的 stage 回退。
- 因此即使当前 `task_items.stage` 被重置为 pending，`stopped_write_checks` 审计行仍是独立的、不可被覆盖的 ever-started 历史事实。

**旁证持久事实（非唯一来源，仅作旁证）**

- `execution_uncertainty` marker（`task.result_json`）：持久绑定 outcome 未知时仍 running 的具体 `item_id`，读取时校验该 item 必须仍为 running。
- `market_update_results` / `yearly_execution_results`（migrations 005/006）：按 `(task_id,item_id)` 的执行回执，仅属 update/yearly 契约族。
- `publication_batches` / `task_publications`：成功执行产生的发布绑定。

**永不构成 ever-started 证据**

- `requests` 行（受理事实）、`tasks.status='pending'`、`task_items.stage='pending'`、`accepted_run_id`、单独的 `owner_run_id`（claim 先于 StartItem）、`scope_occupancy`（受理时即写入、终态清除，pending 任务也占有）。

**契约族过滤（强制）**：`item_id = scope_id(symbol,period) = ["market",symbol,period]` 被 collection、`update_data`、`backfill_data` 共享（`src/task_control/legacy.rs`、`src/market_update/tasks.rs`）。判定"对应 collection item"必须 join `tasks.command_key_json`，限定 `operation='start_collection'` 且 `dataset='market.daily'`（重试任务携带原 command key，自然覆盖）。update/backfill item 的启动**不**计入首次采集已启动。

### 1.2 proven-gap：`PROVEN_GAP_FACT_NOT_AVAILABLE`

逐表核对结果：

- **migrations/**：全部迁移文件无 gap 表，无 proven-gap 持久记录。
- `data_versions.coverage_status`/`coverage_reason`：版本级自由文本标签（如 `"complete"`/`"Unknown"` + reason），来自发布记录 `batch.coverage` 字段——**不是**机器可验证的逐证券 gap 区间。
- `version_evidence`：按 `(version_id,field)` 的字段级溯源（volume_unit/amount_currency/amount_unit 等），**不是**日期区间 gap 证据。
- `AvailabilityItem.confirmed_gaps`（`src/data_supply/wire.rs`）：wire 类型存在，但唯一生产者 `src/data_supply/observe.rs` 恒输出 `vec![]`；`unresolved_ranges` 仅在 availability=Unknown 时输出（语义相反）。
- 明示限制：`observe.rs` limitation "range completeness is not inferred from dates or version coverage alone; no implicit collection or repair"；`src/data_supply/registry.rs` RANGE_DESCRIPTION "Default selection, automatic gap discovery and catalog updates are not implemented"；`docs/tasks/issue-92.md` 同述。

结论：当前无任何可作为 #4"已明确历史缺口"的 durable fact。不得从交易日不连续、缺失记录、日期范围、版本缺席或当前日期构造 gap。

## 2. 设计冻结

### D1. `first_collection_started` 唯一判定来源

ever_started(security S, period P) 为**读取时派生事实**，必须在受理的**同一个 SQLite 事务快照**内求值：

```
ever_started(S,P) ⟺ 存在持久证据表明 collection item scope_id(S,P) 获得过真实执行 claim：
  (a) task_items JOIN tasks ON task_items.task_id=tasks.task_id
      WHERE task_items.item_id = '["market","S","P"]'          -- scope_id 序列化
        AND json_extract(tasks.command_key_json,'$.operation') = 'start_collection'
        AND json_extract(tasks.command_key_json,'$.dataset')   = 'market.daily'
        AND task_items.stage IN ('running','finished')
  OR
  (b) stopped_write_checks 行（target_task_id = 该 collection 任务）
      其 audit_json.target_before / target_after 快照中
      该 item stage ∈ {running,finished}
      —— 覆盖唯一会把 running item 重置回 pending 的路径；
      后续若有新增"针对业务任务的重置/审计"路径，必须纳入 (b) 集合并经评审
```

fail-closed 条件：

- 无 (a)/(b) 证据 → 判定为**未曾启动**。
- 存储错误、audit digest 不一致、快照校验失败 → **受理整体失败**，不得按任一方向猜测。
- pending/accepted/occupancy/owner/调用方声明 → 永不作为证据。
- `cancelled`/`failed`/`interrupted` 终态不抹去已存在的 (a)/(b) 证据（协调口径）。

### D2. 默认证券选择器 = 当前有效 catalog + ever-started

- "当前有效 catalog" 唯一来源：`catalog_head.version_id`（singleton）→ `read(version_id)`（compat_json sha256 + catalog_members 投影全量校验，见 `src/sqlite_catalog.rs`）。无 head 或校验失败 → 受理失败，fail-closed。
- 默认候选 = 该 catalog 版本成员中 `ever_started(S,'day') = false` 的证券（即仍需首次采集者）；每个证券产生一个 item（沿用现有 item 派生与 `scope_id` 占用语义）。
- 候选为空 → `invalid_arguments` 拒绝，不建空任务（与显式选择 1..100 预算同风格；默认选择是否沿用相同预算上限由实现阶段按契约评审确定，设计冻结不放宽上限）。
- 解析所得实体集合、`catalog_version_id`、`catalog_digest` 写入每个 item 的 `frozen_inputs.selection_evidence`（沿用 `WriteSelection::Catalog` 已建立的冻结先例 `src/data_supply/collect.rs`）。

### D3. 首次受理冻结 + request_id 重放不重算

- 动态选择器**只在首次受理时、单个写事务内**求值一次：normalize → future-date guard → 解析（catalog head + ever-started）→ 插入 task/items/scopes/occupancy/request，全部原子。
- 同一 `request_id` 重放：返回已存 `requests` 记录 → 同一 task_id → 同一冻结 items；**不**重读 catalog_head、**不**重算 ever-started、**不**重采时钟（#96 先例：重放不查时钟/基表）。
- 事后可复核：`frozen_work_json` / 每 item `frozen_inputs.selection_evidence` 持久保存解析依据。

### D4. dynamic vs explicit 参数冲突

- `requests` 按 command JSON digest 判等：同一 request_id 携带不同归一化命令（dynamic↔explicit、或不同 dynamic 参数）→ `RequestConflict`（现有 `accept` 语义），即使解析结果成员相同也属不同命令，不共享 request_id。
- 不同 request_id 的合法任务可并存；scope 重叠由受理时 `scope_occupancy` 检查整体拒绝（`sqlite_tasks.rs` accept 路径：先查冲突后插行），不产生部分选择。

### D5. future-date guard 顺序

- guard 必须在 request binding / task / occupancy **之前**执行（#96 已确立的行为：任一未来范围整批 `invalid_arguments` 拒绝，无 request 绑定、无 task 行、无 occupancy；`tests/daily_future.rs`）。
- 默认选择器引入后顺序不变：解析在 guard 之后、写库之前；guard 拒绝时不得产生任何副作用，包括不得消耗 catalog/ever-started 读结果的可见性（纯只读求值，无副作用）。

### D6. proven-gap 输入契约：NOT_IMPLEMENTED / fail-closed

- 因 1.2 结论，**本阶段不为 gap 选择设计任何实现路径**：wire 不新增 gap 参数，application 不做 gap 选择，SQLite adapter 不提供 gap 查询。
- 未来若引入，输入契约必须满足：每条约数为持久证据（专用 gap 事实表或等价物，带证据绑定与 digest），逐条可在快照内复核；缺失即拒绝。
- 明确禁止：交易日不连续、缺失行、日期范围、版本缺席、当前日期——任何启发式推断。

### D7. 关注点分离（冻结边界）

- **MCP adapter**：只映射 selector 输入并透传；不含选择语义、不解析 catalog、不判 ever-started。
- **application（data_supply / task_control）**：解析动态选择、执行 guard、在受理事务内冻结 FrozenWork 与 selection_evidence。
- **SQLite adapter**：只提供事实——`catalog_head`/`catalog_versions`/`catalog_members` 读取、`task_items`/`tasks`/audit 表查询、`scope_occupancy` 占用检查；**不**决定业务选择、不推断 gap。
- 与未来 inspector/verification 的关系不变：审计表是事实来源之一，不产生选择决策。

## 3. 验证证据索引（current main）

| 断言 | 证据 |
|---|---|
| stage 持久且 finished 不重置 | `migrations/tasks/001.sql`；`src/task_control/mod.rs` Change 转移 |
| running→pending 唯一业务路径 + 审计兜底 | `src/task_verification/decision.rs`（target_before 快照保留）；`src/sqlite_verification.rs::commit_stop_check`/`read_receipt` 原子提交与读校验 |
| verification_inspections 同类重置仅限核实契约 | `src/verification_inspection/decision.rs`；`src/verification_execution.rs::supported` |
| FinishExecution 拒绝 running item | `src/task_control/mod.rs` FinishExecution 分支 |
| 受理时占用插入+冲突拒绝 | `src/sqlite_tasks.rs` accept 路径（scope_occupancy 检查与插入同事务） |
| catalog 当前版本来源 | `src/sqlite_catalog.rs::head`/`read`（digest+投影校验） |
| selection 冻结先例 | `src/data_supply/collect.rs::selection`（catalog_version_id+digest 写入 frozen_inputs） |
| item_id 跨契约共享 | `src/task_control/legacy.rs::scope_id`；`src/market_update/tasks.rs` 复用 |
| future-date guard 在绑定前 | `tests/daily_future.rs`；`src/market_update/tasks.rs` guard |
| gap 无持久事实 | migrations 全量无 gap 表；`observe.rs` confirmed_gaps 恒空；`registry.rs` 明示未实现 |

## 4. 显式未决事项（留待协调决策，不属本设计）

- 默认选择器在公共 wire 上的具体参数形态（`start_collection` 新 selection 变体）——本冻结只定语义与事实来源，wire 形态评审后定。
- ever-started 证券进入默认集合后的业务动作（补采范围如何确定）——依赖 gap 契约，当前 NOT_IMPLEMENTED。
- 默认选择的数量预算是否与显式 1..100 相同。
