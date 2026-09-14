# #92 proven-gap 与默认 update 范围语义（bounded design / feasibility）

状态：设计与可行性冻结，供协调决策评审。**本文件不落任何代码、不改
wire/schema、不创建 PR、不解除任何来源门禁；系统继续
`NOT_PRODUCTION_READY`，#54/#6-46/#71 门禁不因本设计解除。**

基线：`origin/main` = `8e18ba0e1ebdb3d94ac87a0f1f66d3bf853a7274`；
分支 `issue-92-proven-gap-design`。前一阶段：
`docs/tasks/issue-92-default-selection.md`（Phase1 冻结）→
`aaf45b6`（Phase2 schema v10 durable execution-start facts，PR #137）→
`df7bd41`（Phase3 internal default-selection resolver，PR #138）。

---

## A. 当前事实层（live main 复核，不按旧上下文假定）

### A.1 已落地（不重复设计）

- **ever-started durable fact**：`task_item_execution_starts`
  （`migrations/tasks/010_execution_starts.sql`），INSERT-only +
  `BEFORE UPDATE/DELETE` 拒绝触发器；随 `Change::StartItem` 同一
  Immediate 事务写入（`src/sqlite_tasks.rs`）；verified interruption /
  cancel / fail 均不回退。
- **internal resolver**：`src/data_supply/default_selection.rs::
  resolve_default_daily` = 当前 catalog head ∩
  `start_collection` 契约族 daily ever-started。typed key
  `market.daily/v1`、legacy key `legacy_market_compound/task-acceptance-v1`；
  `tasks.command_key_json` 为扁平执行 key，retry-of-collection 自然计入；
  `retry_of` 仅 evidence。同一只读事务快照取 facts；输出
  `DefaultDailySelection{catalog_version_id, catalog_digest, securities,
  starts, evidence_digest}`。fail-closed：无 head/corrupt/malformed
  key/typed 非 day item/空交集（`EmptySelection`/`Invalid`）。
- **显式范围契约**：`market_update/tasks.rs` `explicit-range-task-v1`，
  item = `{symbol, base_version_id, start_date, end_date}`，受理时
  `repository.fixed_many` 校验 base identity（symbol/period/version_id）
  + `integrity_sha256` 写入 `frozen_inputs`；future guard 在写库前整批
  拒绝（Asia/Shanghai today）。

### A.2 durable proven-gap fact：仍不存在

复核 migrations 全量与生产代码：

- 无 gap 表、无逐证券 gap 区间持久记录。
- `data_versions` 有 `first_date/last_date/record_count/unique_count/
  duplicate_count/coverage_status/coverage_reason`（`migrations/tasks/
  003_market.sql`、`sqlite_market.rs:378`）——是**版本自身**的元数据，
  不证明"版本之外还缺什么"；`coverage_status` 是发布时的自由文本标签，
  非机器可复核 gap 证据。
- `AvailabilityItem.confirmed_gaps` wire 类型存在（`wire.rs:361`），唯一
  生产者 `observe.rs:55` 恒输出 `vec![]`。
- 结论保持 `PROVEN_GAP_FACT_NOT_AVAILABLE`：交易日不连续、缺失行、
  日期范围、版本缺席、当前日期——一律不得构造 gap。

---

## B. 默认 daily update 的"范围"语义

### B.1 已确认 vs 未确认

已确认（#4 AC-06/07、#3 系列 + #92 评论冻结）：

- 默认证券集合 = 当前 catalog ∩ `ever_started(S,'day')`（Phase3 已解）。
- 默认更新应"更新最新行情 + 同时尝试补齐已明确历史缺口"（AC-07），
  剩余未完成项如实报告。
- 动态范围首次受理冻结（AC-133~135 系列规则）；future 范围整批拒绝
  （AC-92）；Asia/Shanghai 语义（AC-90）。

**未确认（当前无契约，不得发明）**：

- "最新行情"的 `start_date/end_date` 推导规则。可机械推出：
  `base_version_id` 可取该证券当前 published daily head
  （`market_versions_scope` 索引 + `list_data_versions` 的
  `latest_per_entity` 已有先例）；`data_versions.first_date/last_date`
  是该版本的自报元数据。**但**：默认 update 的 start 是否 =
  `base.last_date + 1`、end 是否 = Asia/Shanghai today、重叠/重取已发布
  区间的语义、base 选 head 还是沿 frozen 链——**均无已确认业务规则**。
  特别是"last_date+1 到 today"依赖交易日历推断（哪些天应有行情），
  正是 AC-08 禁止的日期推断方向；today 也不保证是交易日。
- `ever_started=true` 证券的**首次采集是否已覆盖到最新**、update 与
  backfill 的分工边界——未确认。
- AC-07 的"已明确历史缺口"证据形态——未确认（即 C 节要补的最小事实）。

### B.2 当前 fail-closed 点

- **default range 目前契约不足**：`resolve_default_daily` 只产证券集合；
  把集合变成 `explicit-range-task-v1` items 需要
  `base_version_id/start_date/end_date`，三者均缺已确认推导规则。
- **proven-gap 扩展更无契约**：无事实模型。
- 因此 default update/backfill 任务创建保持 NOT_IMPLEMENTED；
  不先实现证券集合→范围的猜测映射。

### B.3 最小需要补齐的业务决策（列给协调，非本设计断言）

1. `base_version_id` 语义：默认 update 是否恒取受理快照时刻的 published
   daily head？（head 移动 vs frozen 冻结的一致性由 D 节受理快照保证。）
2. `start_date/end_date` 语义：是否采用"受理时刻已确认的某个边界"
   （如 `min(today, 来源/版本证据能支持的最大日期)`），以及是否允许
   `end < today`（非交易日）。**必须有一个不依赖交易日推断的规则**，
   例如"end = today，空结果按既有空结果规则处理（AC-46/#3 AC-88~90）"
   是否可接受——需业务确认。
3. update vs backfill 分工：默认 update 是否只产"最新区间"item，
   gap 补采是否另成 item/任务，还是同一任务内混合。
4. proven-gap 的**证据定义**：什么样的事件算"已明确缺口"
   （例如：任务已验证结果表明范围内取数失败而源确认该日有交易？
   发布时 coverage 声明的排除区间？核实决策产物？）——这是 C 节事实
   模型的输入，缺它 gap 表也只能空转。

---

## C. durable proven-gap 最小事实模型（2→3 候选 + 推荐）

### C.0 共同约束（全部候选满足）

- append-only、immutable、schema 级禁止 UPDATE/DELETE（沿
  `task_item_execution_starts` 先例：触发器拒绝改写）。
- 每条 gap 绑定：证券+period、gap 区间（含端点语义明确）、
  证据来源对象（kind + 对象 id）、evidence digest、observed_at、
  关联 published version（如适用）、producer 身份。
- **谁可产生**：只有 durable 证据路径——发布/覆盖声明路径、
  任务已验证终态、核实决策。普通 query、日期扫描、调用方声明
  永不产生 fact。
- **supersede 语义（推荐，见下）**：历史证据行不删除；后续版本补齐
  以独立的 covering 绑定使 gap 不再参与默认补采，而非抹掉历史。

### C.1 候选 M1（推荐）：专用 append-only 事实表 + covering 绑定

```
proven_gaps(
  gap_id TEXT PRIMARY KEY,
  symbol TEXT NOT NULL, period TEXT NOT NULL,          -- 'day'
  start_date TEXT NOT NULL, end_date TEXT NOT NULL,
  evidence_kind TEXT NOT NULL,       -- e.g. 'verified_task_outcome',
                                     -- 'publication_coverage_decl',
                                     -- 'verification_decision'
  evidence_id TEXT NOT NULL,         -- 对象 id（task_id/batch_id/check_id）
  evidence_digest TEXT NOT NULL,     -- 证据对象 sha256
  observed_at TEXT NOT NULL,
  covering_version_id TEXT NULL REFERENCES data_versions,  -- 补齐绑定
  covered_at TEXT NULL
)
```

- `covering_version_id` 非空 → 该 gap 已被后续 published version 覆盖；
  默认补采只取 covering IS NULL 的行。不删行：历史 gap 与覆盖证据
  同时保留可复核。
- 触发器：`BEFORE UPDATE` 仅允许 `covering_*` 两列从 NULL 写入一次
  （不可逆），其余列与 DELETE 一律拒绝。若评审认为"连覆盖列也不可改"
  更稳，则覆盖用第二张 append-only `gap_coverings(gap_id, version_id,
  covered_at)` 表，open-gap = LEFT JOIN 无 covering——代价是多一次
  join，但纯 append-only 语义最干净。**倾向后者**：两表都纯
  INSERT-only，触发器最简单。
- 优点：单一事实源、逐条可复核、supersede 干净、与 schema 惯例一致。
- 代价：新迁移（v11）。

### C.2 候选 M2：通用 evidence-facts 表（kind 多态）

一张 `evidence_facts(kind, subject_json, payload_json, digest, observed_at)`
承载 gap 与后续其它事实。优点：一次 schema 服务多用途。代价：payload
无 schema 约束，逐条复核要先解 JSON，迁移内难做强 FK（symbol/period/
version 引用变弱）；偏离本仓"事实表结构显式"的惯例。**不推荐**作为
gap 首选。

### C.3 候选 M3：发布路径附带 gap 声明（无独立表）

published version 的 compat_json/coverage 里由生产者声明"本版本明确
排除的区间"。优点：零新表、与发布事务同生共死。代价：gap 语义绑死在
发布产物上——核实决策、已验证任务终态等非发布证据无处落；查询要扫
所有版本 JSON；supersede/复核困难。**不推荐**。

### C.4 推荐

**M1（两表纯 append-only 变体）**：`proven_gaps` + `gap_coverings`。
理由：不可变证据与"已补齐"状态完全分离；无 UPDATE 路径；每条 gap 的
open/covered 由 join 决定且可审计；与 `task_item_execution_starts`
的 schema 级防篡改惯例一致。**注意**：该表只提供事实存储，"谁写入、
按什么规则写入"仍依赖 B.3-4 的业务确认——事实表落地不等于 gap 已可
被生产。

---

## D. 首次受理冻结 / 幂等 / 并发

沿用 Phase1 D3~D5 冻结，扩展到 range/gap：

- **单快照冻结**：normalize → future-date guard → 在受理写事务的
  同一只读快照内完成 `resolve_default_daily` + head base 绑定 +
  （若有事实模型）open-gap 读取 → 同一事务插 task/items/scopes/
  occupancy/request。`frozen_inputs` 除既有
  `{base_version_id, base_integrity_sha256, range}` 外，增补
  `selection_evidence`（catalog_version_id/catalog_digest/
  securities/starts/evidence_digest，Phase3 已可序列化）与
  `gap_evidence`（每条 gap 的 gap_id + evidence_digest + covering
  快照位）。
- **request_id replay 不重算**：命中 `requests` 即返回原 task——不重读
  catalog head、不重算 ever-started/gap、不重采时钟（#96 先例）。
- **dynamic vs explicit 异参**：digest 按命令 JSON 判定；
  `{"selection":{"kind":"default"}}` 与等价显式 items 不共享
  request_id（AC-134）。
- **future guard 在一切写入前整批拒绝**：动态解析本身也先过 guard
  （解析产出的 end_date 若 > today 同样整批拒，而非裁剪——与显式
  一致）。
- **retry 沿用原 frozen**：retry 只复制原 item/range/gap 集合；新 gap、
  日期前进、catalog 更新都不得扩大 retry 的工作集
  （`tasks.command_key_json` 扁平 key 已支持该语义）。
- **并发**：两个并发默认受理各自冻结各自快照，可能得到不同 securities/
  gaps——合法（不同 request_id 允许并存）；scope 重叠由
  `scope_occupancy` 同事务拒绝第二个，不产生半任务。

---

## E. public wire 选项（只设计，不改 wire）

现状：`RangeWrite::Daily` 只有 `items[{entity,base_version_id,
start_date,end_date}]`（`wire.rs:485-502`），`update_data`/
`backfill_data` 显式入口无 default 形态。

候选（按兼容性与关注点分离排序）：

1. **新契约版本 `default-range-task-v1`**（推荐）：`update_data`/
   `backfill_data` 请求体新增互斥的 `selection` 字段
   `{kind:"default"}`（与 `items` 二选一），映射到独立
   `CommandKey{operation, dataset:"market.daily",
   contract_version:"default-range-task-v1"}`。契约族在持久层与显式
   分离：default 任务与显式任务 digest 天然不同、retry 展平语义不变、
   wire 只是加字段不破坏旧 `items` 调用方。
2. `items` 内嵌 selector 项（`{kind:"default"}` 混入 items 数组）：
   破坏 `RangeWriteItem` 的显式不变量、把选择语义塞进数组元素，
   不推荐。
3. 新 MCP 工具（如 `update_default`）：扩大 public 面且无必要——
   operation 维度已由 `update_data`/`backfill_data` 表达。

**分离原则**：MCP 层只把 `selection.kind=default` 映射为契约输入；
证券集合、base/range、gap 语义全部在 application 层解析冻结——
wire 不携带"默认=什么"的任何语义参数，避免选择逻辑泄漏到 adapter。

**fail-closed**：`PROVEN_GAP_FACT_NOT_AVAILABLE` 期间，
`selection` 中带 gap 相关参数（若有）→ NOT_IMPLEMENTED；
default update 自身在 B 节范围契约确认前同样 NOT_IMPLEMENTED。

---

## F. 后续实现切片（按风险递增）

### Slice 1 — proven-gap 事实存储（仅事实层，无业务生产者）

- 内容：schema v11 `proven_gaps` + `gap_coverings`（纯 INSERT-only +
  触发器）；SQLite adapter 的 read/write 原语（写入入口存在但**不**被
  任何生产路径调用，或仅被 B.3-4 已确认的生产者调用——二选一按评审）。
- AC：旧库升级不伪造行；插入不可改/删；covering 绑定不可逆；
  open-gap 查询=无 covering 的行；同事务快照读取。
- 测试：迁移 identity、触发器拒绝 UPDATE/DELETE、升级行数=0、
  covering join 语义。
- 不做：任何生产者接线、selector 消费、wire。

### Slice 2 — default range 解析契约（依赖 B.3 业务确认）

- 内容：在 `default_selection.rs` 增加 per-security `base_version_id`/
  `start_date`/`end_date` 解析（按确认规则），并入 `FrozenWork`/
  `frozen_inputs` 的 `selection_evidence`；future guard 接线。
- AC：受理单快照冻结；replay 不重算；guard 整批拒；空交集 fail-closed。
- 测试：冻结/replay/guard/快照一致性/head 移动期间并发受理。
- 不做：wire、任务创建执行语义、gap 消费。

### Slice 3 — wire `selection.kind=default`（`default-range-task-v1`）

- 内容：`RangeWrite::Daily`/`Yearly`?（daily only）加 `selection`
  互斥字段；accept 路径接 Slice 2 解析；MCP 透传。
- AC：dynamic↔explicit 异参冲突；digest 幂等；occupancy 行为不变；
  18 工具集不变（无新工具，参数扩展）。
- 测试：wire normalize、异参冲突、replay、fail-closed。
- 不做：gap、yearly、executor。

### Slice 4 — gap 消费（依赖 B.3-4 + Slice 1 有真实数据）

- 内容：默认受理时读 open gaps → 生成 gap 补采 item（或并入 update
  item，按确认分工）；`gap_evidence` 冻结；执行后 covering 绑定。
- AC：无事实模型数据时 fail-closed；retry 不扩集；covered 不重采。
- 测试：gap 冻结、covering 写回、supersede 后不再默认补采。
- 不做：gap 生产者规则本身（那是业务/执行侧，不是本切片）。

---

## 未决阻塞（汇总，均为协调决策而非实现问题）

1. B.3-1/2/3：默认 update 的 base/range 业务规则未确认 ⇒ default
   update 目前 **契约不足，NOT_IMPLEMENTED**。
2. B.3-4：proven-gap 的证据定义未确认 ⇒ Slice 1 即使落地也无生产者；
   Slice 4 依赖。
3. wire 形态评审（E-1 是否采用 `default-range-task-v1`）。
4. 默认选择数量预算是否沿用显式 1..100（Phase1 已列未决）。
5. `PROVEN_GAP_FACT_NOT_AVAILABLE` 与来源门禁（#54/#6-46/#71）均不
   因本设计解除；`NOT_PRODUCTION_READY` 不变。
