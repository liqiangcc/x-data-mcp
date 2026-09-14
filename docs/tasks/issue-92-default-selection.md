# #92 默认选择可实现性核对与设计冻结（第一阶段）

状态：设计冻结，供协调决策评审。**本文件不落任何代码、不改 wire/schema、不创建 PR。**
基线：`origin/main` = `01643ba092adc3c038dc6023108e4c7eb366f432`；分支 `issue-92-default-selection`。

**第三阶段状态修正（实现期追加）**：1.1 的 `EVER_STARTED_FACT_NOT_AVAILABLE` 为第一阶段历史结论；机械 durable fact 缺口已由 #137/第二阶段解决（schema v10 `task_item_execution_starts`，随 `Change::StartItem` 同事务原子提交，INSERT-only 单调）。D1 的判定来源随之落地，契约族精确口径与 retry 语义见下方 D1 修正。

协调口径（#92 评论 + 本轮边界裁定）：
- 首次采集"已启动" = 至少一个对应 collection item 发生过真实执行 / 获得实际执行 claim；`accepted`/`pending` 不算；之后的 `failed`/`cancelled`/`interrupted` 不抹去 ever-started 历史。
- default 动态选择首次受理时必须冻结。
- gap 只能来自可靠持久证据，不能按日期不连续猜测。
- **合格事实边界**：只有普通 collection execution/claim 路径**必然持久写入且终态后保留**的事实才可作为 default selector 的通用 ever-started 依据；仅在特定核实操作后出现的审计（含 running-before 快照）不合格——未做核实的正常执行同样必须可判定。

## 1. 可实现性结论

### 1.1 ever-started：`EVER_STARTED_FACT_NOT_AVAILABLE`

按上述边界逐条核对普通执行路径的全部持久写入，**不存在满足条件的通用事实**：

**普通路径写入且终态保留的候选事实**

| 事实 | 写入路径 | 终态保留 | 证明力 |
|---|---|---|---|
| `task_items.stage='finished'` + `outcome` | `FinishItem`（必经） | 永久 | 证明该 item 曾启动（FinishItem 要求前置 running），但**仅覆盖已结束 item** |
| `task_items.stage='running'` | `StartItem`（必经） | 进程死亡后保留（needs_verification 不改 stage） | 证明读取时已启动，但 verified interruption 可将其重置为 pending——**非单调保留** |
| `execution_uncertainty` marker（task.result_json） | 失联/结果未知时写入 | 中断时随 result 改写 | 仅绑定当前 running item，非常设历史 |
| `market_update_results`/`yearly_execution_results`/`publication_batches`/`task_publications` | 各契约执行/发布提交 | 永久 | 仅属 update/yearly/发布契约的**成功结果**见证，不覆盖 start_collection 的失败/中断 item |

**关键不可判定状态（覆盖缺口）**

- collection item 曾 `StartItem`→running，之后经 verified interruption 收尾：`stage` 被重置为 `pending`（`src/task_verification/decision.rs` 唯一业务任务重置路径），`outcome` 为空。
- 该终态与**从未启动**的 pending item 在普通路径数据上完全相同；`task.result_json.interruption.unfinished_items` 同时包含"曾 running"与"从未启动的 pending"两类 item，无法区分（never-started 中断产生同形 result）。
- 唯一能区分两者的见证是 `stopped_write_checks.audit_json.target_before` 快照——但它**仅在核实操作实际执行后才存在**，属核实条件事实，按协调边界不合格。
- 正常路径无其它候选：`requests`/`accepted_run_id`/`owner_run_id`/`scope_occupancy`/`task_scopes` 证明受理或当前占用而非启动；`task_items` 无 DELETE 路径但 stage 本身可回退。

**结论**：存在可达状态（已启动→核实中断→pending）使该 item 的启动历史在合格事实集合中不可恢复 ⇒ 通用 ever-started durable fact 不可得 ⇒ `EVER_STARTED_FACT_NOT_AVAILABLE`。

**最小 durable fact 需求（设计提案，待评审，不在本阶段实现）**

- 一个随 `Change::StartItem` **同一事务原子提交**的单调启动标记：例如 `task_item_starts(task_id, item_id, run_id, state_revision, started_at)` INSERT-only 表，或 `task_items` 上不可回退的 `ever_started` 标记列。
- 任何后续转移（含 verified interruption 的 running→pending 重置、取消、收尾、重试）**不得**删除/回退该标记；优先 schema 级保证而非代码约定。
- 经 `task_id → tasks.command_key_json` 限定 collection 契约族后，该标记即 `first_collection_started` 的唯一判定来源。
- 在该标记落地并通过评审前，默认选择器保持 **NOT_IMPLEMENTED / fail-closed**，不得以审计、`stage` 当前值或任何旁证代替。

### 1.2 proven-gap：`PROVEN_GAP_FACT_NOT_AVAILABLE`

逐表核对结果：

- **migrations/**：全部迁移文件无 gap 表，无 proven-gap 持久记录。
- `data_versions.coverage_status`/`coverage_reason`：版本级自由文本标签（如 `"complete"`/`"Unknown"` + reason），来自发布记录 `batch.coverage` 字段——**不是**机器可验证的逐证券 gap 区间。
- `version_evidence`：按 `(version_id,field)` 的字段级溯源（volume_unit/amount_currency/amount_unit 等），**不是**日期区间 gap 证据。
- `AvailabilityItem.confirmed_gaps`（`src/data_supply/wire.rs`）：wire 类型存在，但唯一生产者 `src/data_supply/observe.rs` 恒输出 `vec![]`；`unresolved_ranges` 仅在 availability=Unknown 时输出（语义相反）。
- 明示限制：`observe.rs` limitation "range completeness is not inferred from dates or version coverage alone; no implicit collection or repair"；`src/data_supply/registry.rs` RANGE_DESCRIPTION "Default selection, automatic gap discovery and catalog updates are not implemented"；`docs/tasks/issue-92.md` 同述。

结论：当前无任何可作为 #4"已明确历史缺口"的 durable fact。不得从交易日不连续、缺失记录、日期范围、版本缺席或当前日期构造 gap。

## 2. 设计冻结

### D1. `first_collection_started` 判定来源：`task_item_execution_starts` 单调启动标记（最小 durable fact 已由 #137 落地）

目标语义（标记已落地后生效）——在受理的**同一个 SQLite 事务快照**内求值：

```
ever_started(S,P) ⟺ EXISTS task_item_execution_starts 行
  WHERE item_id = '["market","S","P"]'                       -- scope_id 序列化
    AND tasks.command_key_json 的契约族 ∈ 首次采集（下述精确 key）
```

- 契约族精确口径（application 层判定，SQL 只取 raw `command_key_json` 事实、不做业务过滤）：
  - typed 首次采集：`operation="start_collection"`、`dataset="market.daily"`、`contract_version="v1"`（public `start_collection` 带 `dataset` 字段 → `Collection::Daily` → `CollectionContract`，见 `src/data_supply/mcp.rs`、`src/data_supply/collect.rs`）。
  - legacy 首次采集：`operation="start_collection"`、`dataset="legacy_market_compound"`、`contract_version="task-acceptance-v1"`（public `start_collection` 不带 `dataset` 字段 → local-owner 兼容路径 `src/sqlite_task_mcp.rs`）。
- retry 语义修正：retry 请求的 `requests.command_json` 记 `operation="retry_task"`，但 `tasks.command_key_json` 存的是**原任务的执行契约 key**（`sqlite_tasks` accept 中 `execution_key = old.task.key`，逐代展平到根契约）——retry-of-collection 的 marker 按上述 key 自然计入，无需 `retry_of` 链追溯；`retry_of`（直接父）仅作 evidence 返回。
- item 规则：`item_id` 必须解析为 `["market",symbol,period]` 三字符串，否则 fail-closed；typed daily 只允许 `period="day"`，非 day 为 inconsistency → fail-closed；legacy `day` 计入、`year` 合法跳过、其它 period → fail-closed。
- `item_id = ["market",symbol,period]` 被 collection、`update_data`、`backfill_data` 共享（`src/task_control/legacy.rs`、`src/market_update/tasks.rs`）——契约族过滤**强制**。
- fail-closed：无标记行 → 未曾启动；存储/完整性错误 → 受理整体失败；pending/accepted/occupancy/owner/核实审计/调用方声明 → 永不作为证据；`cancelled`/`failed`/`interrupted` 终态不抹去已存在标记。
- 标记落地前（Phase1 历史结论，已由 #137 supersede）：依赖 ever-started 的默认选择一律 `not_implemented` 拒绝，不做退化实现。

### D2. 默认证券选择器 = 当前有效 catalog ∩ ever-started（#4 已确认规则）

- 语义：本选择器服务**默认 update/backfill**——默认证券集合 = 当前有效 catalog 成员 ∩ `ever_started(S,'day') = true`。
- **正反边界（不可反转）**：`ever_started = false` 表示首次采集尚未启动，该证券必须被**排除**出默认更新集合，继续由 #3 首次采集路径处理；update/backfill **不得**对未启动首次采集的证券自动代采。
- "当前有效 catalog" 唯一来源：`catalog_head.version_id`（singleton）→ `read(version_id)`（compat_json sha256 + catalog_members 投影全量校验，见 `src/sqlite_catalog.rs`）。无 head 或校验失败 → 受理失败，fail-closed。
- 默认候选 = 该 catalog 版本成员中 `ever_started(S,'day') = true` 的证券；每个证券产生一个 item（沿用现有 item 派生与 `scope_id` 占用语义）。**受 D1 阻塞：ever-started 事实落地前本选择器整体 NOT_IMPLEMENTED。**
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
- 未来若引入，输入契约必须满足：每条 gap 为持久证据（专用 gap 事实表或等价物，带证据绑定与 digest），逐条可在快照内复核；缺失即拒绝。
- 明确禁止：交易日不连续、缺失行、日期范围、版本缺席、当前日期——任何启发式推断。

### D7. 关注点分离（冻结边界）

- **MCP adapter**：只映射 selector 输入并透传；不含选择语义、不解析 catalog、不判 ever-started。
- **application（data_supply / task_control）**：解析动态选择、执行 guard、在受理事务内冻结 FrozenWork 与 selection_evidence；ever-started 标记写入属于 `StartItem` 转移的持久化部分，不在 MCP 层。
- **SQLite adapter**：只提供事实——`catalog_head`/`catalog_versions`/`catalog_members` 读取、`task_items`/`tasks`/标记表查询、`scope_occupancy` 占用检查；**不**决定业务选择、不推断 gap。
- 核实审计表不是 default selector 的事实来源；审计用于核实回放，不产生选择决策。

## 3. 验证证据索引（current main）

| 断言 | 证据 |
|---|---|
| StartItem/FinishItem 持久转移，finished 不重置 | `migrations/tasks/001.sql`；`src/task_control/mod.rs` Change 转移 |
| running→pending 唯一业务任务重置路径 | `src/task_verification/decision.rs`（verified interruption）；`src/sqlite_verification.rs::commit_stop_check` |
| 中断后 pending 与 never-started 不可区分 | `decision.rs` interruption result：`unfinished_items` 含两类 item；never-started 中断同形 |
| FinishExecution 拒绝 running item | `src/task_control/mod.rs` FinishExecution 分支 |
| 受理时占用插入+冲突拒绝 | `src/sqlite_tasks.rs` accept 路径（scope_occupancy 检查与插入同事务） |
| catalog 当前版本来源 | `src/sqlite_catalog.rs::head`/`read`（digest+投影校验） |
| selection 冻结先例 | `src/data_supply/collect.rs::selection`（catalog_version_id+digest 写入 frozen_inputs） |
| item_id 跨契约共享 | `src/task_control/legacy.rs::scope_id`；`src/market_update/tasks.rs` 复用 |
| future-date guard 在绑定前 | `tests/daily_future.rs`；`src/market_update/tasks.rs` guard |
| gap 无持久事实 | migrations 全量无 gap 表；`observe.rs` confirmed_gaps 恒空；`registry.rs` 明示未实现 |

## 4. 显式未决事项（留待协调决策，不属本设计）

- ever-started 最小 durable fact 的具体形态（独立 INSERT-only 表 vs `task_items` 不可回退列）——需 schema 评审；落地前默认选择器整体 NOT_IMPLEMENTED。
- 默认选择器在公共 wire 上的具体参数形态（`update_data`/`backfill_data` 的默认 selection 变体或其他入口）——本冻结只定语义与事实来源，wire 形态评审后定。
- `ever_started=true` 证券的默认 update/backfill 范围如何确定——依赖 proven-gap/范围契约，当前 NOT_IMPLEMENTED；`ever_started=false` 证券不属于默认更新范围，由 #3 首次采集处理。
- 默认选择的数量预算是否与显式 1..100 相同。
