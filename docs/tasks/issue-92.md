# 更新/补采（#92，PR #93；完整工具尚未交付）

业务依据 #4 的区间保护、精确 Decimal、未知字段补充以及 AC-84—87 无变化/覆盖变化规则；#75 固定设计 2c706325aa2020382631355f802403c05b9662d8。
实现 base ddbf50fe0887152cdc90eb138587b1944dfc77c8。用户完整目标仍包含全部工具、重新部署和测试，本任务的局部进展不是该目标的完成声明。

## 应用判定

market_update 输入为 CheckedRecord、daily batch、显式区间及可信内部证据引用。证据绑定 base version/digest、候选 batch digest 和区间；非空引用不是官方证明，不接受 MCP 放行布尔值。真实证据验证器与来源链仍未实现。

保留范围外旧记录；范围内旧日期缺失（包括整个空响应）整项待核实。已知 volume/amount 不降为 unknown；补充未知字段与修改已知价格分别判断。精确 Decimal 比较，不增加 tolerance 或逐股规则。
输出 NoChange、NeedsVerification 或 PreparedRange。覆盖说明/已确认单位的补充也纳入版本变化，不能仅比较价格；仅 acquired_at 变化只保留执行记录。元数据变化时返回全部受影响年份，不假定可局部沿用旧 yearly。
PreparedRange 自身不是发布批准。yearly 必须从固定 published daily 派生，本模块不拼接旧 yearly。

## SQLite 更新提交（设计先记录，随后实现）

应用层 Publisher.prepare_range 重新核验固定 base 并执行区间判定。有变化须单独提交完整候选和 CombinedEvidence，绑定 base/segment/完整候选摘要及全范围覆盖依据；完整候选必须符合区间合并结果并保留已确认单位。不能以 segment Complete 冒充合并全范围 Complete。可信内部证据可序列化保留审计，但不能从 MCP 反序列化成放行输入。

发布意图仅 daily + expected_head=base。SQLite 短事务重查固定 base、候选、head、task revision/owner/occupancy。任一变化拒绝，不 rebase。业务资格留在应用层，Adapter 执行封闭计划和条件事务。

schema v5 新增 market_update_results，保存原版本、结果版本、判定输入、摘要及收据。audit 包含实际 acquired_at；摘要同时覆盖审计及收据。无变化不重复写原 publication_batches/task_publications，不建新版本、不推进 head；结果与任务完成/占用释放同事务。有变化沿用 #79 原始发布表，并与更新结果同事务提交。

响应丢失用 range_receipt 按 result id 核对；重放完整命令和证据必须一致。核对在一个读事务中验证审计/收据、版本完整性及成功执行项；不因当前 head 已前进拒绝历史收据，也不自动换版。
普通读取不建库/迁移。显式 init/upgrade 才增加 v5；已部署预览仍 v4，本 PR 尚未部署。

## AC 与具体测试

- 区间保护、遗漏、Decimal、已知值降级、未知字段补充：tests/market_update.rs 的 update_* 测试。
- #4 AC-84—87：update_coverage_change_needs_version_but_acquisition_time_alone_does_not；range_coverage_only_revision_publishes_and_acquisition_only_reuses。
- 有变化/无变化原子提交及重放：range_changed_and_no_change_commit_real_sqlite_and_replay。
- 结果写失败回滚 task/head/publication：range_result_write_failure_rolls_back_publication_and_task。
- 预期 head/revision 与真实两连接竞争：range_head_and_task_revision_changes_never_rebase；range_candidates_compete_on_the_same_real_task_and_head。
- base/候选/结果篡改拒绝：range_base_and_candidate_tampering_reject_before_publish；range_receipt_refuses_corrupt_result_or_fixed_version。
- 组合结果/独立全范围覆盖：range_full_coverage_evidence_cannot_be_inferred_from_segment；range_candidate_must_preserve_outside_rows_even_with_bound_whole_evidence。
- 无变化不造版本、重放比较完整输入：range_needs_verification_and_no_change_do_not_fabricate_new_versions；range_result_id_replay_compares_all_evidence_inputs。
- 真实子进程退出：sqlite_market::update::tests::range_real_exit_before_commit_and_lost_response_after_commit，在 changed/no-change × before/after commit 四种组合退出码 79 后重开核对。range_crash_child 单独空跑不能作为故障证明。
- 显式 v4→v5：range_schema_v4_requires_explicit_upgrade_and_preserves_published_records；既有通用 schema/CLI/stdout 回归继续运行。

Actions 执行 fmt、clippy --all-targets --all-features --locked -D warnings、test --locked、build --locked、diff-check 和既有 evidence 诊断，准确 SHA/run/job 见 PR。服务器空间约 99%，本机仅 fmt/diff-check，不完整编译。

## 尚未交付

已接入公共显式 daily 受理与能力发现；仍缺默认范围/明确缺口冻结、yearly/catalog 更新编排及部署。显式 daily 单项 RangeRunner 及公共 worker 分发已实现，生产 provider 仍拒绝。SQLite 集成测试已使用内部 RangeContract 受理更新/补采；初始 published fixture 及退出故障测试仍用 legacy contract。内部契约不冒充公共 update/backfill 已实现。

#4“已启动首次采集”的受理/实际执行定义已向用户提问，未据此擅自实现默认选择。日线现有模型实际截至日来自参与记录；更丰富的范围/覆盖结构不能仅凭 reason 字符串视为完整通用契约。
#54/#71/#46/文件一致性仍未解决；live source、真实 normalization、真实行情闭环均 NOT_RUN/NOT_VERIFIED。没有来源采集，不部署此半成品。

### 显式范围任务切片（编码前边界）

已实现内部 explicit-range-task-v1 契约受理 update_data/backfill_data 的显式 daily 范围；还不是公共 MCP v1 schema。每实体一个完整范围/base binding，重复同项规范化，冲突范围不合并。固定版本在单次 fixed_many 读取中核验并冻结 digest/范围，沿用证券日线 scope 占用（日期元数据不缩小占用）。提交应用层必须核对冻结输入，不由 MCP 判断资格。默认对象、自动缺口及 yearly 不以此契约冒充完成。

显式范围测试：explicit_range_admission_normalizes_sets_and_freezes_base_without_requery_on_replay；explicit_range_operations_and_different_dates_do_not_evade_request_or_scope_conflicts；explicit_range_invalid_inputs_leave_no_request_binding；range_publish_cannot_widen_the_accepted_frozen_dates。覆盖同参重复、旧 base 损坏后原请求重放、异操作 request_id 冲突、异日期同 scope 冲突、未知字段/冲突项/非法输入不消费 ID 和发布范围不能扩大。

### 公共 MCP 显式 daily 入口（编码前边界）

update_data/backfill_data 的 v1 首个公共形状为 request_id、dataset=market.daily、contract_version=v1、items（entity、base_version_id、start_date、end_date）。全部显式，不把空/省略列表当作全市场或默认范围；每实体一个范围。响应沿用 WriteAcceptance，其中 contract_version 标明实际持久化执行契约 explicit-range-task-v1。输入映射在应用契约模块，资格判定仍由 RangeContract 完成。

发现 registry 仅为这两个工具列 daily，implemented=true/readiness=blocked，说明仅显式范围受理、来源仍不可信且当前 worker 只记录拒绝。yearly/catalog/default selection 不伪装已实现；非本机所有者上下文不发现/调用写控制。既有 13 工具增加到 15，版本读取与 task 查询不改变语义。

公共接线回归：real_range_stdio_acceptance_replay_conflicts_and_blocked_execution（真实 mcp_data 二进制的 schema/受理/幂等/冲突/拒绝来源执行/重试/取消，确认无新行情）；range_controls_are_not_discovered_or_callable_without_owner_access；range_storage_failure_is_not_reported_as_invalid_input_or_success。通用发现测试要求 15 工具与 registry 一致。Preview package 的离线 smoke 计数同步，不表示部署。

### 更新结果只读查询（编码前边界）

get_task_progress 增加 frozen_daily_ranges（每项已受理 base/digest/日期）及 range_results（原/结果 version ref、是否无变化、实际 acquired_at、result id）。已成功项仍可见这些关联，不把复用版本写成新发布。SQLite 在读取 task 的同一事务核验结果收据/关联及固定版本，cursor 续页也重新核验已返回结果；损坏拒绝，不换 latest。保留旧 publication_versions 字段表示原始发布，普通查询不补写/修复/执行。仅描述实际持久事实，不从来源拒绝或任务完成推断存在行情。

### 单项发布执行器（编码前边界）

应用 RangeRunner 显式执行一个已受理 daily 项。先核对 task/item 的持久结果；已有结果只返回收据，running 无结果不自动重做。仅当前 run 的 pending 项可 Claim/StartItem；固定 base/digest/范围在来源调用前后核对。有变化使用确定的候选 ID、独立完整候选和覆盖证据；无变化复用原版本。提交仍走 sealed plan/SQLite 短事务。

可信同步 provider 只提供 batch/证据/候选，不负责发布；真实装配只能使用 OfficialFilePipeline，当前必然拒绝，不引入来源选择开关。显式 fixture provider 仅测试使用。已知未发布且来源停止的失败可记录原失败；提交/存储结果不明保留 running/占用，先核对收据，不能重采/换 ID。取消在开始前收尾，执行中取消阻止发布，按现有任务状态机记录实际未发布结果。该单项应用执行器尚不等于全部生产 worker/yearly 编排。

RangeRunner 回归：range_runner_executes_and_replays_real_publication_without_reacquiring；range_runner_does_not_reexecute_running_or_old_run_items；range_runner_cancellation_and_official_gate_never_publish；range_runner_reconciles_lost_commit_response_and_preserves_unconfirmed_execution。最后一项在真实 SQLite 前后注入响应不确定，不把此包装器称为进程退出测试；实际进程退出证据仍来自 sqlite_market::update::tests。public_range_request_to_fixture_execution_and_verified_query_is_connected 验证公共 JSON 受理→显式测试 provider 执行→持久结果/行情查询；不存在 MCP 选择 fixture provider 的参数。

取消沿用既有任务状态机：开始前取消可返回 cancelled；正在执行的单项若确认未发布并记录失败、且所有项均已结束，则返回实际 failed 终态和原因，不能把未发布当成功。存储/提交结果不明不执行这种失败收尾。
