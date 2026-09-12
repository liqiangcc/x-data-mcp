# MCP tools 契约

## 最新目标契约

[通用数据工具设计](generic-data-tools.md)是本轮目标权威：同类业务动作复用工具，以 dataset 和 contract_version 分派类型化参数。本文“当前八工具”及 schema 快照保留原样；后续章节的分类专用工具名称、组合示例与扩展策略属于此前草案，须按新设计的映射和兼容规则理解，不能与新目标并列注册为两套业务实现。

## 版本与阅读方式

设计任务 [#75](https://github.com/liqiangcc/x-data-mcp/issues/75)；
[整体设计](design.md)维护架构与业务流程。
当前实现核对基线：`b3fe2b3bae22d558c3f193e668773728a2e0df34`。
[当前八工具输入 schema 快照](contracts/mcp-tools-current.json)对应此提交的
`src/mcp.rs::public_tools`，仅供审查，不是运行时配置或未来工具注册表。

本文“当前”描述可调用原型；“目标/拟议”描述尚未实现的契约。
输入 schema、Serde 校验和业务规则并非完全一致，差距在文末列出。
未实现工具不能传给当前 tools/call，也不能因本文列名就宣称交付。

## 协议、权限和通用约定

当前 stdio 入口支持 initialize、tools/list、tools/call；
initialize 返回 protocolVersion=2025-06-18、serverInfo=x-data-mcp/0.1.0。
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

## 当前工具目录

| 工具 | 类别 | 当前作用 |
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
- Bar：date、open、high、low、close、volume、amount。
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

## 后续工具定义（拟议，当前不可调用）

下列名称与字段是本次可评审设计。它们承接已确认能力，并非新增任意管理权限。
注册前必须将这里的参数约束转成 inputSchema/outputSchema 和真实契约测试；
不能将未解决的来源算法或证据形式伪装成已经可用。

### 共享目标类型

- WriteAcceptance：request_id、operation、task_id:string|null、status、
  reused:boolean、frozen_scope、request_outcome:null|object。
  异步操作通常有 task_id；只允许既定请求级终态没有任务。
- TargetScope：显式证券列表和周期，或一个明确的动态目录选择器（二选一）；
  首次受理固化 catalog_version_id、成员、范围、默认值和 contract_version。
  动态选择器与刚好相同的显式集合不自动视为等价请求。
- TaskSnapshot：task_id、snapshot_id、snapshot_time、status、last_progress_at、
  progress_is_last_known、total/not_started/running/ended 计数、结果分类、
  unfinished_items、next_cursor、page_complete。计数应满足总量不变量。
- UnfinishedItem：security、period、execution_state、outcome、reason、
  retry_eligibility（可为 unknown），不把来源限制直接归类为可重试失败。
- VerificationConclusion：target、kind、checked_at、evidence_refs、outcome、
  retained_protections、state_change。outcome 需分别表述证据充分/不足和实际结论。
- Error：沿用 code/message，可增加结构化上下文；拟议错误类在实现前定稿，
  不能让客户端依赖本文尚未注册的 code 字符串。

所有异步写入口必填独立 request_id。普通前置拒绝不消费标识；
无需恢复、无任务受理失败与有界检查等例外按原用例持久绑定。
副作用结果不明先 get_request，不立即以新标识重复执行。

### refresh_catalog / get_catalog

| 工具 | 输入 | 输出 / 行为 |
|---|---|---|
| refresh_catalog | request_id | WriteAcceptance；异步获取官方目录，全部校验后发布，失败保留旧目录 |
| get_catalog | 可选 catalog_version_id、industry_code、page_size、cursor | 固定目录版本、来源/覆盖、证券成员、分类状态、分页信息 |

refresh_catalog 不允许用户传任意 URL 作为未经验证生产源。
get_catalog 只读，默认固定当前目录；指定版本不可用拒绝，不回退。
行业缺失/冲突与成员消失规则遵循 #6；查询当前目录不能声称历史行业有效期。
拒绝类别：无可用目录、版本不匹配/损坏、来源未验证、请求冲突/存储错误、
分页无效。目录分页采用与行情一致的固定快照原则，页上限在实现中明确。

### update_market_data / backfill_market_data

| 工具 | 输入 | 输出 / 行为 |
|---|---|---|
| update_market_data | request_id、scope:TargetScope | WriteAcceptance；更新已启动首次采集成员并尝试已确认缺口，随后重建受影响年线 |
| backfill_market_data | request_id、显式 securities、period、daily 日期区间或 yearly 年份集合（二选一） | WriteAcceptance；仅补指定范围或从固定 daily 重聚合指定年 |

日期范围不能包含尚未发生的写入日期；yearly 选择年度，不按月裁剪年度结果。
不在当前 start_collection 中偷偷接受这些尚不存在的字段。
update 默认动态范围固化；显式范围中的未启动证券拒绝或按既定整体受理规则说明，
不隐式调用首次采集。区间扩大、来源替换必须显式，不接受任意 fallback 配置。
无变化和元数据变化按 #4 判定，不承诺每次调用必建行情版本。

拒绝类别：范围冲突/未来范围/未初始化成员、来源门禁、不可证明 QFQ 基准一致、
缺口待确认、基础 daily 缺失、请求或存储结果待核实。
历史修订检查范围与“已启动”的证据定义仍需实现任务细化，不能宣称能检出全部修订。

### get_task_progress

输入 task_id，可选 page_size、cursor。输出 TaskSnapshot。
首次读取同一时点的状态、计数和未完成清单；按证券身份/周期稳定排序，
续页保持快照，即使后台已结束也不替换其中部分字段。
新查询获得新状态；cursor 失效明确重查。
“执行结束 100%”不是历史完整；取消/中断后保留实际计数，不把未执行项补成失败。
只读，查询和翻页不触发核实或释放。

采集/更新以证券 × 周期计数；核实/目录任务需各自定义工作单元，
TaskSnapshot 必须带 task_kind/progress_unit，不能强塞证券计数。
任务枚举是否新增 list_tasks 需单独用例，不为展示方便默认开放无限历史列表。

### 四类显式核实

| 工具 | 必填输入（均加 request_id） | 核实对象与完成条件 |
|---|---|---|
| verify_task_stopped | target_task_id | 查实际执行和写入证据；确认停止后安全收尾并按规则释放 |
| verify_publication_result | target_task_id，目标执行项标识 | 查候选、版本、head 与结果关系；不重新发布 |
| verify_restoration_result | target_task_id | 查原恢复任务与版本一致性/审计；不重新恢复 |
| verify_request_acceptance | target_request_id | 查请求、执行和完整占用；不凭缺记录重新受理 |

返回 WriteAcceptance，随后通过任务/请求查询读取 VerificationConclusion。
不能复用被核实原 request_id；同目标不同类核实串行，同类按 #3 重复/并发规则
复用或拒绝，不新增第二条竞争证据链。
核实允许针对原占用开展，但不能因受理就撤销原保护。
不适用状态、已有结论、证据不足须分别返回；已有结论的读取不重新执行。
具体证据定位由服务读取可信存储/运行信息，调用者传入的“已停止=true”不作为证据。

### restore_market_data_version

输入 request_id、exchange、code、period、version_id、backup_reference。
backup_reference 是服务登记的副本身份，不接受任意路径/URL 替代副本校验。
返回 WriteAcceptance；若可靠证据显示无需恢复，返回已持久化的请求级终态，
同 request_id 重发返回原结论，不重新检查。

异步恢复校验行情及所有关联说明相同后恢复原可读性，保留原 version_id、记录审计。
不能恢复则明确报告原版本不可用；若需重采，由调用者另发新任务创建新版本。
取消/结果不明须核实，不能把复制文件完成当作恢复已验收。
拒绝类别：副本无效/不一致、范围冲突、目标不适用、请求冲突、存储结果不明。

### inspect_verification_task

输入 request_id、verification_task_id。返回一次持久化检查记录：
request_id、verification_task_id、checked_at、check_outcome、target_status、
evidence_refs、retained_protections。
这是有界显式管理操作，不创建“核实的核实”任务。
只检查当前证据，不等待失联进程恢复、不循环等待；
预算耗尽或证据不足返回目标仍待核实并保留串行保护。

同标识同参数只读返回该次历史检查；要使用新证据再次检查需新 request_id。
确定结论只能在可靠持久化后返回；结果不明先查 request 记录。
不直接释放原业务占用、不复活原核实、不隐式发起替代核实。
时间预算、证据来源及同时处置保护需实现前定稿。

## 错误与兼容性设计

当前稳定可观察 code 见每个已实现工具，不把拟议错误直接当作现有实现。
目标区分：参数/权限拒绝、幂等冲突、范围冲突、来源限制、数据不可用、
结果待核实、存储暂不可用、内部错误。是否可重试不能只由 HTTP/RPC 状态判断。
无数据、无任务、无绑定均只在可靠读到对应事实时返回，不掩盖 I/O 故障。

新增可选字段也须检查旧客户端和 request_id 规范化兼容；
语义变化或状态扩展须明确契约版本与迁移，不用代码版本代替业务契约。
历史请求按首次绑定规则重放，不能用新默认值重新解释。
未来已实现工具才进入 tools/list；能力发现不暴露未实现或受保护不可用的管理入口。
是否新增 outputSchema/annotations 由实现任务按实际 MCP 版本核对，本次 JSON 文件仅输入快照。

## 当前差距与验收清单

| 差距 | 目标测试 |
|---|---|
| get_task 缺 execution 显示 pending | 残缺绑定返回待核实，读取不修复 |
| retry_task 复用固定返回 pending | 复用终态返回实际状态，不触发执行 |
| Running 无条件映射待核实 | 活跃 owner 与重启遗留区分，读取状态一致 |
| start_collection 证券 schema 过宽 | 显式身份校验与目录证明，未知字段拒绝 |
| 原型无完整进度/请求级终态 | 同时点计数+清单分页；请求终态不误报未绑定 |
| 同步 stdio 阻塞取消 | 独立 worker；断连不取消；重启不自动续跑 |
| 输入 schema 与运行时约束不同 | 空值、空数组、日期、嵌套额外字段逐项测试 |
| 输出无独立 schema 快照 | 真实 binary 锁定 Decimal/null、错误及年度响应 |
| 管理工具尚未实现 | 四类核实、恢复、有界处置与原范围保护 |

验证顺序：schema/示例静态校验 → 真实 stdio 工具发现与响应 → 用例与存储故障 →
授权客户端/后台生命周期 → 真实来源与服务器。
文档示例中的股票和任务 ID 仅说明形状，不是已采集行情、已存在任务或生产测试证据。
本次不改变当前八工具，不新增远程端口、不迁移、不部署。

## AI 可组合能力（目标契约补充，尚未实现）

本节收敛后续设计，不更改“当前八工具”的 schema 快照。
目标是让 AI 选择并组合明确业务操作，无需了解 SQL、文件布局和内部事务步骤。

### 统一身份和传递类型

| 类型 | 目标字段 | 组合用途 |
|---|---|---|
| SecurityRef | exchange:"sh"或"sz", code:六位 string | get_catalog 输出直接传采集/更新，不要求 AI 拼接 symbol |
| ScopeRef | security:SecurityRef, period:"day"或"year" | 进度未完成项直接传 retry |
| VersionRef | security:SecurityRef, period, adjustment:"qfq", version_id | 版本列表直接定位查询/核实目标 |
| TaskRef | task_id | 受理输出直接传进度/取消工具 |
| RequestRef | request_id | 超时或响应丢失后查询持久结果 |

这些对象的嵌套位置须由各目标 inputSchema 明确，不能要求 AI 从描述推断字段名。
scope、version 与 request 引用是定位信息，不是授权凭证。
未来写工具使用 securities:SecurityRef[]；现有 symbols:string[] 和 selected_scopes
保持其当前契约，切换需显式版本化及兼容处理，不能静默改变旧 request_id 的含义。
查询现有 exchange/code 可由 SecurityRef 无损展开；若后续改成嵌套对象，同样版本化。
当前 Bar/AnnualRecord 不强行合并：周期决定输出类型，客户端不猜测结构。

### 统一结果及操作建议

异步受理结果保留 WriteAcceptance，并增加明确的 operation/outcome。
outcome 区分 accepted、reused、request_terminal；任务成功只在任务结果中表达。
目标错误上下文包含：

- category：validation、conflict、source、unavailable、verification_required、
  storage、internal 之一；具体 code 继续标识细分原因。
- outcome_certainty：not_accepted、committed、unknown，避免把网络失败当业务失败。
- target：适用的请求/任务/版本引用。
- next_actions：当前证据下适用的操作建议，包含 tool、可安全预填的 arguments、
  reason、requires_explicit_request。无可确认动作时为空，并列出缺少的证据。
- retry_eligibility：allowed、not_allowed 或 unknown，附原因；不等于自动重试授权。

字段按错误场景提供，不能为了统一伪造未知引用或作出不可靠的提交判断。
next_actions 仅建议，不自动执行、不包含凭据、不决定权限；服务再次受理时
重新校验状态、范围与证据，快照中的 allowed 可能已经过期。
响应丢失时客户端没有错误体，因此提交前仍需自行保存 request_id。
错误字符串保留面向人解释，AI 不应从 message 提取标识或猜测下一步。

### 组合 A：目录选择 → 采集 → 进度 → 显式重试

这是目标调用流程，包含尚未实现工具，不是当前可运行脚本。

1. get_catalog 返回固定 catalog_version_id、SecurityRef 成员及分类/覆盖限制。
2. AI 显式选择成员，将 securities 和 periods 传给新版 start_collection，
   使用保存好的 request_id；受理返回冻结范围与 task_id。
   如果意图是“按目录快照采集”，请求还应绑定 catalog_version_id；
   显式成员请求与动态目录选择器不能因为结果相同而视为同一幂等操作。
3. 独立 worker 执行；AI 调 get_task_progress，固定一次 snapshot_id 读取所有页。
   普通交互不调用 run_worker_once 驱动后台。
4. 从 unfinished_items 选择 retry_eligibility=allowed 的 ScopeRef，
   在用户已授权的重试范围内调用新版 retry_task，传新 request_id 和原 task_id。
5. 重试前服务重新检查当前完成事实；已完成、被占用或不可重试项按规则拒绝，
   不仅相信旧快照。原任务保留原结果，新任务通过 retry_of 关联。

目录分页未读完时不得声称选中了全目录；任务执行 100% 不意味着行情覆盖完整。
来源限制、覆盖未知和普通执行失败分开，不把未完成清单作为自动重试队列。

### 组合 B：版本列表 → 固定查询 → 可复核研究

当前 list_market_data_versions 与 get_market_data 已支持核心链路：

1. 指定 exchange/code/period 查询版本列表，选择一个明确 version_id。
2. 将相同证券、周期和所选 version_id 传 get_market_data，另给闭日期区间。
3. 后续页重复原条件与 page_size，只替换 cursor；缓存失效时重新发起固定版本查询，
   不切到默认最新版本后接在旧页后面。
4. 记录实际 version_id、来源、单位、查询时点；跨证券再记录目录版本与每证券版本清单。

版本列表读完不表示来源历史完整。year 查询保留 base_daily_version_id 与计算 as_of，
不能拿当前日期改写旧年线完成性。固定版本损坏则停止该次组合并返回不可用。

### 组合 C：受理响应丢失 → 请求查询

1. AI 在调用前保存 request_id 与原参数；RPC id 不作持久恢复身份。
2. 响应丢失后用 get_request 查询该标识，不立即生成新标识重发。
3. 若已绑定任务，继续读取该 task_id；若为可靠请求级终态，返回该历史结论。
4. 若可靠判定未绑定，可用同一 request_id 和原参数重发，由幂等受理解决竞争。
5. 若存储不可读/受理结果待核实，保留 unknown；需要时显式发起
   verify_request_acceptance，不能推断未受理或释放占用。

第 3 步请求级终态与第 5 步核实仍属目标能力。当前 get_request 仅覆盖任务绑定。

### 组合 D：版本不可用 → 副本恢复 → 核实 → 原版本查询

显式版本查询失败后先解释原因；只有存在可识别可靠副本且恢复操作已获授权时，
才用 restore_market_data_version 创建独立任务。AI 不自行生成 backup_reference，
副本登记/发现由受保护的运维流程提供，缺少时明确阻塞。
结果不明通过 verify_restoration_result 核实，不重复恢复。
恢复证据确认一致后，仍以原 version_id 查询；副本不一致时原版本保持不可用。
若要重采，另行显式创建任务并生成新版本，不能冒充恢复成功。

### 组合验收

| 场景 | 预期 |
|---|---|
| 目录输出传新版采集 | SecurityRef 无字符串拼接，未知身份拒绝 |
| 未完成页传新版重试 | ScopeRef 无猜测转换，服务拒绝过期资格 |
| 列表传历史查询 | 版本、证券、周期一致；不匹配明确拒绝 |
| 调用成功但响应丢失 | 原 request_id 找到唯一事实，不重复任务 |
| 组合在任一步中断 | 已完成步骤保持事实，下次从持久引用恢复读取 |
| next_actions 已过期 | 服务再次校验并拒绝；不越权、不隐式扩大范围 |
| 非技术 AI 执行正常流程 | 无需调用 SQL、文件写入、head 切换或 worker 步进 |
| 核实/恢复前置条件不足 | 明确阻塞，不补造证据或自动执行替代操作 |

实现任务应以跨工具契约测试证明组合成立，同时分别验证单工具原子边界。
不能只测试每个工具单独返回 JSON 就宣称可组合。

## 后续数据类别的 MCP 扩展边界

用户已确认后续需要扩展不同数据能力。当前 start_collection 仍是行情专用契约，
不能现在加入未声明的 dataset、source 或任意 options 参数。

目标采用明确的业务工具加共享任务协议：
新类别扩展通用采集/查询工具的类型化 schema 分支，复用 request_id、task_id、进度读取、
取消和幂等机制。新增工具名称在对应 Issue 定稿；财务采集等仅作潜在例子，
不加入当前工具清单，也不承诺立即实现。

通用任务查询应返回 task_kind、contract_version、progress_unit；
retry 等公共控制通过原任务类型分派，按该类别验证 selected_items，
不能永远要求所有数据传 ScopeRef(security,period)。
当前 selected_scopes 字段保持原状，目标扩展需显式契约升级和兼容测试。

不同类别的版本使用带 dataset_kind 的类型化引用；查询响应记录类型明确，
不让 AI 从字段猜类别。能支持多个已批准来源的同类工具，可在对应版本引入显式
source_id；源选择进入幂等与血缘，不由 AI 传任意 URL 或在失败时自动换源。
工具发现只公布实际装配且可用的能力，未实现草案不能参与组合。

新增工具应回答：一个调用表达什么业务意图、受理/完成各代表什么、
有哪些前置条件、输出可传给哪个工具、失败后如何凭持久引用核对。
既有四条组合流程可复用控制方式，但其行情字段与业务规则不套到所有类别。
