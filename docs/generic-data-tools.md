# 通用数据工具目标契约

## 决策与适用范围

2026-09-12 用户要求现在考虑扩展，并授权按“工具按业务动作稳定、数据类别通过类型化参数扩展”
开始设计。本设计承接 #75 / PR #76，是通用工具的当前目标定义，尚未实现。
它取代 mcp-tools.md 中“新数据类别通常新增采集/查询工具”的旧目标方案；
该文档的八工具现状记录与当前 schema 快照保持有效，不意味着已有接口已经升级。

数据业务规则仍来自 #3/#4/#5/#6。本设计调整入口和内部扩展契约，不扩大首期数据范围，
不批准新来源，不取消查询、更新、补采、核实、恢复等能力。
新增 dataset 通常只扩展已有工具的 schema 分支与处理器；真正不同的业务动作才新增工具。

## 工具集合与能力边界

| 目标工具 | 意图 | 通用输入要点 | 成功结果 |
|---|---|---|---|
| get_data_capabilities | 发现当前调用者可用的数据能力 | 可选 dataset、operation | 能力修订号、数据类型和操作契约 |
| start_collection | 首次建立指定数据 | dataset、contract_version、request_id、selection | WriteAcceptance |
| update_data | 刷新已有数据 | 同上 | WriteAcceptance |
| backfill_data | 补指定历史范围 | 同上，加类型化 range | WriteAcceptance |
| query_data | 读取已发布数据 | dataset、contract_version、selection、filter、version_id?、page_size?、cursor? | DataPage |
| list_data_versions | 列出指定对象的版本 | dataset、contract_version、selection、page_size?、cursor? | VersionPage |
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
run_worker_once 保留为原型/受控运维能力，不进入正常 AI 采集组合。

## Dataset 的含义与首期矩阵

dataset 是稳定业务数据契约身份，不是供应者名称、文件扩展名或数据库表名。

| dataset | 数据含义 | collect | update | backfill | query / versions |
|---|---|---|---|---|---|
| securities.catalog | 当前沪深 A 股与研究行业二级目录快照 | 首次建立目录 | 显式刷新目录 | 不支持 | 固定目录版本 |
| market.daily | 标准 QFQ 日线 | 官方文件处理生成历史日线 | 更新/修订检查 | 显式交易日期区间 | 单证券日线版本 |
| market.yearly | 固定已发布 daily 本地派生年线 | 派生指定范围年线 | 从明确基础版本重新派生 | 指定自然年集合 | 单证券年线版本 |

这些是目标支持矩阵，现有程序并未注册这些通用入口。
能力发现同时区分 implemented 与 readiness，不把有 handler 等同生产可用。
未通过来源门禁时写能力可以描述为 blocked，提供明确原因，但执行仍必须拒绝。
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

- dataset、operation、contract_version、implemented、readiness（ready/blocked）；
- blocking_reasons：结构化原因和证据引用，不包含密钥；
- 完整 input_schema、output_schema，以及对应 tool_name；
- supported_sources、默认来源规则、数值/时间/覆盖语义；
- selection_modes、page_size 上限、progress_unit、取消/重试是否支持；
- schema_digest、capability_revision。

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

错误沿用结构化类别、outcome_certainty、target、next_actions；
新错误至少区分 unknown_dataset、unsupported_operation、unsupported_contract、
invalid_selection、source_blocked、request_conflict 和 outcome_unknown。
这些为目标语义，具体 wire code 在实现时冻结；错误不能消耗普通未受理请求标识。
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
