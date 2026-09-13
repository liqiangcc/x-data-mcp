# 显式发布结果核实（#104）

父业务规则是 #3 的显式发布结果核实、重复/状态变化保护及核实串行规则；固定设计为 #75 的 2c706325aa2020382631355f802403c05b9662d8。基于 main 7172ba5dc64b425cbde66b4950c41ae6c03b2c4f，关联 #79/#92/#94/#99。本任务不解除来源门禁，不扩大为恢复或异常核实处置实现。

## 已实现的边界

- `verify_publication_result` 接受 v1、独立 request_id、target_task_id；拒绝额外证据/成功声明/强制参数，仅本地所有者可调用。共有 17 个公开工具，registry 与 tools/list 同源。接口 ready 仅表示能检查既有 SQLite 证据，不表示 production_ready。
- 受支持的 market 初次采集及显式 daily/yearly 更新任务只有执行结果待核实时才创建独立任务；普通运行和从未开始不受干预。核实任务使用 `verification:task:<目标>` 和一个 evidence_check 项，与停止核实共用串行保护，不占用或释放原行情范围。
- EvidenceRepository 在同一 SQLite 读快照取得任务、完整项集合及固定版本证据。验证初次发布、新版本和 no-change 复用的收据/审计、compat digest、typed projection、yearly lineage 与任务身份；不查询 latest 来替代坏版本。旧式发布额外核对任务目标/范围、执行者与日/年发布意图。
- 应用层形成 sealed Plan。schema v8 的 `publication_checks` 保存 actor/time、完整 facts、核实前后快照及 receipt/digest。Adapter 在短写事务复查核实 revision/占用、原任务和证据指纹；只提交核实审计及核实终态，不写原任务、行情或 head。
- `AllRecorded` 表示所有项都有可核验提交证据，不表示写入者已停止。`InsufficientEvidence` 明确列出缺收据项并说明不能由缺失推断失败/未发布。原任务不再适用时记录 `NotApplicable`。部分已发布、运行中和未开始的事实不混同。
- runner 先查持久收据；提交响应丢失按实际收据核对。条件变化最多一次重新读取事实和条件提交，不循环、不重发原发布。取消只结束本次检查；旧运行/已运行无收据不自动重跑。证据损坏或检查异常且无可靠收据时显式保持 needs_verification 和串行保护，不伪造完整审计。
- 已有 AllRecorded 的新全局请求绑定请求级终态，不新建检查、不改写审计、不释放原占用；同请求重放、异参冲突及后续证据损坏拒绝均保留。get_request 的旧停止核实 wire 形状兼容，publication_check 通过 get_task_progress 读取并核验；非所有者不能借损坏诊断读取核实信息。
- init/upgrade 才显式建立/升级 v8；普通读取不迁移。生产执行仍为 OfficialFilePipeline，无调用者开关。线上 preview.4/schema7 不因本 PR 自动更新。

## AC 与验证入口

所有 Rust 测试随 `cargo test --locked` 进入 Actions；完整验证还包含 fmt、clippy --all-targets --all-features --locked -- -D warnings、build --locked、diff-check，以及离线 evidence 回归。服务器磁盘有限，本机只做格式/差异检查。准确 HEAD/run/job 与最终结果在 PR #105 / Issue #104 评论记录，本文不是独立的 PASS 声明。

1. AC1：`publication_facts_verify_new_and_reused_versions_without_mutating_tasks`、`publication_facts_verify_yearly_new_and_reuse_after_head_advances`、`publication_facts_verify_legacy_initial_publish_and_reject_receipt_damage`；初次发布、daily/yearly 新发布/复用及 fixed base 非 latest。
2. AC2：`publication_facts_reject_missing_success_receipt_and_damaged_fixed_data`、`legacy_publication_evidence_rejects_changed_execution_owner_or_period_intent`、两个 `publication_fact_snapshot_*` 真实两连接一致读；`publication_check_preserves_mixed_published_running_and_never_started_items` 验证混合事实及非所有者拒绝。
3. AC3：`publication_check_accepts_persisted_uncertainty_and_two_connections_replay_one_request`、`known_publication_two_connections_share_one_binding_and_failed_insert_leaves_none`；`publication_check_rejects_changed_target_or_cancelled_check_before_recording`、`publication_check_rejects_changed_evidence_even_when_receipt_and_task_are_unchanged`；`publication_check_real_exit_keeps_audit_and_completion_atomic_without_target_mutation`（真实 exit79）、`known_publication_real_exit_binds_only_after_commit_and_replays_without_new_check`（真实 exit78）。空的子进程入口测试不单独作为退出证据，父测试断言真实退出码及重开库状态。
4. AC4：`publication_runner_records_actual_conclusion_and_replays_it_without_target_mutation`、`publication_runner_cancels_only_pending_check_and_does_not_reexecute_old_or_running_checks`、`publication_runner_reconciles_response_loss_cancellation_and_bounded_conflicts`、`publication_runner_read_integrity_failure_marks_check_unresolved_without_false_receipt`；`malformed_evidence_port_result_cannot_leave_check_looking_normally_running` 是显式应用端口故障注入，不声称真实 SQLite 返回了伪造 facts。
5. AC5：`real_publication_stdio_inspects_fixed_evidence_without_changing_original_task` 使用真实 mcp_data 子进程验证 schema/发现/受理/worker/结果/取消/重放/错误；既有停止核实 stdio 保持。`range_controls_are_not_discovered_or_callable_without_owner_access` 同时覆盖两类核实。`scripts/smoke_release.py` 用隔离 synthetic 库实际检查 17 工具及从未开始任务拒绝，无未绑定任务副作用。
6. AC6：完整 CI 和 Preview package 的实际检查/报告；`publication_check_schema_upgrade_is_explicit_and_preserves_published_facts` 及既有 v6 升级/FK/回滚测试覆盖显式升级与旧数据保留。

## 尚未验证与后续边界

真实行情来源、#53 四股及新版 ChatGPT 消费侧调用是 NOT_RUN；fixture/stdout/tunnel health 不能代替真实来源或消费者验收。#54、#6/#46、#71 和 .day/gbbq 一致性门禁仍 NOT_VERIFIED，系统仍 NOT_PRODUCTION_READY。已开始写入者的停止协议、异常核实任务的显式处置、其他核实/恢复及数据闭环由后续任务推进；本核实不会自动解除这些保护。完整目标仍包括后续开发、重新部署与最终测试。

历史失败日志保留在对应 Actions：34743324844 为旧 v6 升级测试写死目标7；34744665115 为测试模块顺序 clippy；34745224699 为可折叠 if；34745325993 为新示例缺目标前置条件和待核实错误映射；34745703275 为 smoke 新增实际检查后旧计数断言未同步。修复未重写既有来源 evidence 或把未运行场景标 PASS。
