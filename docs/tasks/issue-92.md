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

update/backfill 类型化受理、默认范围/明确缺口冻结、MCP/schema/能力发现、发布感知执行器、后续 yearly 编排及部署。SQLite 测试使用 legacy fixture task contract 取得真实占用，不冒充公共 update/backfill 已实现。

#4“已启动首次采集”的受理/实际执行定义已向用户提问，未据此擅自实现默认选择。日线现有模型实际截至日来自参与记录；更丰富的范围/覆盖结构不能仅凭 reason 字符串视为完整通用契约。
#54/#71/#46/文件一致性仍未解决；live source、真实 normalization、真实行情闭环均 NOT_RUN/NOT_VERIFIED。没有来源采集，不部署此半成品。
