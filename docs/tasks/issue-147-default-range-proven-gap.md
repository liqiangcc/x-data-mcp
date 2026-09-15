# #147 设计冻结：#92 默认更新范围与 proven-gap 持久事实契约

状态：第一阶段设计/可行性冻结，供协调决策评审。**本文件不落代码、不改
wire/schema、不创建 PR、不解除任何来源门禁；系统继续
`NOT_PRODUCTION_READY`，#54/#46/#71 门禁不因本设计解除。**

基线：`origin/main` = `8e18ba0e1ebdb3d94ac87a0f1f66d3bf853a7274`；
分支 `issue-147-range-gap-design`，worktree
`/root/x-data-mcp-issue147-design`。前置阶段：Phase1 冻结
（`docs/tasks/issue-92-default-selection.md`）→ `aaf45b6`（Phase2
schema v10 execution-start durable facts, PR #137）→ `df7bd41`（Phase3
internal default-selection resolver, PR #138）。

## 明确结论

- **`PROVEN_GAP_FACT_NOT_AVAILABLE`**
- **`DEFAULT_UPDATE_RANGE_NOT_AVAILABLE`**

两项均因"持久可复核事实/已确认契约不足"，而非实现难度。禁止以日期
不连续、缺行、版本缺席、当前日期或推测交易日制造 gap 或推导范围。

---

## 1. current-main 事实盘点（逐项：能证明什么 / 不能证明什么）

### 1.1 `data_versions`（migrations/tasks/003_market.sql:1-7）

字段：`version_id/symbol/period/adjustment/state/source/published_at/
record_count/unique_count/duplicate_count/coverage_status/coverage_reason/
first_date/last_date/compat_json/content_digest/digest_format`；
`market_versions_scope` 索引按 `(symbol,period,adjustment,state,
published_at,version_id)` 排序。

- 能证明：某版本**自身**的内容身份、完整性 digest、自报的
  first/last_date 与计数、`coverage_*` 标签——这些是写入时由候选
  记录 `batch.coverage` 字段带入的版本自报元数据（`sqlite_supply.rs:295`）。
- 不能证明：**版本之外还缺什么**。`coverage_status`/`coverage_reason`
  是版本级自由文本标签（`"complete"`/`"Unknown"`+reason），非机器可
  复核的逐证券 gap 区间；first/last_date 是已有内容边界，不证明相邻
  区间"应有数据而缺失"。

### 1.2 `daily_bars`/`yearly_bars`/`yearly_lineage`（003:12-25）

- 能证明：某版本内实际持久化的逐日/逐年记录及其 trade_date——
  "版本内有什么"是精确事实。
- 不能证明："某日期应有记录"。版本内缺某日 ≠ gap；当日是否交易日、
  是否源端有数据，均无对应事实。

### 1.3 `effective_heads`（003:36-39；`sqlite_market.rs:675`）

- 能证明：每个 `(symbol,period,adjustment)` 当前 published head
  version——发布事务内 `INSERT ON CONFLICT DO UPDATE`，是持久、
  单调前进（不回退）的 head 事实。
- 不能证明：默认 update **应当**以 head 为 base——"用哪个版本做
  base"是业务契约，head 只是机械可取的候选。

### 1.4 `version_evidence`（003:8-11）

- 能证明：发布时逐字段（单位/币种/调整等）证据状态与理由——
  字段级溯源。
- 不能证明：日期区间级缺口；无 gap 字段语义。

### 1.5 `publication_batches`/`task_publications`（003:27-35）

- 能证明：哪个 task/item 发布了哪个版本（receipt_json + 唯一性约束）。
- 不能证明：采集了哪些日期、哪些日期源端无数据——receipt 不携带
  逐日覆盖证据。

### 1.6 `market_update_results`/`yearly_execution_results`
（005/006）

- 能证明：某 task/item 以某 `base_version_id` 产出
  `result_version_id`（或 `no_change` 复用回执），audit_json +
  result_sha256 可复核。
- 不能证明：执行区间、请求区间、或区间覆盖完整性——表内不存
  start/end（范围在 `tasks.frozen_work_json` 的 per-item
  `frozen_inputs` 中，见 1.9）。

### 1.7 `task_item_execution_starts`（010；`sqlite_tasks.rs`）

- 能证明：某 collection item **曾真实启动**（随 `StartItem` 同一
  Immediate 事务写入，INSERT-only + UPDATE/DELETE 拒绝触发器，
  verified interruption/失败/取消均不抹除）。
- 不能证明：执行结果、覆盖区间、数据存在与否——仅是"启动过"事实。
- 已由 Phase3 resolver 消费：当前 catalog head ∩ `start_collection`
  契约族 daily ever-started → 默认证券集合。

### 1.8 `catalog_versions`/`catalog_members`/`catalog_head`（004）

- 能证明：当前有效目录版本、成员集合、版本 digest。
- 不能证明：目录成员是否有数据、何时有数据。

### 1.9 `tasks.frozen_work_json`/`requests`/`task_items`（001/007）

- 能证明：受理时冻结的 items/scopes/per-item `frozen_inputs`
  （explicit 契约冻结 `{base_version_id, base_integrity_sha256,
  range}`，`market_update/tasks.rs:141-143`）；`requests` 绑定
  request_id→command_digest→task_id，重放返回原任务。
- 不能证明：历史"意图"范围与实际数据差异；frozen range 是输入而非
  结果证据。

### 1.10 `availability` 观察层（`data_supply/observe.rs`、`wire.rs`）

- 能证明：manifest 存在性/完整性检查、`first_record`/`last_record`/
  `record_count` 观察值、`coverage_reason` 透传。
- 不能证明：gap。`confirmed_gaps` wire 类型存在
  （`wire.rs:480-483`）但唯一生产者恒输出 `vec![]`
  （`observe.rs:55`）；`unresolved_ranges` 仅在 `Unknown` 时回填
  请求范围本身，意为"未判定"而非"已确认缺口"；明确声明
  "range completeness is not inferred from dates or version
  coverage alone"。

### 1.11 `stopped_write_checks`/`verification_inspections`（007/009）

- 能证明：停止写核实与核实巡检的审计结论（audit_json+sha256）。
- 不能证明：数据缺口——审计对象是任务生命周期，非覆盖区间。

### 1.12 汇总

| 事实 | gap 证明力 | 默认范围推导力 |
|------|-----------|---------------|
| data_versions 元数据 | 无（自报标签） | head/日期边界候选，无业务规则 |
| effective_heads | 无 | 机械可取 head，选谁未定 |
| execution_starts | 无 | 仅证券集合（已解） |
| update_results/审计 | 无 | 无 |
| availability 观察 | 无（confirmed_gaps 恒空） | 无 |

⇒ `PROVEN_GAP_FACT_NOT_AVAILABLE`；
`DEFAULT_UPDATE_RANGE_NOT_AVAILABLE`（证券集合已解，
`base_version_id`/`start_date`/`end_date` 推导规则无已确认契约）。

---

## 2. 默认 update 与 explicit backfill 的职责边界

- **explicit backfill/update**（`explicit-range-task-v1`）：调用方显式
  给出 `base_version_id+start_date+end_date`；契约只做校验/冻结，
  不推导业务意图。已可用。
- **默认 update**（未实现）：语义 = "对默认证券集合更新最新行情 +
  同时补齐已明确历史缺口"（#4 AC-07）。当前阻塞是**业务契约未
  确认**，不是任何技术缺失：默认证券集合已由 resolver 解决，但每
  证券的 `base_version_id/start_date/end_date` 推导规则未冻结。
- **不得**把"update 最新范围"与"backfill 已明确缺口"静默合并成一
  种 heuristic：两者是不同工作项来源（版本边界 vs gap 事实），
  任务结构（一个 item 多段 vs 分 item）也依赖业务分工确认。
- ever_started=false 证券继续走 #3 首次采集路径，update/backfill
  不隐式首次采集（已冻结）。

---

## 3. 最小 durable fact/schema 草案（设计提案，未实现）

### 3.1 proven-gap：纯 append-only 两表

```
proven_gaps(
  gap_id TEXT PRIMARY KEY,
  symbol TEXT NOT NULL, period TEXT NOT NULL CHECK(period IN ('day','year')),
  start_date TEXT NOT NULL, end_date TEXT NOT NULL,
  evidence_kind TEXT NOT NULL,   -- e.g. 'verified_task_outcome' |
                                -- 'publication_coverage_decl' |
                                -- 'verification_decision'
  evidence_id TEXT NOT NULL,     -- 证据对象 id
  evidence_digest TEXT NOT NULL, -- 证据对象 sha256
  observed_at TEXT NOT NULL
)
gap_coverings(
  gap_id TEXT NOT NULL REFERENCES proven_gaps(gap_id),
  version_id TEXT NOT NULL REFERENCES data_versions(version_id),
  covered_at TEXT NOT NULL,
  PRIMARY KEY(gap_id, version_id)
)
```

- **写入时机**：只能由 durable 证据路径写入——发布/覆盖声明、任务
  已验证终态、核实决策——且这些路径当前也**尚无 gap 产出语义**
  （见 §5 未决）；本草案只提供事实存储。
- **单调/不可变**：两表均 INSERT-only + `BEFORE UPDATE/DELETE`
  拒绝触发器（沿 `task_item_execution_starts` 先例）；补齐不写回
  gap 行，而是 `gap_coverings` 追加绑定——历史证据不删。
- **读取验证**：open-gap = `proven_gaps` LEFT JOIN 无 `gap_coverings`
  的行；每条 gap 必须携带可复核 evidence_id+digest；读取方校验
  evidence 对象存在且 digest 匹配，损坏 → fail-closed。
- **升级/不回填**：显式 migration；旧库无行 = "无可证明缺口"，
  严禁扫描历史数据回填。

### 3.2 default-range 事实：不需要新表，需要新契约

`base_version_id`/`start_date`/`end_date` 的候选事实
（`effective_heads`、`data_versions` 元数据、受理时钟）都在，缺的是
**已确认的选择规则**。本节不预设规则——见 §5 最小决策清单。规则
一旦冻结，所需事实全部可从受理快照读取，无需 schema 变更；若确认
的规则需要额外事实（如源端交易日历证据），则届时另评。

---

## 4. 受理原子性 / 幂等冻结 / future guard

顺序（default/dynamic 受理，设计提案）：

1. normalize 输入（`selection.kind=default`）；
2. 在受理写事务的**同一只读快照**内完成 `resolve_default_daily`（证
   券集合）+ 按已确认规则推导候选 `base/start/end` + open-gap 读
   取，组装 FrozenWork 所需全部证据；
3. **整批 future-date guard**：解析产出的任一 `end_date >
   Asia/Shanghai today` → 整批拒绝（不裁剪），此检查发生在任何
   request 绑定、task 创建、occupancy 写入**之前**；
4. 同一事务插 `requests`/`tasks`/`task_items`/`scope_occupancy`；
   `frozen_inputs` 除 `{base_version_id, base_integrity_sha256,
   range}` 外增补 `selection_evidence`（catalog_version_id/
   catalog_digest/securities/starts/evidence_digest）与
   `gap_evidence`（gap_id+evidence_digest 列表）。

幂等与并发：

- 同 request_id replay：命中 `requests` 即返回原 task——不重读
  catalog head、不重算 ever-started/gap、不重采时钟（#4 AC-133）。
- dynamic vs explicit 异参冲突：digest 按命令 JSON 判定，等价结果
  不共享 request_id（AC-134）。
- 并发受理各自冻结各自快照合法；scope 重叠由 `scope_occupancy`
  同事务拒绝，不产生半任务。
- retry 沿用原 frozen items/ranges/gaps，不因新 gap、catalog 更新或
  日期前进扩大（AC-135 语义；`tasks.command_key_json` 扁平 key
  已支持）。
- 空集合/预算：空交集 → fail-closed（已有 `EmptySelection`）；默认
  证券数量预算是否沿用显式 1..100 → 未决（§5）。
- 损坏证据 fail-closed：无 catalog head/corrupt/malformed key/gap
  evidence digest 不匹配 → `Invalid`，不降级继续。

---

## 5. 关注点分离与最小待确认业务决策

分离（既有边界，不新增）：

- **SQLite**：只提供事实（表+只读快照 API），不做业务过滤。
- **application**：契约解析、范围推导、证据冻结、guard、任务组装。
- **MCP**：仅映射 `selection.kind=default` 为契约输入，不携带范围
  语义。

最小待确认（协调决策，非本设计断言）：

1. 默认 update 的 `base_version_id`：是否恒取受理快照时刻的
   `effective_heads` published daily head？
2. `start_date`/`end_date`：候选规则（如 end=受理时钟 Asia/Shanghai
   today、start 由某个已确认边界给出）需明确指定；**任何规则均不
   得依赖交易日历推断**；"候选规则 A/B"的选择是业务决定。
3. update vs backfill 分工：默认 update 产"最新区间"item，gap 补
   采是否并入同一任务/同 item 多段，需指定。
4. proven-gap 证据定义：何种事件构成"已明确缺口"（已验证任务终
   态/发布覆盖声明/核实决策，或其组合），以及对应 producer 接线。
5. 默认证券数量预算：是否沿用显式 1..100。

---

## 6. 测试矩阵（供后续切片复用）

| 层 | 测试 |
|----|------|
| schema v11 | migration identity、触发器拒 UPDATE/DELETE、v10→v11 升级零行、FK→data_versions |
| gap 事实 | covering 追加后 open-gap 查询不再返回、双 covering 幂等、evidence digest 校验失败 fail-closed |
| 受理冻结 | 同 request_id replay 不重算（catalog/ever-started/gap/时钟注入探针）、异参 RequestConflict |
| future guard | dynamic 解析出 future end → 整批拒且无 requests/tasks/occupancy 行 |
| 并发 | 两连接同 scope 竞争恰一成功；无半任务 |
| 空/预算 | 空交集 fail-closed；超预算拒绝 |
| retry | retry 沿用 frozen，新 gap/新 catalog/时钟前进不扩集 |
| 升级 | 旧库升级无伪造 gap/无范围回填 |

## 7. 后续最小实现切片（按风险递增，互不合并）

1. **Slice 1 — gap 事实存储**：schema v11 两表 + adapter read/write
   原语 + 测试；不接生产者、不接 selector、不接 wire。
2. **Slice 2 — default range 契约**：依赖 §5-1/2/3 确认；resolver
   扩展 per-item base/range 推导 + 冻结 + guard 接线。
3. **Slice 3 — wire `selection.kind=default`**：契约版本
   `default-range-task-v1`，MCP 仅透传；explicit 入口不变。
4. **Slice 4 — gap 消费**：依赖 §5-4 + Slice 1 数据；受理时读
   open-gap 生成补采工作，覆盖后 `gap_coverings` 写回。

每个 slice 独立评审；在 §5 确认前 Slice 2-4 保持 NOT_IMPLEMENTED。
