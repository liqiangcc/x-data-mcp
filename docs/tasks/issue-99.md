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

## 条件收尾与 schema v7（第二切片）

显式 migration 007 重建 tasks 的状态约束以容纳 interrupted，并增加 stopped_write_checks
（核实 task PK、原 task FK、完整审计 JSON 与 SHA256）。旧迁移字节不改。显式 upgrade
使用私有连接，在事务外暂关 FK enforcement，事务内完整迁移并执行 foreign_key_check；
失败回滚全部 schema/记录，成功后恢复 FK；普通读写连接一直启用 FK 且不会迁移。
独立旧 v6 DDL 的真实临时 SQLite 测试验证引用保留及坏 FK 不允许升级。

StopInspector 是可信应用端口；输出 Observation 不可从 JSON Deserialize。
默认 UnverifiedStopInspector 明确 STOP_EVIDENCE_NOT_VERIFIED；非空引用本身不是证据，
没有安装真实进程/副作用证据协议。FixtureStopped 仅测试中提供受控实验结论。
prepare 核对原任务、核实任务、执行身份、快照摘要及严格停止结果，产生封闭 StopPlan。
SQLite 比较原/核实完整快照，在一个事务内保存原任务中断（通过时）、检查结论、审计和占用释放。
结果未知先查 stop_receipt；同计划重放返回原收据，不释放后来任务的 claims。

核实执行成功但证据不足时 conclusion=insufficient_evidence，原任务和业务占用完全不变；
“检查已完成”不等于“原任务已停止”。成功中断保留已有单项结果，running 项转未完成，
原最后已知 stage/owner/修订保留在审计；只允许新请求显式重试，原任务不复活。
普通原任务/检查结果读取均要求审计存在、摘要/关联/前后投影一致，缺失或损坏明确拒绝。
旧历史 insufficient 检查可读，不强求其目标现在仍保持旧状态。

第二切片测试包括停止/不足分支、真实两连接条件提交、busy、审计插入前失败回滚、
陈旧原任务与取消修订、成功项保留/重试、不重释放新 claims、审计篡改及显式旧 v6 升级。
真实子进程退出、自动收据核对 runner、公共 MCP 及终态重复核实分支尚待后续补齐。
