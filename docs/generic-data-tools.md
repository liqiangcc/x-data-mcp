# 通用数据工具目标契约

## 决策与适用范围

2026-09-12 用户要求现在考虑扩展，并授权按“工具按业务动作稳定、数据类别通过类型化参数扩展”
开始设计。本设计承接 #75 / PR #76，是通用工具的目标契约。当前部署 preview.28
（`b88612b`）：data socket 注册 20 data tools，acquire socket 注册
10 acquisition tools，fresh init 库为 compact schema 20；通用查询、任务、
availability、compute、capability 和 generation-bound 数据路径已实现，全市场
collection/update 已通过分批 MCP 编排取得真实 generation 证据
（preview.26–28 连续验收）；
[#259](https://github.com/liqiangcc/x-data-mcp/issues/259) 已在 2026-09-28
owner re-baseline 后关闭——同代 no-change `update_data` 现行合同为 ≤15min /
≤4KiB/item / `data_versions +0`，preview.27/schema20 实测 5,222 items / 616s /
+18.5MB / 0 new versions PASS。剩余 DoD 缺口：cross-generation envelope 约
37min 仅为 measured baseline、真实 append/rewrite 待独立证据、独立新会话
Skill 验收；
[#294](https://github.com/liqiangcc/x-data-mcp/issues/294) 为独立 admission
协议问题（[#262](https://github.com/liqiangcc/x-data-mcp/issues/262) 残余项
同列）。

本文取代 `mcp-tools.md` 中“新数据类别通常新增采集/查询工具”的旧目标方案，但不作为运行时
readiness 来源。当前机器契约必须通过 `tools/list` / `get_data_capabilities` 读取；跨 server 编排见
[每日同步说明](daily-sync.md)。

数据业务规则仍来自 #3/#4/#5/#6。本设计调整入口和内部扩展契约，不扩大首期数据范围，
不批准新来源，不取消查询、更新、补采、核实、恢复等能力。
新增 dataset 通常只扩展已有工具的 schema 分支与处理器；真正不同的业务动作才新增工具。

本设计的 AI 数据供给扩展见末节：新增 get_data_availability、list_tasks，批量读取和目录筛选复用 query_data；帮助返回采用两级目录/详情。

## 工具集合与能力边界

| 目标工具 | 意图 | 通用输入要点 | 成功结果 |
|---|---|---|---|
| get_data_capabilities | 发现当前调用者可用的数据能力 | 可选 dataset、operation | 能力修订号、数据类型和操作契约 |
| start_collection | 首次建立指定数据 | dataset、contract_version、request_id、selection | WriteAcceptance |
| update_data | 刷新已有数据 | 同上 | WriteAcceptance |
| backfill_data | 补指定历史范围 | 同上，加类型化 range | WriteAcceptance |
| republish_data | 授权重发固定基线（#247 纯 enrichment：仅填未晋升字段或新增日期，已知值改/丢 fail-closed；仅显式 items，无 default selection） | 同 update_data | WriteAcceptance |
| query_data | 读取已发布数据 | dataset、contract_version、selection、filter、version_id?、page_size?、cursor? | DataPage |
| list_data_versions | 列出指定对象的版本 | dataset、contract_version、selection、page_size?、cursor? | VersionPage |
| get_data_availability | 检查已存版本与覆盖证据 | dataset、selection、range、分页 | AvailabilitySnapshot |
| list_tasks | 发现已有任务 | 类型/状态/时间过滤、分页 | TaskListSnapshot |
| get_request | 核对持久请求结果 | request_id | RequestResult |
| get_task / get_task_progress | 查询任务/固定进度快照 | task_id；进度支持分页 | TaskResult / TaskSnapshot |
| cancel_task | 申请取消 | task_id | CancellationResult |
| retry_task | 对原任务未完成项显式重试 | original_task_id、new_request_id、item_ids | WriteAcceptance |

任务 ID 已能定位 dataset、操作和版本，控制工具不要求调用者重复传这些字段。
item_ids 是原任务中稳定执行项 ID，替代未来对所有类别强制使用证券/周期结构；
查询返回其业务定位和资格，受理时重新检查实际状态。旧 selected_scopes 走兼容路径。

四类 verify_*、restore_market_data_version、inspect_verification_task 的业务边界不合并进
start_collection。恢复的目标名称调整为 restore_data_version，接收 DatasetVersionRef；
其余核实工具优先用原任务/请求定位类型，由对应处理器提供证据检查。
run_worker_once 保留为 operator/debug 运维能力，不进入正常 AI 采集组合；长期服务由后台 worker 执行任务。

跨工具编排不进入 Tool/Capability 输出。Domain 和单一用例只返回当前动作的
`state/outcome/certainty/refs/evidence/reason_codes`；不返回 `next_actions`、其他工具名或 Skill 内容。
工作流顺序、分支、request_id 策略、轮询和恢复由项目级
`.devin/skills/x-data-daily-sync/SKILL.md` 负责。Skill 不得重定义业务 PASS，也不得通过 shell/SSH/SQLite
绕过 MCP。Skill 分发和 capability revision 握手见[每日同步说明](daily-sync.md)及 #260。

## Dataset 的含义与首期矩阵

dataset 是稳定业务数据契约身份，不是供应者名称、文件扩展名或数据库表名。

| dataset | 数据含义 | collect | update | backfill | query / versions |
|---|---|---|---|---|---|
| securities.catalog | 当前沪深 A 股与研究行业二级目录快照 | 首次建立目录 | 显式刷新目录 | 不支持 | 固定目录版本 |
| market.daily | 标准 QFQ 日线 | 官方文件处理生成历史日线 | 更新/修订检查 | 显式交易日期区间 | 单证券日线版本 |
| market.yearly | 固定已发布 daily 本地派生年线 | 派生指定范围年线 | 从明确基础版本重新派生 | 指定自然年集合 | 单证券年线版本 |

这些是目标支持矩阵。现有程序已注册同名通用入口，但部分工具仍共用过宽的 range schema，
不能因名称存在就认定每个 dataset/operation 分支已正确交付。能力发现必须同时区分 implemented、
runtime readiness 与 governance readiness，不把有 handler 等同当前可执行；未满足 generation/source
前置条件时必须返回结构化 blocked 原因，执行仍须拒绝。
不列出尚未开发的财务等类别为 supported。

.day 与 gbbq 是日线处理的内部原始输入，不要求 AI 分别采集再手动发布。
raw daily / 公司行动未来如需成为对外数据产品，需单独定义 dataset 和查询需求，
不因内部已有表就自动暴露。
目录刷新映射到 update_data(dataset=securities.catalog)，保留原刷新失败保护；
不得机械套用行情的“已有证券”选择规则。

## 类型化参数与 schema

每个数据操作均有具体 (dataset, operation, contract_version) 契约。
工具 inputSchema 使用封闭的 oneOf 分支，dataset 和 contract_version 用 const 判别，
每层 additionalProperties=false；分支仅包含该操作实际支持的 dataset。
不使用任意 options、自由 SQL、任意 URL 或无约束 filter。

公共字段只共享结构；selection/filter/range/输出记录由分支准确声明。
首期举例：

- catalog 的 selection 为 scope=current_sh_sz_a；查询 filter 可含 industry_code。
- daily 写选择 explicit securities 或 frozen catalog selector（二选一）；
  查询选择单个 SecurityRef，filter 为闭日期区间。
- yearly 写选择证券及明确 base_daily_versions，range 使用年份集合；
  查询按起止自然年取完整年度，不用月份裁剪 OHLC。
- 未明确的动态默认值不能写入 schema 假装已经实现；有效目录选择器必须有
  明确标记，与显式证券集合保持不同的幂等语义。
- 同一请求只处理一种 dataset。组合多类别由 AI 显式发多次请求；
  后续若需要跨类别工作流，应独立定义依赖/失败语义，不能增加隐式万能批次。

[采集输入 schema 示例](contracts/generic-start-collection-v1.json)给出日线与目录两个闭合分支，
是目标 schema 的可审查切片，并非完整工具注册表。yearly、update/backfill/query 分支须在
实现对应能力前完整补齐，不能只靠 capability 文本绕过 inputSchema。

契约版本 v1 只在本设计目标空间有效，不替换当前 task-acceptance-v1。
现有行情 tool 的 symbols/periods 与此目标 schema 不兼容，不能直接替换而不迁移。

## 能力发现

get_data_capabilities 输入为空对象或精确 dataset/operation 过滤，不产生任务、文件初始化或
数据库迁移副作用。返回完整有界描述；数据集增多到需要分页时另行版本化，
当前不能截断却声称列全。只披露已授权调用者的能力。

每个能力项至少包含：

- tool_name、dataset、contract_version、implemented、operator_only；
- runtime_readiness 与 governance_readiness（ready/blocked）分开表达：
  前者反映当前运行事实（已安装 inspected source、catalog 输入位置），
  后者反映授权/一致性/语义证据门槛；
- blocking_reasons：结构化 {code, detail, evidence_ref?}，不包含密钥或工作流指令；
- declared_scope 与 active_generation_ref（generation 契约落地前为 null）；
- 完整 input_schema、output_schema，以及 schema_digest；
- supported_sources、默认来源规则、数值/时间/覆盖语义；
- selection_modes、page_size 上限、progress_unit、取消/重试是否支持。

顶层同时返回 capability_revision、declared_scope、production_ready 与
production_blocking_reasons（同样结构化）。任何 capability 不返回 next_actions、
其他工具名、Skill 引用或跨 server 编排指令。

能力目录、tools/list schema 和路由由同一静态 Rust 注册描述生成，
不能维护三份手写且不一致的能力表。本文 JSON 是设计样例，不是第二份生产配置。
启动检查：schema 分支必须有处理器，每个处理器必须有准确 schema/输出契约。
源状态可以变化，但已绑定请求的 schema/规范化规则不能随能力修订号变化而重新解释。

tools/list 本身必须足够严格；能力发现用于补充详细业务语义，不能要求 AI 先读说明
才能知道一个无约束 options 对象应该怎么传。
扩展工具 schema 后的客户端缓存刷新和通知取决于采用的 MCP 协议及客户端能力，
须通过目标客户端矩阵验证；未验证时通过新会话重新发现，不假定客户端自动刷新。

## 公共输出与组合

WriteAcceptance：request_id、task_id（允许既定请求级终态为 null）、operation、dataset、
contract_version、outcome、reused、frozen_selection、request_outcome。
受理不等于采集/发布完成。任务结果持久记录每个 item_id、阶段、结果、原因、
可用版本引用和 retry_eligibility。

DatasetVersionRef：dataset、contract_version、entity（由 dataset 定义）、version_id。
DataPage：dataset、contract_version、record_schema_id、version_ref、query_time、
records（分支类型）、coverage、source/units/lineage、next_cursor、page_complete。
无版本与“有版本但过滤后为空”须可区分；未知仍为 unknown/null，不伪造完整性。

版本列表返回可直接供 query_data 选择的 version_ref；当前单对象查询不扩展成
未经定义的跨市场快照。跨对象研究保存版本清单与目录版本。
进度返回 item_id，retry_task 使用原 ID，不要求 AI 重建业务键。
cursor 固定 dataset、契约、身份、过滤、页大小与版本/快照；
跨 dataset 使用、契约变化、失效均拒绝，不切换数据。

错误沿用结构化类别、outcome_certainty、target、state、稳定 refs 与 evidence/reason codes；
新错误至少区分 unknown_dataset、unsupported_operation、unsupported_contract、
invalid_selection、source_blocked、request_conflict 和 outcome_unknown。
错误不返回其他工具名或跨动作建议；Skill 根据结构化事实编排。具体 wire code 在实现时冻结，
错误不能消耗普通未受理请求标识。
不支持操作明确拒绝，不自动变成 start_collection 或其他来源。

## 处理器与控制分离

注册键为 (dataset, operation, contract_version)，静态装配即可，不引入动态插件平台。

| 部分 | 职责 |
|---|---|
| MCP Adapter | schema 形状验证、权限上下文、协议映射 |
| 契约/命令解码 | 分支类型解析、规范化、旧契约兼容 |
| 数据用例处理器 | 业务选择/冻结范围、规则调用、采集与发布编排 |
| 领域策略 | 冲突、完整性、可重试、发布资格等纯判断 |
| 公共任务执行器 | 持久任务领取、执行上下文、取消、进度与结果持久化 |
| 来源/存储 Adapter | 具体 I/O、条件事务和数据映射 |

handler 不把自由 JSON 一路传给领域层；验证后形成 Rust 类型化命令。
worker 不 switch 各种文件/数据字段；按持久注册键分派，未知版本进入明确不可执行状态，
保留保护，不按最新 handler 猜测解释。
注册共同能力不意味着所有 dataset 支持所有动作；比如 catalog backfill 应在 schema 和路由同时拒绝。

幂等命名空间仍全局持久，规范化内容含 operation、dataset、contract_version、selection、
来源选择与所有影响结果的参数；同 request_id 不同 dataset 应冲突。
旧请求使用原兼容契约，不能仅加新字段就对旧请求重新执行。
恢复与核实所需证据由相应用例处理器提供，公共执行器不决定什么证据算充分。

## SQLite 公共任务模型修正

SQLite 专项的行情表保持行情专用；公共任务表不得固定 Scope(symbol,period)。
实现前补足：

- requests：operation、dataset、contract_version、canonical_payload/hash、binding/outcome；
- tasks：同一契约身份、owner/run 身份、阶段、结果与最后已知进度时间；
- task_items：task_id + item_id、类型化业务键、progress_unit、阶段/结果、冻结输入引用；
- task_scopes/occupancy：dataset/资源身份、规范化业务范围、占用 owner 与修订号；
- task_publications：带 dataset 的版本引用，由对应类型化发布关系或受约束关联实现。

公用命令/范围序列化可带判别字段，但不能把行情业务查询退化成 JSON 全表扫描。
不同类别的范围冲突由领域语义定义；同名证券不当然冲突，共享资源也不能因 dataset 不同就绕过冲突。
多态版本关联不能仅存无法验证的字符串：Adapter/DDL 明确目标表及存在性约束，
实现 PR 提交可验证的映射方案，不宣称 SQLite 自带跨表多态外键。
原子受理和发布仍由公共事务能力保护，不能拆为 AI 多次调用。

## 现有工具迁移映射

| 当前/旧目标 | 新目标 | 兼容要求 |
|---|---|---|
| start_collection(symbols,periods) | start_collection(dataset,selection) | 不兼容升级；旧请求按旧契约重放 |
| get_market_data | query_data | 保留日期、年度、固定版本和错误语义 |
| list_market_data_versions | list_data_versions | 不跳过坏版本，保留固定列表快照 |
| refresh_catalog / get_catalog（旧拟议） | update_data或start_collection / query_data，dataset=catalog | 保留首次失败与刷新保旧区别 |
| update_market_data / backfill_market_data（旧拟议） | update_data / backfill_data | 保留更新对象、修订和范围保护 |
| restore_market_data_version（旧拟议） | restore_data_version | 相同可靠副本与原 ID 规则 |

现有 start_collection 可一次选 day/year，新目标一次一个 dataset。
兼容层不得把一个旧 request_id 拆成两个无关联新请求；必须保留原任务的冻结项与幂等结果。
兼容实现可维持旧复合命令处理器，新客户端明确先采 daily、再固定 published daily 派生 yearly。
这一组合不是跨调用事务，daily 成功/yearly 失败时分别报告。

迁移方案：在明确的下一契约版本中发布通用入口；旧工具可临时作为兼容 Adapter 指向相同用例。
同名 start_collection 过渡时使用互斥 legacy/new schema 分支，严禁混合两套字段；
能力发现明确说明。下线旧分支需独立兼容评估，不在本次删除或重命名运行工具。

## 示例与验收

示例仅为目标请求，不可用于当前生产：

```json
{"request_id":"daily-001","dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[{"exchange":"sh","code":"600000"}]}}
```

```json
{"request_id":"catalog-001","dataset":"securities.catalog","contract_version":"v1","selection":{"scope":"current_sh_sz_a"}}
```

两个请求调用同一 start_collection，由 schema 分支与处理器明确区分。
新增财务类别时增加注册描述、类型化 schema、领域/来源/表及测试，通常不增加 tool 名称；
不是只加 enum 字符串就完成业务实现。

验收包括：支持矩阵与 tools/list/capabilities/handler 一致；不同 dataset 错参拒绝且无写入；
未知契约拒绝；同 request 跨 dataset 冲突；旧请求重放；稳定 item_id 重试；
跨 dataset cursor 拒绝；来源 blocked 不发布；客户端重新发现新增分支；
新增测试类别无需修改 worker 调度逻辑且旧行情回归通过。
文档阶段只能验证示例形状与设计一致性，不宣称上述实现测试已通过。

## AI 分析数据供给补充设计

依据：用户明确主要场景为“给 AI 分析提供数据和采集数据”，并要求下一步推进。
本节新增两个目标工具并扩展 query_data；export_data 仅列后续候选，不加入首期验收。
这些是本次接口设计决定，尚未实现；此前单对象查询限制按本节批量规则扩展。

### get_data_availability：先判断有什么数据

只读输入：dataset、contract_version、selection、range，可选 page_size/cursor。
selection 和 range 使用该 dataset 的闭合 schema；daily 为证券选择与闭日期区间，
yearly 为证券选择与自然年范围，catalog 为目录范围，不接受行情日期伪装目录生效期。

响应包含 snapshot_id、snapshot_time、dataset、contract_version、
分页 items、next_cursor、page_complete。每个 item 包含：

| 字段 | 含义 |
|---|---|
| entity、version_ref | 标准业务对象及本次解析的已发布版本；无版本明确 null |
| availability | available、partial、absent、unknown、unavailable；以请求范围为判断对象 |
| first_record / last_record、record_count | 固定版本实际保存边界/条数，不冒充完整历史 |
| coverage_status、coverage_evidence | 范围覆盖结论及依据；没有证据就是 unknown |
| confirmed_gaps | 有证据的缺失范围，含原因与证据引用；不能从不连续日期推断 |
| unresolved_ranges | 无法判断的范围，不能自动转成补采计划 |
| published_at、freshness | 发布时间与业务新鲜度分别表达；未定义规则则 freshness=unknown |
| related_tasks | 本快照中相关任务引用和已知占用，不能当作保证会补齐 |
| limitations、reason_codes | 来源限制和结构化事实；不返回跨工具编排建议 |

available/partial 必须有相应覆盖证据，不能仅凭有行数判定。absent 仅表示可靠读到
没有已发布版本；有版本但请求区间无记录且原因未知应为 unknown。
坏版本为 unavailable，不能作为 absent 重新采集；全局存储故障返回工具错误。
若只能获取元数据而未完成内容核验，明确 integrity=not_verified，不能声称版本可读。

版本选择、任务引用与覆盖说明在一次服务读快照解析；不在查询中实时访问来源。
分页冻结全部结果，不随采集推进替换部分字段；新查询才反映新数据。
仅依据日期包含未来不能制造历史缺口，未来范围单独标记。
availability 只返回事实和限制，不建议或触发采集/补采；是否发起写调用由 Skill/用户策略决定，
受理时仍重新核对冲突和证据。

### list_tasks：发现已有执行，避免盲目重复采集

只读输入可选 dataset、operation、status[]、created_from/created_to、page_size、cursor，
所有字段严格校验；不支持任意条件表达式或 SQL。
返回 snapshot_id、snapshot_time、items、next_cursor、page_complete。
item 含 task_id、request_id、dataset、operation、contract_version、status、
created_at、last_progress_at、简要结果；详细范围/未完成项通过 get_task_progress 查询。
版本化旧复合任务可用 legacy task_kind 明确表达，不能伪造单 dataset。

排序 created_at 降序、task_id 升序打破并列，后续页固定初次快照。
授权过滤先于枚举、计数和分页；不可泄露其他调用者无权读取的任务。
列表为空不等同某 request_id 未受理，确定请求事实仍使用 get_request。
不启动任务、不取消或修复；未知过滤值拒绝，不静默忽略。
page_size 默认 100、最大 1000 为本设计工程上限，不是性能承诺；
cursor 采用实例内有界快照策略，具体资源上限由能力发现公开，不隐含永久保留。

### query_data：批量多对象读取

同一 dataset 内增加 single/batch 两个互斥 schema 分支。
batch selection 为非空、无重复的标准 entity 列表；目录动态条件先固定成员快照。
filter 对所有对象共享，不能在自由 options 中传任意每对象参数。
目标批量对象上限初定 100，作为可调整的工程限制在 capabilities 中公开并测试。
超出明确拒绝，不能悄悄截断；不同 dataset 仍需分别查询。

版本选择为 latest_per_entity 或 explicit_version_map，二选一；
explicit map 必须与所选对象精确对应，无缺项、重复或额外项。
首个请求在同一个存储读快照中解析各对象版本，返回固定 version_manifest：
每对象版本引用、元数据或明确不可用状态及原因。
该快照保证“选中了哪些版本”的一致读取，不证明所有来源在同一市场时点发布。

输出记录按规范业务对象键、dataset 内记录键升序排列：
行情为 exchange/code/date 或 year；其他类别由契约声明稳定顺序。
每条记录带 entity 与 version_ref，数字保留精确类型。
一个 cursor 固定选择、过滤、契约、排序、页大小、版本清单及每对象状态，
不为每只股票返回独立且可混用的 cursor。page_size 限制每页记录数，不是对象数。

目标首期批量采用严格错误策略：任一所选已有版本损坏、指定版本不存在/不匹配、
存储不可读，整体拒绝，不返回看似完整的部分成功批次；
默认 latest 下可靠判定某对象尚无版本，可在 manifest 显式 absent 并返回其他对象数据。
范围内无记录仍保留该对象版本与限制。manifest 的 absent/空范围不能证明历史不存在。
续页任一固定版本不可核验即停止；不自动移除该对象后继续其他页。

版本 manifest 是独立响应元数据，不受记录分页裁剪；上限随批量对象数有界。
响应标注 selection_complete（每个对象都有明确状态）与 page_complete，
二者均不等于 coverage complete。cursor 失效时重新固定同一 explicit map 开始，
不得把新 latest 结果接到旧页后面。

### 目录筛选：复用 query_data

dataset=securities.catalog 的 filter 可声明 code、name_query、industry_code；
字段组合采用 AND，名称首期按字面子串匹配，不做正则或模糊身份猜测。
筛选基于一个明确目录版本，结果返回 SecurityRef、名称、分类状态和 catalog_version_id。
行业缺失不伪造为“其他”；查询不到不能推断证券退市。
命中多项由 AI 显式选择，不能自动取第一项采集。
schema、大小写/规范化和排序规则在实现契约中固化，绑定请求保留原选择语义。

### 帮助与能力发现的两级返回

get_data_capabilities 无过滤参数时返回简洁 dataset/operation 目录与 readiness，
不重复发送所有完整 schema；指定 dataset + operation 后返回详细参数定义、
最小调用示例、常见拒绝、前置条件、结果解释和可组合的后续操作。
只指定 dataset 时返回该类别操作目录。此规则替代此前无参数返回全部详细描述的目标。
详细 schema、示例及 handler 由同一注册描述校验；工具基础描述仍须足够清楚，
不强制每次调用前先查帮助。来源门禁明确显示，不用示例暗示生产已经可用。

### export_data：后续候选

当分页数据不适合进入 AI 上下文时，再交付固定版本导出任务：
固定 dataset、版本清单、过滤与格式，生成文件及行数/哈希/精度/来源清单。
应复用 task/request 机制；读源数据但会创建导出产物，不能标为完全无副作用。
访问授权、文件可获取方式、过期清理、配额、Decimal 编码和格式尚需单独设计。
本次不新增下载端点、不开放任意服务器路径，也不宣称文件导出可用。

### 正常 AI 组合与验收

1. get_data_capabilities 发现类别与调用方式。
2. query_data(catalog) 固定目录并选择证券。
3. get_data_availability 检查已存版本、覆盖证据及相关任务。
4. 有可用版本则批量 query_data；确需采集且已获授权才显式发写请求。
5. 丢失 task_id 时用 list_tasks 发现任务；丢失受理响应用 get_request 精确核对。
6. get_task_progress 获取结果后，以发布版本清单开始研究；不把任务执行成功代替覆盖证据。

新增验收：未知缺口不生成虚假补采建议；坏库不返回 absent；列表跨权限不泄漏；
任务/版本变化期间分页保持快照；多证券同日记录不丢失或重复；explicit map 完整匹配；
一个版本损坏批量明确拒绝；无版本对象显式列出；帮助示例与 schema 一致。
上述为目标验收，文档提交不作为实现测试通过。

该阶段设计曾按 19 个业务工具计数；当前 release smoke 验证 data socket 为 20 个工具。
历史计数不是当前契约，精确集合/语义由 registry golden test、运行时 `tools/list` 与
capability 验证。
`query_data` 的批量/目录筛选不新增工具；export_data 未计入，run_worker_once 仍为运维能力。

## 通用计算入口

[计算能力与 AI 编排边界](compute-capabilities.md)定义 `compute_metrics` 按
metric/algorithm_version 类型化扩展。该工具已注册，真实 yearly explicit-version inline 正向路径已有 #225
证据；未注册算法或任务模式不能因设计文字冒充可用。它只处理明确的确定性计算，不隐式采集或发布派生数据。
工具数量不再作为交付证明，支持矩阵以 runtime registry/capability 和契约测试为准；export_data 仍是后续候选。
