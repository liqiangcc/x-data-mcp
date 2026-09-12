# 实施基线与开发任务边界

## 状态与权威顺序

用户授权整理设计并创建开发 Issue。本文件将迭代草案收敛为分阶段实施范围，
不代表设计已合并、代码已完成或已向 d2 派发任务。
业务依据仍为 #1/#3/#4/#5/#6，来源门禁 #54/#71/#46 不变。

当前目标定义按以下顺序查阅：
1. 本文件：阶段范围和实现准入。
2. generic-data-tools.md：通用数据动作、能力帮助、可用性与批量读取。
3. compute-capabilities.md、preset-consecutive-direction.md：计算及预制算法。
4. design.md：职责、不变量与运行设计。
5. mcp-tools.md：固定旧八工具的兼容事实；历史草案已移入 history/，不作新工具清单。

SQLite 设计固定版本：
[9bae8592eed4394c672734f95132f4e4fa1e4d24](https://github.com/liqiangcc/x-data-mcp/blob/9bae8592eed4394c672734f95132f4e4fa1e4d24/docs/sqlite-storage-design.md)。
旧摘要兼容已明确；PR 审查通过不等同对全部未来接口逐项验收。
设计合并/基线审查是实施前置；执行者核对 live 分支，不改他人工作区。

## 阶段 A：SQLite 通用任务受理（第一个开发任务）

交付目标：相同业务命令在 SQLite 原子绑定 request/task/items/occupancy，
当前受理/查询/取消/重试语义可回归，公共任务不写死为证券 × 周期。
本阶段不迁移实际数据、不切换生产默认后端、不实现所有通用工具。

### 类型与表

- CommandKey：operation、dataset、contract_version；旧 day/year 复合命令使用明确 legacy 契约身份，
  一个原请求仍绑定一个原任务，不能拆为两个无关联请求。
- RequestRecord：request_id 全局 PK、CommandKey、规范化命令、结果种类、task_id 可空。
  规范化命令保存完整语义，hash 仅作辅助，不能仅凭 hash 相等忽略命令比较。
- TaskRecord：task_id PK、CommandKey、status、state_revision、owner_run_id 可空、
  created_at、last_progress_at、result；未领取不伪造 owner。
- TaskItem：复合键 task_id/item_id、冻结业务定位、progress_unit、阶段、结果。
- ScopeClaim：task_id、资源身份、范围、修订号。范围相交由用例规则决定，
  在原子受理边界保证读取和提交不被竞争绕过。

仅 SQLite Adapter 持有连接/事务。公开内部能力为 accept、read_request、read_task_snapshot、
conditional_transition；命令解码和规则无 SQL 依赖。
验收第二种 test-only task_kind 验证可扩展性，不在 tools/list 暴露虚构数据类别。

### 精确受理与错误契约

accept(command) 返回 Accepted(task_id,reused=false)、Reused(task_id,实际状态)；
既定无任务请求级终态可由 RequestTerminal 表达，但本阶段不实现其全部业务触发入口。
拒绝分类 InvalidCommand、RequestConflict、RangeConflict、AcceptanceNeedsVerification、
StorageUnavailable、OutcomeUnknown。
这些为内部类型，不擅自改现有 MCP wire code；当前八工具输入快照不变。
通用新 schema 在阶段 C 注册，阶段 A 不接受未声明 dataset 参数。

同 request_id 的操作/数据类别/规范化参数不同即冲突；普通前置拒绝无绑定和占用。
同一事务检查全部冲突，写完整请求、任务、执行项与占用，提交后才返回受理。
请求存在但执行/占用证据缺失返回待核实，不修复成 pending。
SQLite busy 有界失败；提交结果不明不能冒充未受理。
get_request/get_task 从同一读快照返回一致事实。
取消/重试复用原业务条件，重试复用返回实际终态；条件提交防止过期状态覆盖。
重启未结束执行保持待核实保护，本阶段不交付后台 worker 自动恢复。

### 准入与验证

提交 DDL/migrations、状态/错误映射及 fixture 回归；沿用 README fmt/clippy/test/diff。
真实 SQLite 覆盖竞争受理、半绑定崩溃、异参冲突、相交范围、坏库、残缺绑定、
结果丢失重查、取消竞争、重试实际状态；不声称断电或多业务 worker 已验证。
通过显式测试/开发装配选后端；未初始化 SQLite 明确拒绝，初始化/升级为独立操作，
不由普通查询创建库或回退 JSON。
行情仍可使用已有 JSON Adapter，不为任务改造顺带实现行情迁移。

## 阶段 B：SQLite 行情发布与固定版本查询

依赖 A。逐行 daily/yearly、类型化元数据与 lineage、完整兼容发布记录/摘要、
batch/version/head/任务输出原子提交、固定版本读取和范围 SQL。
按 SQLite 修正文档检验完整记录及其表投影，篡改任一侧均拒绝。
仅 fixture/dry-run，保留未验证记录属性；真实迁移、源接线和生产切换不在本阶段。
提交 schema 和摘要样例，测试旧记录缺字段/null、坏摘要、表行篡改、并发 head、发布结果不明、
历史版本及年度基础版本，运行已有查询契约。
不自动清理历史，不将数据库结构检查当作来源验证。

## 阶段 C：通用 AI 数据供给契约与实现

依赖 A/B。实现 get_data_capabilities、query_data、list_data_versions、
get_data_availability、list_tasks，并对首批 start_collection 提供显式兼容分支。
更新/补采/核实/恢复虽属于完整目标，不因本阶段交付而关闭其父用例；各自再分任务。

本阶段开工先补全所有计划注册工具的 input/output schema、错误及示例，
以同一注册描述生成能力和路由检查，随后实现。
现有 generic-start-collection-v1.json 只是两个分支切片，不能冒充全部接口完成。
采用 closed discriminated schemas，unknown dataset/operation/contract 拒绝。
冻结版本/任务分页、多对象 manifest、无数据/坏数据/覆盖未知分别表达，
无查询写副作用。来源 blocked 允许准确发现，但不能发布。
新旧 start_collection 必须互斥分支，旧复合任务保持原幂等身份。

## 阶段 D：首个计算闭环 consecutive_direction

依赖 B/C 的版本读取和能力契约。首个实现只做 inline、有界、单证券固定 yearly 输入，
方向 up/down、完整周期、左边界 lower_bound 与 matches 三态，
精确比较与累计比值一次 half-even 舍入按算法文档执行。
compute_metrics 的 task 模式、计算结果分页、布林线和序列引用继续保留后续范围，
不得标为 supported；资源上限必须在注册时给出并实测拒绝行为。
证据不足的 fixture 返回不可计算；纯算法通过不代表真实年度完整性通过。
补完整该切片的输入/输出 schema 和错误样例，不实现一个接受任意 options 的空壳。

## 设计尚未定稿但不阻塞 A 的内容

阶段 C 之外的更新/补采/核实/恢复完整 schema、远程认证与后台生命周期、
布林标准差与精度、计算产物引用、导出获取方式/保留和真实性能目标。
它们只阻塞相关实现，不自动取消完整产品需求，也不要求 A 顺便解决。
官方数值、目录、文件使用与跨快照一致性继续阻塞真实生产验收。
