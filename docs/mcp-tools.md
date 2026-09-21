# MCP tools 契约

## 版本与阅读方式

设计任务 [#75](https://github.com/liqiangcc/x-data-mcp/issues/75)；
[整体设计](design.md)维护架构与业务流程。
2026-09-21 当前部署分为两个 MCP server：release smoke 对 acquisition socket
验证 10 个工具，对 data socket 验证 20 个工具。工具数不是稳定业务契约；客户端必须以
运行实例的 `tools/list` 与 capability descriptor 为准。
运行时 `tools/list` 与 `get_data_capabilities` 是机器契约入口；本文不复制完整 schema。

下文“当前八工具”内容是提交 `b3fe2b3bae22d558c3f193e668773728a2e0df34` 的历史快照，
对应 [旧输入 schema](contracts/mcp-tools-current.json)，不再代表当前部署。通用工具目标与现状差距见
[通用工具设计](generic-data-tools.md)；跨两个 server 的每日同步边界、Skill 编排和当前禁止声明见
[每日同步说明](daily-sync.md)与 [Issue #260](https://github.com/liqiangcc/x-data-mcp/issues/260)。

#260 的 immutable generation、验证后激活、generation-bound update 和 capability
主体已实现，并在 ECS 完成 100-item 功能 smoke；全市场、容量和独立 Skill 会话 DoD
仍未完成。文档、Skill 或示例不能替代 generation consistency、runtime readiness，
也不能把 100-item 结果外推为 5,221-symbol PASS。

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

只读工具不采集、不重试、不修复、不升级 schema；分页缓存允许内存变化。
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

此前逐类别工具、共享结果和组合探索已移到 [历史草案](history/mcp-tools-proposals.md)，仅供追溯，不据此额外注册工具。当前兼容差距与首个任务验收统一见实施基线。
