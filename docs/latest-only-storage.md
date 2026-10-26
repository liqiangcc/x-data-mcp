# 单版本（latest-only）行情存储模型

> **状态：已部署（schema 21，preview.32 起）。** schema 21 随 preview.32 于 2026-10-13 全新 init
> 部署（旧 v20 库归档），`generic-data-v7` 自 preview.33 起运行；当前部署事实以
> [STATUS](STATUS.md) 为准。
> 历史：本文写于实现合入前（实现 PR：`feat/latest-only-storage`，已随 PR #329 合入 main），
> 下面「待合并」「合并并部署之前」等表述是当时的语境，保留作历史记录。
> 年线 fresh / behind / invalid 的对外出参形状、契约版本号与 Skill 变更**以 S3/S4 定稿为准**，
> 本文只写已裁定的语义，凡未定稿处标为「占位」，不编造 wire 形状。
> 凡标「估计」的数字均未在生产形态上实测。

## 1. 模型一句话

每个 `(实体, 周期, 复权口径)` 只保存**当前序列**：一行状态加逐根 bar。不保留版本历史，也不保留
除权前的旧值。「数据为什么变了」由追加式**事件日志**记录（旧/新摘要、依据、任务），事件日志不存旧价格。

这取代 schema 20 的版本链模型（`data_versions` 版本行 + checkpoint/delta 链 + `effective_heads`）。
用户决定：只要数据正确，只存一个版本；只需要最新数据。

## 2. 数据布局

### 2.1 新表（schema 21，见实现的 `021_latest_only` 迁移脚本）

| 表 | 作用 |
|---|---|
| `market_series` | 每 `(entity, period, adjustment)` 一行当前序列状态：`version_id`、`content_digest`、`record_count`、`first_day`/`last_day`、`coverage_status`、`source_binding_digest`、`evidence_digest`、`last_event_seq`、时间戳。`UNIQUE(entity, period, adjustment)`，吸收原 `effective_heads`。 |
| `series_bars` | 一行一根 bar，`PRIMARY KEY(series_key, trade_day)`，`WITHOUT ROWID`；沿用原 bar 的 CHECK。年线的 `trade_day` 是该年最后一个交易日。 |
| `yearly_series_lineage` | 年线血统：派生时的日线 `base_daily_digest`、**`base_event_seq`**（派生时日线 `last_event_seq`，用于 stale 分类）、`algorithm_version`、`first_trade_day`/`last_trade_day`/`as_of_day`。 |
| `market_events` | 追加式事件日志，全局递增 `seq`；记录 `kind`、`outcome`、旧/新 `content_digest`、连续性证据、窗口摘要、`evidence_digest`、`task_id`/`item_id`、`result_id`、`result_digest`。 |

实现中还追加了与年线 stale 分类相关的事件列（日线基摘要 `base_content_digest`、语义变化标记
`semantics_changed`）；**其确切定义以实现 DDL 为准**（占位，S3 定稿后补充到本节）。

### 2.2 保留与删除

- **保留**：`evidence_bundles`、`source_bindings`、`evidence_parts`（证据照用）；`catalog_*`（目录仍是多版本，与行情 bar 无关）；任务子系统各表（`tasks`、`task_items`、`requests`、`scope_occupancy`、验证与 gap 相关表等）。
- **删除**：`data_versions`、`version_checkpoint_clips`、`version_delta_clips`、`bar_segments`/`segment_bars`、`yearly_lineage`、`result_registry`、`market_update_results`、`market_update_delta_clips`、`publication_results`、`task_publications`、`effective_heads`。事件日志替代各结果表；`result_id UNIQUE` 承担幂等重放锚点。
- 任务的 `frozen_inputs`、`requests.command_json` 里的版本引用是 JSON 文本字段，没有外键，不受表删除影响。

## 3. 摘要与 `version_id`

- bar 的规范编码沿用现有 `bar_encoding`（价格以分为整数、量额可空、年线禁 `r7_raw`）。
- 序列摘要是**顺序折叠**：`h_0 = SHA256(种子 ‖ entity ‖ period)`，`h_n = SHA256(h_{n-1} ‖ bar_encoding(bar_n))`，bar 按 `trade_day` 严格递增；`content_digest = h_{record_count}`。空序列非法。
- **追加是 O(k)**：k 根新 bar 从已存的 `content_digest` 继续折叠，不重扫旧 bar；**替换是 O(n)**：整序列重折叠（约 6,000 根 bar 为数毫秒量级，估计）。
- 摘要**只取决于** `(entity, period, 有序 bar 序列)`：同一内容无论经追加还是替换得到，摘要相同。因此摘要**不含** coverage、来源、证据等元数据。
- **`version_id = lower(hex(content_digest))`**（裸 64 位 hex，无前缀）。`VersionRef.integrity_sha256` 是同一个 hex。
  - 与 schema 20（历史）的区别：schema 20 的 `version_id` 是时序派生的 `version-{nanos}-{seq}`，不是内容派生。
  - **契约要点（必须在契约文档写明）：`version_id` 锚定 bar 的取值，不含 coverage 与来源元数据。** 只改元数据、bar 不变时 `version_id` 不变；pin 仍有效，但元数据（`evidence_digest`、`coverage_status`）已变，由事件日志表达。
- `no_change` 不改 `version_id`；同内容重提交得到同一个 `version_id`。

## 4. 事件日志

事件 `kind`：

| kind | 含义 | outcome |
|---|---|---|
| `first_publish` | 无行 → 一行，旧摘要为空 | 2（已变更） |
| `append` | 追加 bar（前缀追加） | 2 |
| `replace` | 整序列替换（除权/修订，含 envelope 路径） | 2 |
| `no_change` | 审计记录，序列不动，旧摘要 = 新摘要 | 1 |
| `metadata` | bar 不变而 coverage/来源/证据变（旧摘要 = 新摘要，`metadata_changed=1`） | 2 |
| `yearly_rederive` | 年线行被重算 | 2 |

不变式（写入侧保证，启动检查消费）：

1. `market_series.last_event_seq` 等于该 `(entity, period)` 的 `max(seq)`，且该事件的 `new_content_digest` 等于序列的 `content_digest`。
2. `record_count` 等于 `series_bars` 行数，`first_day`/`last_day` 等于 bar 的最小/最大 `trade_day`（全量对账放在显式深检，见第 8 节）。
3. 每个 `(entity, period)` 的第一个事件是 `first_publish`，旧摘要为空。
4. 摘要链：同一 `(entity, period)` 按 `seq` 排序，`events[i].new == events[i+1].old`。

## 5. 写事务与并发

- **一个 item 一个写事务**：该 item 的序列行、bar、事件、任务簿记在同一事务提交；崩溃等于回滚，不存在「替换到一半」的序列，因此不需要「替换中」标志位。
- 禁止逐语句 autocommit（逐语句提交的 fsync 代价已在测量中确认）。是否把同一任务内多个 item 合并成一个事务由实测决定，但必须保持**每 item 一条事件**，且失败只回滚该 item/该批并有明确语义。
- **并发控制（CAS 比较 `last_event_seq`，不只比 `version_id`）**：写事务以 `BEGIN IMMEDIATE` 开始，在事务内重读序列行并核对 `last_event_seq` 等于预期，才允许从存量 `content_digest` 续折叠。这是因为 metadata-only 事件不改摘要，只比 `version_id` 发现不了并发的元数据变更。
- 追加依赖主键冲突兜底：对已存在的 `trade_day` 插入会让整个事务失败（fail-closed）。
- **幂等重放**：每个事件的 `result_id` 唯一；重试时先按 `result_id` 查找，命中即返回已存证据对应的回执。一个 `(task_id, item_id)` 至多一个结果事件。
- 同一 `(entity, period)` 的并发写由 SQLite 单写者与 `UNIQUE` 约束天然串行；任务层的范围占用语义不变。

## 6. 追加、替换与修订的判定

判定语义沿用 schema 20（历史）模型，只是输入从「版本行」换成「当前序列」：

- `prepare()`：缺失旧日期拒绝；改了已存日期的值而没有 revision 依据 → `RevisionUnverified`（fail-closed）；coverage 非 Complete 拒绝；单位/币种元数据变化影响全部年份。
- 连续性证据三档：unchanged / append / full（源 `.day` 成员的前缀字节、单证券公司行动事件摘要）。typed append 谓词（候选只有新增尾部、没有删除、起始日晚于序列末日）保留；不满足就回落到全量校验路径。
- 仅向前窗口的除权重写作为 `replace` 提交；窗口覆盖被改写的历史则 `RevisionUnverified`。
- 被取消的机制：stage → validate → commit 三态管线、`plan_delta`/checkpoint+delta、rollover。无版本行可校验；证据捆绑仍按原规则校验。

## 7. 年线

- 年线仍是独立步骤，在日线之后：派生读取**当前日线序列**（不再是 pin 住的日线版本），聚合算法不变。
- 写事务内复核**当前日线 `content_digest` 等于派生时的 base 摘要**；不等则该 item 失败（stale，由任务层重派），不得提交基于旧日线的年线。
- 写集：受影响年份的 bar UPSERT、年线序列行、`yearly_series_lineage` UPSERT（含 `base_daily_digest` 与 `base_event_seq`）、一个 `yearly_rederive`（或 `first_publish`/`no_change`）事件。年线内容不变时 lineage 仍推进到新 base。
- 除权（日线 `replace`）使该实体全部年份重算；`affected_years` 的其余情形沿用现有逻辑。

### 7.1 年线 stale 分类（fresh / behind / invalid）

日线每天追加都会让年线落后，所以**不能**一刀切 fail-closed。分类依据是该实体日线事件日志中、`seq` 大于 lineage 记录的 `base_event_seq` 的事件：

| 状态 | 条件 | 行为 |
|---|---|---|
| fresh | 没有新事件 | 正常返回 |
| behind | 新事件只有 `append` / `no_change` / `metadata`（且不改变年线语义） | 年线对其 `as_of_day` 仍正确（当年那根 bar 缺最近几天）。`query_data` 照常返回，并如实带落后标记与 `as_of_day` |
| invalid | 出现 `replace`（除权/修订）或改变年线语义的变化 | 该实体年线不得返回（fail-closed） |
| integrity 错误 | `lineage.base_daily_digest` 不在该实体日线事件链里 | 伪造或损坏 |

- 批量 `query_data` / availability 的 invalid 实体按**行级状态**返回（与 Absent 同层，给出原因）；100 个实体里 1 个 invalid 不能让整次调用失败；单实体查询才直接报错。
- 「改变年线语义」的判据（年线只依赖 bar 取值、`source`、`unit_currency`、`adjustment_evidence`；coverage reason、generation、证据摘要本身的变化不影响年线正确性）由实现中的语义变化标记表达。
- **占位（以 S3 定稿为准）：** 对外出参的字段名与位置、`reason` 的枚举取值、`get_data_availability` 与 `get_yearly_base_years` 的具体形状。

## 8. 启动检查与深检

启动检查必须是 **O(series) 或走索引，不能是 O(events)**（事件日志每天约新增 1.04 万行，估计，见下）。否则会重复 XP-043 暴露的问题（启动 sweep 随历史线性增长，而隧道只等 10 秒）。

- **启动必跑**（只看已提交状态）：序列与事件的闭合（尾事件的 seq 与摘要一致）；孤儿事件（事件对应的序列不存在；采用 loose-scan 仿真，对每个 `(entity, period)` 做一次索引 seek，共 O(#pairs) 次，**不做 DISTINCT 全索引扫描**）；孤儿 lineage（必须指向年线序列）；首发事件。全部基于覆盖索引。
- **不在启动路径**：逐 bar 重折叠、`record_count` 与 bar 行数的全量对账、年线 bar 与 lineage 的对账。这些放在 **`task_sqlite check` 的显式深检子命令**，不并入启动，也不并入 XP-043 的 sweep。
- **实测（合成夹具，非生产数据）：** 夹具为 1,008,800 个事件 / 10,400 个序列 / 约 352MB 的合成数据库。单线程，使用 C 级 libsqlite3 计时（与 rusqlite 成本近似，不是端到端的服务启动）。分阶段结果（冷缓存为清空页缓存后）：

  | 阶段 | 冷缓存 | 热缓存 |
  |---|---|---|
  | tail closure（每序列两次索引 seek） | 6.93s | 49ms |
  | yearly lineage orphan | 0.00s | 0ms |
  | orphan events（loose-scan，O(#pairs) seek） | 4.43s | 257ms |
  | first_publish（按序列驱动的有界 seek） | 0.03s | 48ms |
  | **合计** | **11.38s** | **0.36s** |

  独立复测：热 ≈ 0.45 秒，冷 ≈ 11–13 秒（冷缓存主要是随机 I/O，如实记录，不掩盖）。查询计划为覆盖索引上的 SEARCH。冷启动逼近隧道 10 秒等待的预算，换 schema 时应在目标主机上用冷缓存再测一次。
- 事件日志增长：约每日 1.04 万行（5,224 条日线事件 + 5,224 条年线事件）。按 1,008,800 事件的夹具约 352MB 推算，约 350 字节/行，即约 3.6MB/日、约 1.3GB/年（估计，未在生产形态上实测）。**接受无界增长，不做归档**（审计链完整性优先）；该增长与任务簿记的增长（每年 10–16GB 量级，见 STATUS 的已知问题）分开计，后者不是本模型引入的。

## 9. 契约影响（概要，详情以 S4 为准）

- `list_data_versions`：每实体至多 1 项，`version_id` 为当前内容标识。
- `query_data`：显式 `version_id` 过期 → `stale_version`（该摘要曾存在但已被取代）；从未存在 → `version_not_found` / `invalid_arguments`（与查询侧对齐）。`update_data` 的 `base_version_id` 旧 id 同理。
- `get_data_availability`、`get_yearly_base_years`：年线条目带落后/失效信息（形状占位）。
- `verify_publication_result`：对仍是当前的事件，验证事件存在、新摘要等于序列摘要、证据可重放；对已被后续更新覆盖的历史任务，只验证「该 `result_id` 的事件存在且摘要链闭合到当前序列」，**不能再验证那一刻的数据内容**。
- `republish_data`：保留（它是显式 republish 合同，不是版本概念），语义改为「显式区间的源数据刷新」，落到统一的事件提交函数。
- capability 版本号提升、Skill 与下游 KlineReader 的 pin 失效处理：占位，S4 另行处理。

## 10. schema 21、部署与回滚

- 部署方式不变：归档旧库，全新 init，全市场重建。**不做数据迁移**；schema 21 只出现在 fresh init 链尾。
- 新二进制**只认 schema 21**；旧二进制遇到 schema 21 库报 unsupported schema 并 fail-closed。
- 代码（main）已升到 **schema 22**（M1 PR-1…PR-5，issue-383 §14：口径/精度列、双序列写入、
  因子事件表与派生数据集版本登记、指数序列与逐指数口径元数据），尚未部署；部署中的二进制与库仍是 schema 21。
- **回滚 = 旧二进制 + 归档的 schema 20 库**（既定做法，不改变）。
- 已部署事实不因本文改变：本文写作时生产仍是 schema 20 / preview.31（历史）；2026-10-13 起
  生产已是 schema 21（preview.32 全新 init），当前事实见 [STATUS](STATUS.md)。

## 11. 与 schema 20（历史）模型对照

| 方面 | 历史（schema 20，版本链；preview.31 及更早） | 当前（schema 21，单版本；preview.32 起） |
|---|---|---|
| 存储单位 | 每次变更一个不可变版本；checkpoint + delta 链 | 每 `(实体, 周期)` 一行当前序列 + 逐根 bar |
| 历史 | 保留所有版本，可按旧 `version_id` 读 | 不保留旧值；事件日志只记摘要变化 |
| `version_id` | 时序派生 | 内容摘要派生 |
| 当前指针 | `effective_heads` | `market_series` 自身 |
| 审计 | 结果表 + 发布表 + delta clip | `market_events` |
| 日线追加 | 新版本（delta），到阈值 rollover | 原地追加 bar，续折叠摘要 |
| 除权 | 全量重写为新版本 | 原子替换该实体序列并重算其年线 |
| 年线一致性 | lineage 钉住一个日线版本，`verify_lineage` 重新聚合 | lineage 记录派生时日线摘要与事件 seq；fresh/behind/invalid 分类 |
| 启动检查 | registry sweep（随历史线性增长） | 索引上 O(series)，深检另设子命令 |

## 12. 占位清单（留给 S3 / S4）

1. 年线 stale 的对外出参：字段名、位置、`reason` 枚举。
2. `market_events` 追加列（日线基摘要、语义变化标记）的最终 DDL 与比较实现位置。
3. capability 版本号与 `tools/list` 描述；`mcp-tools.md`、`generic-data-tools.md`、`daily-sync.md`、README 与 Skill 的改写。
4. 下游 KlineReader 在 `stale_version` 后的处理（重新列版本并重试）。需另行批准，不在本文范围。
5. STATUS / SLO 的更新（实现合并并部署后）。
