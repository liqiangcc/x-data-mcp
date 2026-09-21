# #259 最终紧凑存储设计冻结候选

## 状态与目的

状态：**DESIGN FROZEN / S0-S2 PROTOTYPE PASS / S3-S6 NOT STARTED**
适用基线：`issue-262-phase-d`，schema 18，2026-09-21。
关联：#259、#260、#262。

本设计只收敛 #259 的最终物理存储、完整性摘要、更新审计、SQLite 连接/WAL 与容量验收。immutable generation、QFQ、任务状态机、MCP 工具编排和 catalog 范围不重新设计。

schema 18 已实现 O(items) task transition、integer segment key、persistent manifest tree 和部分 content-addressed evidence，但仍有两个已实测缺口：

1. `segment_bars.bar_json` 的生产实测物理成本约 172 bytes/bar；按已知约 33.3M bars 外推，canonical bars 单项即超过首次 DB 预算。
2. `market_update_results.audit_json.segment.bars` 虽改成 payload ref，no-change 仍为每只证券写入完整历史 bars payload；ECS 100-item 数据库中该路径占约 84.9MB。

因此本设计不把 schema 18 的“compact”命名视为 #259 完成，不以 100-item 功能 smoke、fixture soak 增长率或部署目录大小替代全市场容量 AC。

## 决策摘要

以下项目在实现前冻结：

1. fresh production schema 使用 `compact-market-version-v1`，不再生成或重建 `legacy-published-record-v1`。
2. QFQ daily/yearly OHLC 以 **整数分** 保存；当前 QFQ 契约输出 2dp HALF_UP，超出 2dp 的标准行情拒绝写入，不静默舍入。
3. `trade_day` 使用 Unix epoch day `INTEGER`；volume 使用非负 `INTEGER`；amount 保留规范十进制 `TEXT`；`r7_raw` 使用 nullable u32 `INTEGER`。
4. canonical bar 只存于 `segment_bars`；version、audit、receipt、task outcome 和 MCP wire 不保存 bars 数组副本。
5. immutable segment 每段最多 512 bars；初采按该上限切段，日更 append 产生每证券一个小 tail segment，不重写旧段。
6. schema 18 的二叉 path-copy manifest 不作为最终布局：它虽为 O(log N)，但 5,221 个证券每日复制多层节点仍不满足绝对增长预算。schema 19 使用 immutable parent-delta；最多 256 层后写一个只含 segment clip 引用的 checkpoint，不复制 bars。
7. version 保存绑定完整投影历史的 `projection_digest` 和绑定身份/元数据/证据的 `integrity_digest`；二者均使用明确二进制 framing，不依赖 JSON 字段顺序。
8. update result 使用类型化紧凑行；no-change 不创建 bar payload、segment、version delta/checkpoint 或 data version。
9. schema 18 中已有 published market data **不迁移为生产数据**。schema 19 只允许空业务库升级；非空库明确返回 `FRESH_REBUILD_REQUIRED`。旧库保留只读证据，不被原地改写或静默删除。
10. `MAX_DEFAULT_SECURITIES=100` 在 full-market 容量和时间 AC 通过前保持不变。

## 目标和硬约束

### 正确性不变量

- 已发布 version、segment、checkpoint/delta、evidence bundle 和 receipt 均不可原地修改。
- 任一固定 version 的有序 bar 投影由其 checkpoint/parent-delta chain 唯一确定。
- 缺 segment/parent、clip 越界、chain 成环、日期重叠、行数不符或摘要不符均 fail closed；不读修复、不退回 legacy JSON。
- no-change 复用固定 base version；不创建新 version 或复制 bar。
- append/revision 产生新 version；旧 version 仍可按原 projection 查询。
- publication、effective head、task item outcome、task publication 和 receipt 在同一 SQLite 事务提交。
- 任务冻结的 generation、base version、expected head 和 evidence digest 在提交时重新核对。
- 内容相同不等于来源相同；`projection_digest` 不能替代 generation/evidence binding。

### 容量和性能 AC

口径均为 ECS x86_64、2 vCPU、约 2GiB RAM、release build、SQLite checkpoint 后；网络下载时间单独报告。

| 项目 | 目标 | 硬上限 / 判定 |
|---|---:|---:|
| fresh 全市场 DB | ≤2.25GiB | ≤2.5GiB |
| 完整部署目录 | ≤3.5GiB | ≤4GiB |
| 部署后空闲磁盘 | — | ≥2GiB |
| 正常单交易日 DB 净增长 | ≤8MiB | ≤16MiB |
| 正常运行 WAL | ≤8MiB | ≤32MiB，idle/shutdown 后 0 |
| 本地正常日更新 | ≤3min | ≤5min |
| fresh 全市场首采 | ≤15min | ≤20min |
| 峰值 RSS | ≤1.25GiB | ≤1.5GiB |
| 250 日 late/early 增长比 | ≤1.20 | ≤1.20 |

首次 DB 的内部预算不是独立 PASS，而是定位门槛：

| 对象族 | 预算 |
|---|---:|
| canonical bars + bar/segment indexes | ≤1.90GiB |
| versions/projections/evidence/catalog/tasks/results | ≤256MiB |
| SQLite page/fragmentation 与后续 headroom | ≥300MiB |

已知 33,302,420 bars 下，canonical bar 对象族平均物理成本必须 ≤61 bytes/bar；设计原型阶段以 ≤56 bytes/bar 为目标，为索引和不同数据分布留余量。超过 61 bytes/bar 时停止进入全实现，先修布局。

单日 5,221-item 更新的预算：

| 对象族 | 目标 |
|---|---:|
| 新 canonical bars + segments | ≤1.0MiB |
| parent-delta/checkpoint refs + versions + heads | ≤2.0MiB |
| task/items/results/receipts | ≤4.0MiB |
| page/索引余量 | ≤1.0MiB |

每 item 在 `task_items`、update result/receipt 及其 indexes 中新增的总物理空间目标 ≤768 bytes（5,221 items 约 3.83MiB）、硬上限 ≤2KiB。共同 command、selection 和 generation evidence 只能按 task/generation 保存一次，不计入每 item 重复预算。

## schema 19 物理模型

以下 DDL 是实现目标；命名可因 SQLite 迁移语法做机械调整，但字段语义、键、类型和禁止重复 bars 的边界不可改变。

### Evidence 与来源绑定

```sql
CREATE TABLE evidence_bundles(
  evidence_digest BLOB PRIMARY KEY CHECK(length(evidence_digest)=32),
  evidence_kind TEXT NOT NULL CHECK(evidence_kind IN (
    'generation','batch_selection','version','continuity','catalog','yearly'
  )),
  canonical_json TEXT NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE source_bindings(
  source_binding_digest BLOB PRIMARY KEY CHECK(length(source_binding_digest)=32),
  generation_id TEXT NOT NULL,
  generation_manifest_sha256 BLOB NOT NULL CHECK(length(generation_manifest_sha256)=32),
  day_member_sha256 BLOB NOT NULL CHECK(length(day_member_sha256)=32),
  security_event_sha256 BLOB NOT NULL CHECK(length(security_event_sha256)=32),
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest)
) STRICT, WITHOUT ROWID;
```

`generation` 绑定文件集合与 consistency decision；`batch_selection` 绑定一次 task 的冻结选择；`version` 绑定 unit/currency/coverage/adjustment；`continuity` 绑定 member prefix/revision/event 事实；`catalog` 和 `yearly` 分别绑定目录与派生血缘证据。`canonical_json` 只允许低频、共享、无 bars 的证据对象。摘要按下文 framing 计算；读取必须重算，不能只相信主键。

`evidence_kind='version'` 的 canonical object 固定为且只允许以下字段：

```text
{
  "source": text,
  "coverage_reason": text,
  "unit_currency": typed UnitCurrencyEvidence,
  "adjustment_evidence": typed AdjustmentEvidence
}
```

`source`、reason 和所有 evidence value/reason 均不得为空；adjustment evidence 必须 confirmed 且 value=`qfq`。`coverage_status` 仍存于 `data_versions` 的类型化列并由 version integrity digest 绑定，不在 JSON 重复。严格递增的 canonical bars 决定 `record_count=unique_record_count`、`duplicate_count=0`，因此后两项不另存。verified read 用该 typed object 恢复现有 `EffectiveVersion` 元数据；未知或多余字段 fail closed。

### Immutable segments 与 canonical bars

```sql
CREATE TABLE bar_segments(
  segment_key INTEGER PRIMARY KEY,
  segment_digest BLOB NOT NULL UNIQUE CHECK(length(segment_digest)=32),
  entity TEXT NOT NULL,
  period INTEGER NOT NULL CHECK(period IN (1,2)), -- 1=daily, 2=yearly
  first_day INTEGER NOT NULL,
  last_day INTEGER NOT NULL,
  row_count INTEGER NOT NULL CHECK(row_count BETWEEN 1 AND 512),
  CHECK(first_day <= last_day)
) STRICT;

CREATE TABLE segment_bars(
  segment_key INTEGER NOT NULL REFERENCES bar_segments(segment_key),
  trade_day INTEGER NOT NULL,
  open_cents INTEGER NOT NULL,
  high_cents INTEGER NOT NULL,
  low_cents INTEGER NOT NULL,
  close_cents INTEGER NOT NULL,
  volume INTEGER,
  amount TEXT,
  r7_raw INTEGER,
  PRIMARY KEY(segment_key, trade_day),
  CHECK(high_cents >= open_cents AND high_cents >= close_cents AND high_cents >= low_cents),
  CHECK(low_cents <= open_cents AND low_cents <= close_cents),
  CHECK(volume IS NULL OR volume >= 0),
  CHECK(r7_raw IS NULL OR r7_raw BETWEEN 0 AND 4294967295)
) STRICT, WITHOUT ROWID;
```

约束：

- `entity` 沿用当前 canonical symbol：小写交易所 `sh|sz`、点号、六位代码，例如 `sh.600006`；写入前按 catalog identity 验证，不接受别名或大小写变体。
- `trade_day` 是 `date - 1970-01-01` 的有符号日数；wire 仍输出 ISO date。
- `*_cents` 是精确 2dp Decimal ×100；写入前必须验证 `value == cents/100` 且 i64 不溢出。
- daily volume 来自已验证非负原始整数；yearly volume 是完整已知 daily volume 的精确和。超过 SQLite signed i64 时拒绝候选。
- amount 使用现有规范化十进制文本：无科学计数法、无无意义尾零、零为 `0`，并在 Rust codec 验证非负及 `Decimal` 可表示。选择 TEXT 是因为 `.day` amount 源自 f32 十进制投影且 yearly 需要精确求和，当前没有统一安全 fixed scale。
- `r7_raw` 仅 daily 可有值；yearly 必须 NULL，由 Adapter 验证。
- segment 写入后没有 UPDATE/DELETE 业务路径；相同 digest 的复用必须重算并逐字段确认，不能仅依赖 hash 相同。
- fresh initial collection 将每证券历史切成最多 512 bars 的连续 segment，使完整性读取的单 segment 验证成本有界。

### Parent-delta manifest

日常版本不复制二叉树路径。每个 version 要么是一个 checkpoint，要么在一个 parent projection 上应用少量有序 delta。权威投影始终可由 immutable 链重建；checkpoint 只压缩引用链，不复制 bars。

```sql
CREATE TABLE data_versions(
  version_key INTEGER PRIMARY KEY,
  version_id TEXT NOT NULL UNIQUE,
  entity TEXT NOT NULL,
  period INTEGER NOT NULL CHECK(period IN (1,2)),
  adjustment INTEGER NOT NULL CHECK(adjustment=1), -- 1=qfq
  state INTEGER NOT NULL CHECK(state IN (1,2,3)), -- 1=staged,2=validated,3=published
  source_binding_digest BLOB NOT NULL REFERENCES source_bindings(source_binding_digest),
  parent_version_key INTEGER REFERENCES data_versions(version_key),
  manifest_mode INTEGER NOT NULL CHECK(manifest_mode IN (1,2)), -- 1=checkpoint, 2=delta
  delta_depth INTEGER NOT NULL CHECK(delta_depth BETWEEN 0 AND 256),
  record_count INTEGER NOT NULL CHECK(record_count > 0),
  first_day INTEGER NOT NULL,
  last_day INTEGER NOT NULL,
  coverage_status INTEGER NOT NULL CHECK(coverage_status IN (1,2,3)), -- 1=complete,2=incomplete,3=unknown
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  projection_digest BLOB NOT NULL CHECK(length(projection_digest)=32),
  integrity_digest BLOB CHECK(integrity_digest IS NULL OR length(integrity_digest)=32),
  digest_format TEXT NOT NULL CHECK(digest_format='compact-market-version-v1'),
  created_at TEXT NOT NULL,
  published_at TEXT,
  CHECK(first_day <= last_day),
  CHECK((state IN (1,2) AND published_at IS NULL AND integrity_digest IS NULL)
     OR (state=3 AND published_at IS NOT NULL AND integrity_digest IS NOT NULL)),
  CHECK((manifest_mode=1 AND parent_version_key IS NULL AND delta_depth=0)
     OR (manifest_mode=2 AND parent_version_key IS NOT NULL AND delta_depth BETWEEN 1 AND 256))
) STRICT;

CREATE INDEX data_versions_entity_period_published
  ON data_versions(entity, period, adjustment, published_at DESC, version_key DESC);

CREATE TABLE version_checkpoint_clips(
  version_key INTEGER NOT NULL REFERENCES data_versions(version_key),
  seq INTEGER NOT NULL CHECK(seq >= 0),
  segment_key INTEGER NOT NULL REFERENCES bar_segments(segment_key),
  clip_start_day INTEGER NOT NULL,
  clip_end_day INTEGER NOT NULL,
  row_count INTEGER NOT NULL CHECK(row_count > 0),
  PRIMARY KEY(version_key,seq),
  CHECK(clip_start_day <= clip_end_day)
) STRICT, WITHOUT ROWID;

CREATE TABLE version_delta_clips(
  version_key INTEGER NOT NULL REFERENCES data_versions(version_key),
  seq INTEGER NOT NULL CHECK(seq >= 0),
  delta_kind INTEGER NOT NULL CHECK(delta_kind IN (1,2)), -- 1=insert/replace, 2=remove
  range_start_day INTEGER NOT NULL,
  range_end_day INTEGER NOT NULL,
  segment_key INTEGER REFERENCES bar_segments(segment_key),
  clip_start_day INTEGER,
  clip_end_day INTEGER,
  row_count INTEGER NOT NULL CHECK(row_count >= 0),
  PRIMARY KEY(version_key,seq),
  CHECK(range_start_day <= range_end_day),
  CHECK((delta_kind=1 AND segment_key IS NOT NULL AND clip_start_day IS NOT NULL
         AND clip_end_day IS NOT NULL AND row_count > 0)
     OR (delta_kind=2 AND segment_key IS NULL AND clip_start_day IS NULL
         AND clip_end_day IS NULL AND row_count=0))
) STRICT, WITHOUT ROWID;
```

规则：

- staged→validated 只改变 state；发布事务一次写入 `published_at`、`integrity_digest` 和 state=published。published 后 version/checkpoint/delta/lineage 不再 UPDATE。
- initial collection 和 full revision 写 checkpoint clips；通常每证券约 `ceil(history/512)` 行。
- append/backfill 写 delta version；正常单日 append 每证券只有一条 delta clip。
- delta_kind=insert/replace 按 segment 中实际 `trade_day` upsert parent 投影，不删除该范围内未出现的 parent bars；delta_kind=remove 才删除显式日期范围。remove 只为未来已批准语义预留，当前 update/backfill/republish 一律不产生 remove。
- 同一 version 的 delta ranges 必须有序且互不重叠；应用后投影日期严格递增、无重复。
- parent 必须与 child 的 entity/period/adjustment 相同且已 published；提交事务重新核对。
- parent `delta_depth < 256` 时写 delta；达到 256 时，新 version 写 checkpoint clips，引用重建后的全部既有 segment，不复制 `segment_bars`。checkpoint clip 数和重建结果必须机械相等。
- checkpoint 是新业务 version 的存储表示选择，不创建额外“压实版本”，不改变 request/head 语义。

Yearly lineage 保持类型化且只能绑定已发布 daily：

```sql
CREATE TABLE yearly_lineage(
  version_key INTEGER PRIMARY KEY REFERENCES data_versions(version_key),
  base_daily_version_key INTEGER NOT NULL REFERENCES data_versions(version_key),
  algorithm_version TEXT NOT NULL,
  first_trade_day INTEGER NOT NULL,
  last_trade_day INTEGER NOT NULL,
  as_of_day INTEGER NOT NULL,
  CHECK(first_trade_day <= last_trade_day),
  CHECK(last_trade_day <= as_of_day)
) STRICT;
```

Adapter 另行验证 `version_key` 为 yearly、base 为同 entity 的 published QFQ daily，且 yearly projection 由该 base 确定性重算一致。

`envelope_json`、manifest path-copy nodes、`records`、`batch.bars` 和完整 compatibility JSON 不进入 schema 19。schema 18 的 `payloads` 表在 fresh schema 中删除；任何共享非-bar evidence 只进入有 kind/大小限制的 `evidence_bundles`。MCP wire 在 verified read 后由类型化行投影生成。

### Result registry、update result 与 receipt

所有 publication/result ID 继续共享一个命名空间，避免同一 ID 被不同结果类型重复消费：

```sql
CREATE TABLE result_registry(
  result_id TEXT PRIMARY KEY,
  result_kind INTEGER NOT NULL CHECK(result_kind IN (1,2)) -- 1=market_update,2=publication
) STRICT, WITHOUT ROWID;

CREATE TABLE market_update_results(
  result_key INTEGER PRIMARY KEY,
  result_id TEXT NOT NULL UNIQUE REFERENCES result_registry(result_id),
  task_id TEXT NOT NULL,
  item_id TEXT NOT NULL,
  base_version_key INTEGER NOT NULL REFERENCES data_versions(version_key),
  result_version_key INTEGER NOT NULL REFERENCES data_versions(version_key),
  outcome INTEGER NOT NULL CHECK(outcome IN (1,2)), -- 1=no_change, 2=published
  range_start_day INTEGER NOT NULL,
  range_end_day INTEGER NOT NULL,
  algorithm_version TEXT NOT NULL,
  source_binding_digest BLOB NOT NULL REFERENCES source_bindings(source_binding_digest),
  continuity_mode INTEGER NOT NULL CHECK(continuity_mode IN (1,2,3)), -- 1=unchanged,2=append,3=full
  prefix_bytes INTEGER,
  base_projection_digest BLOB NOT NULL CHECK(length(base_projection_digest)=32),
  result_projection_digest BLOB NOT NULL CHECK(length(result_projection_digest)=32),
  delta_digest BLOB CHECK(delta_digest IS NULL OR length(delta_digest)=32),
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  task_revision INTEGER NOT NULL,
  result_digest BLOB NOT NULL CHECK(length(result_digest)=32),
  UNIQUE(task_id,item_id),
  FOREIGN KEY(task_id,item_id) REFERENCES task_items(task_id,item_id),
  CHECK(range_start_day <= range_end_day),
  CHECK((continuity_mode IN (1,2) AND prefix_bytes IS NOT NULL AND prefix_bytes>=0)
     OR (continuity_mode=3 AND prefix_bytes IS NULL)),
  CHECK((outcome=1 AND continuity_mode=1 AND base_version_key=result_version_key AND delta_digest IS NULL)
     OR (outcome=2 AND continuity_mode IN (2,3) AND base_version_key<>result_version_key AND delta_digest IS NOT NULL))
) STRICT;

CREATE TABLE publication_results(
  result_key INTEGER PRIMARY KEY,
  publication_id TEXT NOT NULL UNIQUE REFERENCES result_registry(result_id),
  task_id TEXT NOT NULL,
  item_id TEXT NOT NULL,
  publication_kind INTEGER NOT NULL CHECK(publication_kind IN (1,2,3)), -- 1=collection,2=yearly,3=republish
  base_version_key INTEGER REFERENCES data_versions(version_key),
  result_version_key INTEGER NOT NULL REFERENCES data_versions(version_key),
  outcome INTEGER NOT NULL CHECK(outcome IN (1,2)), -- 1=no_change,2=published
  algorithm_version TEXT NOT NULL,
  source_binding_digest BLOB NOT NULL REFERENCES source_bindings(source_binding_digest),
  evidence_digest BLOB NOT NULL REFERENCES evidence_bundles(evidence_digest),
  task_revision INTEGER NOT NULL,
  result_digest BLOB NOT NULL CHECK(length(result_digest)=32),
  UNIQUE(task_id,item_id),
  FOREIGN KEY(task_id,item_id) REFERENCES task_items(task_id,item_id),
  CHECK((outcome=1 AND base_version_key IS NOT NULL AND base_version_key=result_version_key)
     OR (outcome=2 AND (base_version_key IS NULL OR base_version_key<>result_version_key)))
) STRICT;

CREATE TABLE task_publications(
  task_id TEXT NOT NULL REFERENCES tasks(task_id),
  version_key INTEGER NOT NULL UNIQUE REFERENCES data_versions(version_key),
  result_id TEXT NOT NULL UNIQUE REFERENCES result_registry(result_id),
  PRIMARY KEY(task_id,version_key)
) STRICT;

CREATE TABLE effective_heads(
  entity TEXT NOT NULL,
  period INTEGER NOT NULL CHECK(period IN (1,2)),
  adjustment INTEGER NOT NULL CHECK(adjustment=1),
  version_key INTEGER NOT NULL UNIQUE REFERENCES data_versions(version_key),
  PRIMARY KEY(entity,period,adjustment)
) STRICT;
```

`publication_batches` 和 `yearly_execution_results.audit_json` 不再复制 command/receipt JSON。daily update/backfill 的单一 durable receipt 是 `market_update_results`；collection/yearly/republish 的单一 durable receipt 是 `publication_results`。result 与匹配 kind 的 registry row、item outcome 在同一事务提交；outcome=published 同时提交 `task_publications`、published version 和可选 head，no_change 不写 `task_publications`。publication verification 从这些类型化事实重建结论。

`publication_results.base_version_key` 表示同 entity/period 的既有结果版本：首次 collection 可 NULL，no_change 必须等于 result；yearly 的 daily 派生输入只在 `yearly_lineage.base_daily_version_key` 表达，不复用该列。command、selection、generation 和公共 evidence 存在 task/frozen-work 层一次；result 行只保存稳定 refs/digests。现有 `task_items.outcome_json` 保留协议所需的 success/reason/result ref，但 canonical bytes 目标 ≤192 bytes，禁止嵌入 bars、batch、完整 evidence 或重复 receipt。MCP `get_task_progress` 按引用组合响应，不要求持久化其 wire JSON。

## Canonical encoding 与摘要

所有摘要为 SHA-256，输入采用显式 domain separator、固定宽度大端整数、长度前缀 UTF-8 和 nullable tag。禁止使用 `serde_json::to_vec` 作为 schema 19 摘要规范。

基础编码：

```text
u8(value)
u32be(value)
u64be(value)
i64be(value)
bytes = u32be(length) || raw-bytes
text  = bytes(UTF-8)
nullable<T> = 0x00 | 0x01 || T
sha256 = 32 raw bytes, not hexadecimal text
```

### Bar encoding

```text
"x-data-bar-v1\0"
|| i64be(trade_day)
|| i64be(open_cents)
|| i64be(high_cents)
|| i64be(low_cents)
|| i64be(close_cents)
|| nullable<i64be>(volume)
|| nullable<text>(canonical_amount)
|| nullable<u32be>(r7_raw)
```

### Segment digest

```text
SHA256(
  "x-data-segment-v1\0"
  || text(entity)
  || u8(period)
  || u32be(row_count)
  || bar_encoding(row_1) ... bar_encoding(row_n)
)
```

bars 必须按 trade_day 严格递增且无重复。

### Projection digest

checkpoint clip encoding：

```text
segment_digest
|| i64be(clip_start_day)
|| i64be(clip_end_day)
|| u32be(row_count)
```

checkpoint projection：

```text
SHA256(
  "x-data-projection-checkpoint-v1\0"
  || text(entity)
  || u8(period)
  || u32be(clip_count)
  || ordered_checkpoint_clip_encoding...
)
```

delta operation encoding：

```text
u8(delta_kind)
|| i64be(range_start_day)
|| i64be(range_end_day)
|| nullable<segment_digest>
|| nullable<i64be>(clip_start_day)
|| nullable<i64be>(clip_end_day)
|| u32be(row_count)
```

delta evidence：

```text
SHA256(
  "x-data-version-delta-v1\0"
  || u32be(delta_count)
  || ordered_delta_operation_encoding...
)
```

该值保存为 update result 的 `delta_digest`。delta projection：

```text
SHA256(
  "x-data-projection-delta-v1\0"
  || parent_projection_digest
  || delta_digest
)
```

`projection_digest` 是存储投影身份：它唯一绑定 parent/checkpoint、ordered segment clips 和 canonical segment bytes。相同 bar 序列若通过不同、合法的 revision 历史形成，可以有不同 projection digest；业务 no-change 复用原 version，不通过重新分段制造等价 version。它不能替代来源、coverage 或发布资格。

checkpoint 重建时必须从 parent chain 得到 ordered clips，再写 checkpoint rows；新的 checkpoint projection digest 按 checkpoint framing 计算。实现必须同时逐 bar 证明 checkpoint 投影和 parent-chain 投影相等，不能只比较摘要。

### Source-binding 与 evidence digest

`source_binding_digest` 定义为：

```text
SHA256(
  "x-data-source-binding-v1\0"
  || text(generation_id)
  || generation_manifest_sha256
  || day_member_sha256
  || security_event_sha256
  || evidence_digest
)
```

Yearly version 复用其固定 base daily 的 source binding；本地聚合算法和 base daily identity 另由 yearly lineage 绑定，不伪装成新的外部 source member。

`canonical_json` 使用 RFC 8785 JSON Canonicalization Scheme；Decimal 和日期仍作为现有规范字符串，不转 IEEE-754 JSON number。摘要为：

```text
SHA256("x-data-evidence-v1\0" || text(evidence_kind) || bytes(canonical_json_utf8))
```

读取重新 canonicalize 并重算；不符合 JCS 或含 `bars`/`records` 数组的对象拒绝写入。canonical bytes 上限按 kind 固定：`continuity` ≤4KiB；`version`/`yearly` 各 ≤16KiB；`generation`/`catalog` 各 ≤256KiB；`batch_selection` ≤4MiB。超限必须改为已有稳定 refs 的类型化集合或拒绝，不能拆成每 item 副本绕过。

### Version integrity digest

`yearly_lineage_encoding` 定义为：

```text
text(base_daily_version_id)
|| text(algorithm_version)
|| i64be(first_trade_day)
|| i64be(last_trade_day)
|| i64be(as_of_day)
```

`base_daily_version_id` 由 `base_daily_version_key`、`parent_version_id` 由 `parent_version_key` 在同一读/写事务解析；checkpoint 的 parent 编码为 null，delta parent 必须存在且与 key 对应。

```text
SHA256(
  "x-data-market-version-v1\0"
  || text(version_id)
  || text(entity)
  || u8(period)
  || u8(adjustment)
  || source_binding_digest
  || nullable<text>(parent_version_id)
  || u8(manifest_mode)
  || u32be(delta_depth)
  || projection_digest
  || u64be(record_count)
  || i64be(first_day)
  || i64be(last_day)
  || u8(coverage_status)
  || evidence_digest
  || text(published_at_rfc3339)
  || nullable<yearly_lineage_encoding>
)
```

`published_at` 在发布事务前固定；只接受 UTC `DateTime<Utc>::to_rfc3339()` 的 canonical 文本（零偏移写作 `+00:00`），digest 与 version row 同事务写入。`created_at` 是 staging 运维时间，不属于 published 业务身份；`state` 在 digest 存在时必为 published；`digest_format` 已由 domain separator 和 SQL CHECK 固定，因此三者不重复进入 framing。固定版本读取必须核对这些 SQL 条件、parent identity、checkpoint/delta rows、所有涉及的 segment digest、row count/date bounds、projection digest 和 integrity digest。

`list_data_versions` 只读取 version、source/evidence binding 和 typed yearly lineage，重算 metadata/integrity digest 后投影轻量 summary；禁止读取 `segment_bars` 或重建 bar 序列。它证明列表元数据身份，不替代固定版本查询对 projection/segment 的完整校验。分页 cursor 固定 summary identity；续页重新核对对应 metadata/integrity digest。

### Result digests

`market_update_results.result_digest` 使用以下 framing；version key 在同一事务解析为 stable `version_id`：

```text
SHA256(
  "x-data-market-update-result-v1\0"
  || text(result_id)
  || text(task_id)
  || text(item_id)
  || text(base_version_id)
  || text(result_version_id)
  || u8(outcome)
  || i64be(range_start_day)
  || i64be(range_end_day)
  || text(algorithm_version)
  || source_binding_digest
  || u8(continuity_mode)
  || nullable<u64be>(prefix_bytes)
  || base_projection_digest
  || result_projection_digest
  || nullable<sha256>(delta_digest)
  || evidence_digest
  || u64be(task_revision)
)
```

它不包含任何 bars 数组；`delta_digest` 指向新 segment 集合的有序摘要。

`publication_results.result_digest` 使用 domain `x-data-publication-result-v1\0`，按以下顺序编码：

```text
text(publication_id)
|| text(task_id)
|| text(item_id)
|| u8(publication_kind)
|| nullable<text>(base_version_id)
|| text(result_version_id)
|| u8(outcome)
|| text(algorithm_version)
|| source_binding_digest
|| evidence_digest
|| u64be(task_revision)
```

所有 result read 都重新解析 stable version ID 并重算；surrogate key 不进入摘要，数据库复制/恢复不能改变业务 identity。

## 三种 update 审计语义

### No-change

必须满足：

- base/result version key 相同；
- base/result projection digest 相同；
- source binding 和 continuity evidence 足以证明 member/event 不变；
- `delta_digest IS NULL`；
- schema 19 不存在 `payloads`；本路径不 INSERT `bar_segments`、`segment_bars`、`version_checkpoint_clips`、`version_delta_clips`、`data_versions` 或 `publication_results`；
- 只在同一事务写 task item outcome、`market_update_results` 和必要 task counters。

### Append

必须满足：

- base `.day` member 是新 member 的精确字节 prefix；
- per-security event digest 不变；
- 只解析、QFQ 并写入严格晚于 base last_day 的 tail bars；
- `delta_digest` 绑定 tail segment digest 的有序序列；
- parent depth <256 时只新增一个 delta version 和 tail clip；depth=256 时新增 checkpoint clip refs，但不复制 bars；
- audit 只引用 base/result projection digest、delta digest、prefix bytes 和 evidence digest。

### Full revision

必须满足：

- revision 经现有业务规则和证据路径批准；无法证明则 `needs_verification`，不发布；
- 创建完整新 segment 集合和 checkpoint projection，但 audit 仍只引用 digests；
- 旧 checkpoint/delta/segments/version 不变；
- 不以 content-addressed payload 保存完整 history 副本。

## verified read

1. 在一个短 read transaction 中固定 version row、parent chain/checkpoint 和请求范围。
2. 校验每层 parent identity、delta depth、projection digest 和 evidence/source refs；chain 超过 256、成环或跨 entity/period 即损坏。
3. 从 checkpoint 顺序应用 delta，生成有序 clip projection；只对请求范围相交的 segment 取行，但每个被触达 segment 最多 512 bars，读取整段并重算 segment digest。
4. 校验 clip、日期严格递增、无重叠/重复、row count 和 bounds；coverage 声明需要完整性时，缺口另按既有 coverage evidence 判断，不能仅由交易日不连续推断。
5. 全版本 integrity 检查、版本列表检查或 publication verification 重算完整 projection/integrity digest；普通分页至少验证完整 parent chain 和所有被返回 segment，不保留跨 MCP 页长事务。
6. 返回 typed rows；不展开 legacy compatibility record。

当前 verified-read 的完整性来自 immutable chain + 每层 digest + 被读取 segment 的内容摘要，不需要每页扫描未请求的全部 bar。全库 scrub/backup verification 另行遍历所有 segments。若现有公开 Contract 测试证明每一页必须重新扫描完整版本，则该冲突在 S0 上报并冻结裁定，不得静默恢复 JSON cache。

## SQLite 连接、写入和 WAL

长期 `data_service` 使用一个 service-lifetime `WriteCoordinator`：

- 启动时打开一个 owner RW connection，检查 schema、inode/lease 和 PRAGMA 后保持到退出；idle 时无 transaction、无 read snapshot。
- background worker 和 MCP task acceptance/cancel/retry 等写操作通过同一 coordinator 串行执行；SQLite 仍只有一个业务 writer。
- 每个 item 的 publication+receipt 保持独立 `BEGIN IMMEDIATE` / COMMIT；不以全市场大事务换性能。
- coordinator 复用 prepared statements；schema identity 在启动和 transaction boundary 做轻量 `user_version`/migration digest 检查，不为每 item 重扫全部 migration rows。
- 读取使用有界短连接；materialize 完结果后立即结束 transaction。最多 16 readers，空闲连接不得持有 transaction。
- `synchronous=FULL`、`foreign_keys=ON`、`wal_autocheckpoint=1000`、`journal_size_limit=16MiB` 保持。
- task 终态和 idle 时记录 `(busy,log,checkpointed)` 并尝试 PASSIVE；graceful shutdown 在所有 transaction 结束后执行 TRUNCATE，成功时 WAL=0。
- crash/SIGKILL 后只走 SQLite recovery，禁止删除 WAL；重启后 committed receipts 不丢失，未知提交按现有 verification 流程处理。

CLI 和测试可继续使用短连接 repository；生产 `data_service` 必须使用 coordinator。若实现者主张短连接已足够，须先以同一 full-market workload 证明 checkpoint 数量、WAL、wall time 全部通过本设计 AC，经 review 后才能修改该决策。

## schema 升级与部署边界

### fresh production

- 新 release 使用 schema 19 初始化空库。
- 从已验证 active generation 重新发布 catalog/daily/yearly。
- 不导入 v14–18 market versions、audit payload 或 legacy digest。
- 旧数据库保留只读备份及证据，不由部署脚本自动删除。

### `task_sqlite upgrade`

- schema 18→19 前置检查所有 market/catalog/task/result 业务表为空。
- 非空时事务开始前返回稳定错误 `FRESH_REBUILD_REQUIRED: schema 19 compact market storage requires a new database`，原库字节和 `user_version` 不变。
- 空库可升级；历史 v4/v5/v7 等 migration 测试先升级到 18，再在空业务条件进入 19。
- migration 19 在同一事务内以 `_v19` 临时表创建上述同名目标表，按外键依赖顺序删除空的 schema-18 `publication_batches`/results/version projection/bar 表和 `payloads`，再 rename；不是直接对已存在同名表执行本节字面 DDL。
- `initialize` 仍可顺序应用 1..19，但最终 sqlite_master 必须只保留 schema-19 目标表；禁止把 schema-18 空壳或 payload 表留在 fresh DB。
- 普通服务启动仍不建库、不迁移。

该边界来自 #259 的 fresh-deployment 裁定。未来若确需保存旧版本，另立 migration Issue；不得在本实现中重新引入 legacy JSON 兼容路径。

## 实现切片与退出条件

### S0：production-shaped 失败测试和物理原型

只新增测试/benchmark，不改变生产写路径：

1. 真实形状 full-history no-change 当前会新增 bars payload，测试先红。
2. 使用候选 schema 装载 retained 数据的代表性证券，测量 `dbstat` bytes/bar。
3. amount 文本长度、NULL 比例、SQLite varint/page fill 和 segment size 64/256/512 的对比报告。

退出条件：512 上限布局目标 ≤56 bytes/bar、硬门 ≤61 bytes/bar；否则回到设计，不进入 S1。

执行结果（2026-09-21，base commit `fc0091948`，S0 working tree）：retained gen-2 三证券共 20,576 bars 的 512-segment 候选布局为 43.60 object bytes/bar，目标门通过；8,479-bar production-shaped no-change 在 schema 18 新增 1,129,885 bytes `payload_json`，失败测试按预期为红。证据见 `evidence/issue-259-s0-storage-prototype/result.json`。结论仅为 `PASS_TO_S1`，不代表 full-market 容量通过。

### S1：compact bar codec + segment/projection digest

实现 epoch day、cents、canonical amount codec，新 segment、checkpoint/delta 表及 corruption tests；不接 MCP。

退出条件：retained corpus逐 bar 与现有 typed `Bar` 相等；digest determinism；缺失/篡改/clip 错误全部 fail closed。

执行结果（2026-09-21，S1 working tree）：10 个 codec/digest 单测、15 个 SQLite primitive/corruption 单测通过；四个独立 framing 固定向量一致；retained gen-2 三证券 20,576 bars 逐条 compact round-trip 相等。`compact_v19` 尚未接 migration 或生产路径。证据见 `evidence/issue-259-s1-compact-storage/result.json`；结论为 `PASS_TO_S2`，不代表 schema 19 或容量验收完成。

### S2：compact version + verified read

移除 fresh schema 的 envelope expansion/legacy digest，接入 `compact-market-version-v1`、固定版本查询、版本列表和 yearly lineage。

退出条件：现有 query contract 等价；schema 19 无 `compat_json`、`envelope_json`、`bar_json`、`payloads` 或 bars payload；分页和损坏拒绝回归通过。

执行结果（2026-09-21，S2 working tree）：RFC 8785 evidence/source/version identity 18 个测试、compact version store 13 个测试及 S1 segment primitive 15 个测试通过；staged→validated→published、256 层 parent-delta、bar-by-bar checkpoint rollover、fixed/range verified read、metadata-only version list 和 yearly fixed-daily 重算均已覆盖。fresh S2 test schema 不含 legacy JSON/payload 对象，但模块尚未接 migration、`SqliteMarket`、MCP 或 production publication。证据见 `evidence/issue-259-s2-compact-version-read/result.json`；结论为 `PASS_TO_S3`，不代表 schema 19 部署或容量验收完成。

### S3：类型化 update audit/receipt

先完成 no-change，再 append，再 revision。schema 18 的 `audit_json.segment.bars` 和 `publication_batches` command/receipt 不迁入 schema 19。

退出条件：production-shaped no-change 对 bars/segments/checkpoints/deltas/versions 的 before/after 计数全部相同，且 schema 19 不存在 `payloads`；append 只新增 tail segment、一个 delta version 和对应紧凑 result。

### S4：WriteCoordinator 与 WAL

接入长期 writer、statement reuse、checkpoint metrics、graceful/SIGKILL tests。

退出条件：16 readers 下无 WAL 单调增长；idle/shutdown WAL=0；committed receipt 不丢失。

### S5：本机 full-market 验收

使用固定 retained generation 和 fresh DB：catalog → 5,221-scope daily → yearly sample → one-day no-change/append → 250-day production-shaped soak。

退出条件：容量、增长、时间、RSS、WAL 全部提供机械报告；任一硬上限失败即停止，不部署 ECS。

### S6：release 与 ECS

构建固定 release 包，SHA-256 对账后 ECS fresh deploy；只通过两个 MCP server + repository Skill 执行 generation/status/data workflow。

退出条件：full-market AC、重启恢复、Skill revision mismatch、availability/query 均通过后，才允许评审 `MAX_DEFAULT_SECURITIES` 和 `production_ready`。

## 验收矩阵

| 场景 | 必须断言 |
|---|---|
| no-change | 0 new bars/segments/checkpoints/deltas/versions；schema 19 无 payloads 表；head 不动；每 item（task row share 除外）新增物理空间 ≤2KiB |
| one-day append | 每有交易证券只新增一根 canonical bar；旧 segment/version 不变；新增一条 delta clip，depth=256 时新增 checkpoint refs |
| stopped/no new bar | 不制造空 segment；有证据的 no-change 或明确 needs_verification |
| event revision | 只影响对应证券；未批准 revision 不推进 head |
| corrupt segment row | segment/version/query fail closed，不回退旧版本 |
| corrupt parent/clip | projection reconstruction fail closed |
| lost commit response | 原 request/result receipt 可恢复，不重复发布 |
| 16 readers + writer | writer 有界完成；reader 结束后 checkpoint 推进 |
| SIGKILL | SQLite recovery 后 committed facts 保留，未确认任务保持保护 |
| 250-day soak | late/early DB-growth bytes 比 ≤1.20；wall time 报告绝对值并分别满足日更时间 AC |
| full-market initial | checkpoint DB ≤2.5GiB、RSS ≤1.5GiB、≤20min |
| full-market daily | DB growth ≤16MiB、WAL ≤32MiB、≤5min |

每份容量/性能报告必须记录：

- commit SHA、schema version、release/debug；
- 主机、CPU、RAM、文件系统、SQLite version、page size；
- generation ID、证券数、bars/versions/tasks/results 数；
- checkpoint 前后 DB/WAL bytes；
- `dbstat` 每表/索引 bytes、payload、unused；
- source index、GBBQ decode、QFQ、task transition、SQLite commit、checkpoint 的 wall/CPU；
- peak RSS；
- 未验证项和失败断言。

## 明确禁止

- 不以 payload ref 替代 inline bars 后宣称已去重；同一完整 history 若仍新增一次即失败。
- 不持久化 MCP response、完整 `MarketDataBatch`、`PublishedRecord` 或 command JSON 作为审计副本。
- 不以 VACUUM、只保留 latest、删除历史、降低 FULL durability 或全市场大事务达标。
- 不把 fixture 1-bar soak、100-item 外推、部署目录大小或功能 E2E 当容量 PASS。
- 不在 full-market AC 前放宽 100-item、启动 ECS 5,221-symbol collection 或改 `production_ready=true`。
- 不为兼容 schema 18 published identity 恢复 legacy JSON reconstruction；旧库只读保留。

## Review Gate

实现开始前必须由 review 明确接受：

1. cents/epoch-day/amount TEXT 的 canonical codec；
2. schema 19 DDL 和 fresh-only upgrade 边界；
3. `compact-market-version-v1` framing；
4. no-change/append/revision 类型化 audit；
5. ≤61 bytes/bar 原型门槛和完整容量预算；
6. WriteCoordinator/WAL 模型；
7. S0–S6 顺序及停止条件。

在该 Gate 前，本文件只代表设计候选，不代表 #259 AC、schema 19 或生产部署已经完成。
