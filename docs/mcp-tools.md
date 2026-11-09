# MCP tools 契约

## 版本与阅读方式

设计任务 [#75](https://github.com/liqiangcc/x-data-mcp/issues/75)；
[整体设计](design.md)维护架构与业务流程。
当前部署为双 MCP server：一个 `data_service` 进程服务三个 socket——`mcp.sock` 的 `ops` 入口
16 data tools（部署面 capability revision 为 `generic-data-v11`；含只读
`get_yearly_base_years`、`plan_market_update`/`run_market_update` 与 `refresh_exchange_sources`，
文本模式 `full`，market 单版本序列：version_id=内容 hex，latest-only），`research.sock` 的 `research` 入口
（只读研究面，工具数 17）、`maint.sock` 的 `maint` 入口（仅本机运维使用，不经隧道，工具数 9）；
acquire socket 提供 10 acquisition tools；隧道指 `research.sock`（aws-node fallback 上 2026-11-02 起；vm2-126 生产机无 tunnel，D-74）。
注册 37 data tools（三个入口；`ops` 16 / `research` 17 / `maint` 9）；已部署实例为 compact schema 27（M1 的 schema 22 +
M2 PR-1 的 023 + M2 PR-2 的 024 + M3 PR-1 的 025 + R2 的 026 + M3-2 PR-B 的 027）。
`market.daily` 前复权价的精度随 schema 22 起的 `qfq-exact-v1` 为**最多 6 位小数**
（精确有理数链、末次一次舍入，尾随零不补）；部署已到 schema 27，不再有 schema 21 的
2 位小数实例（历史数值可能与旧 2dp 在"恰好落在半分位"的 bar 上差 1 分——这不是回归，
是旧 2dp 本身依赖中间精度、不可跨实现复现，[issue-383 设计](tasks/issue-383-m1-research-data.md)
§4.3）。
**代码面（main）的 capability revision 已是 `generic-data-v12`（未部署，#607 PR-D 的 ETF 前复权读面；
部署面仍是 `generic-data-v11`）**：本修订不改 schema（仍 27）、不改工具签名与工具数，只改
`market.daily` + ETF + `adjustment=qfq` 的结果与 `adjustment_unavailable` 的 `details`，
详见 [generic-data-v12](contracts/generic-data-v12.md) 与下面的「ETF 前复权」条。部署版本、
generation、catalog 与行情数据量等机器事实只在
[STATUS](STATUS.md) 维护，本文不复述；工具数不是稳定业务契约；客户端必须以
运行实例的 `tools/list` 与 capability descriptor 为准。旧 strict Skill 会话遇
revision 不匹配即停止而非静默适配。

历史记录（preview.29 时期，非当前部署）：2026-09-29 explicit fresh rebuild 后
operator-path 激活过 generation `2e9183306d4292c96602185152770a5ef54fbc28d1c9c909d3297c9d9bfe70a8`，
发布 catalog 5,227 records、5,223 个 `market.daily` 与 5,223 个 `market.yearly`
versions；当时的 live discovery 为 `generic-data-v3`、20 data tools。
运行时 `tools/list` 与 `get_data_capabilities` 是机器契约入口；本文不复制完整 schema。

读路径有界化说明（XP-025 D，开发分支）：
- `list_tasks` 改为 SQL keyset 分页：不再物化全表快照，也不再有"超过 10000 个任务即失败"的硬上限。
  快照时间仍作为上界（首次调用之后创建的任务不会出现在后续页），排序（`created_at` 降序、`task_id` 升序）、
  `page_complete`、游标与授权绑定不变。**状态口径变化：** 过滤在首次调用时对全表求值的旧语义改为按
  每页读取时的任务状态逐页求值（页内展示与过滤依据一致，但跨页之间任务状态可能已变化）。
- `get_data_availability` 的 `related_tasks`（v5 语义：在途 + 最近 5 个已结束，附 `related_tasks_total` / `related_tasks_truncated`）改为按 `task_scopes.scope_key`（`["market",symbol,"day"|"daily"|"year"]`，其中 `daily` 是缺口核查任务使用的周期）
  关联所请求实体的任务，不再扫描、解析全部任务。此前按任务条目 `target.symbol` 关联；市场类任务两者一致。
  仅有 `target.task_id`、无 symbol 的任务（如 `task.verification` 的 `verify_stopped_writes`）此前也不会
  出现在实体的 `related_tasks` 中，现在同样不会。缺口核查任务（`daily`）仍会出现；publication / task verification 本来就没有 symbol 目标。
- 单个实体的每日与年线统计在查询范围覆盖整个已发布版本时直接取自版本行（committed tier：仍校验该行的已发布字段，
  年线的 lineage 取自 `yearly_lineage` 行并经行级绑定校验），不再遍历版本链、解码 bar，年线也不再逐 bar 重新聚合基础日线；
  子范围过滤仍走完整校验。该 tier 下不重复"年线 bar 与基础日线重新聚合一致"这一内容级检查（读取行情内容的 `query_data` 不受影响）。

下方八工具章节是提交 `b3fe2b3bae22d558c3f193e668773728a2e0df34` 的历史快照，
对应 [旧输入 schema](contracts/mcp-tools-b3fe2b3.json)（历史快照，文件名取自基线提交 `b3fe2b3`），不再代表当前部署。通用工具目标与现状差距见
[通用工具设计](generic-data-tools.md)；跨两个 server 的每日同步边界、Skill 编排和当前禁止声明见
[每日同步说明](daily-sync.md)与 [Issue #260](https://github.com/liqiangcc/x-data-mcp/issues/260)。

immutable generation、验证后激活、generation-bound update 和 capability 已实现，
全市场 collection/update 已经分批 MCP 编排取得真实 generation 证据
（历史：preview.26–28 连续验收：catalog 5,227 实体、daily 首采约 340s、yearly 约
900s、同代 no-change `update_data` 5,222 items 约 616s）。#259 已在
2026-09-28 owner re-baseline 后关闭：同代 no-change `update_data` 现行合同为
≤15min / ≤4KiB/item / `data_versions +0`，preview.27/schema20 实测
5,222 items / 616s / +18.5MB / 0 new versions PASS。剩余缺口：cross-generation
envelope 约 37min 仅为 measured baseline、真实 append/rewrite 待独立证据、
独立新会话 Skill 未验收；#294 为独立 admission 协议问题。文档、Skill 或示例不能
替代 generation consistency、runtime readiness。

## 协议、权限和通用约定

当前 stdio/socket 入口支持 initialize、tools/list、tools/call；
initialize 返回 protocolVersion=2025-06-18，`serverInfo.version` 由当前包版本生成。
wire 上 acquisition socket 的 `serverInfo.name` 仍为 `x-data-mcp`，而二进制/服务角色
称为 `x-data-acquire`；客户端应按已配置的 server alias 和工具集合区分，不按静态版本字符串猜测。
这是代码事实，不是最新协议符合性或完整协商的认证结论。
本次不升级协议；完整协商、生命周期及目标客户端矩阵由实现任务验证。

所有工具仅供授权本人及代理 AI；当前信任边界为受保护的本机进程，
没有 HTTP 认证实现。未来入口必须先验证权限，未授权时不披露任务/版本是否存在。
参数错误不得创建任务、占用 request_id、写行情或修复状态。
未知字段及不支持选项拒绝，嵌套对象同样适用。

当前工具调用成功与业务错误的外层结构：

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [{"type": "text", "text": "{\"request_id\":\"r1\",\"bound\":false,\"task\":null}"}],
    "structuredContent": {"request_id": "r1", "bound": false, "task": null},
    "isError": false
  }
}
```

业务错误使用相同 result 外壳，isError=true，
structuredContent 为 `{code:string,message:string,scopes?:Scope[]}`。
JSON 解析错误为 RPC -32700；不支持 method 为 -32601；缺少工具名为 -32602。
unknown_tool 当前属于工具错误。客户端按 code 分类，不匹配 message 文本。
SOURCE_NOT_VERIFIED 可能位于任务单项 error 字符串，而非顶层 API code。

数值契约：Bar 的 open/high/low/close 为精确 Decimal；volume/amount 可空。
目标客户端按十进制字符串处理，禁止经二进制浮点中转。
实际 wire 编码由当前 Cargo 配置和 Serde 决定，发布契约快照时须用真实二进制锁定，
不能只凭 Rust 类型断言所有客户端都正确处理。单位/币种证据不自动换算。

query_data 的 `version_manifest.metadata.adjustment` 对 market daily/yearly 明确返回
持久化的 price_adjustment evidence：当前官方发布为 confirmed `qfq`，OHLC 是 QFQ
值，volume/amount 是未复权原值；catalog 的 adjustment 明确为 null。原始未复权
daily 仍不是公开 dataset；不能据此宣称 historical_accurate。

只读工具不采集、不重试、不修复、不升级 schema；分页缓存允许内存变化。

`query_data` 的 v2 `selection.kind=universe`（`market.daily`/`market.index`）逐页返回：每页的
`entities`、`versions`、`columns` 只含本页行涉及的实体，单页字节与本页行数成比例；所有页合起来
得到与整体答案相同的行和版本。单快照 16 MiB 上限对这类读计的是列式答案的字节，不是内部快照的序列化。
细则见 [generic-data-v8](contracts/generic-data-v8.md) §4.1。`single`/`batch` 的响应不变。
写请求的 request_id 使用持久命名空间并包含操作类型/规范化参数语义；
不得拿 JSON-RPC id 代替业务 request_id。

## 历史八工具目录（不代表当前部署）

以下表格只描述上述旧 schema 快照。当前工具必须通过运行时 discovery 获取。

| 工具 | 类别 | 历史作用 |
|---|---|---|
| start_collection | 写 / 受理 | 冻结证券 × 周期并创建或复用任务 |
| get_task | 读 | 单任务状态与执行项结果 |
| run_worker_once | 写 / 执行 | 同步执行最多一个 worker step |
| cancel_task | 写 / 取消 | 申请取消，不等同停止已证实 |
| retry_task | 写 / 受理 | 对明确未完成子集创建关联任务 |
| get_market_data | 读 | 固定已发布版本行情分页 |
| list_market_data_versions | 读 | 固定历史版本列表分页 |
| get_request | 读 | 查询 request_id 持久绑定 |

### 共享返回类型

- Scope：`{symbol:string,period:"day"|"year"}`。
- ScopeResult：`{symbol,period,success:boolean,error:string|null}`。
- TaskView：`{task_id,status,frozen_range:Scope[],scope_results:ScopeResult[],retry_of:string|null}`。
- 当前 status：pending、running、cancel_requested、needs_verification、
  succeeded、partially_failed、failed、cancelled。暴露值受下述当前映射差距影响。
- EvidenceValue：`{status:"Confirmed"|"Unknown",value:string|null,reason:string}`；
  这是当前 Serde 大小写，不能擅自转成全小写。
- UnitCurrencyEvidence：price_currency、price_unit、volume_unit、amount_currency、
  amount_unit 五个 EvidenceValue。
- Bar：date、open、high、low、close、volume、amount；存储记录另含 `r7_raw`
  （.day 第 8 字段原始 u32，#240 内部证据字段，不在查询 wire 契约中，
  语义仅 A 股 1999–2010 区间锚定）。volume（股）/amount（元）自 #239 锚定后
  由官方快照源填充真实值，旧版本仍为 null。
- YearlyDerivation：base_daily_version_id、algorithm_version、
  first_trading_date、last_trading_date。
- AnnualRecord：`{bar:Bar,lineage:YearlyDerivation,year_unfinished:boolean}`。

### start_collection

> v8 当前形状是 `{request_id, dataset, contract_version, selection}`（见
> [generic-data-v8](contracts/generic-data-v8.md) §5.1 与 `get_data_capabilities`）。
> M3 PR-4 起 `market.daily` 另接受 v2
> `{"kind":"scope","scope":"sh_sz_a_listed_ever"}`：服务端从证券主表 ∩ 当前 generation
> 解析无 daily head 的沪深 A 股，预算 1..500，操作员一次性任务，不进日更 plan；
> 下列 legacy symbols/periods 形状仅作历史记录。

输入必填：request_id:string、symbols:string[]、periods:("day"|"year")[]。
无日期参数，无 adjustment 参数；当前固定 QFQ。集合不能为空。
目标证券格式使用 `sh.600000` / `sz.000001`，与查询显式身份一致；
当前受理 schema 仅声明 string，业务身份校验不足不能被视为支持任意字符串。

输出：`{task_id,status,frozen_range:Scope[],reused:boolean}`。
新受理返回 pending；同 request_id 等价参数复用已有任务和冻结范围，
异参 request_conflict。规范化集合排序/去重与保存契约版本参与判断，
不能将原请求重发变成重试。

副作用：原子持久绑定、执行记录、完整范围占用；不在受理响应中证明采集完成。
批量范围任一冲突整体拒绝。受理后仍需 worker，当前来源门禁将阻止真实发布。
year 是从已发布 QFQ daily 派生，无输入时失败；当前不通过查询/受理自动补基础日线。

主要错误：invalid_arguments、invalid_period、empty_request_id、empty_symbols、
empty_periods、request_conflict、range_conflict、storage_error。
ACCEPTANCE_NEEDS_VERIFICATION 当前可能包在 storage_error.message，不要虚构同名顶层 code。

```json
{"name":"start_collection","arguments":{"request_id":"collect-001","symbols":["sh.600000"],"periods":["day","year"]}}
```

### get_task

输入：task_id:string 必填。输出 TaskView。
只读；不存在 task_not_found，I/O 失败 storage_error。
目前不是完整任务快照接口：无 snapshot_time、计数和分页未完成清单。
实现把 Running 映射成 needs_verification，缺少 execution 时映射 pending；
不能依据这两个映射判断 worker 是否实际运行或绑定是否完整。

目标：读取单一一致快照，区分活跃 owner 与重启遗留，缺执行证据返回待核实，
不编造 pending。完整分页进度由后述 get_task_progress 承接。

### run_worker_once

输入为空对象 `{}`，额外字段拒绝。
输出：`{idle:boolean,task_id:string|null,status:string|null}`；
无可选任务时 idle=true 且两个值为 null，否则返回本次 step 的任务与状态。
错误：storage_error、execution_error、invalid_arguments。

此工具有副作用、同步阻塞，不可自动重放为幂等请求；它不接收 request_id，
重复调用可能处理另一个任务。超时后先查询已知任务状态，不盲目重复执行。
当前单连接逐行处理，不能承诺执行期间通过同一 stdio 连接及时取消。
部署目标将执行生命周期交由独立 worker；该工具保留为受控原型/运维入口，
与后台模式互斥或共用领取器。恢复任务的行为须另验，不能用描述文字代替恢复证据。

### cancel_task

输入：task_id:string 必填。
输出：`{task_id,status,scope_results:ScopeResult[],changed:boolean}`。
按目标任务状态重复安全；当前没有 request_id，重复取消使用任务状态保护。
pending 可取消；running 申请取消后需停止/核实，不能将申请响应解释成已停止。
终态不因再次取消改变；不存在 task_not_found，存储失败 storage_error。

目标后台取消与领取/发布使用事务条件更新。缺少停止证据保留占用；
不能以客户端断开、HTTP 超时或 changed=true 证明停止写入。

### retry_task

输入必填 original_task_id:string、new_request_id:string、
selected_scopes:非空 Scope[]（每项 symbol、period 必填，拒绝额外字段）。
输出：`{task_id,status,frozen_range:Scope[],retry_of:string|null,reused:boolean}`。

仅显式选择原范围内尚未完成且可重试的执行项，不自动选择全部失败、不复用未发布候选；
使用新 request_id，关联原任务，原任务不复活。受理前重新检查完成事实与范围冲突。
同 new_request_id 等价参数重放复用，异参冲突。
当前响应 status 固定 pending，即使 reused=true；目标须返回实际状态，
调用者目前应通过 get_task/get_request 核对，不能将该差距当成正确终态语义。

错误：task_not_found、retry_request_id_conflict、empty_selected_scopes、
invalid_retry_selection、request_conflict、range_conflict、invalid_period、
invalid_arguments、storage_error。

```json
{"name":"retry_task","arguments":{"original_task_id":"task-example","new_request_id":"retry-001","selected_scopes":[{"symbol":"sh.600000","period":"day"}]}}
```

### get_market_data

| 字段 | 类型与约束 |
|---|---|
| exchange | 必填 string，sh 或 sz |
| code | 必填 string，六位 ASCII 数字 |
| period | 必填 day 或 year |
| start_date / end_date | 必填 YYYY-MM-DD，合法日期、起点不晚于终点，年份 1..9999 |
| version_id | 可选非空 string；显式版本必须匹配证券/周期/QFQ |
| page_size | 可选 integer，1..1000，默认 100 |
| cursor | 可选 string；续页必须重复原条件及相同页大小 |

首次省略 version_id 选择最后提交的可核验版本；显式不存在/损坏均拒绝，无 fallback。
日线按闭日期区间升序；year 按起止自然年选完整年度，不按月切割 OHLC。

输出公共字段：symbol、period、adjustment="qfq"、query_time（上海时区偏移）、
start_date、end_date、coverage="UNKNOWN"、missing_data_reason="UNKNOWN"、
future_interval_not_occurred:boolean、version_id:string|null、
records、next_cursor:string|null、page_complete:boolean。

有版本时还返回 first_record_date、last_record_date、freshness="NOT_VERIFIED"、
source、published_at、unit_currency、adjustment_evidence。
这些元数据当前在无版本时可能缺字段，不是统一全部 null。
day 的 records 为 Bar[]；year 为 AnnualRecord[]，另有
derivation_as_of、derivation={kind:"local_aggregation",lineage,daily_source}。
年线当前仅接受 local-natural-year-v1，重算固定基础日线核验已发布年线，
不生成未发布数据。year_unfinished 相对 derivation_as_of，不因今天跨年自动改变。

无已发布版本且没有显式 version_id 时返回空 records、null 版本；
有版本但区间无记录仍保留该版本元数据。两者都不推断无交易原因。
future_interval_not_occurred 表示包含查询时点之后的日期，不说明历史缺口。
页结束只表示结果取完，不是 coverage Complete。

错误：invalid_arguments、invalid_cursor、published_data_unavailable；
内部 I/O/摘要错误在此映射为 published_data_unavailable。
副作用：无持久业务写入，只保存实例内分页状态。

```json
{"name":"get_market_data","arguments":{"exchange":"sh","code":"600000","period":"day","start_date":"2025-01-01","end_date":"2025-12-31","page_size":100}}
```

### list_market_data_versions

输入必填 exchange/code/period，与行情查询约束一致；
可选 page_size/cursor。没有日期和 version_id 输入字段。
输出 symbol、period、adjustment="qfq"、query_time、records、next_cursor、page_complete。
每条记录包含 version_id、published_at、source、record_count、yearly_derivation（可空）。

按 published_at 降序、version_id 升序打破并列；固定初次列表快照。
任一候选列表记录损坏时列表拒绝，不静默过滤；仍可显式查询其他完整历史版本。
错误同 get_market_data；列表为空不表示从未采集或来源不存在。

### 两类行情分页共同规则

cursor 为当前实例内不透明凭据，不是认证、长期书签或稳定 ID。
固定版本/列表、条件和 query_time；每页重核固定内容，损坏/同 ID 替换明确拒绝。
新版本不影响已有分页，条件变化或重启/淘汰后使用旧 cursor 返回 invalid_cursor。
当前最多缓存 64 个后续页状态，超过淘汰最早状态；这是资源上限，不是 TTL 或性能保证。
实现仍加载完整版本和分页行到内存，SQLite 迁移后改范围读取仍须维持核验语义。

### get_request

输入 request_id:string 必填且非空白。
输出：`{request_id,bound:boolean,task:TaskView|null}`。
未绑定为 bound=false/task=null；存在绑定则检查执行和占用完整性。
不完整返回 request_needs_verification；I/O 返回 storage_error，不能冒充未绑定。
只读，不消费 request_id，不补建任务。

当前仅表示任务绑定，尚不能完整返回“无需恢复”“无任务受理失败”或有界处置等
已消费标识的请求级终态。目标需版本化扩展 result_kind、operation、recorded_at 与
request_outcome，区分未绑定、任务绑定、请求级终态、结果待核实；不能用 task=null 混淆。

## 当前目标与历史草案

通用查询、任务和计算入口已有运行实现，但 capability 真实性、generation 生命周期和每日同步仍存在
#260 记录的阻塞。后续实现统一使用 [通用工具设计](generic-data-tools.md)、
[计算设计](compute-capabilities.md)、[每日同步说明](daily-sync.md)和[实施基线](implementation-baseline.md)。
本页以上仅为旧八工具的固定历史事实。

v7 增量（历史记录，代码已实现、部署日随二进制切换生效）：`plan_market_update`
/`run_market_update` 无状态计划+受理工作流，契约见
[generic-data-v8](contracts/generic-data-v8.md)，部署切换步骤见
[v7-switch-checklist](contracts/v7-switch-checklist.md)。

此前逐类别工具、共享结果和组合探索已移到 [历史草案](history/mcp-tools-proposals.md)，仅供追溯，不据此额外注册工具。当前兼容差距与首个任务验收统一见实施基线。

## 读取工具（preview.46 已部署）

D-75 / [#525](https://github.com/liqiangcc/x-data-mcp/issues/525)：在 `research` 入口新增窄输入的只读工具（PR-1 的 7 个，PR-1b 再加 `get_security_state`、`get_security_events`，PR-2 再加 `get_security_history`，共 10 个）。
**状态：已部署（preview.46，2026-11-07）。** 这 10 个工具已在 `research` 入口的 `tools/list` 里（线上实测 17 个工具）；部署面 `capability_revision` 为 `generic-data-v11`（契约见 [generic-data-v11](contracts/generic-data-v11.md)）。设计与裁决见 [issue-525-read-tools](tasks/issue-525-read-tools.md)。
`ops`、`maint` 入口不变。

每个工具把窄输入转成现有 `query_data` 的 v2 查询（`latest_per_entity`），由同一条读路径做校验、完整性检查和列式投影，
所以同样条件下 `columns`、`entities`、`versions` 与 `query_data` v2 完全一致，响应沿用 v2 信封和分页（`page_size` 1..1000，默认 100；`cursor`）。
输入类型拒绝多余字段，不暴露 `contract_version`、`version`、`selection`、`filter`。标的写作 `sh.600000`，日期 `YYYY-MM-DD`（也接受 `YYYYMMDD`）。

| 工具 | 必填 | 可选 | 对应的 `query_data` |
|---|---|---|---|
| `find_securities` | 无 | `query`（名称片段）、`code`、`industry_code`、`kind`（`stock` 默认/`etf`）、分页 | `kind=stock`：`securities.catalog`，范围现行沪深 A 股；`kind=etf`：没有对应的 `query_data` 请求，见下 |
| `get_daily_bars` | `securities`（1..100）、`start_date`、`end_date` | `adjustment`（`qfq`/`raw`）、`fields`、分页 | `market.daily`，`single`/`batch` |
| `get_yearly_bars` | `securities`（1..100）、`start_year`、`end_year` | `fields`、分页 | `market.yearly`，`single`/`batch` |
| `get_index_bars` | `indices`（登记的 9 条指数）、`start_date`、`end_date` | `fields`、分页 | `market.index`，`single`/`batch` |
| `get_market_snapshot` | `start_date`、`end_date` | `universe`（`a_current` 默认、`a_listed_ever`、`etf_current`、`etf_listed_ever`）、`adjustment`、`fields`、分页 | `market.daily`，`universe` + `cross_section` |
| `get_trading_calendar` | `start_date`、`end_date` | `page_size`（1..1000，默认 100）、`cursor` | `market.calendar`，`market` |
| `get_price_limit_rules` | 无 | `as_of`、分页 | `market.price_limit_rules`，`market` |
| `get_security_state` | `securities`（1..100）、`fact`（`security_master`/`security_names`/`risk_warnings`/`suspensions`/`industry`） | `as_of`、`known_at`、分页 | `fact` 对应的数据集（`market.security_master`、`market.security_names`、`market.risk_warnings`、`market.suspensions`、`market.industry_assignments`），`single`/`batch`，`filter={kind:daily,start_date:as_of,end_date:as_of,as_of}`，顶层 `known_at` |
| `get_security_history` | `securities`（1..100）、`fact`（`security_names`/`risk_warnings`/`suspensions`/`industry`）、`start_date`、`end_date` | `known_at`、分页 | 同上四个数据集，`single`/`batch`，内部 `filter.kind=history`（不应用 `as_of`；`query_data` 不能构造）|
| `get_security_events` | `securities`（1..100）、`event`（`capital`/`adjustment`）、`start_date`、`end_date` | 分页 | `market.capital_events` / `market.adjustment_events`，`single`/`batch`，`filter.kind=daily` |

语义，均沿用现有规则，没有放松：

- `find_securities`：缺省 `kind=stock`，与 `kind` 出现之前逐字相同（`columns`、`entities`、`versions`、`limitations` 都不变，与 `query_data` 的 `securities.catalog` 等价）。
  `kind=etf` 按名称或代码找 ETF，现行和退市都在内（退市的只能按代码或不带条件列出）。底层是 `market.security_names` 上的内部过滤 `Filter::FindEtf`（不可反序列化、不进 schema，`query_data` 构造不了，`filter.kind=find_etf` 返回 `invalid_arguments`），读取在 `sqlite_supply/m2.rs` 的 `find_etf`。
  - `query` 的归一化（D-76 Q8）：Unicode NFKC，去掉全部空白，转小写，对**最新名称**做子串匹配。`沪深300ETF`、`300etf`、`沪深３００ＥＴＦ`、`沪深 300 Etf` 命中同一批。不做拼音、不做模糊。去空白后为空的 `query` 返回 `invalid_arguments`。`code` 是六位精确匹配，和 `query` 同时给出时取交集。
  - 范围：主表里 `instrument_class=2` 的沪深行。`status=current` 与 `sh_sz_etf_current` 用同一个判断（`research::universe::is_current_etf`），否则 `delisted`。按代码排序，分页和游标与其他工具相同，游标绑定到发出它的请求。
  - 列（固定，没有 `fields`）：`name`、`name_basis`、`status`、`instrument_class`、`listing_date`、`delisting_date`、`name_observed_from`。`name_basis` 取 `observed_from_first_snapshot` 或 `unavailable`；没有名称时 `name` 为 `null`（不会是空串），不会按代码段、基金公司或 A 股名称去猜。`name_observed_from` 是**最新名称**首次被名单快照看到的日子，不是交易所的改名日。
  - 响应的 `limitations` 写明：名称历史从首次观测日开始，更早的改名不可得；`华夏300`、`广发500` 这类交易所短写法搜不到；退市 ETF 可能没有名称（`name: null`、`name_basis: unavailable`），用代码找；`delisted` 的含义。结果为 0 行时同样带这些说明。
  - `kind=etf` 不接受 `industry_code`（ETF 没有行业）；其他非法值（`ETF`、`all`、空串、非字符串）和多余字段都返回 `invalid_arguments`。ETF 名称只来自 PR-2 写入的 `exchange_snapshot` 行；`market.security_names` 与 `get_security_*` 对 ETF 的读取不变（仍不返回这些行，见 [etf-name-search](tasks/etf-name-search.md) §5.3）。
  - 退市 ETF 的名称（PR-4）未做：2026-11-03 之前退市的存量没有名称。
- `get_daily_bars`：A 股默认 `qfq`。ETF 不传 `adjustment` 返回 `adjustment_required_for_etf`；传 `qfq` 时按实体的核对门槛放行或返回 `adjustment_unavailable`（`details.reason` 说明原因，见下一条）；A 股与 ETF 混查时传 `raw`，全部返回不复权。
- ETF 前复权（`#607` PR-D，代码面 `generic-data-v12`，未部署）：放行时返回 `price_scale=6` 的前复权行（尾随零不补，如 `4.0449`），`volume`/`amount` 与不复权逐字节相同，`metadata.etf_checks_digest` 写明判定所用的核对记录集合；阻断时 `adjustment_unavailable` 的 `details` 为 `{reason, blocking_events, categories}`，`reason` 取 `etf_factor_unverified` / `etf_event_order_unverified` / `etf_event_category_unexpected` / `etf_factor_unparsable` / `etf_qfq_not_built`。阻断是实体级的，`versions` 与 `get_data_availability` 同样报这个错。`get_data_capabilities` 照实列出 ETF `qfq` 的 `series` 行（`default=false`）和 `algorithm_versions`（`qfq`、`etf_factor_checks`）。契约见 [generic-data-v12](contracts/generic-data-v12.md)。
- **前复权价的 ≤0 值（D-94，A 股与 ETF 都适用）**：减法前复权保留价差、不保留比例，早期价格可能 ≤0（ETF 如 `sh.510050`、`sh.510880`；A 股 747 只 / 777,958 根 bar，[#621](https://github.com/liqiangcc/x-data-mcp/issues/621)）。这类价格**照原值返回**，不阻断、不截断、不改成 0。因此**不要直接用前复权序列算收益率**（比例关系在 ≤0 的 bar 上不成立），要收益率时用不复权价 + 因子自行还原。见 [generic-data-v12 §4](contracts/generic-data-v12.md)。
  传入登记的指数（`index_registry`，含别名 `sz.399300`、`sz.399905`）返回 `invalid_arguments`，消息指向 `get_index_bars`；整批拒绝，不丢弃指数。
  `query_data` 查指数的行为不变，仍返回 `absent` 和 0 行。
- `get_yearly_bars`：只有 A 股，由前复权日线按自然年派生，没有 `adjustment` 参数。ETF 的 `versions[].status` 为 `absent`、0 行，不影响同批 A 股。
- `get_index_bars`：`indices` 只能是登记的 9 条（`sh.000001`、`sh.000016`、`sh.000300`、`sh.000688`、`sh.000852`、`sh.000905`、`sh.000906`、`sz.399001`、`sz.399006`）。
- `get_market_snapshot`：最多 5 个交易日，超过由共享的读路径报 `invalid_arguments`，封装层不绕过；ETF 范围必须传 `adjustment=raw`。
- `get_price_limit_rules`：不传 `as_of` 时取最近交易日（`last_trading_day_upto(today)`，`today` 为上海日）。该工具没有 `known_at`。
- `get_trading_calendar`：和其他工具一样分页。范围超过一页时响应是 `page_complete=false` 并带 `next_cursor`，把它作为 `cursor` 传回即取下一页，合并后与 `query_data` 一次取全的结果相同。
- `get_security_state`：问“`as_of` 那天是什么状态”，只返回当天生效的记录；区间结束日当天已不生效（结束日不含）。不传 `as_of` 时取最近交易日，并把这一天同时写进 `start_date`、`end_date` 和 `as_of`。`known_at` 不传表示用现在已知的全部信息，回测应传；被排除的行和 `known_at` 为空的行数在响应的 `temporal` 里。响应带 `temporal`，与 `query_data` 一致。一次调用只读一个 `fact`（一次查询、一个快照）。
  对沪市标的查 `security_names` 时，`limitations` 追加“name history covers SZ only (D-61)…”：名称历史只覆盖深市，0 行表示没有这段历史，不表示没有名称。混合批次里有沪市标的时同样追加；`query_data` 的响应不变。
- `get_security_history`：问“这段时间有过哪些状态”，返回与 `start_date..end_date` 有重叠的全部区间（多段 ST 全部返回，窗口外的不返回），没有 `as_of`，响应的 `temporal` 没有 `as_of` 键。`known_at` 照旧先应用：晚于它才知道的行和 `known_at` 为空的行被排除并计数。`fact` 不含 `security_master`（主表是每个证券的生命周期记录，不是状态序列）。沪市查 `security_names` 的 `limitations` 与 `get_security_state` 相同。细节见 [generic-data-v11](contracts/generic-data-v11.md) §2.1。
- `get_security_events`：问“这段时间发生了什么”，返回日期范围内的事件。这两个数据集没有可用的知晓时间，所以没有 `known_at`，也没有 `as_of`。
  `event=adjustment` 的响应带 `effective_day` 列（除权除息生效日，`YYYY-MM-DD`，与 `category`、`c1..c4` 按行对齐）；`event=capital` 本来就有。行仍按源顺序，不按日期排序，需要按日期排序时由调用方处理。`query_data` 读 `market.adjustment_events` 得到同一列（v11 起，见 [generic-data-v11](contracts/generic-data-v11.md) §6）。

默认 `as_of` 的“今天”由 `data_supply::query::today()` 给出，现在是上海日（`calendar::shanghai_today()`）。
此前它用 `chrono::Local::now()`，随主机时区变化：vm2-126 是上海时区、aws-node 是 UTC，UTC 16:00–24:00 之间两台机器的默认 `as_of` 会差一天。
`query_data` 的事实类数据集不传 `as_of` 时同样受益。

`research` 入口 `tools/list` 的实测大小（代码面，无 `outputSchema`）：19,264 B（17 个工具，D-82 PR-B 实测），CI 预算 19,600 B（owner 批准，D-82）；preview.46 线上字节数本轮未单独测量；
每个工具描述不超过 240 B。PR-2 起读工具的输入 schema 去掉根上的 `$schema` 和 `title`（约 80 B/工具），所以 `ops` 实测 29,872 B（预算 30,800）、`maint` 13,195 B（预算 14,500）也比之前小；
工具清单不变。字节数和测量方式见 [generic-data-v11](contracts/generic-data-v11.md) §3。

本批未包含：`screen_market` 暂不做（D-78：能写脚本的 pi 用 codemode 筛选；#295 保持开放，出现不能写脚本的客户端需求时再提）。

## 派生只读工具（preview.46 已部署）

D-82 / [#433](https://github.com/liqiangcc/x-data-mcp/issues/433) PR-B：`research` 入口新增 `get_tradability` 和 `get_market_cap`，薄封装 research 层的 `tradability`、`price_limit` 和 `market_cap` 函数。
**状态：已部署（preview.46，2026-11-07）。** 这两个工具已在 `research` 入口的 `tools/list` 里；部署面 `capability_revision` 为 `generic-data-v11`（契约见 [generic-data-v11](contracts/generic-data-v11.md)）。设计与裁决见 [issue-433-tradability-read](tasks/issue-433-tradability-read.md)。

两个工具接受 `securities`（1..100）、可选 `as_of`（默认最近交易日），不接受 `known_at`（输入表无知晓时间），不分页。
响应为 `layout=table`，每个请求的实体一行，包括不在主表、不是 A 股的代码（不静默丢行）；
有 `inputs`（输入数据集版本）、`algorithm_versions`、`temporal`、`limitations`，没有 `snapshot_id` 和 `next_cursor`。
复现靠输入版本加算法版本。

### get_tradability

返回每个 A 股在 `as_of` 的可交易状态和当日涨跌停价（若已知）。
响应列：`state`（`tradable`/`not_tradable`/`unknown`）、`reason`（原因码）、`suspension`（停牌区间字符串或 null）、`flags`（预留）、
`limit_state`（`known`/`unbounded`/`unknown`）、`limit_reason`、`limit_candidates`（候选规则，格式 `"500bp|1000bp"`）、`risk_status`、
`limit_basis`（涨跌停价的依据：`prev_close`/`official_r7`/`reference_price`）、`limit_up`、`limit_down`、`prev_close`（前收盘，不复权）、`prev_close_day`、`rule_id`。

- `limit_basis` 与 research 层 `price_limit_in` 的 `limit_basis` 逐行一致：`prev_close` 是无除权事件时的前收盘，`official_r7` 是 2010-09-14 前的官方参考价（`r7_raw`），`reference_price` 是按 gbbq 公式算出的参考价。
  没有涨跌停价时（`limit_state=unknown` 含 `ex_rights_unverified`、`unbounded`、不在主表、非 A 股、输入缺失）该列为 `null`，不借前收盘或其它标签填充。
  它与 `state` 相互独立：停牌日（`not_tradable`）若当日涨跌停价可推导，仍给出 basis——basis 描述当日涨跌停价，不表示订单能成交。
- `unknown` 不写成可交易。
- 指数代码报 `invalid_arguments` 并指向 `get_index_bars`。
- 传了 `known_at` 时显式报错（A 股主表行、停牌、规则等无知晓时间），消息指向 `get_security_state`。
- `algorithm_versions` 含 `tradability: "tradability-v1"` 和 `price_limit: "limit-v2"`。
- `temporal` 有 `as_of` 和 `known_at: null`、`datasets_without_known_at`。

示例：
```json
{"securities":["sz.000631","sh.600519"],"as_of":"2007-06-01"}
```

### get_market_cap

返回每个 A 股在 `as_of` 的市值（指定口径）。
输入必填 `scope`（`float_a`/`company_total`/`a_share_total`）。
响应列：`state`（`known`/`unknown`）、`reason`、`market_cap`（元，两位小数字符串）、`close`（不复权收盘）、`shares`（股）、
`share_event_day`、`share_event_category`、`chain_break_day`、`reconciliation`（恒 `not_available`）。

- `a_share_total` 恒 `unknown`（历史 A 股总股本未被接纳）。
- `company_total` 是 A+B+H 总股本，`limitations` 里写明。
- `reconciliation` 恒 `not_available`，不暗示做过对账。
- `algorithm_versions` 含 `market_cap: "market-cap-v1"`。

示例：
```json
{"securities":["sz.000002","sh.600028"],"scope":"float_a","as_of":"2026-11-05"}
```

`research` 入口 `tools/list` 的实测大小（本代码版本，无 `outputSchema`）：19,264 B（17 个工具），CI 预算 19,600 B（owner 批准，D-82）；
紧凑目录 `get_data_capabilities({})` 实测 2,790 B（测试上限 2,800 B）。加 `limit_basis` 列（#590）不改工具描述与输入 schema，两个数都不变（重新实测）。每个工具描述不超过 240 B。

一次 `get_tradability` 调用的实测耗时与内存（夹具：主表 7,400 行、停牌 72,000 行、日历 300 天、规则书；生产写入器构造）：
1 / 10 / 100 只分别约 207–266 / 159–204 / 258–312 ms；共享加载一次解码约 8.9 MiB 峰值堆，同夹具后续调用 0.0–0.1 MiB。
整表解码（约 200 ms）主导，单只边际成本不足 1 ms，所以设计里「单只约 1 秒」的估算是没有共享加载时的情形。
调用按 `derived_tools::READ_RESERVE`（16 MiB）向 `ReadBudget` 预留，超限返回 `memory_budget_exceeded`。
