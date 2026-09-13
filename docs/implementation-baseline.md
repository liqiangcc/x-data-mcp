# 实施基线与开发任务边界

## 当前授权与阅读规则

2026-09-12 用户先要求完善文档与 Issue，随后于 08:54:20Z 明确授权“让 d2 按照设计文档开发”。
阶段 A #78 已按固定设计实施、审查并由 PR #82 合并（merge `d8480dbfb03bafb5269381c447cbc5e46fa2c9a8`），
Issue #78 已关闭。本次修订先冻结 #79 的剩余发布语义，再允许阶段 B 执行；不把文档修订本身冒充
阶段 B 实现，不采集真实来源、不迁移现有行情、不部署，也不自动进入 #80/#81。

业务规则及最新明确修正以 #1/#3/#4/#5/#6 为依据；本文件只收敛技术范围，
不能覆盖已确认业务规则。技术文档相互冲突时按本文件的明确修订处理；未决项不由执行者猜测。
AC 的权威位置在各 Issue，本文不复制另一套 AC 编号或维护完成百分比。

| 内容 | 阅读入口与用途 |
|---|---|
| 本轮范围、跨文档修订、实施准入 | 本文件 |
| 通用动作、数据类别、帮助和查询 | [generic-data-tools.md](generic-data-tools.md) |
| 确定性计算与连续涨跌 | [compute-capabilities.md](compute-capabilities.md)、[preset-consecutive-direction.md](preset-consecutive-direction.md) |
| 职责与领域不变量 | [design.md](design.md) |
| 旧八工具的固定代码事实 | [mcp-tools.md](mcp-tools.md)；不是目标工具注册表 |
| SQLite 表、事务与摘要兼容 | [SQLite 设计固定版本](https://github.com/liqiangcc/x-data-mcp/blob/9bae8592eed4394c672734f95132f4e4fa1e4d24/docs/sqlite-storage-design.md)，PR #72 |
| 规范及验证命令 | [conventions.md](conventions.md)、[README](../README.md) |

最初整理时核对 main：`1e270c648e7884fffd65ef13cfe9f47e0dab22ad`；阶段 A 已合并为
`d8480dbfb03bafb5269381c447cbc5e46fa2c9a8`。整理前整体设计 HEAD：
`83524f45a5e967da3705b96f529fee031125c880`。
旧文档中的代码快照仍按各自 SHA 理解，不能仅修改日期就声称已重验新代码。
后续实现须在 Issue 记录实际采用的已审查设计 SHA 与实现 base SHA；分支名不是固定基线。

## 本轮修正与依赖归属

| 原有歧义或遗漏 | 本轮处理 | 验收归属 |
|---|---|---|
| 采集样例 securities 设置 uniqueItems，与已确认无序集合幂等冲突 | 接受完全相同重复项，规范化后只计一次；冲突选项拒绝，不任意丢弃 | #78 参数语义；#80 schema/协议 |
| 正常 AI 流程调用 get_task_progress，但阶段 C 未列交付 | #78 提供内部一致快照；#80 交付只读进度/未完成清单分页入口 | #78 / #80 |
| 目录筛选依赖不存在的 SQLite 目录读取 | #80 包含最小类型化目录 fixture 存储与 verified read，复用 #6 规则；真实文件 Adapter 不在该任务 | #80；真实来源 #6/#46 |
| 缺占用一律待核实，会误伤合法终态 | 仅在该状态按规则应持有占用却缺失时待核实；合法终态已释放占用是正常事实 | #78 |
| “同快照读取”被误解为两个独立调用保证同一时点 | 每次读取内部一致；跨调用可有不同修订。只有同一个分页凭据续页固定快照 | #78 / #80 |
| TdxQuant Key 路线仍写成下一生产依赖 | 当前只走官方文件；#46 承接目录/行业文件契约证据，旧 SDK 调查仅历史 | #6 / #46 / #71 |
| SQLite 专项把 #68 写成正在执行、版本保留期限写成未确认 | #68 已关闭，不再派发；首期不自动清理已发布版本，只有未来清理规则未定 | #69 / #75 |
| 原型 JSON 行情与新 SQLite 任务混装容易被称为整体事务 | 阶段 A 仅显式开发测试；跨 JSON/SQLite 发布不得宣称原子，阶段 B 才验收同库关联提交 | #78 / #79 |

写请求的集合去重不自动推广到所有数组。批量查询的无重复 entity、计算 input_id/metric_id 唯一性，
以及有业务顺序的参数，继续遵循各自契约。旧 request_id 重放按其绑定契约解释。

## 后续依赖与准入

技术依赖：#78（已完成）→ #79 → #80 → #81；#81 同时直接依赖 #79 的 verified read。
来源证据 #54、#71、#6/#46 可独立整理，但不因技术任务通过而解除真实生产门禁。
不以“来源仍未验证”阻塞无网络 fixture 设计；也不以 fixture 成功证明真实数据可发布。

进入每个实现任务前应同时具备：用户已允许当前开发阶段；本任务设计已审查并固定 SHA；
所依赖代码已合并或明确采用可复核基线；本任务的关键设计未决项已解决；
核对 live Issue/评论/PR 与执行归属，没有已有执行者或重复 claim。
#79 必须以本次年线 head 规则冻结后的设计 SHA 和包含 #78 的 main 为实际实施基线。

## 阶段 A：SQLite 通用任务受理与一致读取（#78，已完成）

实现与审查证据见 PR #82；最终审查 HEAD `8338c2e5884ac7ce93c2094701d9311e6961299b`，
合并提交 `d8480dbfb03bafb5269381c447cbc5e46fa2c9a8`。其交付边界仍是任务子系统的 SQLite 实现和
显式开发/测试装配，不切换生产默认后端，
不实现行情迁移、后台守护进程或全部通用工具。

### 最小内部契约

- CommandKey：operation、dataset、contract_version；旧 day/year 复合命令保留独立 legacy 契约身份，一个原请求仍是一个任务。
- RequestRecord：全局 request_id、完整规范化命令与辅助摘要、结果种类、可空 task_id。不能仅凭 hash 判断命令相等。
- TaskRecord：task_id、契约身份、status、state_revision、accepted_run_id、可空 owner_run_id、created_at、last_progress_at、结果。受理启动身份与领取者分开。
- TaskItem：task_id/item_id、固定业务定位、进度单位、执行阶段、结果及冻结输入引用。
- ScopeClaim：资源身份、范围、task owner 和修订；行情依然按证券 × 周期冲突，不因日期不重叠绕过。

内部能力为原子 accept、read_request、read_task_snapshot、conditional_transition；
只有 Adapter 持有 SQLite 连接/事务，命令解码、规范化和业务判断不依赖 SQL。
同事务固定范围、核对全部冲突、绑定请求、任务、执行项与占用，可靠提交后才能报告受理。
不同操作/类别/语义参数复用 request_id 冲突；完全相同重复项、无序调序按既定集合语义处理。
动态选择方式与恰好相同的显式范围不等价，固定默认值和旧契约重放仍按原规则。

Accepted / Reused 返回实际状态；RequestTerminal 表达既定无任务请求级结果，
但本阶段不实现全部核实/恢复业务入口。InvalidCommand、RequestConflict、RangeConflict、
AcceptanceNeedsVerification、StorageUnavailable、OutcomeUnknown 为内部分类，
须提交到旧 wire code 的兼容映射，不能把内部类型名假装成已发布协议。

### 状态和故障边界

当前运行的合法 running 不无条件改成 needs_verification；缺 execution 不伪装 pending。
合法终态无占用不误判损坏；仍需保护的非终态缺占用则明确异常，不通过查询修复。
每次请求/任务读取使用一个一致读快照，跨调用不承诺同一修订。
重启前遗留的 pending/running/cancel_requested 等非终态均不得自动续跑，
保留最后已知进度及保护；启动恢复识别是显式生命周期处理，不把普通查询变成写操作。
取消只表示申请；释放必须有执行结束及停止写入依据，过期 state_revision 条件更新拒绝。

未初始化 SQLite、未知 schema、坏库或记录不可读明确失败；普通查询不建空库、不升级、
不回退 JSON。忙碌处理有界；提交结果不明先核对持久事实，不以新 request_id 绕过。
SQLite 事务中退出的实验应证明无半绑定；人工构造残缺记录另测保守读取，二者不能混称。
阶段 A 可保留旧 JSON 行情 Adapter，但不把该混装当成跨存储原子发布证据。

交付 DDL/迁移入口、类型/错误映射、状态转换条件、真实 SQLite 故障测试与离线 stdio 回归。
第二个 test-only task_kind 只证明公共控制不依赖证券字段，不注册虚构数据类别。

## 阶段 B：SQLite 行情发布与固定版本读取（#79）

依赖 #78（已合并）。交付逐行 daily/yearly、单位/版本/血缘、旧摘要兼容记录，
batch/version/head/任务输出关联的同库原子提交，以及固定版本范围读取。
仅 fixture/dry-run，不导入实际库、不解除来源门禁、不自动清理历史。

摘要严格沿用固定实现的完整 PublishedRecord JSON value：仅去掉顶层 integrity_sha256，
按相同 serde_json 序列化行为计算；不以重新构造领域对象丢失“缺字段”和 null 的区别。
SQLite 行情与元数据投影必须逐字段对应已核验记录。任一侧篡改、缺摘要、错误血缘或
原本未经验证的记录均不能因重算摘要被升级。该格式不是泛化的 JSON canonicalization 标准。

提交时检查预期目标 head、任务状态修订与占用身份；过期决定拒绝，旧版本和结果保持一致。
verified read 在同一个有界读事务/一致快照内核验内容并提取返回行，避免先校验旧快照，
再返回新快照的行。跨用户翻页不持有长期事务；每页重核固定版本，损坏即停止，不换版。
范围查询可用索引，但全版本完整性核验成本单独记录，不能声称整个读取只扫描一页。

年线发布已收敛为两个内部发布意图，公开协议字段由后续所属契约决定：

- `historical-fixed-base`：显式绑定的 historical published daily 仍可核验时，即使 daily effective head 已前进，
  也可发布一个新的 immutable yearly version；固定 `base_daily_version_id`，**不得推进** yearly effective head。
- `current-head`：候选基于 expected current daily head；提交事务同时核对 expected daily head、expected yearly head、
  任务修订和占用。全部匹配才发布并推进 yearly effective head；任一 head 变化均返回前置条件冲突。
- base 不存在/损坏/身份口径不匹配是基础版本/完整性错误，与“非最新”或 head 冲突分开。
- current-head 冲突后不自动 rebase、不重算、不静默降级成历史发布；要基于新 daily head 继续需形成新的候选/决定。

这使“已发布历史事实”和“普通查询默认最新”分离：历史版本通过显式版本读取发现，默认查询只跟随 effective head。

## 阶段 C：通用 AI 数据供给（#80）

依赖 #78/#79。明确承接下列范围，注册清单和 schema 必须在代码实施前审查：

| 操作 | 本任务范围 | 不代表已完成的内容 |
|---|---|---|
| get_data_capabilities | 空参简洁目录、类别目录、操作详情；schema/handler/帮助同源 | 未实现能力不能标 implemented；blocked 不等于可生产使用 |
| query_data / list_data_versions | catalog/daily/yearly 的类型化固定版本读取；行情 single/batch manifest | 不读取任意文件、不开放 SQL，不自动造目录或行情 |
| get_data_availability | 已存版本、范围覆盖、完整性、相关任务一致快照 | 不访问外部来源，不用日期不连续推断漏采 |
| list_tasks / get_task_progress | 任务发现、类型化进度与未完成项、固定分页快照 | 不隐式执行/核实，不用执行百分比代表历史完整度 |
| start_collection | legacy/new 互斥契约；新 dataset 分支范围在 schema 清单逐一冻结 | 不把两个样例分支当作完整 schema，不拆旧复合请求 |
| get_request / get_task / cancel_task / retry_task | 与阶段 A 类型/状态连接的兼容回归；新形状须显式版本化 | 不强制旧客户端立即采用未定义输出，不改变全局幂等命名空间 |

目录子范围只为此查询用例提供最小类型化 fixture 持久化与 verified read，
复用 #6 的身份/行业缺失冲突/成员消失/失败保旧规则；不在 #79 悄悄假设目录已实现，
不在生产启动时注入 fixture，不把目录全部缺行业当作真实来源已验证。
真实官方文件解析及字段映射仍由 #6/#46，目录生产 AC-12 不在本任务关闭。
若某 dataset 写分支的处理器/规则未完成，则该分支不注册为 implemented，
在清单记录未实现和具体依赖，不以永远成功或空数据的假处理器凑数。

每个计划注册分支提交完整 input/output schema、判别键、参数规范化、错误编码与
outcome_certainty、分页绑定、资源预算、正反例，以及示例输出如何传给下一工具。
schema、帮助和 handler 共享注册定义；普通读调用不得创建任务、目录、库或数据版本。
批量查询冻结所有对象版本及状态，explicit map 精确匹配；任一既有版本损坏整体拒绝。
可靠无版本可列 absent；有版本但覆盖不明为 unknown；坏版本 unavailable；全局坏库工具错误。
进度分页携带稳定 item_id；retry 仍在受理时重新检查资格，不相信旧快照。

本阶段不实现全部 update/backfill/四类 verify/restore/inspect、远程认证或后台运行。
这些完整需求仍由 #3/#4/#5/#75 承接，不因本次 AI fixture 查询闭环而删除。

## 阶段 D：连续涨跌首个计算闭环（#81）

依赖 #79/#80。首个范围为 compute_metrics inline、一个固定 yearly 版本，
单证券、consecutive_direction；不接受 latest、task 模式、任意代码、布林线或回测。
阶段 D 的首批注册参数必须在 Issue 明确，不能因计算总设计提到某选项就默认全部实现。

沿用算法定义：相邻 close 比较，N 次变化需 N+1 点；相等打断；
找到尾部打断点为 exact，延伸至输入左界为 lower_bound。
下界达到阈值 true，否则 null；已证明不足才 false，不将数据不足当作不满足。
必要尾部周期缺失/完整性证据不足不可计算；不得删除中间不完整周期后跨年比较。
已找到打断点以前的无关历史不要求无限扫描，但所选版本完整性仍须校验。

completed_only=true 为默认；仅明确未结束的尾部周期可剔除。
completed_only=false 如纳入首批 schema，必须同步交付 provisional 标记和独立向量；
未纳入则明确拒绝，不静默当成 true。此参数支持范围在 #81 文档审查中确定。
非正价格仅是本算法不可计算，不反向增加行情发布的正数规则。
精确比较；累计比值按整数系数/尺度的有理运算，scale=12、half-even 一次舍入。
运行时点/响应 ID 不参与数值确定性断言；不隐式读取范围外预热或补采数据。

交付独立手算向量、完整闭合 schema/错误、版本读取与算法/MCP 组合测试。
资源预算须有限且公开，超过预算明确失败，不截断、不隐式转后台；不虚构性能 SLA。

## 验收材料和未决项管理

后续每个实现 PR 提供：采用设计 SHA、代码 SHA、改动职责、AC→测试入口→实际结果，
以及明确的 NOT_RUN/NOT_VERIFIED 和仍开放的父需求。保留修复前证据，不用实现生成自己的期望。
稳定命令使用 README；不存在的 scripts/verify 不得写成已执行。
本轮文档校验仅检查 Markdown/JSON/schema 样例，不证明 Rust、SQLite、来源或部署通过。

| 未决项 | 权威归属 | 阻塞范围 |
|---|---|---|
| 全量阶段 C 输入/输出 schema、写分支清单和资源预算 | #75 / #80 | 对应工具注册与实现 |
| 阶段 D provisional 参数支持范围和完整性证据输入 | #75 / #81 | 计算注册与实现 |
| 官方目录、名称、研究行业二级映射及快照一致性 | #6 / #46 | 真实目录与生产链路 |
| GBBQ 业务十进制和 QFQ 契约 | #54 | 真实 QFQ 发布 |
| 官方文件长期获取和使用证据 | #71 | 真实服务器采集与使用 |
| 其余管理能力、运行/认证、备份恢复及迁移 | #3/#4/#5/#69/#75 | 各自后续实现，不塞进 #78 |

采集样例及其 [20 个正反例](contracts/generic-start-collection-v1.examples.json)用于文档校验。
它们不替代阶段 C 全量工具契约，也不证明数据库幂等已经实现。
