# 固定 daily 派生 yearly 应用链（#94）

依据 #3 当前本地年线规则、#4 AC-80—89 与 #79 已冻结发布矩阵。
SQLite 设计 c2f89085850423734193f3090dd435c34f4688ec；实施基线 2c706325aa2020382631355f802403c05b9662d8。
实现 base 5e3d645fa97506cb8640ccc6567253e4b4ec01f4。不修改开放设计 PR。

首个切片：Publisher.derive_yearly 只读 fixed daily，验证身份及证据绑定，调用现有精确聚合。
输入显式 symbol/base/年度集合/derived_at；DerivationEvidence 只供可信内部 verifier/test 装配，不支持反序列化/MCP。
完整取得说明须绑定 base digest、年度及 Shanghai as_of；不代表全年/上市以来完整，不生成 analysis_coverage_v1。
legacy record 保留 batch/digest；新增 local_yearly_derivation_v1 扩展保留逐年 lineage、未结束标记及原 daily 获取时间。
不修改任何旧 record。候选构造本身不新增 DDL；执行结果切片另有 schema v6，见下文。没有在线 provider。

prepare_derived_yearly 校验固定 base 未变，stage 候选并复用已有 Publisher.prepare。
sealed PublishPlan 的 base digest、daily/yearly head、revision/occupancy 条件继续由已有原子提交核对。
historical-fixed-base 不推进 head；current-head 两头任一变化拒绝，不降级、不 rebase。
候选构造不改变任务/head。prepare 失败可能留下未发布候选，与已发布事实严格区分。

AC → 测试（tests/yearly_publication.rs）：
- AC-01/03 coverage_binding_and_base_identity_reject_before_staging；fixed_base_damage_after_derivation_never_substitutes_another_version。
- AC-02/05 local_candidate_publishes_and_queries_fixed_year_with_original_daily_untouched：多年 daily 只聚合 2025，真实 SQLite 发布/固定查询，保留原 daily。
- AC-04 historical_base_can_publish_after_daily_advances_but_current_intent_conflicts。
- 既有 tests/sqlite_market.rs 继续覆盖双 head、task/occupancy、篡改与真实进程退出矩阵；不是新范围/worker 的证明。

显式内部 yearly 更新受理、范围保护及无变化原子结果已实现。尚未交付：public wire/worker/进度与部署。
内部受理/候选/结果可以分别测试，但尚未连成 public update_data(yearly) 执行链。
真实 verifier/source 尚未实现，#54/#71/#46/#6/文件一致性 NOT_VERIFIED，live source 与四股复验 NOT_RUN。
所有新增测试只使用固定 fixture；NOT_PRODUCTION_READY。不扩展到目录、来源研究或迁移。

本机仅 fmt/diff；完整编译和测试由 Actions 执行，准确 run/job/结果记录在 PR 评论。未执行不得称 PASS。

范围保护：current-head 的既有 yearly 若含候选未选择的年度，prepare 拒绝，不隐式删除或从不同 daily 拼接。明确 historical 意图仍可发布选定年度 immutable version，原 head 不变。测试 selected_historical_year_cannot_discard_other_years_from_effective_head 使用真实 SQLite 验证两条路径；不是自动降级。

## 显式任务切片（编码前记录）

新增内部 explicit-yearly-task-v1：每项 symbol、固定 daily version、年度集合，以及明确 historical/current-head 意图。年度去重排序，证券集合规范化；同证券不同年度仍共用股票 × yearly 占用。request_id 全局语义沿用 #78，不从执行状态重新选择范围。使用单次 fixed_many 核验全部基础版本并冻结 digest/years/intent；resolve 按服务端上海日期拒绝未来年度，不能用调用方的 as_of 绕过。日期依赖是应用装配输入，不是 MCP 字段。

不新增 DDL。prepare_derived_yearly 对 update/backfill 任务要求匹配此内部契约及精确冻结输入（包括 intent）；无法扩大年度/更换 base/改变 head 策略。旧首次采集 fixture 契约仅继续验证原发布基础设施，不被当作新受理证据。

测试计划：真实 SQLite 同语义乱序/重复幂等、异参 request 冲突、不同年度占用冲突；缺失/未来年度无绑定；两连接同时受理一个 request；取消/重试保持冻结事实；有效任务可发布，扩大年度或改变意图拒绝。

## 无变化执行结果（编码前记录）

#4 AC-84—89 要求同一口径/数值/截至日期/覆盖只记录执行，不能重复行情版本。新 schema v6 仅增加 yearly_execution_results，关联固定 daily、结果 yearly、task/item、no_change、完整候选审计与收据摘要。已有 publication_batches/task_publications 的“一个 immutable version 一次发布”约束不改。

应用层精确比较已有 yearly 与候选：固定 base/lineage、bars、单位/口径、覆盖说明、逐年参与日和未结束标记；仅 derived_at/as_of 日期改变且所有这些事实一致时复用。没有新版逐年证据的旧记录不能推断等价。base 变化、未结束标记或 coverage 变化均不是无变化。当前 head 意图的 prior 必须等于 expected_yearly_head；historical 显式 prior 可复用，但不推进 head。

复用路径不 stage 新版本，只把收据/审计与任务完成、占用释放同事务提交。变化路径复用已有条件发布并在同事务写结果。base/prior/result 完整性、head/revision/occupancy 仍须满足；提交后丢失响应按已持久化的同结果 ID 核验，不重复发布。

新增真实 SQLite 测试覆盖无变化/变化、coverage/年度状态变更、head冲突回滚、结果篡改拒绝、重放与显式 v5→v6 升级；既有 v4→最新测试同步从真正 v4 schema 构造，不伪造 migration 历史。完整编译仍走 Actions。

真实退出测试 yearly_real_exit_rolls_back_or_replays_committed_reuse_and_new_publication：父进程启动限定子测试，分别在 commit 前/后退出 79，覆盖新版本与无变化两条路径；重新打开 SQLite 验证结果/发布条数、task/occupancy，重放同一批准计划不重复版本。单独空跑 child 不是验证证据。

## 接线切片（编码前记录）

yearly worker 先核验已有 item 收据；旧 run/未确认 running 不重新执行。只接受已冻结的 yearly task，固定本次 derived_at；可信 YearlyVerifier 返回绑定 digest/years/as_of 的范围证据后才调用本地聚合与结果提交。生产仅装配 OfficialFilePipeline 的拒绝门禁，不通过重新下载 daily 或外部 year 获取证据。fixture verifier 只能由 Rust 测试注入，无 MCP provider 参数。

公开 update_data/backfill_data 增加封闭 market.yearly 分支，保留已有 daily 输入。每项 entity/base_daily_version_id/years/publication 明确；publication 的 mode 为 historical_fixed_base 或 current_head，后者显式 expected_yearly_head（可空），expected daily 恒为该固定 base。historical 可指定 comparison_version_id 以核验并复用原 immutable yearly。比较版本与 digest 在受理时冻结；current 比较版本就是 expected yearly head。禁止通过字段切换来源或声明覆盖通过。新请求的未来年校验读取调用时服务器上海日期，不把进程启动日永久缓存。

进度只读返回 frozen_yearly_inputs/yearly_results，并在同一快照核验 task、收据和固定版本；成功项缺收据、cursor 后证据损坏均拒绝。能力发现仍 blocked/NOT_PRODUCTION_READY，接口就绪不代表真实覆盖 verifier 已实现。
