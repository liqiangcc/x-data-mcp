# #99 停止写入核实（开发中）

父 #3 的重启/停止核实及四类异步核实串行规则；设计 #75 固定提交
2c706325aa2020382631355f802403c05b9662d8。实现 base 为 main
e04681b801db002958a3407d3ea4d612b4ffe688。完整 AC/实现顺序已先记录 #99。

第一切片提供 StopVerification 应用契约，通过现有真实 SQLite 原子受理建立独立核实任务。
内部 dataset=task.verification、operation=verify_stopped_writes、
contract_version=stopped-writes-task-v1。输入只有 target_task_id；调用者不能提供停止证明。
核实有一个 evidence_check 执行项，不以股票/周期计数。

所有核实类型共同使用 verification:task:<target_task_id> 资源，不接管原业务 scope。
受理冻结目标 ID、初见修订与快照摘要/执行身份；这些只是审计输入，不能证明已停止。
正常目标/缺失/残缺记录分别拒绝；核实任务不能递归成为目标，须未来独立有界处置。
同 request 重放读取原受理事实，异参冲突；不同目标可分别受理，同目标其他类型不能绕过保护。
原范围和待核实状态在受理后不改变。

真实 SQLite 测试 tests/stopped_verification.rs：
- verification_admission_never_takes_original_claims_or_reinterprets_replay
- verification_missing_normal_recursive_and_untrusted_flags_never_bind
- verification_two_connections_replay_same_request_and_reject_competing_ids
- verification_serial_scope_is_shared_across_types_and_survives_cancel_and_restart
- verification_corrupt_original_is_not_missing_or_permission_to_unlock

这还不是公共 MCP 或完整核实执行器。可信停止证据协议、interrupted 显式 schema 迁移、
原任务/核实收据同事务收尾、取消与退出故障测试及公共 schema 待实现；不会据本切片宣称 #99 完成。
当前无 schema/线上改动，真实停止核实 NOT_RUN，来源门禁仍 NOT_VERIFIED。
本机只做 fmt/diff-check；完整编译测试提交 Actions 后按实际结果记录。
