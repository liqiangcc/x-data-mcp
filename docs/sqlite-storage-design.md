# SQLite 行情存储与分析查询设计

> **历史设计文档。** 本文描述 2026-09-12 首期方案，不代表当前 schema 18。
> 当前实现已使用 compact envelope、persistent manifest tree 和 O(items) task
> transition；但仍保留 `legacy-published-record-v1`、读时完整 compatibility
> record expansion、`segment_bars.bar_json` 和 schema 16/17 legacy migration replay。
> `compact-segmented-record-v2`、紧凑列式 canonical bars 及 no-change audit
> 不复制完整 history 尚未完成。当前权威状态见
> [#259 physical audit](https://github.com/liqiangcc/x-data-mcp/issues/259#issuecomment-5756919342)。

## 状态与决策来源

设计任务：[Issue #69](https://github.com/liqiangcc/x-data-mcp/issues/69)。编写日期：2026-09-12。
初稿代码核对基线：`84e5a2bdbf04ba37793f969d311627ab97bb4d13`。
2026-09-12 对齐已读取的 main `1e270c648e7884fffd65ef13cfe9f47e0dab22ad` 的交付状态与
[实施基线](https://github.com/liqiangcc/x-data-mcp/blob/2002518cad08493722473e2132d3e7f4a86518da/docs/implementation-baseline.md)。
本轮只完善文档与 Issue，不派发、不实现、不迁移、不部署、不合并 PR；不把文档核对冒充代码重验。
本文为待实现的设计基线，不是数据库迁移、真实来源或部署的验收记录。

用户已确认：首期使用 SQLite；结构化行情应便于后续分析和查询。
用户已授权本次编写并提交设计。下文表划分、事务和实现顺序属于本次设计决定，
不能反推所有具体字段、性能指标或迁移范围都已由用户逐项确认。

本段“当前代码”仅指 2026-09-12 基线；当时仍采用 JSON 文件，已有[发布单点提交](publish-storage.md)、
[任务受理单点提交](task-storage.md)和 #67 历史版本完整性读取。
[#68](https://github.com/liqiangcc/x-data-mcp/issues/68) 已关闭，其已合并查询契约作为兼容回归基线，不再派发。
SQLite 实现替换持久化机制，须保留这些用例的可观察语义。

本文是首期存储目标设计的入口；既有文档保留其对应提交的实现证据。
[官方文件架构](production-source.md)继续负责来源边界，部分 stage-1 状态为历史记录，
实际已实现能力以 README 与关联提交为准。本文不解除任何来源门禁。

## 目标与范围

主要使用路径：

1. 采集和解析结果经过独立质量检查，保存为可追溯版本。
2. 用户或 AI 按证券、日期、行业、版本查询行情。
3. 分析程序取得固定版本数据，以精确十进制计算或显式近似方式开展统计。
4. 更新产生新版本，旧查询和研究结果仍能解释其输入。

首期采用一个本地 SQLite 数据库保存结构化数据，原始下载文件独立保留。
不要求先引入 PostgreSQL、Parquet 或对象存储服务。不按股票创建数据库或动态表。
SQLite 文件位于存储服务所在主机的持久磁盘；跨主机计算通过受保护的服务接口
或一致性备份副本读取，不把活动数据库放到网络共享盘实现存算分离。
SQLite 的选择本身不构成远程数据库服务。

本次仅交付文档。不修改 d2 工作区、不迁移旧仓库数据、不实现或部署新服务。
来源与首期市场范围沿用 #1/#55/#6：当前沪深 A 股、TDX 研究行业二级、
官方文件本地 QFQ daily，以及从固定 daily version 派生的 yearly。

## 职责和依赖

| 边界 | 职责 | 不承担的职责 |
|---|---|---|
| 领域计算 | 精确数值、质量规则、QFQ、自然年聚合 | SQL、下载、MCP 参数 |
| 应用用例 | 固定输入、编排采集/校验/发布、查询与任务状态转换 | 具体数据库连接和文件布局 |
| 存储能力契约 | 原子受理、原子发布、固定版本读取、来源查询 | 业务规则的第二套实现 |
| SQLite Adapter | 表映射、事务、约束、索引、完整性核验 | 放宽门禁、猜测字段单位 |
| 原始文件 Adapter | 临时下载、校验、不可变文件落盘、哈希读取 | 决定数据可发布 |
| MCP / 分析读取入口 | 权限、参数校验、结果和限制表达 | 隐式采集、任意 SQL 写入 |
| 装配与运行 | 连接配置、schema 升级、worker 生命周期 | 隐藏恢复、自动解除待核实 |

复用现有领域类型和用例契约；必要时增加具体的事务能力方法，不建立通用仓储框架。
应用层不得依赖 rusqlite 连接或 Transaction 类型。Rust SQLite 绑定在实现任务中
核对维护状态与 bundled 构建方式、锁定版本，不在本文凭空指定依赖版本。
原始下载和计算在数据库写事务外执行。

## 数据布局与逻辑模型

建议在现有 `X_DATA_MCP_STORE` 目录下使用 `market-data.sqlite3` 与 `raw/`。
路径为实现设计，不表示当前二进制已经支持。迁移前后后端由装配明确选择。

以下是逻辑 schema；最终 DDL、CHECK、外键和迁移脚本由实现 PR 提交。
所有 ID 使用稳定 TEXT；证券代码保留六位字符串，不能转换为整数。
日期用经过领域校验的 ISO `YYYY-MM-DD`，交易日和自然年按 Asia/Shanghai。
获取/发布时间用统一 UTC 时间戳，来源时间允许 NULL，三者不互相替代。

| 表 | 关键字段 / 键 | 含义与约束 |
|---|---|---|
| schema_migrations | version PK, checksum, applied_at | 有序迁移及脚本身份；未知新 schema 拒绝启动写入 |
| securities | security_id PK, exchange, code; UNIQUE(exchange,code) | 稳定证券身份；名称和当前行业来自目录版本 |
| catalog_versions | catalog_version_id PK, manifest_id, digest | 不可变目录快照、来源与校验状态 |
| catalog_members | (catalog_version_id,security_id) PK, name, industry_code, classification_status | 目录成员与分类；缺失/冲突显式表达，不伪造历史有效期 |
| raw_files | file_id PK, sha256 UNIQUE, relative_path, byte_size | 原始字节的内容身份；不在大 BLOB 中重复保存原文件 |
| raw_acquisitions | acquisition_id PK, file_id FK, source_url, acquired_at, source_updated_at?, etag? | 相同内容可有多次获取记录 |
| raw_members | (archive_file_id,member_path) PK, member_file_id | 压缩包与成员关系，分别保留哈希 |
| source_manifests | manifest_id PK, contract_version, parser_version, evidence_refs | 不可变来源/规则清单；仅小型扩展证据可用 JSON |
| manifest_inputs | (manifest_id,input_role,input_id) PK | 固定 catalog、day、gbbq 与一致性证据的身份 |
| event_versions | event_version_id PK, manifest_id, status, digest | 公司行动事件快照；未确认语义的解析结果不可发布 |
| corporate_actions | (event_version_id,event_id) PK, security_id, event_date, event_type, raw_member_id, record_offset | 事件身份与原始位置；业务十进制字段须 #54 确认后定稿 |
| data_versions | version_id PK, security_id, period, adjustment, state, published_at?, manifest_id, content_digest, digest_format, row_count, first_date, last_date, coverage_status | 每证券/周期/口径的不可变完整逻辑快照 |
| version_inputs | (version_id,input_role,input_version_id) PK, algorithm_version, basis_date? | raw daily、事件版本、目录与复权基准；用具体外键或约束验证输入类型 |
| daily_bars | (version_id,trade_date) PK, open, high, low, close, volume?, amount? | 每行一根日线，关联唯一版本；raw 与 QFQ 由版本口径区分 |
| yearly_bars | (version_id,year) PK, open, high, low, close, volume?, amount?, first_trade_date, last_trade_date, is_finished | 每年一行；结束标记相对版本 as_of，不单独证明年度覆盖或最终性 |
| yearly_lineage | version_id PK/FK, base_daily_version_id FK, algorithm_version, as_of | 每个年线版本固定同一个已发布 QFQ daily 输入 |
| publication_batches | batch_id PK, task_id?, candidate_version_id, outcome, evidence_refs | 保存候选/发布结果，不能与有效版本事实矛盾 |
| effective_heads | (security_id,period,adjustment) PK, version_id FK | 当前版本指针；只指向已发布且身份匹配的版本 |
| requests | request_id PK, operation, dataset/legacy_kind, canonical_payload, contract_version, task_id?, request_outcome | 全局幂等；完整语义载荷和无任务请求级终态，摘要仅作辅助 |
| tasks | task_id PK, contract_identity, status, state_revision, accepted_run_id, owner_run_id?, result, created_at, last_progress_at | 受理启动身份与执行 owner 分开；合法转换由用例决定 |
| task_items | (task_id,item_id) PK, typed_target, progress_unit, stage, outcome, frozen_inputs | 稳定执行项及类型化定位；公共模型不固定证券字段 |
| task_scopes | (task_id,scope_key) PK, normalized_scope | 首次受理固化的完整范围 |
| scope_occupancy | scope_key PK, task_id FK | 写范围占用；范围相交按现有业务规则检查 |
| task_publications | (task_id,version_id) PK | 将已提交输出与任务结果关联，支持结果核实 |

`scope_key` 必须来自对应业务的冲突粒度。当前行情始终按证券 × 周期占用，
同一证券同一周期即使日期范围不重叠也冲突，不按日期缩小既定保护。
未来类别若有区间相交语义，应在同一受理写事务内核对，不只比较字符串 key。
批量范围任一冲突整体拒绝；多态版本关联必须有明确目标类型及存在性约束。
表格不穷举 #3/#4/#5 的核实记录字段；实现时逐项映射既有状态和持久证据，不能因简表遗漏而删除能力。

版本保存完整逻辑快照，首期允许按版本复制完整行情行，以简化历史读取和修订。
不提前实现行级增量链或共享分片。存储增长随证券数、交易日数和保留版本数增长；
首期不自动清理已发布版本及关联说明已由 #4 确认，未来清理期限与授权另定。后续实测空间后再决定压缩、共享或归档。

行情主键防止同版本重复日期/年度。外键验证版本存在，Adapter 另检查周期、口径、
证券、单位/币种一致。价格单位、币种、成交量/额单位与契约版本作为版本元数据保存，
不根据列名默认为元、股或手。原始整数和浮点位模式仍可从固定原始文件及位置恢复；
未经确认的 raw bits 不冒充标准价格。

## 精确小数与分析能力

首期精确数值统一使用规范化十进制 TEXT，经 Rust 精确十进制类型校验后写入：
不使用科学计数法，去除无意义尾零，零统一为 `0`，不擅自固定两位或六位小数。
输入超出当前领域数值范围时拒绝并报告，不能截断。源格式有特殊值时由解析契约拒绝或
保留原始证据，不能把 NaN/Infinity 放进标准行情。未知值使用 SQL NULL。

SQLite REAL 为近似浮点；仅声明 DECIMAL/NUMERIC 不能保证精确十进制语义。
参见 [SQLite 数值说明](https://www.sqlite.org/floatingpoint.html)。

TEXT 的普通排序是字典序，内建 SUM/AVG 或 CAST AS REAL 也不能作为精确业务计算。
因此首期查询分为：

- 日期、身份、版本、行业过滤与关联：直接用索引 SQL。
- 精确价格比较、求和、OHLC 聚合：取出规范化文本，由 Rust Decimal 或 Python Decimal 计算。
- 探索性近似统计：分析者显式 CAST AS REAL，结果标注近似，不回写标准行情或发布校验。

若后来确需数据库内精确数值范围过滤，再以实际分析用例增加精确函数/排序规则，
或确定可验证的定点整数编码；不是首期 schema 的隐含可用能力。

示例：应用先确认 `:version_id` 为该证券已发布且可核验的日线版本，再执行：

```sql
SELECT trade_date, open, high, low, close, volume, amount
FROM daily_bars
WHERE version_id = :version_id
  AND trade_date >= :start_date
  AND trade_date <= :end_date
  AND (:after_date IS NULL OR trade_date > :after_date)
ORDER BY trade_date
LIMIT :page_size;
```

示例：以明确目录快照查询某行业当前选定版本的数据（示例中的当前指针应在同一读快照解析）：

```sql
SELECT s.exchange, s.code, b.trade_date, b.close, h.version_id
FROM catalog_members AS c
JOIN securities AS s ON s.security_id = c.security_id
JOIN effective_heads AS h ON h.security_id = s.security_id
JOIN data_versions AS v ON v.version_id = h.version_id
JOIN daily_bars AS b ON b.version_id = v.version_id
WHERE c.catalog_version_id = :catalog_version_id
  AND c.industry_code = :industry_code
  AND h.period = 'day' AND h.adjustment = 'qfq'
  AND v.state = 'published'
  AND b.trade_date BETWEEN :start_date AND :end_date;
```

这是“给定目录快照的行业成员”分析，不是行情日期当时的行业归属，
也不保证当时可获得这些资料。跨股票研究应记录每只股票的 version_id 清单、
目录版本及查询时点；各股票最新版本并不自动构成全市场同步快照。
直接 SQL 示例不替代服务完整性核验；研究入口应输出所用版本清单与核验结果。

最小索引：行情主键；`data_versions(security_id,period,adjustment,published_at,version_id)`；
`catalog_members(catalog_version_id,industry_code,security_id)`；`tasks(status,created_at,task_id)`。
跨全市场按日期扫描是否增加日期优先索引，由 EXPLAIN QUERY PLAN 和实测决定。

## 发布事务与完整性

版本阶段为 staging → validated → published；失败候选保留诊断或按明确清理规则处置。
这是目标存储状态，不是重命名现有公共任务状态。

1. 下载到临时文件，校验字节数/哈希，完成文件同步后原子移到内容寻址位置；
   在 Linux 实现中同步父目录。数据库只引用已经落盘的不可变文件。
2. 解析、计算并分批写 staging 版本的行情行。staging 对普通查询不可见。
3. 固定输入并完成数值、范围、来源一致性和 digest 校验；validated 后禁止修改行情。
4. 在一个短写事务中重新验证发布资格、任务/取消状态、占用及发布意图要求的 head 前置条件，
   同时提交 batch 结果、version 发布状态、task_publications；只有 `current-head` 派生满足全部
   前置条件时才一并推进对应 yearly `effective_heads`。`historical-fixed-base` 派生只发布不可变版本，
   不改变默认 head。若该发布恰为任务终结条件，终态和占用释放在同一事务完成；否则仅更新该项结果。
5. COMMIT 是数据库内唯一发布可见点。需要推进 head 的事务任一前置条件或写入失败则整体回滚，
   旧 effective head 保留；历史固定基础版本的发布不以 head 推进作为成功条件。

批量多证券任务允许按既有规则部分完成；不将整个市场改成一个大事务。
提交后不可改已发布行情、lineage 或原始输入，修订必须创建新版本。
使用条件更新检查发布意图要求的旧 head，避免过期候选覆盖较新版本；冲突明确报告，不最后写入者胜出。
`published` 与“当前默认版本”是不同事实：历史固定基础可以产生新的 published 版本，但只有明确的
`current-head` 派生在提交时仍满足 daily/yearly head 前置条件，才能推进 yearly effective head。

数据库与原始文件不能组成一个 SQLite 事务。“文件先落盘，数据库后引用”意味着
崩溃可能产生未引用文件；扫描可报告孤儿，但本次不授权自动删除。
若引用文件缺失或损坏，依赖核验明确失败，不静默重下载替换或改用其他版本。

摘要兼容以固定代码 `1e270c648e7884fffd65ef13cfe9f47e0dab22ad` 的
`src/storage.rs::record_digest` 为依据：将完整原始 PublishedRecord JSON value
删除顶层 integrity_sha256 后，用该版本 serde_json::to_vec 序列化再取 SHA-256。
包含 version_id、published_at、validation、batch/provenance/coverage 及原 JSON 中存在的其他字段；
字段缺失与显式 null 不能通过重新构造领域对象消除。不能泛称为跨实现的 JSON canonicalization 标准。

首个 SQLite 行情实现采用兼容路径：保存原记录 JSON value 的无损表示和旧摘要，
格式标记为 legacy-published-record-v1（本次命名），按固定序列化行为核验；
逐行行情、单位、血缘和版本元数据必须与该已核验记录逐字段对应，防止只验证副本却返回被篡改的表行。
新发布仍构造完整兼容记录并生成同格式摘要，不能只覆盖 OHLC。
完整兼容记录包含该版本全部行情与元数据，会与类型化表重复占用空间；首期接受此成本，
不能称其为仅小型元数据。业务查询仍读取类型化行情表，兼容副本用于核验。

若后续引入 SQLite 逻辑摘要，必须另立 digest_format、完整字段清单和迁移任务；
保留旧记录/摘要、映射与核验证据，不将新摘要冒充旧格式兼容。
原本缺摘要、损坏或来源未验证的记录不能因导入后计算了新摘要就升级为 verified/published。
本次“沿用 #67”仅指完整性保护和拒绝语义，字节兼容必须按上述兼容路径证明。
SQLite integrity_check 只能检查数据库结构，不能证明行情或来源正确。

首期 verified read 按固定版本重算摘要并检查行数与元数据，不仅相信 published 标志。
一次读取的兼容记录核验、表投影对照及返回行必须来自同一个有界一致读快照，
不能先校验一个快照再返回另一个快照的行。每个续页重新核验固定版本；
不在等待用户翻页时保持长期数据库事务。
分页核验可能需要扫描整个版本，这与仅返回一页的 SQL 成本不同，必须单独测量。
不以无效的永久 verified 标记优化；若未来缓存，须定义能够检测后续变更/损坏的失效依据。
显式版本损坏、摘要缺失或类型不匹配都拒绝；默认读取也不自动跳到更旧版本冒充最新。

## 任务事务和故障恢复

受理在一个 `BEGIN IMMEDIATE` 事务内进行：读取 request_id、比较规范化参数及契约版本、
固化范围、检查全部冲突，再写 request/task/scopes/occupancy。
同请求同参数返回原结果及实际状态，异参按原规则拒绝；重复相同写项和无序调序
按既定集合语义规范化，不重复计数或占用。SQL UNIQUE 是最后一道竞争防线，不替代业务判断。
缺少本应存在的执行/绑定/占用证据时返回待核实，不重造任务或伪装 pending；
合法终态已释放占用不构成损坏，当前合法 running 也不无条件映射为待核实。
单次请求或任务读取内部一致，两个独立调用不承诺同一修订；查询不补写或修复记录。

网络下载、QFQ 计算和等待不可持有写事务。worker 领取任务和状态转换使用条件更新，
首期保留单写 worker 的运行约束。取消、重试、终态与释放条件复用现有用例；
重启对所有旧非终态（含 pending/running/cancel_requested）遵循待核实规则，
不自动重新执行或释放范围；accepted_run_id 与 owner_run_id 用于区分新受理及旧执行事实。
数据库自身事务能力不证明多 worker 执行、外部写入或恢复协议已经正确。
阶段 A 若将 SQLite 任务与旧 JSON 行情混装，只用于显式过渡测试，不能宣称两者原子提交；
阶段 B 的发布与任务输出关联必须在同一 SQLite 事务中验收。

| 故障点 | 可观察结果 / 恢复依据 |
|---|---|
| 受理事务提交前退出 | 无半条绑定；重发按实际持久状态判断 |
| 提交成功但响应丢失 | request_id 查询返回持久结果，不新增任务 |
| 计算或 staging 写入失败 | 旧 published 仍可读；新候选不可见 |
| 发布事务失败/磁盘满 | 回滚数据库变更；错误不冒充无数据 |
| 发布成功、任务响应丢失 | 用 task_publications 和版本证据核实，不重复发布 |
| SQLite busy | 有界等待后明确冲突/暂不可用；不无界重试、不假定提交失败 |
| 数据库不可读、摘要不匹配 | 明确存储/完整性错误，无旧 JSON 或空库 fallback |
| 重启留下旧非终态或应有占用证据缺失 | 保守保留冲突保护，进入既定核实路径；不误伤正常已释放终态 |
| 固定历史 base 完整但 daily head 前进 | 若请求明确为 historical-fixed-base，可发布绑定该 base 的不可变 yearly 版本，但不得推进 yearly effective head；不把“非最新”称为损坏 |
| current-head 派生提交前 daily head 前进 | 前置条件冲突；不发布为当前版本、不自动重绑新 daily head，也不静默降级成历史发布 |
| 提交前目标 yearly head 变化 | 前置条件冲突；不最后写入者胜出，不覆盖较新的默认 yearly |

### 年线固定基础与 effective head 规则（已收敛）

`published` 表示该版本本身通过发布门禁并成为不可变历史事实；`effective_heads` 表示普通未指定版本查询
采用的当前默认版本。两者不得混为一个决定。所有 yearly 版本都必须把
`yearly_lineage.base_daily_version_id` 固定到一个已发布且可核验的 QFQ daily version，发布后不改写。

应用用例在进入提交前必须明确是哪一种发布意图；以下名称是内部设计标签，不是本轮新增公开 MCP 字段：

| 发布意图 / 事实 | 处理 |
|---|---|
| base daily 不存在、损坏、口径/身份不匹配或不具发布资格 | 拒绝为基础版本不可用/完整性错误；不能归类为 head 冲突，也不能换用其他 base |
| `historical-fixed-base`：显式绑定的 historical daily 仍有效，但 daily effective head 已前进 | 允许发布新的 immutable yearly version，并保留固定 lineage；**不得**推进 yearly effective head |
| `current-head`：候选绑定的 expected daily head 在提交前已变化 | 返回前置条件冲突；本次不自动换 base、不重新计算、不把候选静默改成 historical 发布 |
| 任一需要推进 head 的提交发现 expected yearly head 已变化 | 返回前置条件冲突；候选不能覆盖新 head |
| `current-head` 的 expected daily head、expected yearly head、任务/占用/修订均仍匹配 | 在同一事务中发布版本、记录任务输出并推进 yearly effective head |

因此“base 不再 latest”本身既不是损坏，也不是推进默认头的授权。显式历史版本仍可通过版本列表和
指定 version_id 查询；普通未指定版本查询继续只跟随 effective head。若 `current-head` 提交因前置条件变化失败，
后续要基于新 daily head 生成当前 yearly，必须重新形成候选/决定；不能在原事务内自动 rebase。

## 查询契约与分析入口

保持 #68 的显式版本、日期区间、稳定排序、page_size 和实例内 cursor 规则。
首个请求解析一次默认版本并固定查询条件、查询时点；续页使用 keyset，
不重新读取 latest、不用 OFFSET 漂移。year 按自然年选择，不能用月份裁剪年度 OHLC。

版本列表在首个请求固定条目清单，不能仅用发布时间上界假定能恢复同一列表。
cursor 仅在当前实例有效，重启/淘汰/条件改变须明确重新查询。
不把每次用户翻页之间的等待保持为一个长期数据库读事务。
返回分页结束不证明历史完整；实际末交易日、发布时间、覆盖状态分开表达。

SQLite Adapter 必须支持范围读取，不能继续把整个 JSON 文件搬进内存后切片并宣称索引生效。
完整性核验的全版本扫描另计，保留上述诚实成本边界。

本人本机分析可用只读连接；重型或长时间研究优先使用一致性备份。
MCP 暴露类型化查询，不因采用 SQLite 就新增任意 SQL 执行工具。
查询不隐式初始化数据库、升级 schema、采集或修复数据。
只读验收以无业务表变更为准；WAL/SHM 的连接管理不是业务数据写入，不能宣称所有文件字节不变。

## 连接、备份与运维

设计采用本地文件、WAL、`foreign_keys=ON`（每个连接）、`synchronous=FULL`，
验证实际设置返回值；busy 超时有界且配置可见，具体默认值由实现说明。
保持一个业务写入者、多个只读连接。WAL 仍只有一个同时写入者，长读事务会影响 checkpoint；
监测 WAL 大小、busy、磁盘余量和备份结果，不预设无依据吞吐或延迟目标。
参见 [SQLite WAL](https://www.sqlite.org/wal.html)。实际耐久性还需目标文件系统故障测试。

备份使用 [SQLite Online Backup API](https://www.sqlite.org/backup.html) 生成一致性副本，
不能仅复制活动 .sqlite3 而遗漏 WAL。
从备份数据库枚举 raw_files 引用，复制对应不可变原始文件并验证哈希，
一起形成备份清单（schema、应用版本、数据库摘要、原始文件列表）。
备份与清理必须互斥或固定引用保护；首期无自动删除策略。

恢复到独立目录：校验备份清单、数据库结构/外键、行情摘要、文件哈希和版本引用，
以只读方式核对关键查询后再显式切换服务配置。失败保留原库；
不把恢复副本当作最新生产事实。备份周期、保留期、RPO/RTO 尚未确认，不写成已经保障。

## JSON 到 SQLite 的迁移

迁移工具的开发与实际导入分开验收；本设计提交不执行导入，也不授权迁移旧 x 仓库数据。

1. 核对现有后端与版本，暂停写入并制作一致性备份；新建独立 SQLite 候选库。
2. dry-run 列出任务/版本/原始引用的数量、重复、损坏和缺失；不可解释的记录隔离并报告。
3. 按稳定来源身份导入，保存迁移批次与源哈希，重复运行不得重复建版本或消费 request_id。
4. 旧数据缺摘要、来源或 lineage 时保留“未验证”，不能导入后自动升级为生产发布。
   待核实任务与保守占用必须保留，不能从缺失执行记录推断 Pending。
5. 比较规范化内容、ID、任务状态、未知值、版本关系以及固定版本查询结果；
   旧 verified API 不接受的记录，新 verified API 也不得放行。
6. 验证通过后显式切换后端配置，旧文件保留只读。选择 SQLite 后缺库/坏库即失败，
   不自动建空库或回退 JSON。
7. 若切换后尚无新写入，可停止服务恢复原配置；若已有新写入，须先保全新增事实并制定
   数据对账/恢复步骤，不能直接切回旧 JSON 丢失新请求和行情。

Schema 升级与业务查询分离，升级失败回滚；禁止普通查询自动迁移。
首期不做长期双写，避免两个后端互相矛盾。

## 实施顺序与验收

本设计可在来源门禁未通过时实现并用 fixture 验证；这不允许发布真实 QFQ。
已关闭 #68 的查询契约继续作为回归基线，后续 SQLite Adapter 运行相同契约测试。
首批实施归属为 #78 任务、#79 发布读取、#80 AI 查询（含最小目录 fixture 和进度入口）、
#81 首个计算；迁移、备份恢复、真实来源接线及运行交付仍是后续范围，不塞进首个任务。
实施任务应引用本设计与原业务 AC，不以表定义替代业务验收。

| 顺序 | 独立可验收结果 | 验证重点 |
|---|---|---|
| 1 | SQLite schema、迁移入口与原子任务受理 | 同 request 并发、异参拒绝、范围相交、崩溃后无半绑定、错误不回退 |
| 2 | 行情逐行存储及原子发布 | 半批不可见、失败保留旧版本、head 冲突、摘要/lineage、终态关联 |
| 3 | 固定版本 MCP 读取与分析示例 | 发布期间分页稳定、版本损坏拒绝、year 语义、行业快照、NULL 与精确小数往返 |
| 4 | JSON dry-run/导入和备份恢复工具 | 重复导入、缺摘要不升级、待核实占用、WAL 备份、引用文件缺失 |
| 5 | 真实来源接线和服务器验收 | #54/#6 来源证据、独立快照一致性、少量证券后全范围验证 |

测试至少包含：价格 `0.1` 与不同小数位往返、TEXT 字典序误用的防护、
大值超范围拒绝、未知量额、进程在受理/发布提交前后退出、SQLite busy/磁盘满、
坏库、缺失原文件、数据内容变更、基础日线修订与年线血缘。
使用真实 SQLite 临时库验证事务，Fake 仅用于应用编排。
执行 README 已有 fmt/clippy/test/diff 入口；文档提交只做文档与示例核对，
不得把这些未来测试写成已通过。

性能报告记录提交、SQLite 版本、主机/磁盘、证券数/交易日/版本数、库和 WAL 大小、
冷/热读取、并发、查询计划、写入耗时与完整性核验耗时。
先测量，再讨论目标；SQLite 选型不等待虚构规模证明，也不意味着容量已验收。

## 未决项与依赖

- #54：GBBQ 数值、单位、舍入、事件字段与 QFQ 基准，阻塞对应标准字段和真实计算发布。
- #6/#46：官方目录及研究行业二级映射，阻塞真实目录可用性。
- #71 官方文件获取/使用边界与独立快照一致性：继续按 production-source.md 及相应证据任务验证。
- 历史时点可见性、未来清理条件与权限、备份周期及性能阈值：仍未确认；首期不自动清理已发布版本已确认，不得据此降为可选，不推断无前视回测能力。
- 受保护远程 MCP、后台 worker 与服务重启恢复的具体装配：由运行设计承接，
  SQLite 本身不提供远程认证或后台执行。

截至本设计修订：#78 的 SQLite 通用任务阶段已由 PR #82 合并；本文件的阶段 B 行情表、发布、
verified read、迁移、真实性能与部署仍未由本设计提交验收。现有来源状态保持 NOT_PRODUCTION_READY。

## 通用任务模型实施入口

公共任务不固定为证券/周期。首批范围、通用 task_items/契约身份、状态与受理事务见
[整体设计 PR #76 的固定实施基线](https://github.com/liqiangcc/x-data-mcp/blob/2002518cad08493722473e2132d3e7f4a86518da/docs/implementation-baseline.md)。
来源/行情业务表仍各自类型化；目录 fixture 的最小持久化和 verified read 由 #80 承接，
真实文件目录契约由 #6/#46 负责，不能假定 #79 已实现目录或把 fixture 当成真实数据。
按阶段交付，不把全 schema 一次实现当成 #78 的完成条件。当前文档更新不构成开工或派发授权。
