# 更新/补采范围核心（#92；完整工具尚未交付）

业务依据 #4 区间外保留、范围内旧日期缺失整项不发布、单位/前复权基准一致和精确 Decimal；#75 固定设计 2c706325aa2020382631355f802403c05b9662d8。
当前实现 base ddbf50fe0887152cdc90eb138587b1944dfc77c8。用户完整目标仍包含全部工具、重新部署和测试，本切片不是该目标的完成声明。

market_update 是无 I/O 的应用判定核心，输入为 CheckedRecord（既有完整性检查结果）、daily batch、显式区间和可信内部证据引用。
内部证据绑定 base version/digest、完整候选 batch digest 和区间；其非空引用不是官方证明，也不接受 MCP 用户提供的放行布尔值。真实证据验证器与来源链仍未实现。

输出 NoChange、带原因/日期的 NeedsVerification 或 PreparedRange。PreparedRange 只给出保留范围外旧记录后的候选及变化日期/受影响年份，绝不是 PublishPlan；没有继承或伪造合并全范围的 Complete 覆盖。真实提交仍需应用检查整体覆盖/provenance，再使用 #79 的 head/revision/occupancy 条件事务。yearly 不直接拼接，从固定 daily 重新派生。

已覆盖测试：
- update_preserves_both_sides_and_tracks_years_without_publishing / update_adds_explicit_new_date_without_deleting_old_rows：区间外保护、增量候选、受影响年份。
- update_refuses_whole_item_when_old_in_range_date_is_missing / update_empty_response_with_old_dates_is_needs_verification：缺旧日期或整个空响应均不发布。
- update_decimal_format_change_reuses_fixed_version：格式不同但精确等价时不产生新版本。
- update_requires_bound_basis_and_actual_revision_evidence：真实微小数值修订仍需证据；版本/digest/range 绑定错误拒绝。
- update_unknown_field_enrichment_is_not_a_known_value_revision：#4 AC-71 允许核实后补充未知字段；同时变更已知价格仍按 AC-73 要求修订证据。
- update_rejects_known_value_loss_even_with_revision_reference：已知成交量/额不得降为 unknown 后发布。
- update_rejects_unknown_coverage_and_conflicting_units / update_rejects_invalid_identity_period_range_and_conflicting_duplicate：覆盖、单位及基础输入拒绝。

未完成：SQLite 原子发布集成、update/backfill 受理及默认范围/明确缺口冻结、MCP/schema/能力发现、部署。#4“已启动首次采集”的受理/实际执行定义已向用户提问，尚未据此实现默认对象选择。
#54/#71/#46/文件一致性仍未解决；无来源采集，无生产算法变更，不部署此半成品。
完整 Rust 验证走 Actions，结果在 PR 记录；本机仅 fmt/diff-check，不能冒充测试 PASS。
